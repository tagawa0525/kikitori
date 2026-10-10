# NixOS / home-manager への組み込み

kikitori の flake は home-manager モジュール `homeManagerModules.default` を公開する。
nixfiles では 2026-08〜10 に組み込んでいたが、使わなくなったため 2026-10-10 に外した
（決定は nixfiles の `docs/adr/` を参照）。再び組み込むときはこの手順で足りる。

## 組み込み手順

1. 利用側の flake に input を足す。nixpkgs は利用側と揃える（二重に評価されない）

   ```nix
   kikitori = {
     url = "github:tagawa0525/kikitori";
     inputs.nixpkgs.follows = "nixpkgs";
   };
   ```

2. モジュールを全ユーザーに公開する

   ```nix
   home-manager.sharedModules = [ kikitori.homeManagerModules.default ];
   ```

3. エンジンを動かすホスト（SenseVoice が走る側）で有効にする。クライアントだけの
   ホストは `enable = false` のままでよい

   ```nix
   services.kikitori = {
     enable = true;
     tcp = "0.0.0.0:41717"; # 他ホストから使うときだけ
   };
   ```

初回起動時に `ExecStartPre` の `kikitori-setup` がモデル（約 200MB）を
`~/.local/share/kikitori/` へ取得する。モデルは nix store に入れない。

## overlay は要らない

`pkgs.kikitori` を overlay で足す必要はない。クライアントのコマンドパスは
`services.kikitori.package` から得る。`enable = false` のホストでも評価できる:

```nix
{ config, lib, ... }:
let
  kikitori = lib.getExe config.services.kikitori.package;
in
{ /* ショートカットなどが ${kikitori} を参照する */ }
```

nixfiles が overlay を足していたのは、この `package` オプションが無く、
`enable = false` のクライアントホストからパッケージを参照する手段が
`pkgs.kikitori` しかなかったため。

## COSMIC のショートカット（Super+V）

COSMIC のカスタムショートカットは宣言的に置ける。反映にログアウトは要らない。

```nix
xdg.configFile."cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom".text = ''
  {
      (modifiers: [Super], key: "v"): Spawn("${kikitoriCommand}"),
  }
'';
```

エンジンが別ホストのとき、接続先は環境変数 `KIKITORI_ENGINE` ではなく引数
`--socket host:port` で渡すこと。`Spawn` は cosmic-comp の環境を継承するが、
cosmic-comp は cosmic-greeter / cosmic-session 経由で起動されてログインシェルを
通らず、`home.sessionVariables`（profile.d 経由）が届かない。届かないまま起動した
クライアントはローカルソケットへの接続に失敗し、バーを出さないままプロセスだけが
残る（接続失敗時に exit しない挙動）。`--socket` は `host:port` 形式を TCP と解釈する。

## ネットワーク

- TCP は認証を持たない。tailnet のように信頼できる経路に限る
- NixOS のファイアウォールは全インターフェース開放（`openFirewall = true`）にせず、
  `networking.firewall.interfaces."tailscale0".allowedTCPPorts = [ 41717 ];` のように
  インターフェースを限る
- クライアントがエンジンに届かない間（tailnet の外など）は、バーを出さずに待機し
  続けるため、無反応に見える

## 設計判断の置き場

ホットキーに evdev を使わない理由、ペーストキー方式を採らない理由、whisper ではなく
SenseVoice を使う理由は [HANDOFF.md](HANDOFF.md) にある。
