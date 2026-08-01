# kikitori（仮称）

Wayland 向け（COSMIC 等）の完全ローカル・リアルタイム日本語音声入力
（開発中）。エンジン/クライアント分離で、クライアントは将来 Windows/Mac
にも展開予定。

- エンジン: sherpa-onnx + SenseVoice small (int8)
- 方式: VAD で発話を区切り、確定セグメントは 1 度だけデコード。進行中の
  発話のみ 400ms ごとに再デコードして部分テキストを表示し、停止時に
  wtype で確定入力（Windows の「聞き取りバー」方式）

## 開発環境

flake + direnv で固定している。`direnv allow` するか、`nix develop` に入る。

## PoC

```bash
# モデル取得（models/ に展開、約700MB）
curl -L -o models/model.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01.tar.bz2
tar xjf models/model.tar.bz2 -C models/

# VAD モデル（発話区切りの検出用、約600KB）
curl -L -o models/silero_vad.onnx \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx

# ファイル逐次デコード（遅延・安定性計測）
python3 poc/poc_incremental.py models/sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01/test_wavs/1.wav

# マイク体感テスト（VAD セグメント方式・こちらが現行）
# --list で入力デバイス一覧、--device で選択、--wav で音声ファイルを流す
python3 poc/poc_vad.py

# 構成・モデルの精度比較（CER / RTF）
python3 poc/bench_asr.py
```

`poc/poc_mic.py` は全バッファ再デコード方式（無音で確定テキストが壊れる問題あり、
比較用に残置）。

## モデル比較 (自分の声 4 本 210 秒、CER)

| モデル | CER | RTF |
| --- | --- | --- |
| **SenseVoice small** | **7.0%** | 0.013 |
| whisper-large-v3 | 11.8% | 1.351 |
| whisper-turbo | 13.2% | 0.159 |
| dolphin-base | 15.3% | 0.010 |
| ReazonSpeech zipformer | 17.0% | 0.017 |

モデル同梱のニュース音声では zipformer が SenseVoice の 3 倍良かったが、
実際のマイク録音では逆転した。詳細は docs/HANDOFF.md §4。

## PoC 計測結果 (r995, 16 threads, 2026-08-01)

全バッファ再デコード方式の限界:

- 13 秒バッファ: 73〜136ms。33 秒バッファ: 343ms（400ms 周期の限界）
- **後ろに無音が伸びると確定済みのテキストが壊れる**。6.6 秒の発話に
  32 秒の無音を足すと語尾が欠落（offline zipformer は全バッファに
  attention をかけるため）

VAD セグメント方式（現行）:

- 確定セグメントは 1 度だけデコードして以後不変。無音を挟んでも壊れない
- デコード時間は発話長で頭打ち（実測 15〜60ms）
- VAD は区切りの決定のみに使い、デコードは前後 0.3 秒を足した区間で行う
  （パディングなしでは「ヤンバルクイナ」が「クイナ」になるなど語頭が落ちる）

精度比較 (CER, モデル同梱の test_wavs 50.5 秒):

| 構成 | CER | RTF |
| --- | --- | --- |
| zipformer int8 greedy | 3.54% | 0.010 |
| zipformer int8 beam4/8 | 3.54% | 0.014 |
| zipformer fp32 greedy/beam4 | 3.54% | 0.012 |
| SenseVoice small (int8) | 10.61% | 0.009 |

beam search も fp32 も精度差なし。SenseVoice は 3 倍悪い。

## ロードマップ

1. [x] PoC: 逐次デコードの遅延・安定性計測
2. [x] マイク体感テスト（実用的な精度・体感を確認）
3. [x] VAD セグメント方式（無音によるテキスト破壊とデコード時間増大の解消）
4. [ ] オーバーレイ表示（GTK4 layer-shell、画面下部バー）
5. [ ] トグル制御（UNIX ソケット）+ wtype 確定入力 + 句読点後処理
6. [ ] flake 化・systemd ユーザーサービス・nixfiles 組み込み
