#!/usr/bin/env python3
"""マイク版 PoC: 話しながら部分テキストがターミナルに逐次表示される体感確認用。

実行: python3 poc_mic.py [--list] [--device 名前または番号] [--threads N]
Ctrl+C で終了。400ms ごとに録音バッファ全体を再デコードして同一行を更新する。
"""

import argparse
import time

import numpy as np
import sherpa_onnx
import sounddevice as sd

from poc_incremental import MODEL_DIR, SAMPLE_RATE, STEP_SECS


def list_devices() -> None:
    for i, d in enumerate(sd.query_devices()):
        if d["max_input_channels"] > 0:
            print(f"{i:3d}  {d['name']}  ({d['max_input_channels']}ch)")


def parse_device(value: str | None) -> int | str | None:
    if value is None:
        return None
    return int(value) if value.isdigit() else value


def level_bar(buf: np.ndarray, width: int = 10) -> str:
    """直近 0.4 秒の RMS を簡易バー表示（マイクが拾えているかの切り分け用）。"""
    recent = buf[-int(STEP_SECS * SAMPLE_RATE) :]
    rms = float(np.sqrt(np.mean(recent**2))) if len(recent) else 0.0
    filled = min(width, int(rms * 40 * width))
    return "#" * filled + "-" * (width - filled)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true", help="入力デバイス一覧を表示")
    parser.add_argument("--device", help="入力デバイス（名前の一部または番号）")
    parser.add_argument("--threads", type=int, default=16)
    args = parser.parse_args()

    if args.list:
        list_devices()
        return

    recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL_DIR / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(MODEL_DIR / "decoder-epoch-99-avg-1.int8.onnx"),
        joiner=str(MODEL_DIR / "joiner-epoch-99-avg-1.int8.onnx"),
        tokens=str(MODEL_DIR / "tokens.txt"),
        num_threads=args.threads,
    )

    chunks: list[np.ndarray] = []
    text = ""

    def on_audio(indata, frames, t, status) -> None:
        chunks.append(indata[:, 0].copy())

    with sd.InputStream(
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="float32",
        device=parse_device(args.device),
        callback=on_audio,
    ) as stream_in:
        print(f"入力: {sd.query_devices(stream_in.device)['name']}")
        print("読み込み完了。話してください（Ctrl+C で終了）")
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
                text = stream.result.text
                # 同一行を上書き表示（端末幅を超えたら末尾側を出す）
                print(
                    f"\r[{len(buf) / SAMPLE_RATE:5.1f}s {elapsed:4.0f}ms {level_bar(buf)}] {text[-60:]}",
                    end="",
                    flush=True,
                )
        except KeyboardInterrupt:
            print("\n--- 最終結果 ---")
            print(text)


if __name__ == "__main__":
    main()
