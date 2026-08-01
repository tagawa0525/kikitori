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
[sherpa-onnx SenseVoice small int8]      ──▶ 確定テキスト + 部分テキスト
      │                                             │
      │                              [オーバーレイ: 画面下部バーに逐次表示]
      │ トグル停止時
      ▼
[確定テキスト] ──wtype──▶ フォーカス中のアプリへ入力
```

- **擬似ストリーミング方式**: 真のストリーミングモデルではなく、offline
  モデルを短い区間に対して繰り返し適用する
- **VAD セグメント必須**: 当初の「録音バッファ全体を再デコード」は構造的に
  破綻する。offline モデルは全バッファを一度に見るため、後ろに無音が伸びると
  **確定済みのテキストまで壊れる**（§4.1）。VAD で区切り、確定済みセグメントを
  再デコードしないことが必須。デコード時間が発話長で頭打ちになる効果も
  同時に得られる
- **VAD は区切りの決定のみに使う**: デコードは前後 0.3 秒を足した区間で行う。
  VAD の切り出しは語頭に食い込むため、パディングなしでは
  「ヤンバルクイナ」が「クイナ」になる（実測）
- **部分表示はオーバーレイ、確定入力のみ wtype**: 任意アプリ内での
  backspace 修正は危険なため行わない。Windows の「聞き取りバー」と同じ方式
- **制御**: COSMIC カスタムショートカット Super+V → UNIX ソケット等で
  デーモンにトグルを通知（voxtype と同じパターン。置き換え時は
  nixfiles の `voice-input.nix` にある Spawn コマンドを差し替えるだけ）
- **句読点**: SenseVoice が句読点と ITN を出すため、後処理は不要になった。
  ただし日本語の途中に空白を入れるので、その除去だけ行っている（§4.5）

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
- パラメータ（実測でスイープして決定、`poc_vad.Params`）:
  `min_silence=0.5`（文中の息継ぎで切らない）、デコード区間のパディングは
  前 0.3 秒・後 0.8 秒、部分表示は 1.0 秒遡る（VAD の検出遅れ吸収）

### 4.3 発話が長いと内容が落ちる（区間長の上限が必須）

無音を挟まない連続発話を丸ごとデコードしたときの CER:

| 発話長 | 14 秒まで | 23.7 秒 | 44.3 秒 | 63.9 秒 |
| --- | --- | --- | --- | --- |
| CER | **0.0%** | 9.4% | 26.6% | 55.2% |

ReazonSpeech は短い発話で学習されており、長い音声では内容そのものを
落とす（テキストが途中から次の話題に飛ぶ）。ユーザーの
「30 秒話し続けると前半が棄却される」という報告の原因。

**sherpa の `max_speech_duration` はハードキャップとして機能しない。**
同一音声で 12 秒指定 → 32.6 秒の区間、15 秒指定 → 22.6 秒の区間と、
指定値と無関係かつ単調ですらない。自前で上限を掛けること
上限到達時は直前 2 秒で最も静かな 100ms を選んで分割する。
なお SenseVoice に切り替えた現在は区間長の影響がほぼないため（§4.5）、
この上限は暴走を防ぐ安全網として 25 秒に置いてある。

### 4.4 構成比較（`bench_asr.py --whole`、test_wavs 50.5 秒の CER）

| 構成 | CER | RTF |
| --- | --- | --- |
| zipformer int8 greedy | 3.54% | 0.010 |
| zipformer int8 beam4 / beam8 | 3.54% | 0.014 / 0.017 |
| zipformer fp32 greedy / beam4 | 3.54% | 0.012 / 0.016 |
| SenseVoice small (int8, ja, itn) | 10.61% | 0.009 |

**beam search も fp32 も精度を1文字も改善しない**。この土俵では
SenseVoice は 3 倍悪い — が、この結論は実音声では逆転する（§4.5）。

### 4.5 モデル比較（自分の声 4 本 210 秒、区間長別 CER）

`bench_data/voice1〜4.wav`。voice1 は調整に使ったので、voice2〜4 が held-out。

| モデル / 区間長 | 全体 | voice1 | voice2 | voice3 | voice4 | RTF |
| --- | --- | --- | --- | --- | --- | --- |
| **SenseVoice 25s** | **7.0%** | 4.3% | 1.7% | 11.9% | 8.7% | 0.013 |
| SenseVoice 6s | 7.3% | 4.8% | 1.7% | 12.3% | 9.0% | 0.014 |
| whisper-large-v3 25s | 11.8% | 10.2% | 8.0% | 18.0% | 10.4% | 1.351 |
| whisper-turbo 25s | 13.2% | 9.1% | 8.4% | 23.4% | 10.7% | 0.159 |
| dolphin-base 4s | 15.3% | 9.1% | 8.0% | 21.5% | 19.7% | 0.010 |
| zipformer 6s | 17.0% | 7.0% | 4.6% | 17.2% | 33.6% | 0.017 |
| zipformer 25s | 28.3% | 51.9% | 4.2% | 14.2% | 45.7% | 0.015 |

- **ニュース音声での結論（§4.4）は実音声で完全に逆転する**。SenseVoice が
  全区間長・全録音で zipformer を上回り、句読点と ITN も付く
- **§4.3 で決めた「6 秒」は voice1 への過学習だった**。voice1 では
  29.5%→7.0% と劇的に改善したが voice4 では 33.6%。最適な区間長が録音ごとに
  異なり、zipformer には全録音で安定する設定が存在しない
- SenseVoice は**区間長にほとんど影響されない**（6〜25 秒で 7.0〜7.3%）ため、
  §4.3 の自前キャップは安全網として残るだけで、調整項目ではなくなった
- whisper 系は 1.7〜1.9 倍悪く 10 倍以上遅い。ただし「デコード時間」を唯一
  正しく認識するなど得意不得意が違うので、語彙によっては見直す価値がある
- `pad_head` は精度にほぼ影響しない（0.3〜1.5 秒で 7.0〜7.5%）。語頭の
  欠落として観測されたもの（「認識した文字が」→「下文字が」）は切り出しでは
  なく認識誤りだった
- 部分デコードの所要時間（16 スレッド、SenseVoice）:
  10 秒 100ms / 15 秒 129ms / 20 秒 190ms / 25 秒 218ms / 30 秒 301ms。
  400ms 周期に対し 25 秒でも余裕がある
- SenseVoice は日本語の途中に空白を入れる（「プルリクエスト の レビュー」）。
  両隣が非 ASCII の空白だけを落とす後処理を入れてある

## 5. ロードマップと各段の設計メモ

1. [x] **PoC**: 逐次デコードの遅延・安定性計測（§4.1）
2. [x] **マイク体感テスト**: 実用的な精度・体感を確認。全バッファ方式の
   破綻（無音でテキストが壊れる）をここで発見
3. [x] **VAD セグメント方式**: `poc_vad.py`（§4.2, §4.3）
4. [x] **モデル選定**: SenseVoice に切り替え（§4.5）。CER 28.3%→7.0%
5. [ ] **オーバーレイ表示**: GTK4 + gtk4-layer-shell（nixpkgs にあり）で
   画面下部中央の固定バー。COSMIC は wlr-layer-shell 対応。
   Python なら PyGObject + gtk4-layer-shell。カーソル位置追従は
   Wayland では不可能なので固定バーで良い（Windows も同じ）
6. [ ] **デーモン化**: トグル制御（UNIX ソケット、`kikitori toggle` CLI）、
   録音開始/停止、確定時 wtype 入力、句読点後処理（§2）。
   `poc_vad.py` の `Segmenter` がそのままコアになる
7. [ ] **配布**: flake 化（パッケージ + Home Manager モジュール or
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

## 8. Rust への移行（方針決定済み、着手前）

最終的な実装は Rust にする（ユーザー指定）。認識精度は変わらない
（sherpa-onnx は C++ 実装で、Python も Rust も同じライブラリを呼ぶだけ）。
狙いは常駐デーモンとしての定常レイテンシ・メモリと、単一バイナリでの配布。

**PoC で方式を固めきってから移すこと。** 現時点で「全バッファ再デコードは
破綻する」「最適な区間長はモデルごとに全く違う」「モデル選定はニュース音声で
判断すると誤る」と設計判断が繰り返し覆っている。

以下は実際にビルド・リンクして確認済み（nixpkgs 26.11pre-git, rustc 1.97.1）。

### 8.1 sherpa-onnx バインディング

- **`sherpa-rs` は使わない**。2026-06-06 にアーカイブ済みで、README が公式
  バインディングへの移行を指示している
- **公式の `sherpa-onnx` / `sherpa-onnx-sys` クレートを使う**。k2-fsa が
  本体リポジトリ内で保守し、C++ 側のリリースと版が揃う
- **nixpkgs の `sherpa-onnx` は 1.13.3 なのでクレートも 1.13.3 に固定する**。
  sys 側の FFI は版ごとの手書きなので、版ずれは構造体レイアウトの齟齬になる
- SenseVoice は `OfflineSenseVoiceModelConfig { model, language, use_itn }` を
  `OfflineRecognizerConfig.model_config.sense_voice` に入れる形。Python の
  `from_sense_voice` のようなコンストラクタではない
- silero VAD も `SileroVadModelConfig` / `VoiceActivityDetector` /
  `SpeechSegment` として揃っている
- **落とし穴: config は `#[derive(Default)]` で全フィールドが 0**。
  sherpa の推奨既定値は入らないので、`min_silence_duration` なども含めて
  全部明示的に設定すること
