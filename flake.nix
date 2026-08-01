{
  description = "kikitori — 完全ローカル・リアルタイム日本語音声入力";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
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
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
            ];

            # 実行時: 生成バイナリに rpath を焼き込み、shell 外でも動くようにする
            RUSTFLAGS = "-C link-arg=-Wl,-rpath,${sherpaLibs}/lib";

            # ビルド時: sherpa-onnx-sys の GitHub ダウンロードを短絡する。
            # store を直接指すと、build.rs が fs::copy で読み取り専用の
            # パーミッションごと target/ に .so を複製し、ビルドスクリプト
            # 再実行時（clippy 等）に上書きできず Permission denied になる。
            # 書き込み可能なキャッシュへ複製してそちらを指す
            # （ディレクトリ名に store ハッシュを含め、更新時に作り直す）
            shellHook = ''
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

      formatter = forAllSystems (pkgs: _: pkgs.nixfmt-tree);
    };
}
