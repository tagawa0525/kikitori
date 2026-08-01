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
[録音バッファ] ──▶ [silero VAD] ──発話区切り──┐
      │                                        ▼
      │                          [確定セグメント: 1度だけデコード]
      │                                        │
      │ 進行中の発話のみ 400ms ごとに再デコード │
      ▼                                        ▼
[sherpa-onnx ReazonSpeech zipformer int8] ──▶ 確定テキスト + 部分テキスト
      │                                             │
      │                              [オーバーレイ: 画面下部バーに逐次表示]
      │ トグル停止時
      ▼
[確定テキスト] ──wtype──▶ フォーカス中のアプリへ入力
```

- **擬似ストリーミング方式**: 真のストリーミングモデルではなく、offline
  transducer を短い区間に対して繰り返し適用する
- **VAD セグメント必須**: 当初の「録音バッファ全体を再デコード」は構造的に
  破綻する。offline zipformer は全バッファに attention をかけるため、後ろに
  無音が伸びると**確定済みのテキストまで壊れる**（§4）。VAD で区切り、
  確定済みセグメントを再デコードしないことが必須。デコード時間が発話長で
  頭打ちになる効果も同時に得られる
- **VAD は区切りの決定のみに使う**: デコードは前後 0.3 秒を足した区間で行う。
  VAD の切り出しは語頭に食い込むため、パディングなしでは
  「ヤンバルクイナ」が「クイナ」になる（実測）
- **部分表示はオーバーレイ、確定入力のみ wtype**: 任意アプリ内での
  backspace 修正は危険なため行わない。Windows の「聞き取りバー」と同じ方式
- **制御**: COSMIC カスタムショートカット Super+V → UNIX ソケット等で
  デーモンにトグルを通知（voxtype と同じパターン。置き換え時は
  nixfiles の `voice-input.nix` にある Spawn コマンドを差し替えるだけ）
- **句読点**: zipformer 出力には句読点がない。当初の「確定時に SenseVoice で
  再デコード」案は**却下**。ベンチで SenseVoice の CER が zipformer の
  3 倍（10.61% vs 3.54%）と判明したため、句読点と引き換えに精度を落とす
  ことになる（§4）。句読点は後処理（別モデルまたはルール）で付けるか、
  精度の高い別モデルを探す

## 3. 既存ベースライン（置き換え対象、稼働中）

nixfiles PR #110（マージ済み、`modules/home/parts/voice-input.nix`）:
voxtype-onnx + SenseVoice(small, ja) + wtype。Super+V トグル、停止後
約 1 秒で一括入力。r995 で実音声動作確認済み。

kikitori 完成までの利用者向け窓口はこのままにしておくこと。

## 4. PoC 実測結果（r995: Ryzen 9950X, 16 threads）

### 4.1 全バッファ再デコード方式の破綻（`poc_incremental.py` / `poc_mic.py`）

- 13 秒バッファ: 73〜136ms。**33 秒バッファ: 343ms** で 400ms 周期の限界
- **後ろに無音が伸びると確定済みテキストが壊れる**（決定的な問題）。
  同一発話（6.6 秒）に無音を付け足していったときの認識結果:

  | バッファ | 結果 |
  | --- | --- |
  | 発話のみ | はやくおじいさんにあのおとこのはなしをきかせたかった**のです** |
  | +8 秒 | …きかせたかった**の**（語尾欠落） |
  | +32 秒 | はやくおじいさんに**あの男の話を**きかせたかった**ので** |

  ユーザーのマイクテストで「沈黙を挟むと前の文章が消える」と報告された
  現象の原因。表示の問題ではなくモデルの挙動

### 4.2 VAD セグメント方式（`poc_vad.py`、現行）

- 確定セグメントは不変。無音を挟んでもテキストが壊れない
- デコード時間 **15〜60ms**（発話長で頭打ち。録音全体の長さに依存しない）
- 合成音声（発話 + 8 秒無音 + 発話 = 24 秒）で発話単独デコードと完全一致
- パラメータ: `min_silence_duration=0.8`（文中の息継ぎで切らない）、
  デコード区間の前後パディング 0.3 秒、部分表示は 1.0 秒遡る
  （VAD の検出遅れ吸収）。いずれも実測でスイープして決定

### 4.3 構成・モデル比較（`bench_asr.py`、test_wavs 50.5 秒の CER）

| 構成 | CER | RTF |
| --- | --- | --- |
| zipformer int8 greedy | 3.54% | 0.010 |
| zipformer int8 beam4 / beam8 | 3.54% | 0.014 / 0.017 |
| zipformer fp32 greedy / beam4 | 3.54% | 0.012 / 0.016 |
| SenseVoice small (int8, ja, itn) | 10.61% | 0.009 |

**beam search も fp32 も精度を1文字も改善しない**。SenseVoice は 3 倍悪い。
精度向上の余地はこれらの設定ではなく別モデル（未検証: whisper 系、
nemo canary、dolphin、fire-red-asr、qwen3-asr）にある。
比較は自分の声で録った音声で行うべき（test_wavs は ReazonSpeech と
同じニュース読み上げドメインで、現行モデルに有利）。

## 5. ロードマップと各段の設計メモ

1. [x] **PoC**: 逐次デコードの遅延・安定性計測（§4.1）
2. [x] **マイク体感テスト**: 実用的な精度・体感を確認。全バッファ方式の
   破綻（無音でテキストが壊れる）をここで発見
3. [x] **VAD セグメント方式**: `poc_vad.py`（§4.2）
4. [ ] **オーバーレイ表示**: GTK4 + gtk4-layer-shell（nixpkgs にあり）で
   画面下部中央の固定バー。COSMIC は wlr-layer-shell 対応。
   Python なら PyGObject + gtk4-layer-shell。カーソル位置追従は
   Wayland では不可能なので固定バーで良い（Windows も同じ）
5. [ ] **デーモン化**: トグル制御（UNIX ソケット、`kikitori toggle` CLI）、
   録音開始/停止、確定時 wtype 入力、句読点後処理（§2）。
   `poc_vad.py` の `Segmenter` がそのままコアになる
6. [ ] **配布**: flake 化（パッケージ + Home Manager モジュール or
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
