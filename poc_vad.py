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

VAD は区切りの決定にのみ使い、デコードは前後にパディングを足した区間で行う。
VAD の切り出しは語頭に食い込むため、パディングなしでは「ヤンバルクイナ」が
「クイナ」になるなど語頭が落ちる（実測）。

区間長には自前で上限を掛ける。zipformer は 15 秒を超えると内容を落とし始め
（44 秒で CER 27%、64 秒で 55%）、sherpa の max_speech_duration は
ハードキャップとして機能しない（12 秒指定で 32 秒の区間が出る）ため。

実行:
  python3 poc_vad.py                      # マイク（Ctrl+C で終了）
  python3 poc_vad.py --list               # 入力デバイス一覧
  python3 poc_vad.py --save rec.wav       # 録音を保存（後から検証するため）
  python3 poc_vad.py --wav path.wav       # ファイルで区切り動作を確認
  python3 poc_vad.py --wav path.wav --pad-tail 1.2   # パラメータを振る
"""

import argparse
import re
import time
import wave
from dataclasses import dataclass, fields
from pathlib import Path

import numpy as np
import sherpa_onnx
import sounddevice as sd

from poc_incremental import MODEL_DIR, SAMPLE_RATE, STEP_SECS
from poc_mic import level_bar, list_devices, parse_device

VAD_MODEL = MODEL_DIR.parent / "silero_vad.onnx"
VAD_WINDOW = 512  # silero が要求する窓長（16kHz）
MIN_SEGMENT_SAMPLES = SAMPLE_RATE // 5  # これ未満の区間はデコードしない
SILENCE_RMS = 0.005  # これ以下を無音とみなす（後パディングの判定用）


@dataclass
class Params:
    """実測でスイープして決める調整項目（--wav と組み合わせて検証する）。"""

    pad_head: float = 0.3  # 確定区間の前パディング。語頭の食い込みを戻す
    pad_tail: float = 0.8  # 確定区間の後パディング。「です・ます」の
    # 無声化した語尾を VAD が無音と判定して切るため、前より厚くする。
    # 次のセグメントは min_silence 以上空くので発話に食い込む心配はない
    lookback: float = 1.0  # 部分デコードの遡り幅（VAD の検出遅れの吸収）
    max_leading_gap: float = 3.0  # 取りこぼしを拾う未転写区間の上限
    min_silence: float = 0.5  # これ未満の間は文中の息継ぎとみなし区切らない
    max_speech: float = 25.0  # 無区切りで話し続けた場合の強制確定。
    # SenseVoice は区間長にほとんど影響されず、長いほうがわずかに良い
    # （録音 4 本 210 秒で 6 秒 7.3% / 25 秒 7.0%）。25 秒でも部分デコードは
    # 218ms で 400ms 周期に収まる。zipformer は 6 秒を外すと壊れた
    threshold: float = 0.5  # silero の発話判定しきい値


SENSEVOICE_DIR = Path.home() / ".local/share/voxtype/models/sensevoice-small"


def sensevoice(threads: int) -> sherpa_onnx.OfflineRecognizer:
    return sherpa_onnx.OfflineRecognizer.from_sense_voice(
        model=str(SENSEVOICE_DIR / "model.int8.onnx"),
        tokens=str(SENSEVOICE_DIR / "tokens.txt"),
        language="ja",
        use_itn=True,
        num_threads=threads,
    )


def zipformer(threads: int) -> sherpa_onnx.OfflineRecognizer:
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(MODEL_DIR / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(MODEL_DIR / "decoder-epoch-99-avg-1.int8.onnx"),
        joiner=str(MODEL_DIR / "joiner-epoch-99-avg-1.int8.onnx"),
        tokens=str(MODEL_DIR / "tokens.txt"),
        num_threads=threads,
    )


def _is_speech(samples: np.ndarray) -> bool:
    return bool(len(samples)) and float(np.sqrt(np.mean(samples**2))) > SILENCE_RMS


def strip_japanese_spaces(text: str) -> str:
    """SenseVoice は日本語の途中に空白を入れる（「プルリクエスト の レビュー」）。
    両隣が非 ASCII のものだけ落とし、英単語間の空白は残す。"""
    return re.sub(r"(?<=[^\x00-\x7f]) +(?=[^\x00-\x7f])", "", text)


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

    def __init__(
        self,
        params: Params,
        recognizer: sherpa_onnx.OfflineRecognizer | None = None,
        threads: int = 16,
    ) -> None:
        self.params = params
        self.recognizer = recognizer or sensevoice(threads)
        config = sherpa_onnx.VadModelConfig()
        config.silero_vad.model = str(VAD_MODEL)
        config.silero_vad.min_silence_duration = params.min_silence
        config.silero_vad.max_speech_duration = params.max_speech
        config.silero_vad.threshold = params.threshold
        config.sample_rate = SAMPLE_RATE
        self.vad = sherpa_onnx.VoiceActivityDetector(config, buffer_size_in_seconds=60)

        self.recording = Recording()
        self.committed: list[str] = []
        self.segments: list[tuple[int, int]] = []  # (開始サンプル, 長さ)
        self.partial = ""
        self._pad_head = int(params.pad_head * SAMPLE_RATE)
        self._pad_tail = int(params.pad_tail * SAMPLE_RATE)
        self._lookback = int(params.lookback * SAMPLE_RATE)
        self._max_speech = int(params.max_speech * SAMPLE_RATE)
        self._max_leading_gap = int(params.max_leading_gap * SAMPLE_RATE)
        self._committed_until = 0  # ここまでは確定済み（重複デコードを防ぐ）
        self._fed = 0  # VAD に投入済みのサンプル数
        self._speech_start: int | None = None  # 進行中の発話の開始位置

    def decode(self, samples: np.ndarray) -> str:
        stream = self.recognizer.create_stream()
        stream.accept_waveform(SAMPLE_RATE, np.ascontiguousarray(samples))
        self.recognizer.decode_stream(stream)
        return strip_japanese_spaces(stream.result.text)

    def _commit(self, begin: int, end: int, speech_end: int) -> str | None:
        """[begin, end) を確定させる。end は後パディング込み、speech_end は
        実際に発話が終わった位置で、次の区間の開始下限になる。"""
        # 直前の未転写区間に声が乗っていれば取りこぼしなので含める。
        # silero は起動直後や話し始めの検出が遅れることがあり
        # （録音開始と同時に話すと「これから音声入力の」が落ちた）、
        # 前パディングだけでは戻らない。無音なら含めない（無音を足すほど
        # 認識は落ちるため）
        gap = self.recording.data[self._committed_until : begin]
        if 0 < len(gap) <= self._max_leading_gap and _is_speech(gap):
            begin = self._committed_until
        begin = max(begin, self._committed_until)
        if end - begin < MIN_SEGMENT_SAMPLES:
            return None
        text = self.decode(self.recording.data[begin:end])
        self._committed_until = speech_end
        self.committed.append(text)
        self.segments.append((begin, end - begin))
        self.partial = ""
        return text

    def _drain_vad(self) -> list[str]:
        audio = self.recording.data
        finalized = []
        while not self.vad.empty():
            seg = self.vad.front
            self.vad.pop()
            speech_end = seg.start + len(seg.samples)
            end = min(len(audio), speech_end + self._pad_tail)
            # 後パディングが無音なら次の区間の開始を speech_end に戻す
            # （語頭のために少し重ねる）。そこに既に声が乗っている
            # 連続発話の場合は end まで確定済みとし、単語の重複を避ける
            if _is_speech(audio[speech_end:end]):
                speech_end = end
            text = self._commit(seg.start - self._pad_head, end, speech_end)
            if text is not None:
                finalized.append(text)
            self._speech_start = None
        return finalized

    def _split_point(self, begin: int, limit: int) -> int:
        """強制分割の位置。直前 2 秒のうち最も静かな 100ms を選び、
        単語の途中で切る確率を下げる。"""
        hop = SAMPLE_RATE // 10
        window = self.recording.data[max(begin, limit - 2 * SAMPLE_RATE) : limit]
        offset = limit - len(window)
        if len(window) < hop * 2:
            return limit
        energies = [
            (float(np.sqrt(np.mean(window[i : i + hop] ** 2))), i)
            for i in range(0, len(window) - hop, hop)
        ]
        return offset + min(energies)[1] + hop // 2

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
            finalized.extend(self._drain_vad())

            # sherpa の max_speech_duration はハードキャップにならない
            # （12 秒指定で 32 秒の区間が出る）ため自前で上限を掛ける。
            # zipformer は 15 秒を超えると内容を落とし始める（44 秒で CER 27%）
            if self._speech_start is not None:
                length = self._fed - self._speech_start
                if length > self._max_speech:
                    split = self._split_point(self._speech_start, self._fed)
                    text = self._commit(self._speech_start, split, split)
                    if text is not None:
                        finalized.append(text)
                    self._speech_start = self._committed_until
        return finalized

    def flush(self) -> list[str]:
        """録音終了時に、進行中の発話を確定させる。"""
        self.vad.flush()
        finalized = self._drain_vad()
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


def run_wav(segmenter: Segmenter, path: str, verbose: bool = True) -> None:
    with wave.open(path) as f:
        assert f.getframerate() == SAMPLE_RATE, f"16kHz が必要: {f.getframerate()}"
        data = np.frombuffer(f.readframes(f.getnframes()), dtype=np.int16)
    samples = data.astype(np.float32) / 32768.0
    print(f"{path}: {len(samples) / SAMPLE_RATE:.1f}s")

    step = int(STEP_SECS * SAMPLE_RATE)  # 録音が進む様子を模して 400ms ずつ流す
    for i in range(0, len(samples), step):
        for text in segmenter.push(samples[i : i + step]):
            print(f"確定: {text}")
        if not verbose:
            continue
        elapsed = segmenter.update_partial()
        if verbose and segmenter.partial:
            print(f"  部分 [{elapsed:4.0f}ms] {segmenter.partial}")
    segmenter.flush()
    for start, length in segmenter.segments:
        print(
            f"  区間 {start / SAMPLE_RATE:6.2f}s 〜 {(start + length) / SAMPLE_RATE:6.2f}s ({length / SAMPLE_RATE:5.2f}s)"
        )
    print(f"\n--- 最終結果 ---\n{segmenter.text}")


def save_wav(path: str, samples: np.ndarray) -> None:
    with wave.open(path, "wb") as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(SAMPLE_RATE)
        f.writeframes((np.clip(samples, -1, 1) * 32767).astype(np.int16).tobytes())
    print(f"録音を保存: {path} ({len(samples) / SAMPLE_RATE:.1f}s)")


def run_mic(segmenter: Segmenter, device: int | str | None, save: str | None) -> None:
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
            for text in segmenter.flush():
                print(f"\r\033[K確定: {text}")
            print(f"\n--- 最終結果 ---\n{segmenter.text}")
            if save:
                save_wav(save, segmenter.recording.data)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true", help="入力デバイス一覧を表示")
    parser.add_argument("--device", help="入力デバイス（名前の一部または番号）")
    parser.add_argument("--wav", help="マイクの代わりに wav ファイルを流す")
    parser.add_argument("--quiet", action="store_true", help="部分テキストを出さない")
    parser.add_argument("--save", help="録音を wav に保存（後からパラメータ検証用）")
    parser.add_argument("--threads", type=int, default=16)
    parser.add_argument(
        "--model", default="sensevoice", choices=["sensevoice", "zipformer"]
    )
    defaults = Params()
    for field in fields(Params):
        parser.add_argument(
            f"--{field.name.replace('_', '-')}",
            type=float,
            default=getattr(defaults, field.name),
        )
    args = parser.parse_args()

    if args.list:
        list_devices()
        return

    params = Params(**{f.name: getattr(args, f.name) for f in fields(Params)})
    builder = sensevoice if args.model == "sensevoice" else zipformer
    segmenter = Segmenter(params, recognizer=builder(args.threads))
    if args.wav:
        run_wav(segmenter, args.wav, verbose=not args.quiet)
    else:
        run_mic(segmenter, parse_device(args.device), args.save)


if __name__ == "__main__":
    main()
