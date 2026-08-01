//! Python 版とのパリティ検証用。wav を 400ms ずつ流して確定テキストを出す。
//!
//! 使い方:
//!   parity [--sensevoice-dir DIR] [--vad-model PATH] a.wav b.wav …
//! 出力: 1 行につき `ファイル名\t確定テキスト連結`

use std::path::PathBuf;

use kikitori_engine::segmenter::{sensevoice, Params, Segmenter, SenseVoicePaths, SAMPLE_RATE};

fn main() {
    let mut sensevoice_dir = dirs_fallback_home()
        .join(".local/share/voxtype/models/sensevoice-small")
        .to_string_lossy()
        .into_owned();
    let mut vad_model = "models/silero_vad.onnx".to_string();
    let mut wavs: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sensevoice-dir" => sensevoice_dir = args.next().expect("--sensevoice-dir の値"),
            "--vad-model" => vad_model = args.next().expect("--vad-model の値"),
            _ => wavs.push(arg),
        }
    }
    assert!(!wavs.is_empty(), "wav ファイルを指定してください");

    let paths = SenseVoicePaths {
        model: format!("{sensevoice_dir}/model.int8.onnx"),
        tokens: format!("{sensevoice_dir}/tokens.txt"),
    };
    let recognizer = sensevoice(&paths, 16);

    for wav in &wavs {
        let samples = read_wav_16k_mono(wav);
        let mut seg = Segmenter::new(&recognizer, &vad_model, &Params::default());
        let step = (0.4 * SAMPLE_RATE as f32) as usize;
        for chunk in samples.chunks(step) {
            seg.push(chunk);
        }
        seg.flush();
        let name = PathBuf::from(wav)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        println!("{name}\t{}", seg.committed.concat());
    }
}

fn read_wav_16k_mono(path: &str) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("wav を開けない");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate as usize, SAMPLE_RATE, "16kHz が必要");
    assert_eq!(spec.channels, 1, "mono が必要");
    reader
        .samples::<i16>()
        .map(|s| s.expect("wav 読み取り") as f32 / 32768.0)
        .collect()
}

fn dirs_fallback_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME が未設定"))
}
