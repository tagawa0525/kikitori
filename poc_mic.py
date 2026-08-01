#!/usr/bin/env python3
"""マイク版 PoC: 話しながら部分テキストがターミナルに逐次表示される体感確認用。

実行: python3 poc_mic.py
Ctrl+C で終了。400ms ごとに録音バッファ全体を再デコードして同一行を更新する。
"""

import sys
import time

import numpy as np
import sherpa_onnx
import sounddevice as sd

from poc_incremental import MODEL_DIR, SAMPLE_RATE, STEP_SECS


def main() -> None:
    threads = int(sys.argv[1]) if len(sys.argv) > 1 else 16
    recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL_DIR / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(MODEL_DIR / "decoder-epoch-99-avg-1.int8.onnx"),
        joiner=str(MODEL_DIR / "joiner-epoch-99-avg-1.int8.onnx"),
        tokens=str(MODEL_DIR / "tokens.txt"),
        num_threads=threads,
    )
    print("読み込み完了。話してください（Ctrl+C で終了）")

    chunks: list[np.ndarray] = []

    def on_audio(indata, frames, t, status) -> None:
        chunks.append(indata[:, 0].copy())

    with sd.InputStream(
        samplerate=SAMPLE_RATE, channels=1, dtype="float32", callback=on_audio
    ):
        try:
            while True:
                time.sleep(STEP_SECS)
                if not chunks:
                    continue
                buf = np.concatenate(chunks)
                t0 = time.monotonic()
                stream = recognizer.create_stream()
                stream.accept_waveform(SAMPLE_RATE, buf)
                recognizer.decode_stream(stream)
                elapsed = (time.monotonic() - t0) * 1000
                # 同一行を上書き表示（端末幅を超えたら末尾側を出す）
                text = stream.result.text
                tail = text[-60:]
                print(
                    f"\r[{len(buf) / SAMPLE_RATE:5.1f}s {elapsed:4.0f}ms] {tail}",
                    end="",
                    flush=True,
                )
        except KeyboardInterrupt:
            print("\n--- 最終結果 ---")
            if chunks:
                print(stream.result.text)


if __name__ == "__main__":
    main()
