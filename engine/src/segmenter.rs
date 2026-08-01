//! VAD セグメント方式の逐次認識コア。`poc/poc_vad.py` の `Segmenter` の移植。
//!
//! 設計の根拠は docs/HANDOFF.md §4 を参照:
//! - offline モデルで全バッファを再デコードすると、無音が伸びるだけで
//!   確定済みテキストが壊れる → VAD で区切り、確定区間は 1 度だけデコード
//! - sherpa の max_speech_duration はハードキャップとして機能しない
//!   （12 秒指定で 32 秒の区間が出る）→ 自前で上限を掛ける
//! - パディングと取りこぼし回収の規則は実音声のスイープで決定

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, VadModelConfig,
    VoiceActivityDetector,
};

use crate::audio::{is_speech, quietest_offset};
use crate::text::strip_japanese_spaces;

pub const SAMPLE_RATE: usize = 16_000;
/// silero が要求する窓長（16kHz）
const VAD_WINDOW: usize = 512;
/// これ未満の区間はデコードしない
const MIN_SEGMENT_SAMPLES: usize = SAMPLE_RATE / 5;

/// 実測でスイープして決めた調整項目（値の根拠は docs/HANDOFF.md §4）。
#[derive(Clone, Debug)]
pub struct Params {
    /// 確定区間の前パディング。語頭の食い込みを戻す
    pub pad_head: f32,
    /// 確定区間の後パディング。無声化した「です・ます」の語尾を拾う。
    /// そこに声が乗っている（発話が続いている）場合は伸ばさない
    pub pad_tail: f32,
    /// 部分デコードの遡り幅（VAD の検出遅れの吸収）
    pub lookback: f32,
    /// 取りこぼしを拾う未転写区間の上限
    pub max_leading_gap: f32,
    /// これ未満の間は文中の息継ぎとみなし区切らない
    pub min_silence: f32,
    /// 無区切りで話し続けた場合の強制確定
    pub max_speech: f32,
    /// silero の発話判定しきい値
    pub threshold: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            pad_head: 0.3,
            pad_tail: 0.8,
            lookback: 1.0,
            max_leading_gap: 3.0,
            min_silence: 0.5,
            max_speech: 25.0,
            threshold: 0.5,
        }
    }
}

/// SenseVoice モデルのファイル配置。
pub struct SenseVoicePaths {
    pub model: String,
    pub tokens: String,
}

pub fn sensevoice(paths: &SenseVoicePaths, threads: i32) -> OfflineRecognizer {
    // 注意: Rust バインディングの Default は全フィールド 0/None で、
    // sherpa の推奨既定値は入らない。必要な値は全て明示する
    let mut config = OfflineRecognizerConfig {
        model_config: sherpa_onnx::OfflineModelConfig {
            sense_voice: OfflineSenseVoiceModelConfig {
                model: Some(paths.model.clone()),
                language: Some("ja".into()),
                use_itn: true,
            },
            tokens: Some(paths.tokens.clone()),
            num_threads: threads,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        ..Default::default()
    };
    // sys::FeatureConfig は再エクスポートされていないため、フィールド代入で設定する
    config.feat_config.sample_rate = SAMPLE_RATE as i32;
    config.feat_config.feature_dim = 80;
    OfflineRecognizer::create(&config).expect("SenseVoice の読み込みに失敗")
}

fn build_vad(model: &str, params: &Params) -> VoiceActivityDetector {
    let config = VadModelConfig {
        silero_vad: sherpa_onnx::SileroVadModelConfig {
            model: Some(model.into()),
            threshold: params.threshold,
            min_silence_duration: params.min_silence,
            // Python バインディングの既定値に合わせる（パラメータ化していない）
            min_speech_duration: 0.25,
            window_size: VAD_WINDOW as i32,
            max_speech_duration: params.max_speech,
        },
        sample_rate: SAMPLE_RATE as i32,
        num_threads: 1,
        provider: Some("cpu".into()),
        debug: false,
        ..Default::default()
    };
    VoiceActivityDetector::create(&config, 60.0).expect("silero VAD の読み込みに失敗")
}

/// 録音を VAD で区切り、確定テキストと進行中の部分テキストを管理する。
pub struct Segmenter<'a> {
    recognizer: &'a OfflineRecognizer,
    vad: VoiceActivityDetector,
    /// 追記専用の録音バッファ
    recording: Vec<f32>,
    pub committed: Vec<String>,
    /// (開始サンプル, 長さ)
    pub segments: Vec<(usize, usize)>,
    pub partial: String,
    pad_head: usize,
    pad_tail: usize,
    lookback: usize,
    max_speech: usize,
    max_leading_gap: usize,
    /// ここまでは確定済み（重複デコードを防ぐ）
    committed_until: usize,
    /// VAD に投入済みのサンプル数
    fed: usize,
    /// 進行中の発話の開始位置
    speech_start: Option<usize>,
}

