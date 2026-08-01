#!/usr/bin/env python3
"""ASR 設定・モデルの精度/速度比較ベンチ。

正解テキスト付きの wav 群に対し、複数構成を同一条件で走らせて
CER（文字誤り率）と RTF（実時間比）を出す。

実行:
  python3 bench_asr.py                      # モデル同梱の test_wavs
  python3 bench_asr.py bench_data           # 自分の声（bench_record.py で作成）
  python3 bench_asr.py bench_data --configs zipformer-int8-greedy,sensevoice

データ形式: ディレクトリ内に *.wav と transcript.txt
（`<ファイル名> <正解テキスト>` を1行ずつ、モデル同梱のものと同形式）
"""

import argparse
import re
import time
import unicodedata
import wave
from pathlib import Path

import numpy as np
import sherpa_onnx

from poc_incremental import MODEL_DIR, SAMPLE_RATE

SENSEVOICE_DIR = Path.home() / ".local/share/voxtype/models/sensevoice-small"
MODELS = MODEL_DIR.parent


def _whisper(name: str, threads: int) -> sherpa_onnx.OfflineRecognizer:
    d = MODELS / f"sherpa-onnx-whisper-{name}"
    return sherpa_onnx.OfflineRecognizer.from_whisper(
        encoder=str(d / f"{name}-encoder.int8.onnx"),
        decoder=str(d / f"{name}-decoder.int8.onnx"),
        tokens=str(d / f"{name}-tokens.txt"),
        language="ja",
        task="transcribe",
        num_threads=threads,
    )


def _dolphin(threads: int) -> sherpa_onnx.OfflineRecognizer:
    d = MODELS / "sherpa-onnx-dolphin-base-ctc-multi-lang-2025-04-02"
    return sherpa_onnx.OfflineRecognizer.from_dolphin_ctc(
        model=str(d / "model.onnx"),
        tokens=str(d / "tokens.txt"),
        num_threads=threads,
    )


# 比較する構成。builder は num_threads を受けて OfflineRecognizer を返す
CONFIGS: dict[str, callable] = {
    "zipformer-int8-greedy": lambda n: _zipformer(n, quant=True, beam=0),
    "zipformer-int8-beam4": lambda n: _zipformer(n, quant=True, beam=4),
    "zipformer-int8-beam8": lambda n: _zipformer(n, quant=True, beam=8),
    "zipformer-fp32-greedy": lambda n: _zipformer(n, quant=False, beam=0),
    "zipformer-fp32-beam4": lambda n: _zipformer(n, quant=False, beam=4),
    "sensevoice": lambda n: __import__("poc_vad").sensevoice(n),
    "whisper-turbo": lambda n: _whisper("turbo", n),
    "whisper-small": lambda n: _whisper("small", n),
    "whisper-large-v3": lambda n: _whisper("large-v3", n),
    "dolphin-base": _dolphin,
}


def _zipformer(threads: int, quant: bool, beam: int) -> sherpa_onnx.OfflineRecognizer:
    suffix = ".int8.onnx" if quant else ".onnx"
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL_DIR / f"encoder-epoch-99-avg-1{suffix}"),
        decoder=str(MODEL_DIR / f"decoder-epoch-99-avg-1{suffix}"),
        joiner=str(MODEL_DIR / f"joiner-epoch-99-avg-1{suffix}"),
        tokens=str(MODEL_DIR / "tokens.txt"),
        num_threads=threads,
        decoding_method="modified_beam_search" if beam else "greedy_search",
        max_active_paths=beam or 4,
    )


def normalize(text: str) -> str:
    """CER 比較用の正規化。

    句読点の有無はモデル差（zipformer は出力しない）であり精度差ではないので
    落とす。全角/半角の揺れも NFKC で吸収する。
    """
    text = re.sub(r"<\|[^|]*\|>", "", text)  # SenseVoice の言語/感情タグ
    text = unicodedata.normalize("NFKC", text)
    return re.sub(r"[\s、。，．,.!?！？「」『』・…ー]", "", text)


def cer(ref: str, hyp: str) -> tuple[int, int]:
    """(編集距離, 正解文字数) を返す。"""
    prev = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        cur = [i]
        for j, h in enumerate(hyp, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h)))
        prev = cur
    return prev[-1], len(ref)


