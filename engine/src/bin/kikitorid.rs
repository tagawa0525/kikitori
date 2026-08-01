//! kikitori エンジンデーモン。docs/PROTOCOL.md v0 を Unix ソケットで話す。
//!
//! モデルは起動時に読み込んで常駐する（クライアントはトグル時に接続する
//! だけなので、モデル読み込みとキャプチャの順序問題が構造的に起きない）。
//! 接続 = セッションとして並行に処理し、無音が続いたセッションは
//! タイムアウトで打ち切る（切り忘れ対策）。

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;

use kikitori_engine::audio::is_speech;
use kikitori_engine::replace::Replacer;
use kikitori_engine::segmenter::{sensevoice, Params, Segmenter, SenseVoicePaths, SAMPLE_RATE};
use kikitori_proto::{self as proto, read_frame, write_frame};
use sherpa_onnx::OfflineRecognizer;

/// PARTIAL を送る周期（受信サンプル数基準。壁時計ではなくデータ量で
/// 決めることで、wav を流す検証でも実時間でも同じ挙動になる）
const PARTIAL_EVERY_SAMPLES: usize = (0.4 * SAMPLE_RATE as f32) as usize;

struct Args {
    socket: PathBuf,
    sensevoice_dir: String,
    vad_model: String,
    replace_file: PathBuf,
    threads: i32,
    idle_timeout_secs: u64,
    /// 例: 0.0.0.0:41717。LAN 内の別マシン（x1ng1 等）からエンジンを使う。
    /// 認証は持たないため、信頼できる LAN / SSH トンネル / Tailscale 前提
    tcp: Option<String>,
}

/// 推論スレッド数の既定値: 論理 CPU 数の半分（SMT 環境ではおおむね物理
/// コア数。r995: 32→16 は実測済みの値と一致、x1ng1: 8→4）。
/// CPU 数を取得できない環境では 4 に落とす。--threads で上書き可能
fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(4) as i32
}

fn parse_args() -> Args {
    let home = std::env::var("HOME").expect("HOME が未設定");
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("{home}/.cache"));
    let config_dir = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    let mut args = Args {
        socket: PathBuf::from(format!("{runtime_dir}/kikitori.sock")),
        sensevoice_dir: format!("{home}/.local/share/voxtype/models/sensevoice-small"),
        vad_model: "models/silero_vad.onnx".into(),
        replace_file: PathBuf::from(format!("{config_dir}/kikitori/replace.tsv")),
        threads: default_threads(),
        idle_timeout_secs: 120,
        tcp: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{arg} の値がない"));
        match arg.as_str() {
            "--socket" => args.socket = PathBuf::from(value()),
            "--sensevoice-dir" => args.sensevoice_dir = value(),
            "--vad-model" => args.vad_model = value(),
            "--replace" => args.replace_file = PathBuf::from(value()),
            "--threads" => args.threads = value().parse().expect("--threads は整数"),
            "--idle-timeout" => {
                args.idle_timeout_secs = value().parse().expect("--idle-timeout は秒数")
            }
            "--tcp" => args.tcp = Some(value()),
            _ => panic!("未知の引数: {arg}"),
        }
    }
    args
}

fn main() {
    let args = parse_args();
    let paths = SenseVoicePaths {
        model: format!("{}/model.int8.onnx", args.sensevoice_dir),
        tokens: format!("{}/tokens.txt", args.sensevoice_dir),
    };
    let replacer = match std::fs::read_to_string(&args.replace_file) {
        Ok(text) => {
            let r = Replacer::parse(&text);
            eprintln!(
                "置換辞書: {} ルール ({})",
                r.len(),
                args.replace_file.display()
            );
            r
        }
        Err(_) => Replacer::parse(""),
    };
    eprintln!("モデル読み込み中…（threads={}）", args.threads);
    let recognizer = sensevoice(&paths, args.threads);
    eprintln!("読み込み完了");

    // 前回の異常終了で残ったソケットを掃除する
    let _ = std::fs::remove_file(&args.socket);
    let listener = UnixListener::bind(&args.socket)
        .unwrap_or_else(|e| panic!("{} に bind できない: {e}", args.socket.display()));
    // ローカル IPC 前提でも他ユーザーからの接続は絞る
    // （XDG_RUNTIME_DIR が無く ~/.cache へフォールバックした場合への備え）
    let _ = std::fs::set_permissions(
        &args.socket,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    );
    eprintln!("listening: {}", args.socket.display());

    // 接続 = セッション。全接続を同一に扱い、main は共有状態を持たない。
    // 以前は「最新接続が先行セッションを蹴る」preemption を実装したが、
    // その調整機構（Weak 保持・shutdown・join）自体が追加の状態であり、
    // 実際に fd リークによるデッドロックを生んだため撤去した（ユーザー指摘）。
    // 切り忘れセッションは無音タイムアウトが回収する
    let recognizer = Arc::new(recognizer);
    let replacer = Arc::new(replacer);
    let ctx = Arc::new(SessionCtx {
        recognizer,
        replacer,
        vad_model: args.vad_model.clone(),
        idle_timeout_secs: args.idle_timeout_secs,
    });

    if let Some(addr) = args.tcp.clone() {
        // bind 失敗はここ（メインスレッド）で失敗終了させる。スレッド内で
        // panic すると Unix 側だけで起動継続し、TCP が無効なのに成功扱いになる
        let tcp_listener = std::net::TcpListener::bind(&addr)
            .unwrap_or_else(|e| panic!("{addr} に bind できない: {e}"));
        eprintln!("listening (tcp): {addr}");
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for stream in tcp_listener.incoming() {
                match stream {
                    Ok(s) => {
                        let _ = s.set_nodelay(true); // PARTIAL の遅延を抑える
                        let reader = match s.try_clone() {
                            Ok(r) => r,
                            Err(e) => {
                                eprintln!("ソケット複製に失敗: {e}");
                                continue;
                            }
                        };
                        spawn_session(ctx.clone(), reader, s);
                    }
                    Err(e) => eprintln!("accept 失敗 (tcp): {e}"),
                }
            }
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let reader = match s.try_clone() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("ソケット複製に失敗: {e}");
                        continue;
                    }
                };
                spawn_session(ctx.clone(), reader, s);
            }
            Err(e) => eprintln!("accept 失敗: {e}"),
        }
    }
}

