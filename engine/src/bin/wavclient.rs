//! kikitorid の検証クライアント。wav をプロトコル経由で流し、
//! COMMIT の連結を `ファイル名\tテキスト` で出力する（parity と同形式）。

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use kikitori_engine::segmenter::SAMPLE_RATE;
use kikitori_proto::{self as proto, read_frame, write_frame};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let socket = match args.iter().position(|a| a == "--socket") {
        Some(i) => {
            args.remove(i);
            args.remove(i)
        }
        None => {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
            format!("{runtime_dir}/kikitori.sock")
        }
    };
    assert!(!args.is_empty(), "wav ファイルを指定してください");

    let stream = UnixStream::connect(&socket).expect("エンジンに接続できない");
    let mut writer = BufWriter::new(stream.try_clone().unwrap());
    let reader = BufReader::new(stream);

    // 受信は別スレッド（送信だけ先行してソケットバッファが詰まると
    // 双方 write でデッドロックするため）
    let receiver = std::thread::spawn(move || collect_events(reader));

    write_frame(&mut writer, proto::HELLO, br#"{"version":0}"#).unwrap();
    writer.flush().unwrap();

    let step = (0.4 * SAMPLE_RATE as f32) as usize;
    for wav in &args {
        write_frame(&mut writer, proto::START, b"{}").unwrap();
        for chunk in read_wav_bytes(wav).chunks(step * 2) {
            write_frame(&mut writer, proto::AUDIO, chunk).unwrap();
        }
        write_frame(&mut writer, proto::STOP, b"{}").unwrap();
        writer.flush().unwrap();
    }
    // write 方向を閉じてサーバに EOF を伝える。これがないと受信スレッドが
    // 最後の STOPPED の後も次のフレームを待ち続け、join が返らない
    writer
        .into_inner()
        .unwrap()
        .shutdown(std::net::Shutdown::Write)
        .unwrap();

    let sessions = receiver.join().unwrap();
    assert_eq!(
        sessions.len(),
        args.len(),
        "STOPPED の数がファイル数と不一致"
    );
    for (wav, texts) in args.iter().zip(sessions) {
        let name = PathBuf::from(wav)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        println!("{name}\t{}", texts.concat());
    }
}

/// STOPPED ごとに区切った COMMIT テキスト列を返す。
fn collect_events(mut reader: impl std::io::Read) -> Vec<Vec<String>> {
    let mut sessions = Vec::new();
    let mut current = Vec::new();
    loop {
        let Ok(frame) = read_frame(&mut reader) else {
            return sessions;
        };
        match frame.kind {
            proto::COMMIT => {
                let v: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
                current.push(v["text"].as_str().unwrap().to_owned());
            }
            proto::STOPPED => sessions.push(std::mem::take(&mut current)),
            proto::PARTIAL | proto::READY => {}
            proto::ERROR => panic!(
                "エンジンがエラーを返した: {}",
                String::from_utf8_lossy(&frame.payload)
            ),
            other => panic!("未知の型 0x{other:02X}"),
        }
    }
}

/// wav を s16le のバイト列として読む（16kHz mono 前提）。
fn read_wav_bytes(path: &str) -> Vec<u8> {
    let mut reader = hound::WavReader::open(path).expect("wav を開けない");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate as usize, SAMPLE_RATE, "16kHz が必要");
    assert_eq!(spec.channels, 1, "mono が必要");
    reader
        .samples::<i16>()
        .flat_map(|s| s.expect("wav 読み取り").to_le_bytes())
        .collect()
}
