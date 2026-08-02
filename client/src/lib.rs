pub mod audio;
pub mod ctl;
pub mod overlay;
pub mod session;

/// エンジン接続先の解決: CLI 引数 → KIKITORI_ENGINE → 既定の Unix ソケット。
/// 2 つのクライアント（kikitori / kikitori-cli）で共通。
pub fn engine_endpoint(cli: Option<String>) -> kikitori_proto::endpoint::Endpoint {
    let value = cli
        .or_else(|| std::env::var("KIKITORI_ENGINE").ok())
        .unwrap_or_else(|| {
            let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
            format!("{dir}/kikitori.sock")
        });
    kikitori_proto::endpoint::parse_endpoint(&value)
}
