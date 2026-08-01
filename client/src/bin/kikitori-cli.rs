//! CLI クライアント: マイク → kikitorid → ターミナル逐次表示。
//! poc/poc_vad.py のマイクモードの Rust 置き換え（GUI なし版）。
//!
//! 実行: kikitori-cli [--socket PATH] [--wtype] [--save rec.wav は未対応]
//! Ctrl+C で停止 → 確定テキストを表示（--wtype なら wtype で入力）。
//!
//! キャプチャはデーモン接続より先に開始する（接続やセッション開始の
//! 遅延中に話した分を取りこぼさないため。docs/HANDOFF.md §4 の教訓）。

use std::io::{BufReader, BufWriter, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use kikitori_client::audio::{downmix, downsample, f32_to_s16le};
use kikitori_proto::{self as proto, read_frame, write_frame};

const TARGET_RATE: u32 = 16_000;

fn main() {
    let mut socket = {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
        format!("{runtime_dir}/kikitori.sock")
    };
    let mut use_wtype = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--socket" => socket = it.next().expect("--socket の値"),
            "--wtype" => use_wtype = true,
            _ => panic!("未知の引数: {arg}"),
        }
    }

    // ---- キャプチャを先に開始（モデルやソケットより前） ----
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>();
    let host = cpal::default_host();
    let device = host.default_input_device().expect("入力デバイスがない");
    let config = pick_input_config(&device);
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    assert_eq!(
        rate % TARGET_RATE,
        0,
        "サンプルレート {rate} が 16kHz の整数倍でない（リサンプラ未実装）"
    );
    let factor = (rate / TARGET_RATE) as usize;
    let stream = device
        .build_input_stream(
            config.config(),
            move |data: &[f32], _| {
                let mono = downmix(data, channels);
                let _ = audio_tx.send(f32_to_s16le(&downsample(&mono, factor)));
            },
            |e| eprintln!("キャプチャエラー: {e}"),
            None,
        )
        .expect("入力ストリームを開けない");
    stream.play().expect("キャプチャ開始に失敗");
    eprintln!(
        "録音開始: {} ({}ch {}Hz → mono 16kHz)",
        device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_default(),
        channels,
        rate,
    );

    // ---- デーモン接続 ----
    let conn = UnixStream::connect(&socket).unwrap_or_else(|e| {
        panic!("エンジン {socket} に接続できない: {e}（kikitorid は起動済みか）")
    });
    let mut writer = BufWriter::new(conn.try_clone().unwrap());
    let reader = BufReader::new(conn.try_clone().unwrap());
    write_frame(&mut writer, proto::HELLO, br#"{"version":0}"#).unwrap();
    write_frame(&mut writer, proto::START, b"{}").unwrap();
    writer.flush().unwrap();

    // ---- 受信スレッド: 表示と確定テキストの収集 ----
    let (done_tx, done_rx) = mpsc::channel::<Vec<String>>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut committed: Vec<String> = Vec::new();
        loop {
            let Ok(frame) = read_frame(&mut reader) else {
                let _ = done_tx.send(committed);
                return;
            };
            let text = |p: &[u8]| -> String {
                serde_json::from_slice::<serde_json::Value>(p)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            };
            match frame.kind {
                proto::READY => eprintln!("エンジン準備完了。話してください（Ctrl+C で終了）"),
                proto::PARTIAL => {
                    let t = text(&frame.payload);
                    let chars: Vec<char> = t.chars().collect();
                    let tail: String = chars[chars.len().saturating_sub(40)..].iter().collect();
                    eprint!("\r\x1b[K… {tail}");
                }
                proto::COMMIT => {
                    let t = text(&frame.payload);
                    eprintln!("\r\x1b[K確定: {t}");
                    committed.push(t);
                }
                proto::STOPPED => {
                    let _ = done_tx.send(committed);
                    return;
                }
                proto::ERROR => eprintln!(
                    "\r\x1b[Kエンジンエラー: {}",
                    String::from_utf8_lossy(&frame.payload)
                ),
                _ => {}
            }
        }
    });

    // ---- Ctrl+C まで音声を送り続ける ----
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))
            .expect("SIGINT ハンドラ設定に失敗");
    }
    while running.load(Ordering::SeqCst) {
        match audio_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(bytes) => {
                write_frame(&mut writer, proto::AUDIO, &bytes).unwrap();
                writer.flush().unwrap();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(stream);

    // ---- 停止: flush 分の COMMIT を受け切ってから出力 ----
    write_frame(&mut writer, proto::STOP, b"{}").unwrap();
    writer.flush().unwrap();
    let committed = done_rx.recv().expect("受信スレッドが異常終了");
    let text = committed.concat();
    println!("\n--- 最終結果 ---\n{text}");
    if use_wtype && !text.is_empty() {
        let status = std::process::Command::new("wtype").arg(&text).status();
        match status {
            Ok(s) if s.success() => eprintln!("wtype で入力済み"),
            other => eprintln!("wtype 失敗: {other:?}"),
        }
    }
}

/// f32 入力の構成を選ぶ。16kHz mono を最優先し、なければ 48kHz 等
/// 16kHz の整数倍 + 任意チャンネル数を選ぶ（変換は自前で行う）。
fn pick_input_config(device: &cpal::Device) -> cpal::SupportedStreamConfig {
    let supported: Vec<_> = device
        .supported_input_configs()
        .expect("入力構成を取得できない")
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .collect();
    // 16kHz の整数倍で表現できる構成を、レートが低い順に試す
    for &rate in &[TARGET_RATE, 32_000, 48_000, 96_000] {
        for c in &supported {
            if c.min_sample_rate() <= rate && rate <= c.max_sample_rate() {
                return c.with_sample_rate(rate);
            }
        }
    }
    device.default_input_config().expect("既定の入力構成がない")
}