- `LinearResampler::create(in_hz, out_hz)` があり 48k→16k に使える

### 8.2 Nix でのビルド

- `sherpa-onnx-sys` の build.rs は既定で GitHub からビルド済みアーカイブを
  **ダウンロードする**（Nix サンドボックスでは不可）。
  `default-features = false, features = ["shared"]` にしたうえで
  環境変数 `SHERPA_ONNX_LIB_DIR` を指すと短絡でき、ネットワークなしで通る
- 指す先は `sherpa-onnx` と `onnxruntime` の `.so` を `symlinkJoin` で
  1 ディレクトリにまとめたもの（両方が同じディレクトリに要る）
- sys クレートは FFI が手書きで **bindgen を使わない**ため、libclang を
  ビルド閉包に持ち込まずに済む
- nixpkgs の `sherpa-onnx` は `BUILD_SHARED_LIBS=true` で
  `lib/libsherpa-onnx-c-api.so` と `include/sherpa-onnx/c-api/c-api.h` を出す。
  `.pc` は `$out/lib/pkgconfig/` ではなく `$out/sherpa-onnx.pc` にあるので注意

### 8.3 音声取得（cpal）

- `cpal` 0.18 で **PipeWire ホストがネイティブ対応**（Linux の優先順位は
  PipeWire > PulseAudio > ALSA）。ただし `default = []` なので
  `features = ["pipewire"]` が要る
