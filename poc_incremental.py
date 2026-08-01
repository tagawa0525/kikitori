#!/usr/bin/env python3
"""擬似ストリーミング PoC: 録音バッファの逐次再デコードの実現性計測。

WAV ファイルを 400ms ずつ「録音が進んだ」ように増やしながら、
その時点のバッファ全体を offline transducer でデコードする。
計測対象:
  - 各デコードの所要時間（バッファ長に対してどう伸びるか）
  - 部分テキストの安定性（前回結果からの共通プレフィックス率）
"""

import sys
import time
import wave
from pathlib import Path

import numpy as np
import sherpa_onnx

MODEL_DIR = (
    Path(__file__).parent
    / "models"
    / "sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01"
)
STEP_SECS = 0.4
SAMPLE_RATE = 16000


def load_wav(path: str) -> np.ndarray:
    with wave.open(path) as f:
        assert f.getframerate() == SAMPLE_RATE, (
            f"expected 16kHz, got {f.getframerate()}"
        )
        assert f.getnchannels() == 1
        data = np.frombuffer(f.readframes(f.getnframes()), dtype=np.int16)
    return data.astype(np.float32) / 32768.0


def main() -> None:
    wav_path = sys.argv[1]
    threads = int(sys.argv[2]) if len(sys.argv) > 2 else 16

    t0 = time.monotonic()
    recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL_DIR / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(MODEL_DIR / "decoder-epoch-99-avg-1.int8.onnx"),
        joiner=str(MODEL_DIR / "joiner-epoch-99-avg-1.int8.onnx"),
        tokens=str(MODEL_DIR / "tokens.txt"),
        num_threads=threads,
    )
    print(f"model load: {time.monotonic() - t0:.2f}s (threads={threads})")

    samples = load_wav(wav_path)
    total_secs = len(samples) / SAMPLE_RATE
    print(f"audio: {wav_path} ({total_secs:.1f}s)\n")

    prev_text = ""
    step = int(STEP_SECS * SAMPLE_RATE)
    for end in range(step, len(samples) + step, step):
        buf = samples[: min(end, len(samples))]
        t0 = time.monotonic()
        stream = recognizer.create_stream()
        stream.accept_waveform(SAMPLE_RATE, buf)
        recognizer.decode_stream(stream)
        elapsed = time.monotonic() - t0
        text = stream.result.text

        # 前回テキストとの共通プレフィックス長（表示のちらつき指標）
        common = 0
        for a, b in zip(prev_text, text):
            if a != b:
                break
            common += 1
        stable = f"{common}/{len(prev_text)}" if prev_text else "-"
        print(
            f"[{len(buf) / SAMPLE_RATE:5.1f}s] decode={elapsed * 1000:6.1f}ms stable_prefix={stable:>8} | {text}"
        )
        prev_text = text


if __name__ == "__main__":
    main()
