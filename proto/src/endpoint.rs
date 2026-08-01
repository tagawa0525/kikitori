//! エンジン接続先の解決。Unix ソケットパスと TCP `host:port` を 1 つの
//! 文字列表記で受ける（クライアントの --socket と KIKITORI_ENGINE 環境変数）。
//!
//! 規則: `/` 始まりは Unix ソケットパス、`:` を含めば TCP、それ以外も
//! Unix パスとして扱う（相対パス）。認証はトランスポート外
//! （SSH トンネル / Tailscale 前提。docs/PROTOCOL.md）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;

#[derive(Debug, PartialEq, Eq)]
pub enum Endpoint {
    Unix(String),
    Tcp(String),
}

pub fn parse_endpoint(_value: &str) -> Endpoint {
    todo!()
}

/// 接続して (reader, writer) を返す。切断は writer 側 drop でなく
/// `shutdown_write` を使う（EOF を相手に伝えるため）。
pub struct Connection {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    shutdown: Box<dyn Fn() -> std::io::Result<()> + Send>,
}

impl Connection {
    pub fn connect(endpoint: &Endpoint) -> std::io::Result<Connection> {
        match endpoint {
            Endpoint::Unix(path) => {
                let s = UnixStream::connect(path)?;
                let r = s.try_clone()?;
                let sd = s.try_clone()?;
                Ok(Connection {
                    reader: Box::new(r),
                    writer: Box::new(s),
                    shutdown: Box::new(move || sd.shutdown(std::net::Shutdown::Write)),
                })
            }
            Endpoint::Tcp(addr) => {
                let s = TcpStream::connect(addr)?;
                let r = s.try_clone()?;
                let sd = s.try_clone()?;
                Ok(Connection {
                    reader: Box::new(r),
                    writer: Box::new(s),
                    shutdown: Box::new(move || sd.shutdown(std::net::Shutdown::Write)),
                })
            }
        }
    }

    pub fn shutdown_write(&self) -> std::io::Result<()> {
        (self.shutdown)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_is_unix() {
        assert_eq!(
            parse_endpoint("/run/user/1000/kikitori.sock"),
            Endpoint::Unix("/run/user/1000/kikitori.sock".into())
        );
    }

    #[test]
    fn host_port_is_tcp() {
        assert_eq!(
            parse_endpoint("r995:41717"),
            Endpoint::Tcp("r995:41717".into())
        );
        assert_eq!(
            parse_endpoint("192.168.1.10:41717"),
            Endpoint::Tcp("192.168.1.10:41717".into())
        );
    }

    #[test]
    fn absolute_path_wins_even_with_colon() {
        // パスに : が含まれても / 始まりなら Unix
        assert_eq!(
            parse_endpoint("/tmp/odd:name.sock"),
            Endpoint::Unix("/tmp/odd:name.sock".into())
        );
    }

    #[test]
    fn bare_name_is_unix_path() {
        assert_eq!(
            parse_endpoint("kikitori.sock"),
            Endpoint::Unix("kikitori.sock".into())
        );
    }
}
