//! クライアント 2 つ（kikitori / kikitori-cli）で共有する引数解析。
//!
//! GUI 版は引数を一切見ずに録音セッションを始めていた。そのため
//! `kikitori --version` のような「問い合わせるだけ」のつもりの起動が、
//! マイクと制御ソケットを掴んだまま終わらないプロセスになる
//! （実際に 3 日以上居座った）。照会用の旗と未知の引数を、
//! 録音を始める前に確実に捌くために解析を 1 箇所へ集める。

/// 解析結果。
#[derive(Debug, PartialEq)]
pub enum Command {
    /// 通常実行（録音セッションを開始する）
    Run(Options),
    /// `--version`: 名前とバージョンを出して正常終了する
    Version,
    /// `--help`: 使い方を出して正常終了する
    Help,
}

/// 通常実行時の設定。
#[derive(Debug, Default, PartialEq)]
pub struct Options {
    /// `--socket PATH`: エンジン接続先の明示指定
    pub socket: Option<String>,
    /// `--wtype`: 確定テキストを wtype で入力する（kikitori-cli のみ）
    pub wtype: bool,
}

/// 実行ファイル名を除いた引数列を解析する。
///
/// `accept_wtype` が false の実行ファイルでは `--wtype` も未知の引数として
/// 拒否する。受け付けない旗を黙って無視すると、利用者は効いたと誤解する。
pub fn parse<I>(_args: I, _accept_wtype: bool) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    todo!("引数解析を実装する")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_gui(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| (*s).to_owned()), false)
    }

    fn parse_cli(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| (*s).to_owned()), true)
    }

    #[test]
    fn no_args_is_plain_run() {
        assert_eq!(parse_gui(&[]), Ok(Command::Run(Options::default())));
    }

    #[test]
    fn version_does_not_run() {
        // 本題: 照会だけのつもりの起動が録音を始めてはならない
        assert_eq!(parse_gui(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn help_does_not_run() {
        assert_eq!(parse_gui(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn unknown_flag_is_rejected() {
        // 黙って無視すると「効かない旗」に気づけないまま録音が始まる
        assert!(parse_gui(&["--nope"]).is_err());
    }

    #[test]
    fn positional_argument_is_rejected() {
        assert!(parse_gui(&["rec.wav"]).is_err());
    }

    #[test]
    fn socket_takes_the_next_value() {
        assert_eq!(
            parse_gui(&["--socket", "/run/x.sock"]),
            Ok(Command::Run(Options {
                socket: Some("/run/x.sock".into()),
                wtype: false,
            }))
        );
    }

    #[test]
    fn socket_without_value_is_rejected() {
        // 値を取り損ねたまま既定値で録音を始めると、意図しない接続先になる
        assert!(parse_gui(&["--socket"]).is_err());
    }

    #[test]
    fn wtype_is_accepted_where_supported() {
        assert_eq!(
            parse_cli(&["--wtype"]),
            Ok(Command::Run(Options {
                socket: None,
                wtype: true,
            }))
        );
    }

    #[test]
    fn wtype_is_rejected_where_unsupported() {
        assert!(parse_gui(&["--wtype"]).is_err());
    }

    #[test]
    fn query_flag_wins_over_other_arguments() {
        // `--socket X --version` でも接続や録音に進まないこと
        assert_eq!(
            parse_gui(&["--socket", "/run/x.sock", "--version"]),
            Ok(Command::Version)
        );
    }
}
