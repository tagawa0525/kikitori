# kikitori（仮称）

COSMIC/Wayland 向けの完全ローカル・リアルタイム日本語音声入力（開発中）。

- エンジン: sherpa-onnx + ReazonSpeech zipformer (int8)
- 方式: 録音バッファを 400ms ごとに再デコードし部分テキストを表示、
  停止時に wtype で確定入力（Windows の「聞き取りバー」方式）

## PoC

```bash
# モデル取得（models/ に展開、約700MB）
curl -L -o models/model.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01.tar.bz2
tar xjf models/model.tar.bz2 -C models/

# ファイル逐次デコード（遅延・安定性計測）
nix shell --impure --expr \
  'with import <nixpkgs> {}; python313.withPackages (p: [p.sherpa-onnx p.numpy p.sounddevice])' \
  -c python3 poc_incremental.py models/sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01/test_wavs/1.wav

# マイク体感テスト（--list で入力デバイス一覧、--device で選択）
nix shell --impure --expr \
  'with import <nixpkgs> {}; python313.withPackages (p: [p.sherpa-onnx p.numpy p.sounddevice])' \
  -c python3 poc_mic.py
```

## PoC 計測結果 (r995, 16 threads, 2026-08-01)

- 13 秒バッファの全再デコード: 73〜136ms（400ms 周期に対し余裕）
- 部分テキストの安定性: ほぼ追記のみ（13.4 秒間で軽微な修正 1 回）
- 認識精度: ニュース読み上げ音声をほぼ完全に認識（句読点なし）

## ロードマップ

1. [x] PoC: 逐次デコードの遅延・安定性計測
2. [ ] マイク体感テスト（poc_mic.py）
3. [ ] オーバーレイ表示（GTK4 layer-shell、画面下部バー）
4. [ ] トグル制御（UNIX ソケット）+ wtype 確定入力 + 句読点後処理
5. [ ] flake 化・systemd ユーザーサービス・nixfiles 組み込み
