/// SenseVoice は日本語の途中に空白を入れる（「プルリクエスト の レビュー」）。
/// 両隣が非 ASCII のものだけ落とし、英単語間の空白は残す。
/// Python 版 `poc/poc_vad.py` の `strip_japanese_spaces`
/// （正規表現 `(?<=[^\x00-\x7f]) +(?=[^\x00-\x7f])`）と同一の挙動にする。
pub fn strip_japanese_spaces(_text: &str) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_spaces_between_japanese() {
        assert_eq!(
            strip_japanese_spaces("プルリクエスト の レビュー"),
            "プルリクエストのレビュー"
        );
    }

    #[test]
    fn removes_multiple_spaces_between_japanese() {
        assert_eq!(strip_japanese_spaces("あ  い"), "あい");
    }

    #[test]
    fn keeps_spaces_between_ascii_words() {
        assert_eq!(strip_japanese_spaces("hello world"), "hello world");
    }

    #[test]
    fn keeps_spaces_adjacent_to_ascii() {
        // 片側でも ASCII なら残す（英単語の境界を壊さない）
        assert_eq!(
            strip_japanese_spaces("設定は Podman です"),
            "設定は Podman です"
        );
    }

    #[test]
    fn keeps_leading_and_trailing_spaces() {
        // 端の空白は両隣が揃わないので対象外
        assert_eq!(strip_japanese_spaces(" あい "), " あい ");
    }

    #[test]
    fn empty_string() {
        assert_eq!(strip_japanese_spaces(""), "");
    }
}