- **PipeWire ホストは 48kHz ステレオしか出さない**（r995 実測。1ch や 16kHz の
  構成は提示されない）。ALSA ホスト経由なら 16kHz mono も選べるが、それは
  `plug` が裏で変換しているだけで PipeWire への余計な往復が入る。
  **48kHz ステレオで取ってダウンミックス＋リサンプルを自前でやる**ほうが
  予測可能（リサンプルは §8.1 の `LinearResampler`）
- **落とし穴: `--features pipewire` は NixOS でビルドが壊れる**
  （`SPA_ID_INVALID` が見つからない）。bindgen が `stdint.h` を拾えないため。
  `BINDGEN_EXTRA_CLANG_ARGS="-isystem $(clang -print-resource-dir)/include"` と
  `LIBCLANG_PATH` を渡せば通る。PipeWire や clang の版には依存しない
- 0.18 で API が変わった（`DeviceTrait::name()` は廃止、`description()` /
  `id()` に。`sample_rate` は `SampleRate` ではなく `u32`）。
  0.18 より前のサンプルコードはそのままでは通らない

### 8.4 オーバーレイ（gtk4-layer-shell）

- クレート `gtk4-layer-shell` 0.8.0 が `gtk` 0.11 に対応し、nixpkgs の
  `gtk4-layer-shell` 1.3.0 / `gtk4` 4.22.4 と版が噛み合う（`v1_3` feature）
- 画面下端のバーは
  `init_layer_shell / set_layer(Overlay) / set_anchor(Edge::Bottom, true) /
  set_keyboard_mode(KeyboardMode::None) / auto_exclusive_zone_enable()`
  でビルドが通ることを確認済み（実機の合成器での表示は未確認）
- 「passively-maintained」を自称しているが、C ライブラリの薄いラッパなので
  実用上の問題はない見込み

### 8.5 確定テキストの入力

- **`wtype` を外部コマンドとして呼ぶ**のが現実解（`pkgs.wtype` 0.4）。
  §6 のとおり COSMIC 1.5 で日本語直接入力が動くことは確認済み
- `zwp_virtual_keyboard_v1` をライブラリとして提供する保守された
  クレートは**存在しない**。`zwp-virtual-keyboard` / `zwp-input-method` は
  DEPRECATED（0.0.0、2022 年で停止）、`enigo` の Wayland 対応は実験的
- 自前で実装するなら `wayland-protocols-misc`（smithay/wayland-rs 系、保守良好）
  に `virtual-keyboard-unstable-v1.xml` がある。ただし XKB キーマップ生成・
  memfd・任意 Unicode のキーコード割り当てを自分で持つことになる。
  `wrtype` が参考実装になるが、単発リリースで保守状況が確認できず依存は避ける

## 9. 関連リソース

- nixfiles: `modules/home/parts/voice-input.nix`（ベースライン実装。
  検証済み知見が全てコメントに残してある）
- voxtype: <https://github.com/peteonrails/voxtype>（設計の参考。
  ストリーミング時の出力方式 `StreamingEvent::Replace` など）
- モデル: <https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models>。
  比較した `sherpa-onnx-whisper-turbo` / `-large-v3` / `-small`、
  `sherpa-onnx-dolphin-base-ctc-multi-lang-2025-04-02`、
  `sherpa-onnx-zipformer-ja-reazonspeech-2024-08-01`、`silero_vad.onnx`
- **採用中の SenseVoice は現在 voxtype の配置を参照している**
  （`~/.local/share/voxtype/models/sensevoice-small`）。配布時は kikitori 自身で
  取得する必要がある（`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17`）
- sherpa-onnx ドキュメント: <https://k2-fsa.github.io/sherpa/onnx/>
