//! セッション中の確定テキストの唯一の蓄積。
//!
//! これまで確定テキストは「表示用（GUI の commits、上限で古い行を破棄）」と
//! 「入力用（受信スレッドの session、全件保持）」の 2 箇所に別々に溜めて
//! いたため、表示された確定と wtype で入力される文字列が食い違い得た。
//! 表示（末尾の数件）と入力（全文連結）を同じ置き場から導出することで、
//! 経路を一本化して食い違いを構造的に防ぐ。

/// 確定テキストの蓄積。表示と入力の両方がここから導出される。
#[derive(Default)]
pub struct Session {
    commits: Vec<String>,
}

impl Session {
    /// 確定テキストを追加する。
    pub fn push(&mut self, text: String) {
        self.commits.push(text);
    }

    /// 確定が 1 つもないか。
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// 表示用: 末尾 `n` 件の確定。件数が `n` に満たなければ全件。
    pub fn tail(&self, n: usize) -> &[String] {
        &self.commits[self.commits.len().saturating_sub(n)..]
    }

    /// 入力用: 全確定の区切りなし連結。表示から隠れた古い確定も含む。
    pub fn text(&self) -> String {
        self.commits.concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_plain_concat() {
        let mut s = Session::default();
        s.push("こんにちは".into());
        s.push("世界".into());
        assert_eq!(s.text(), "こんにちは世界", "区切り文字を挟まない連結");
    }

    #[test]
    fn tail_shows_only_last_n() {
        let mut s = Session::default();
        for i in 0..12 {
            s.push(format!("確定{i}"));
        }
        let tail = s.tail(9);
        assert_eq!(tail.len(), 9);
        assert_eq!(tail[0], "確定3", "古い確定は表示から隠れる");
        assert_eq!(tail[8], "確定11");
    }

    #[test]
    fn text_keeps_commits_hidden_from_tail() {
        // 表示が古い行を隠しても、入力される全文には残る
        let mut s = Session::default();
        for i in 0..12 {
            s.push(format!("確定{i}"));
        }
        let text = s.text();
        for i in 0..12 {
            assert!(text.contains(&format!("確定{i}")), "確定{i} が全文にない");
        }
    }

    #[test]
    fn tail_with_few_commits_returns_all() {
        let mut s = Session::default();
        s.push("あ".into());
        s.push("い".into());
        assert_eq!(s.tail(9), ["あ", "い"]);
    }

    #[test]
    fn empty_session() {
        let mut s = Session::default();
        assert!(s.is_empty());
        assert_eq!(s.text(), "");
        assert!(s.tail(9).is_empty());
        s.push("あ".into());
        assert!(!s.is_empty());
    }
}
