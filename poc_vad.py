#!/usr/bin/env python3
"""VAD セグメント方式の逐次デコード PoC（poc_mic.py の後継）。

poc_mic.py は録音バッファ全体を毎回再デコードしていたため、後ろに無音が
伸びると確定済みの部分まで壊れた（offline zipformer は全バッファに
attention をかけるため）。実測では 6.6 秒の発話に 32 秒の無音を足すだけで
語尾が欠落した。

ここでは VAD で発話の区切りを検出し、
  - 確定セグメント: 1 度だけデコードして以後不変
  - 進行中の発話のみ: 400ms ごとに再デコード
とすることで、確定テキストが後から壊れない・デコード時間が発話長で
頭打ちになる、という二点を同時に解決する。

VAD は区切りの決定にのみ使い、デコードは前後 PAD_SECS を足した区間で行う。
VAD の切り出しは語頭に食い込むため、パディングなしでは「ヤンバルクイナ」が
「クイナ」になるなど語頭が落ちる（実測）。

実行:
  python3 poc_vad.py                      # マイク（Ctrl+C で終了）
  python3 poc_vad.py --list               # 入力デバイス一覧
  python3 poc_vad.py --wav path.wav       # ファイルで区切り動作を確認
"""

import argparse
import time
import wave

import numpy as np
import sherpa_onnx
import sounddevice as sd

from poc_incremental import MODEL_DIR, SAMPLE_RATE, STEP_SECS
from poc_mic import level_bar, list_devices, parse_device

VAD_MODEL = MODEL_DIR.parent / "silero_vad.onnx"
VAD_WINDOW = 512  # silero が要求する窓長（16kHz）
PAD_SECS = 0.3  # 確定セグメントのデコード区間の前後パディング
PARTIAL_LOOKBACK_SECS = 1.0  # 部分デコードの遡り幅（VAD の検出遅れの吸収）
MIN_SILENCE_SECS = 0.8  # これ未満の間は文中の息継ぎとみなし区切らない
MAX_SPEECH_SECS = 15.0  # 無区切りで話し続けた場合の強制確定


class Recording:
    """追記専用の録音バッファ。毎周期 concatenate すると長時間録音で
    メモリコピーが効いてくるため、倍々に伸ばす配列に直接書き込む。"""

    def __init__(self) -> None:
        self._buf = np.zeros(SAMPLE_RATE * 30, dtype=np.float32)
        self._length = 0

    def append(self, samples: np.ndarray) -> None:
        while self._length + len(samples) > len(self._buf):
            self._buf = np.resize(self._buf, len(self._buf) * 2)
        self._buf[self._length : self._length + len(samples)] = samples
        self._length += len(samples)

    @property
    def data(self) -> np.ndarray:
        return self._buf[: self._length]


