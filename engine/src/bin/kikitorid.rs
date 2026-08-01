//! kikitori エンジンデーモン。docs/PROTOCOL.md v0 を Unix ソケットで話す。
//!
//! モデルは起動時に読み込んで常駐する（クライアントはトグル時に接続する
//! だけなので、モデル読み込みとキャプチャの順序問題が構造的に起きない）。
//! v0 は接続を 1 本ずつ順次処理する（クライアントは自分たちのみのため）。

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

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
        threads: 16,
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
    eprintln!("モデル読み込み中…");
    let recognizer = sensevoice(&paths, args.threads);
    eprintln!("読み込み完了");

    // 前回の異常終了で残ったソケットを掃除する
    let _ = std::fs::remove_file(&args.socket);
    let listener = UnixListener::bind(&args.socket)
        .unwrap_or_else(|e| panic!("{} に bind できない: {e}", args.socket.display()));
    eprintln!("listening: {}", args.socket.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle(stream, &recognizer, &args.vad_model, &replacer) {
                    // 切断は正常系（クライアントが閉じただけ）
                    if e.kind() != std::io::ErrorKind::UnexpectedEof {
                        eprintln!("接続エラー: {e}");
                    }
                }
            }
            Err(e) => eprintln!("accept 失敗: {e}"),
        }
    }
}

fn send_text(w: &mut impl Write, kind: u8, text: &str) -> std::io::Result<()> {
    let payload = serde_json::json!({ "text": text }).to_string();
    write_frame(w, kind, payload.as_bytes())?;
    w.flush()
}

fn handle(
    stream: UnixStream,
    recognizer: &OfflineRecognizer,
    vad_model: &str,
    replacer: &Replacer,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    let mut segmenter: Option<Segmenter> = None;
    let mut next_partial_at = PARTIAL_EVERY_SAMPLES;

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
            }
            proto::AUDIO => {
                let Some(seg) = segmenter.as_mut() else {
                    let payload = serde_json::json!({ "message": "START が先" }).to_string();
                    write_frame(&mut writer, proto::ERROR, payload.as_bytes())?;
                    writer.flush()?;
                    continue;
                };
                let samples: Vec<f32> = frame
                    .payload
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect();
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
