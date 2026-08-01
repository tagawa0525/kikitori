//! ユーザー定義の置換辞書（hyprwhspr の word overrides に相当）。
//!
//! 実地テストで誤認識がカタカナ英語の技術語彙に集中することが分かったため
//! （sherpa→シェルパ、cargo→カルゴ、SIGSEGV→シーグセブ）、認識後の
//! テキストにルールを順に適用する。デーモン側で適用し、`Segmenter` は
//! 触らない（Python 版とのパリティを保つため）。
//!
//! 形式: 1 行 1 ルール `誤認識<TAB>置換後`。`#` 始まりと空行は無視。

pub struct Replacer {
    rules: Vec<(String, String)>,
}

impl Replacer {
    pub fn parse(text: &str) -> Self {
        let rules = text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (from, to) = l.split_once('\t')?;
                Some((from.to_owned(), to.to_owned()))
            })
            .collect();
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (from, to) in &self.rules {
            out = out.replace(from, to);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tab_separated_rules() {
        let r = Replacer::parse("シェルパ\tsherpa\nカルゴ\tcargo\n");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let r = Replacer::parse("# コメント\n\nシェルパ\tsherpa\n");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn skips_lines_without_tab() {
        let r = Replacer::parse("タブなしの行\nシェルパ\tsherpa\n");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn applies_all_occurrences_in_order() {
        let r = Replacer::parse("シェルパ\tsherpa\nカルゴ\tcargo\n");
        assert_eq!(
            r.apply("シェルパとカルゴとシェルパ"),
            "sherpaとcargoとsherpa"
        );
    }

    #[test]
    fn earlier_rules_win_on_overlap() {
        // 「シェルパオニックス」を先に書けば「シェルパ」より優先される
        let r = Replacer::parse("シェルパオニックス\tsherpa-onnx\nシェルパ\tsherpa\n");
        assert_eq!(r.apply("シェルパオニックス"), "sherpa-onnx");
    }

    #[test]
    fn empty_dictionary_is_identity() {
        let r = Replacer::parse("");
        assert!(r.is_empty());
        assert_eq!(r.apply("そのまま"), "そのまま");
    }
}