struct SessionCtx {
    recognizer: Arc<OfflineRecognizer>,
    replacer: Arc<Replacer>,
    vad_model: String,
    idle_timeout_secs: u64,
}

/// トランスポート非依存のセッション起動（Unix / TCP 共通）
fn spawn_session(
    ctx: Arc<SessionCtx>,
    reader: impl Read + Send + 'static,
    writer: impl Write + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = handle(
            BufReader::new(reader),
            BufWriter::new(writer),
            &ctx.recognizer,
            &ctx.vad_model,
            &ctx.replacer,
            ctx.idle_timeout_secs,
        );
        if let Err(e) = result {
            // 切断は正常系（クライアントが閉じただけ）
            if e.kind() != std::io::ErrorKind::UnexpectedEof {
                eprintln!("接続エラー: {e}");
            }
        }
    });
}

fn send_text(w: &mut impl Write, kind: u8, text: &str) -> std::io::Result<()> {
    let payload = serde_json::json!({ "text": text }).to_string();
    write_frame(w, kind, payload.as_bytes())?;
    w.flush()
}

fn handle(
    mut reader: impl Read,
    mut writer: impl Write,
    recognizer: &OfflineRecognizer,
    vad_model: &str,
    replacer: &Replacer,
    idle_timeout_secs: u64,
) -> std::io::Result<()> {
    let mut segmenter: Option<Segmenter> = None;
    let mut next_partial_at = PARTIAL_EVERY_SAMPLES;
    // 無音タイムアウト: 切り忘れセッションの回収。STOPPED は送らず接続を
    // 閉じるだけにする（クライアントは切断検知で入力せずに終了するため、
    // 放置後にフォーカス先へ誤入力する事故が構造的に起きない）
    let idle_limit_samples = idle_timeout_secs as usize * SAMPLE_RATE;
    let mut silent_samples: usize = 0;

    loop {
        let frame = read_frame(&mut reader)?;
        match frame.kind {
            proto::HELLO => {
                let payload = serde_json::json!({
                    "version": proto::VERSION,
                    "model": "sensevoice-small-int8",
                })
                .to_string();
                write_frame(&mut writer, proto::READY, payload.as_bytes())?;
                writer.flush()?;
            }
            proto::START => {
                segmenter = Some(Segmenter::new(recognizer, vad_model, &Params::default()));
                next_partial_at = PARTIAL_EVERY_SAMPLES;
                silent_samples = 0;
            }
            proto::AUDIO => {
                let Some(seg) = segmenter.as_mut() else {
                    let payload = serde_json::json!({ "message": "START が先" }).to_string();
                    write_frame(&mut writer, proto::ERROR, payload.as_bytes())?;
                    writer.flush()?;
                    continue;
                };
                if frame.payload.len() % 2 != 0 {
                    let payload =
                        serde_json::json!({ "message": "AUDIO は s16le（偶数バイト）が必要" })
                            .to_string();
                    write_frame(&mut writer, proto::ERROR, payload.as_bytes())?;
                    writer.flush()?;
                    continue;
                }
                let samples: Vec<f32> = frame
                    .payload
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect();
                if is_speech(&samples) {
                    silent_samples = 0;
                } else {
                    silent_samples += samples.len();
                    if idle_limit_samples > 0 && silent_samples > idle_limit_samples {
                        eprintln!("無音 {idle_timeout_secs} 秒でセッションを打ち切る");
                        return Ok(());
                    }
                }
                for text in seg.push(&samples) {
                    send_text(&mut writer, proto::COMMIT, &replacer.apply(&text))?;
                }
                if seg.recording().len() >= next_partial_at {
                    next_partial_at = seg.recording().len() + PARTIAL_EVERY_SAMPLES;
                    if let Some(partial) = seg.update_partial() {
                        let partial = replacer.apply(partial);
                        send_text(&mut writer, proto::PARTIAL, &partial)?;
                    }
                }
            }
            proto::STOP => {
                if let Some(mut seg) = segmenter.take() {
                    for text in seg.flush() {
                        send_text(&mut writer, proto::COMMIT, &replacer.apply(&text))?;
                    }
                }
                write_frame(&mut writer, proto::STOPPED, b"{}")?;
                writer.flush()?;
            }
            other => {
                let payload =
                    serde_json::json!({ "message": format!("未知の型 0x{other:02X}") }).to_string();
                write_frame(&mut writer, proto::ERROR, payload.as_bytes())?;
                writer.flush()?;
            }
        }
    }
}