def load_wav(path: Path) -> np.ndarray:
    with wave.open(str(path)) as f:
        assert f.getframerate() == SAMPLE_RATE, (
            f"{path}: expected 16kHz, got {f.getframerate()}"
        )
        assert f.getnchannels() == 1, f"{path}: expected mono"
        data = np.frombuffer(f.readframes(f.getnframes()), dtype=np.int16)
    return data.astype(np.float32) / 32768.0


def load_dataset(data_dir: Path) -> list[tuple[Path, str]]:
    refs: dict[str, str] = {}
    for line in (data_dir / "transcript.txt").read_text().splitlines():
        if line.strip():
            name, _, text = line.partition(" ")
            refs[name] = text.strip()
    items = [(data_dir / name, text) for name, text in refs.items()]
    missing = [p.name for p, _ in items if not p.exists()]
    if missing:
        print(f"未録音のためスキップ: {', '.join(sorted(missing))}\n")
    return sorted((p, t) for p, t in items if p.exists())


def decode_segmented(recognizer, samples: np.ndarray) -> str:
    """poc_vad と同じ VAD セグメント方式で認識する。長い音声を一括で
    デコードすると内容が落ちるため、モデル比較もこの経路で行う。"""
    from poc_vad import Params, Segmenter

    segmenter = Segmenter(Params(), recognizer=recognizer)
    step = int(0.4 * SAMPLE_RATE)
    for i in range(0, len(samples), step):
        segmenter.push(samples[i : i + step])
    segmenter.flush()
    return "".join(segmenter.committed)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "data_dir",
        nargs="?",
        default=str(MODEL_DIR / "test_wavs"),
        help="wav と transcript.txt を含むディレクトリ",
    )
    parser.add_argument("--threads", type=int, default=16)
    parser.add_argument("--configs", help="カンマ区切りの構成名（既定: 全部）")
    parser.add_argument("--show", action="store_true", help="認識結果を全部表示")
    parser.add_argument(
        "--whole",
        action="store_true",
        help="VAD で区切らず全体を一括デコード（既定は poc_vad と同じ区切り方）",
    )
    args = parser.parse_args()

    names = args.configs.split(",") if args.configs else list(CONFIGS)
    unknown = [n for n in names if n not in CONFIGS]
    assert not unknown, f"未知の構成: {unknown}（候補: {list(CONFIGS)}）"

    items = load_dataset(Path(args.data_dir))
    audio = [(p, load_wav(p), ref) for p, ref in items]
    total_secs = sum(len(a) for _, a, _ in audio) / SAMPLE_RATE
    print(f"データ: {args.data_dir} — {len(audio)} ファイル / {total_secs:.1f}s")
    print(f"threads={args.threads}\n")

    print(f"{'構成':<24} {'CER':>7} {'誤り/文字':>12} {'RTF':>7} {'合計':>8}")
    print("-" * 64)
    for name in names:
        t0 = time.monotonic()
        recognizer = CONFIGS[name](args.threads)
        load_secs = time.monotonic() - t0

        errors = chars = 0
        decode_secs = 0.0
        per_file = []
        for path, samples, ref in audio:
            t0 = time.monotonic()
            if args.whole:
                stream = recognizer.create_stream()
                stream.accept_waveform(SAMPLE_RATE, samples)
                recognizer.decode_stream(stream)
                hyp = stream.result.text
            else:
                hyp = decode_segmented(recognizer, samples)
            decode_secs += time.monotonic() - t0
            e, c = cer(normalize(ref), normalize(hyp))
            errors += e
            chars += c
            per_file.append((path.name, e / c))
            if args.show:
                print(f"  {path.name} [{e}/{c}] {hyp}")

        rtf = decode_secs / total_secs
        print(
            f"{name:<24} {errors / chars:>6.2%} {f'{errors}/{chars}':>12} "
            f"{rtf:>7.3f} {decode_secs:>7.2f}s  (load {load_secs:.1f}s)"
        )
        if len(per_file) > 1:
            # 調整に使った音声だけ良い＝過学習なので、ファイル別も出す
            print("      " + "  ".join(f"{n}: {c:.1%}" for n, c in per_file))


if __name__ == "__main__":
    main()
