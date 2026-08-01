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
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            # Python PoC / 実験ハーネス（poc/）
            (pkgs.python313.withPackages (p: [
              p.sherpa-onnx
              p.numpy
              p.sounddevice
            ]))
            pkgs.ruff
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
