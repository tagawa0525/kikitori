# 引き継ぎ文書 — kikitori 開発

2026-08-01 時点の状態。nixfiles 側での音声入力導入（PR #110）から派生した
後継プロジェクト。この文書だけで開発を再開できることを目的とする。

## 1. 目的

COSMIC/Wayland の全アプリで使える**完全ローカル・リアルタイム表示**の
日本語音声入力を作る。

ユーザー要件（確定済み）:

- **話している最中に文字が見えること**（Windows Win+H の UX が基準）。
  一括出力方式（現行ベースライン）は「入力中に何が入っているか分からない」
  として不採用
- 完全ローカル。クラウド案（voxtype の Soniox エンジン、リアルタイム対応）は
  提示のうえ**ユーザーが明示的に不採用**とした
- 既存ベースライン（下記 §3）は完成まで残し、完成時に置き換える

## 2. アーキテクチャ（合意済み設計）

```text
[PipeWire マイク 16kHz mono]
      │ 連続キャプチャ
      ▼
[録音バッファ] ──400msごとに全体を再デコード──▶ [sherpa-onnx
      │                                       ReazonSpeech zipformer int8]
      │                                             │ 部分テキスト
      │                                             ▼
      │                              [オーバーレイ: 画面下部バーに逐次表示]
      │ トグル停止時
      ▼
[確定テキスト] ──wtype──▶ フォーカス中のアプリへ入力
```

- **擬似ストリーミング方式**: 真のストリーミングモデルではなく、offline
  transducer で録音バッファ全体を 400ms ごとに再デコードする。PoC 実測
  （§4）でこの方式の成立を確認済み
- **部分表示はオーバーレイ、確定入力のみ wtype**: 任意アプリ内での
  backspace 修正は危険なため行わない。Windows の「聞き取りバー」と同じ方式
- **制御**: COSMIC カスタムショートカット Super+V → UNIX ソケット等で
  デーモンにトグルを通知（voxtype と同じパターン。置き換え時は
  nixfiles の `voice-input.nix` にある Spawn コマンドを差し替えるだけ）
- **句読点**: zipformer 出力には句読点がない。確定時のみ SenseVoice
  （use_itn=true、句読点付き）で再デコードするハイブリッド案を検討
  （部分表示=zipformer、確定=SenseVoice。SenseVoice は 8 秒音声を
  0.58 秒で処理できることを voxtype 経由で実測済み）

## 3. 既存ベースライン（置き換え対象、稼働中）

nixfiles PR #110（マージ済み、`modules/home/parts/voice-input.nix`）:
voxtype-onnx + SenseVoice(small, ja) + wtype。Super+V トグル、停止後
約 1 秒で一括入力。r995 で実音声動作確認済み。

kikitori 完成までの利用者向け窓口はこのままにしておくこと。

## 4. PoC 実測結果（r995: Ryzen 9950X, 16 threads）

`poc_incremental.py`（モデル同梱の実音声ニュース `test_wavs/1.wav` 13.4 秒）:

- 全バッファ再デコード: **73〜136ms**（バッファ 13 秒時点でも 400ms 周期に余裕）
- 部分テキスト安定性: ほぼ追記のみ。13 秒間でプレフィックス修正 1 回のみ
  → ちらつきの少ない表示が可能
- 認識精度: ほぼ完全（漢字変換含む）。句読点なし
- モデルロード: 数秒（デーモン常駐で吸収する）

`poc_mic.py` はマイクで同じことを行う体感確認用（ユーザー未実施）。

## 5. ロードマップと各段の設計メモ

1. [x] **PoC**: 逐次デコードの遅延・安定性計測（§4）
2. [ ] **マイク体感テスト**: `poc_mic.py` をユーザーに実行してもらい、
   マイク経由の精度と体感を確認（README に実行コマンドあり）
3. [ ] **オーバーレイ表示**: GTK4 + gtk4-layer-shell（nixpkgs にあり）で
   画面下部中央の固定バー。COSMIC は wlr-layer-shell 対応。
   Python なら PyGObject + gtk4-layer-shell。カーソル位置追従は
   Wayland では不可能なので固定バーで良い（Windows も同じ）
4. [ ] **デーモン化**: トグル制御（UNIX ソケット、`kikitori toggle` CLI）、
   録音開始/停止、確定時 wtype 入力、句読点ハイブリッド（§2）。
   VAD による無音棄却も入れる（silero、sherpa-onnx に同梱機能あり）
5. [ ] **配布**: flake 化（パッケージ + Home Manager モジュール or
   ショートカット差し替え手順）、systemd ユーザーサービス、
   nixfiles への input 追加（qmpo / cc-bar と同じパターン）。
   モデルは巨大なので nix store に入れず、初回セットアップコマンドで
   `~/.local/share/kikitori/` へダウンロードする方式（voxtype と同様）

## 6. 検証済みの環境知見（再調査不要）

- **wtype は COSMIC 1.5 で日本語ユニコード直接入力が動く**（実機確認済み。
  古い COSMIC では壊れていたという報告があるが現環境では問題ない）
- **ペーストキー方式は不採用**: Ctrl+V はターミナル不可、Shift+Insert は
  Alacritty がプライマリセレクションを参照するため、万能キーが存在しない
- /dev/uinput は nixfiles の openlogi.nix (uaccess) で開放済み（dotool 用。
  wtype を使う限り不要だが、フォールバックに使える）
- ホットキーの evdev 監視（input グループ）は**使わない**。キーロガー面を
  開くため nixfiles 側で意図的に避けている方針
- COSMIC カスタムショートカットの宣言的設定は
  `cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`（RON 形式、
  voice-input.nix に実例あり）。反映にログアウト不要
- r995 に dGPU なし（iGPU は RDNA2 2CU で推論には不足）。CPU 16 スレッドで
  設計する。t14g4 は 8、x1ng1 は 4 スレッド想定（voice-input.nix 参照）
- keyd が全キーボードを grab して仮想デバイス経由で再送出しているが、
  wtype / ソケット制御方式では干渉しない

## 7. 開発環境メモ

- 依存の入手（暫定。flake 化までの間）:

  ```bash
  nix shell --impure --expr \
    'with import <nixpkgs> {}; python313.withPackages (p: [p.sherpa-onnx p.numpy p.sounddevice])'
  ```

- モデル取得コマンドは README.md 参照（`models/` は .gitignore 済み、
  中に実音声の `test_wavs/` と正解 `transcript.txt` が同梱されている）
- git フック（グローバル `~/.config/git/hooks/pre-commit`）:
  - main への直接コミット禁止（feature ブランチ必須）
  - Python は ruff format / ruff check、Markdown は markdownlint が走る
- このリポジトリはまだ GitHub リモートなし。リモートなしのリポジトリは
  PR フロー適用外（gh-wait-review.sh が exit 2 を返すケース）。
  公開するかどうかはユーザーに確認すること
- リポジトリ名「kikitori」は仮称。ユーザーが変更する可能性あり

## 8. 関連リソース

- nixfiles: `modules/home/parts/voice-input.nix`（ベースライン実装。
  検証済み知見が全てコメントに残してある）
- voxtype: <https://github.com/peteonrails/voxtype>（設計の参考。
  ストリーミング時の出力方式 `StreamingEvent::Replace` など）
- モデル: <https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models>
  の `sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01`
- sherpa-onnx ドキュメント: <https://k2-fsa.github.io/sherpa/onnx/>
