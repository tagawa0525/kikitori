//! COSMIC 等のショートカットから呼ぶトグル。制御ソケットに接続するだけ。
use std::os::unix::net::UnixStream;

fn main() {
    let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    UnixStream::connect(format!("{dir}/kikitori-ctl.sock"))
        .expect("kikitori-overlay が起動していない");
}