impl<'a> Segmenter<'a> {
    pub fn new(recognizer: &'a OfflineRecognizer, vad_model: &str, params: &Params) -> Self {
        let s = |secs: f32| (secs * SAMPLE_RATE as f32) as usize;
        Self {
            recognizer,
            vad: build_vad(vad_model, params),
            recording: Vec::with_capacity(SAMPLE_RATE * 30),
            committed: Vec::new(),
            segments: Vec::new(),
            partial: String::new(),
            pad_head: s(params.pad_head),
            pad_tail: s(params.pad_tail),
            lookback: s(params.lookback),
            max_speech: s(params.max_speech),
            max_leading_gap: s(params.max_leading_gap),
            committed_until: 0,
            fed: 0,
            speech_start: None,
        }
    }

    pub fn recording(&self) -> &[f32] {
        &self.recording
    }

    fn decode(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.recognizer.decode(&stream);
        let text = stream.get_result().map(|r| r.text).unwrap_or_default();
        strip_japanese_spaces(&text)
    }

    /// [begin, end) を確定させる。end は後パディング込み、speech_end は
    /// 実際に発話が終わった位置で、次の区間の開始下限になる。
    fn commit(&mut self, begin: usize, end: usize, speech_end: usize) -> Option<String> {
        // 直前の未転写区間に声が乗っていれば取りこぼしなので含める。
        // silero は起動直後や話し始めの検出が遅れることがあり、
        // 前パディングだけでは戻らない。無音なら含めない
        let mut begin = begin;
        if begin > self.committed_until {
            let gap = &self.recording[self.committed_until..begin];
            if gap.len() <= self.max_leading_gap && is_speech(gap) {
                begin = self.committed_until;
            }
        }
        let begin = begin.max(self.committed_until);
        if end.saturating_sub(begin) < MIN_SEGMENT_SAMPLES {
            return None;
        }
        let text = self.decode(&self.recording[begin..end]);
        self.committed_until = speech_end;
        self.committed.push(text.clone());
        self.segments.push((begin, end - begin));
        self.partial.clear();
        Some(text)
    }

    fn drain_vad(&mut self, finalized: &mut Vec<String>) {
        while !self.vad.is_empty() {
            let Some(seg) = self.vad.front() else { break };
            self.vad.pop();
            let start = seg.start() as usize;
            let speech_end = start + seg.samples().len();
            let mut end = (speech_end + self.pad_tail).min(self.recording.len());
            // 後パディングは、そこが無音のときだけ意味がある（無声化した
            // 語尾を拾うため）。発話が続いている最中に VAD が切った場合に
            // 伸ばすと、次の区間との境界が語中に落ちて単語が割れる
            if is_speech(&self.recording[speech_end..end]) {
                end = speech_end;
            }
            if let Some(text) = self.commit(start.saturating_sub(self.pad_head), end, speech_end) {
                finalized.push(text);
            }
            self.speech_start = None;
        }
    }

    /// 強制分割の位置。直前 2 秒のうち最も静かな 100ms を選び、
    /// 単語の途中で切る確率を下げる。
    fn split_point(&self, begin: usize, limit: usize) -> usize {
        let hop = SAMPLE_RATE / 10;
        let window_start = begin.max(limit.saturating_sub(2 * SAMPLE_RATE));
        let window = &self.recording[window_start..limit];
        if window.len() < hop * 2 {
            return limit;
        }
        window_start + quietest_offset(window, hop) + hop / 2
    }

    /// 音声を追加し、この呼び出しで新たに確定したテキストを返す。
    pub fn push(&mut self, samples: &[f32]) -> Vec<String> {
        self.recording.extend_from_slice(samples);
        let mut finalized = Vec::new();
        while self.fed + VAD_WINDOW <= self.recording.len() {
            self.vad
                .accept_waveform(&self.recording[self.fed..self.fed + VAD_WINDOW]);
            self.fed += VAD_WINDOW;
            if self.speech_start.is_none() && self.vad.detected() {
                self.speech_start = Some((self.fed - VAD_WINDOW).saturating_sub(self.lookback));
            }
            self.drain_vad(&mut finalized);

            // sherpa の max_speech_duration はハードキャップにならないため
            // 自前で上限を掛ける
            if let Some(start) = self.speech_start {
                if self.fed - start > self.max_speech {
                    let split = self.split_point(start, self.fed);
                    if let Some(text) = self.commit(start, split, split) {
                        finalized.push(text);
                    }
                    self.speech_start = Some(self.committed_until);
                }
            }
        }
        finalized
    }

    /// 録音終了時に、進行中の発話を確定させる。
    pub fn flush(&mut self) -> Vec<String> {
        self.vad.flush();
        let mut finalized = Vec::new();
        self.drain_vad(&mut finalized);
        self.speech_start = None;
        self.partial.clear();
        finalized
    }

    /// 進行中の発話を再デコードする（部分表示用）。無発話なら None。
    pub fn update_partial(&mut self) -> Option<&str> {
        let start = self.speech_start?;
        self.partial = self.decode(&self.recording[start..]);
        Some(&self.partial)
    }

    /// 確定 + 部分の全文。
    pub fn text(&self) -> String {
        format!("{}{}", self.committed.concat(), self.partial)
    }
}
