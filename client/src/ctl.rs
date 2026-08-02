//! 制御ソケットの役割判定（issue #7）。
//!
//! スポーン型クライアントは「1 回目の起動 = 録音、2 回目 = 停止指示」を
//! 単一の Unix ソケットで実現する。判定（connect）と占有（bind）が別操作だと
//! その間に窓が生まれ、ホットキー連打で二重セッションになるため、
//! bind の成否そのものを役割判定にする。

use std::io;
use std::os::unix::net::UnixListener;

/// 役割判定の結果。
pub enum Claim {
    /// bind に成功した。自分が録音役。listener で停止指示を待つ
    Recorder(UnixListener),
    /// 既存インスタンスに停止指示を送った。即終了してよい
    Stopped,
}

/// 制御ソケットを bind し、その成否で役割を決める。
///
/// - bind 成功 → `Recorder`
/// - `AddrInUse` → connect を試み、成功すれば停止指示として `Stopped`
/// - connect も失敗 → 異常終了の残骸と判断して削除し、再 bind
pub fn claim(path: &str) -> io::Result<Claim> {
    let _ = path;
    todo!("GREEN コミットで実装")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_sock(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kikitori-ctl-{}-{name}.sock", std::process::id()))
    }

    #[test]
    fn fresh_path_becomes_recorder() {
        let path = temp_sock("fresh");
        let _ = std::fs::remove_file(&path);
        match claim(path.to_str().unwrap()).unwrap() {
            Claim::Recorder(_) => {}
            Claim::Stopped => panic!("誰もいないので録音役になるはず"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn second_claim_signals_stop() {
        let path = temp_sock("second");
        let _ = std::fs::remove_file(&path);
        let Ok(Claim::Recorder(listener)) = claim(path.to_str().unwrap()) else {
            panic!("1 回目は録音役になるはず");
        };
        // 録音役がまだ accept していなくても（起動中の窓）、停止指示は
        // バックログに積まれ、後から accept できること
        let Ok(Claim::Stopped) = claim(path.to_str().unwrap()) else {
            panic!("2 回目は停止指示になるはず");
        };
        listener.set_nonblocking(true).unwrap();
        assert!(
            listener.accept().is_ok(),
            "停止指示の接続がバックログから accept できるはず"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_socket_is_reclaimed() {
        let path = temp_sock("stale");
        let _ = std::fs::remove_file(&path);
        let Ok(Claim::Recorder(listener)) = claim(path.to_str().unwrap()) else {
            panic!("準備: 録音役になるはず");
        };
        // UnixListener は Drop でソケットファイルを unlink しない =
        // 異常終了の残骸を模擬できる
        drop(listener);
        assert!(path.exists(), "残骸ファイルが残っているはず");
        match claim(path.to_str().unwrap()).unwrap() {
            Claim::Recorder(_) => {}
            Claim::Stopped => panic!("残骸は回収して録音役になるはず"),
        }
        let _ = std::fs::remove_file(&path);
    }
}