class Segmenter:
    """録音を VAD で区切り、確定テキストと進行中の部分テキストを管理する。"""

    def __init__(self, threads: int) -> None:
        self.recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
            encoder=str(MODEL_DIR / "encoder-epoch-99-avg-1.int8.onnx"),
            decoder=str(MODEL_DIR / "decoder-epoch-99-avg-1.int8.onnx"),
            joiner=str(MODEL_DIR / "joiner-epoch-99-avg-1.int8.onnx"),
            tokens=str(MODEL_DIR / "tokens.txt"),
            num_threads=threads,
        )
        config = sherpa_onnx.VadModelConfig()
        config.silero_vad.model = str(VAD_MODEL)
        config.silero_vad.min_silence_duration = MIN_SILENCE_SECS
        config.silero_vad.max_speech_duration = MAX_SPEECH_SECS
        config.sample_rate = SAMPLE_RATE
        self.vad = sherpa_onnx.VoiceActivityDetector(config, buffer_size_in_seconds=60)

        self.recording = Recording()
        self.committed: list[str] = []
        self.partial = ""
        self._pad = int(PAD_SECS * SAMPLE_RATE)
        self._lookback = int(PARTIAL_LOOKBACK_SECS * SAMPLE_RATE)
        self._fed = 0  # VAD に投入済みのサンプル数
        self._speech_start: int | None = None  # 進行中の発話の開始位置

    def decode(self, samples: np.ndarray) -> str:
        stream = self.recognizer.create_stream()
        stream.accept_waveform(SAMPLE_RATE, np.ascontiguousarray(samples))
        self.recognizer.decode_stream(stream)
        return stream.result.text

    def push(self, samples: np.ndarray) -> list[str]:
        """音声を追加し、この呼び出しで新たに確定したテキストを返す。"""
        self.recording.append(samples)
        audio = self.recording.data
        finalized = []
        while self._fed + VAD_WINDOW <= len(audio):
            self.vad.accept_waveform(audio[self._fed : self._fed + VAD_WINDOW].copy())
            self._fed += VAD_WINDOW
            if self._speech_start is None and self.vad.is_speech_detected():
                self._speech_start = max(0, self._fed - VAD_WINDOW - self._lookback)
            while not self.vad.empty():
                seg = self.vad.front
                self.vad.pop()
                begin = max(0, seg.start - self._pad)
                end = min(len(audio), seg.start + len(seg.samples) + self._pad)
                text = self.decode(audio[begin:end])
                self.committed.append(text)
                finalized.append(text)
                self._speech_start = None
                self.partial = ""
        return finalized

    def update_partial(self) -> float:
        """進行中の発話を再デコードし、所要ミリ秒を返す（無発話なら 0）。"""
        if self._speech_start is None:
            return 0.0
        t0 = time.monotonic()
        self.partial = self.decode(self.recording.data[self._speech_start :])
        return (time.monotonic() - t0) * 1000

    @property
    def text(self) -> str:
        return "".join(self.committed) + self.partial


def run_wav(segmenter: Segmenter, path: str) -> None:
    with wave.open(path) as f:
        assert f.getframerate() == SAMPLE_RATE, f"16kHz が必要: {f.getframerate()}"
        data = np.frombuffer(f.readframes(f.getnframes()), dtype=np.int16)
    samples = data.astype(np.float32) / 32768.0
    print(f"{path}: {len(samples) / SAMPLE_RATE:.1f}s")

    step = int(STEP_SECS * SAMPLE_RATE)  # 録音が進む様子を模して 400ms ずつ流す
    for i in range(0, len(samples), step):
        for text in segmenter.push(samples[i : i + step]):
            print(f"確定: {text}")
        elapsed = segmenter.update_partial()
        if segmenter.partial:
            print(f"  部分 [{elapsed:4.0f}ms] {segmenter.partial}")
    print(f"\n--- 最終結果 ---\n{segmenter.text}")


def run_mic(segmenter: Segmenter, device: int | str | None) -> None:
    chunks: list[np.ndarray] = []

    def on_audio(indata, frames, t, status) -> None:
        chunks.append(indata[:, 0].copy())

    with sd.InputStream(
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="float32",
        device=device,
        callback=on_audio,
    ) as mic:
        print(f"入力: {sd.query_devices(mic.device)['name']}")
        print("話してください（Ctrl+C で終了）。確定した文は上の行に積まれます\n")
        next_decode = time.monotonic() + STEP_SECS
        try:
            while True:
                time.sleep(0.02)
                while chunks:
                    for text in segmenter.push(chunks.pop(0)):
                        print(f"\r\033[K確定: {text}")
                now = time.monotonic()
                if now >= next_decode:
                    next_decode = now + STEP_SECS
                    elapsed = segmenter.update_partial()
                    audio = segmenter.recording.data
                    print(
                        f"\r\033[K[{len(audio) / SAMPLE_RATE:5.1f}s {elapsed:4.0f}ms "
                        f"{level_bar(audio)}] {segmenter.partial[-60:]}",
                        end="",
                        flush=True,
                    )
        except KeyboardInterrupt:
            print(f"\n--- 最終結果 ---\n{segmenter.text}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true", help="入力デバイス一覧を表示")
    parser.add_argument("--device", help="入力デバイス（名前の一部または番号）")
    parser.add_argument("--wav", help="マイクの代わりに wav ファイルを流す")
    parser.add_argument("--threads", type=int, default=16)
    args = parser.parse_args()

    if args.list:
        list_devices()
        return

    segmenter = Segmenter(args.threads)
    if args.wav:
        run_wav(segmenter, args.wav)
    else:
        run_mic(segmenter, parse_device(args.device))


if __name__ == "__main__":
    main()
