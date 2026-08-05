{
  description = "kikitori — 完全ローカル・リアルタイム日本語音声入力";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    inputs@{ self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          let
            pkgs = nixpkgs.legacyPackages.${system};
            # sherpa-onnx-sys は SHERPA_ONNX_LIB_DIR の 1 ディレクトリから
            # libsherpa-onnx-c-api と libonnxruntime の両方をリンクするため、
            # 2 パッケージの lib/ を 1 つに合成する
            # 注意: ここに gcc ランタイム（stdenv.cc.cc.lib）を入れてはいけない。
            # sherpa-onnx-sys の build.rs はこのディレクトリの全 .so を
            # target/debug へコピーするが、cargo は build script を
            # LD_LIBRARY_PATH=target/debug で起動するため、2 回目以降の
            # ビルドで「自分がマップ中の libgcc_s を自分で truncate」して
            # SIGSEGV する。libstdc++ の解決は rpath 側で行う（下記）
            sherpaLibs = pkgs.symlinkJoin {
              name = "sherpa-libs";
              paths = [
                pkgs.sherpa-onnx
                pkgs.onnxruntime
              ];
            };
          in
          f pkgs sherpaLibs
        );
    in
    {
      devShells = forAllSystems (
        pkgs: sherpaLibs: {
          default = pkgs.mkShell {
            packages = [
              # Python PoC / 実験ハーネス（poc/）
              (pkgs.python313.withPackages (p: [
                p.sherpa-onnx
                p.numpy
                p.sounddevice
              ]))
              pkgs.ruff

              # Rust（engine/ ほか）
              pkgs.pkg-config
              pkgs.alsa-lib # cpal（client の音声取得）
              pkgs.wtype # 確定テキストの入力（クライアントが実行する）
              pkgs.libxkbcommon # iced/winit が dlopen する
              pkgs.vulkan-loader # wgpu
              pkgs.wayland
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
            ];

            # 実行時: 生成バイナリに rpath を焼き込み、shell 外でも動くようにする。
            # libstdc++（sherpa-onnx-sys が直接リンクする）は別ディレクトリで
            # rpath に足す（sherpaLibs に混ぜると上記 SIGSEGV の原因になる）
            RUSTFLAGS = "-C link-arg=-Wl,-rpath,${sherpaLibs}/lib -C link-arg=-Wl,-rpath,${pkgs.stdenv.cc.cc.lib}/lib";

            # ビルド時: sherpa-onnx-sys の GitHub ダウンロードを短絡する。
            # store を直接指すと、build.rs が fs::copy で読み取り専用の
            # パーミッションごと target/ に .so を複製し、ビルドスクリプト
            # 再実行時（clippy 等）に上書きできず Permission denied になる。
            # 書き込み可能なキャッシュへ複製してそちらを指す
            # （ディレクトリ名に store ハッシュを含め、更新時に作り直す）
            shellHook = ''
              # iced/winit/wgpu が実行時に dlopen するライブラリ
              export LD_LIBRARY_PATH="${
                pkgs.lib.makeLibraryPath [
                  pkgs.libxkbcommon
                  pkgs.vulkan-loader
                  pkgs.wayland
                  pkgs.libGL
                ]
              }:$LD_LIBRARY_PATH"
              export SHERPA_ONNX_LIB_DIR="''${XDG_CACHE_HOME:-$HOME/.cache}/kikitori/${baseNameOf sherpaLibs}/lib"
              if [ ! -d "$SHERPA_ONNX_LIB_DIR" ]; then
                mkdir -p "$SHERPA_ONNX_LIB_DIR"
                cp -L ${sherpaLibs}/lib/*.so* "$SHERPA_ONNX_LIB_DIR/"
                chmod u+w "$SHERPA_ONNX_LIB_DIR"/*
              fi
            '';
          };
        }
      );

      packages = forAllSystems (
        pkgs: sherpaLibs: rec {
          default = kikitori;
          kikitori = pkgs.rustPlatform.buildRustPackage {
            pname = "kikitori";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.makeWrapper
            ];
            buildInputs = [
              pkgs.alsa-lib
              pkgs.libxkbcommon # smithay-client-toolkit (iced_layershell 経由)
            ];
            RUSTFLAGS = "-C link-arg=-Wl,-rpath,${sherpaLibs}/lib -C link-arg=-Wl,-rpath,${pkgs.stdenv.cc.cc.lib}/lib";
            # store を直指定すると build.rs の .so コピーが read-only になり
            # build script 再実行時に壊れる（devShell と同じ問題）。
            # 書き込み可能な複製を向ける
            preBuild = ''
              mkdir -p "$TMPDIR/sherpa-libs"
              cp -L ${sherpaLibs}/lib/*.so* "$TMPDIR/sherpa-libs/"
              chmod u+w "$TMPDIR"/sherpa-libs/*
              export SHERPA_ONNX_LIB_DIR="$TMPDIR/sherpa-libs"
            '';
            postInstall = ''
              # 開発・検証用バイナリは配布しない
              rm -f $out/bin/parity $out/bin/wavclient $out/bin/kikitori-cli
              # cargoInstallHook が 2 バイナリを入れたことを明示的に検証する
              # （workspace ルートに [package] が無い構成への懸念に対し、
              # 動いている実証 + 退行したら音を立てて落ちる保証で応える）
              test -x $out/bin/kikitorid
              test -x $out/bin/kikitori
              wrapProgram $out/bin/kikitori \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.wtype ]} \
                --prefix LD_LIBRARY_PATH : ${
                  pkgs.lib.makeLibraryPath [
                    pkgs.libxkbcommon
                    pkgs.vulkan-loader
                    pkgs.wayland
                    pkgs.libGL
                  ]
                }
              # `--version` が録音セッションを始めずに終わることを実証する。
              # 引数解析より先に GUI 初期化へ進む退行が入ると、ここが
              # timeout で落ちる（かつて 3 日 17 時間居座った事故の再発防止。
              # 単体テストは解析関数しか見られず、main の順序を守れない）
              timeout 60 $out/bin/kikitori --version
            '';
            meta.mainProgram = "kikitori";
          };
          # モデルの初回取得（nix store には入れない。HANDOFF §5 の方針どおり）
          kikitori-setup = pkgs.writeShellApplication {
            name = "kikitori-setup";
            runtimeInputs = [
              pkgs.curl
              pkgs.bzip2
              pkgs.gnutar
            ];
            text = ''
              dir="''${XDG_DATA_HOME:-$HOME/.local/share}/kikitori"
              mkdir -p "$dir"
              base=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models
              sv=sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17
              if [ ! -f "$dir/sensevoice/model.int8.onnx" ]; then
                echo "SenseVoice を取得中（約 200MB）…"
                curl --fail --show-error --retry 3 -L "$base/$sv.tar.bz2" | tar xj -C "$dir"
                mkdir -p "$dir/sensevoice"
                mv "$dir/$sv/model.int8.onnx" "$dir/$sv/tokens.txt" "$dir/sensevoice/"
                rm -rf "''${dir:?}/''${sv:?}"
              fi
              if [ ! -f "$dir/silero_vad.onnx" ]; then
                echo "silero VAD を取得中…"
                curl --fail --show-error --retry 3 -L -o "$dir/silero_vad.onnx" \
                  "$base/silero_vad.onnx"
              fi
              echo "完了: $dir"
            '';
          };
        }
      );

      homeManagerModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.services.kikitori;
          pkg = self.packages.${pkgs.system}.kikitori;
          setup = self.packages.${pkgs.system}.kikitori-setup;
          dataDir = "%h/.local/share/kikitori";
        in
        {
          options.services.kikitori = {
            enable = lib.mkEnableOption "kikitori 音声入力エンジン";
            tcp = lib.mkOption {
              type = lib.types.nullOr (lib.types.strMatching "[^[:space:]]+");
              default = null;
              example = "0.0.0.0:41717";
              description = ''
                TCP でも listen するアドレス。LAN 内の別マシンからエンジンを
                共用する（クライアント側は KIKITORI_ENGINE=host:port）。
                認証は持たないため、信頼できる LAN / SSH トンネル /
                Tailscale 前提。null なら Unix ソケットのみ。
              '';
            };
          };
          config = lib.mkIf cfg.enable {
            home.packages = [
              pkg
              setup
            ];
            systemd.user.services.kikitorid = {
              Unit.Description = "kikitori 音声認識エンジン";
              Service = {
                # モデル未取得のまま起動→即死→Restart ループを防ぐ
                # （setup は冪等。取得済みなら何もしない）
                ExecStartPre = "${setup}/bin/kikitori-setup";
                ExecStart =
                  "${pkg}/bin/kikitorid --sensevoice-dir ${dataDir}/sensevoice --vad-model ${dataDir}/silero_vad.onnx"
                  + lib.optionalString (cfg.tcp != null) " --tcp ${cfg.tcp}";
                Restart = "on-failure";
              };
              Install.WantedBy = [ "default.target" ];
            };
          };
        };

      formatter = forAllSystems (pkgs: _: pkgs.nixfmt-tree);
    };
}
