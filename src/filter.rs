//! Post-filter for recognized text: spoken symbols become characters, and a
//! trailing enter word (default "make it so") becomes an Enter key press.
//! Developers dictate paths and identifiers, so "src slash main dot rs" must
//! come out as "src/main.rs"; the enter word is a rare phrase so ordinary
//! speech never submits a prompt by accident.

use regex::Regex;
use std::sync::OnceLock;

fn symbols() -> &'static [(Regex, &'static str)] {
    static S: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    S.get_or_init(|| {
        [
            (r"(?i)\s*\b(?:forward slash|slash)\b\s*", "/"),
            (r"(?i)\s*\bunderscore\b\s*", "_"),
            // "dot" is also an ordinary word ("a red dot while recording"), so
            // it becomes "." only before a file extension, a slash or another dot
            (r"(?i)\s*\bdot\s+(rs|py|md|toml|json|jsonl|txt|sh|js|ts|tsx|html|css|yaml|yml|lock|log|c|h|cpp|go|rb|lua|conf|cfg|ini|env|git|com|org|net|io|dev|local|localdomain|wav|png|svg|tar|gz|zip|deb)\b", ".$1"),
            (r"(?i)\bdot\s*/\s*", "./"),
            (r"(?i)\bdot\s+dot\b", ".."),
            (r"(?i)\s*\b(?:dash|hyphen)\b\s*", "-"),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).unwrap(), r))
        .collect()
    })
}

/// Returns (text, press_enter). The enter word counts only as the final words
/// of the utterance; it is removed together with a preceding "and" and any
/// punctuation the model put around it.
pub fn postfilter(text: &str, enter_word: &str) -> (String, bool) {
    let mut t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (re, rep) in symbols() {
        t = re.replace_all(&t, *rep).into_owned();
    }
    let kw: Vec<String> = enter_word.split_whitespace().map(|w| w.to_lowercase()).collect();
    if kw.is_empty() {
        return (t, false);
    }
    let words: Vec<String> = t
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if words.len() < kw.len() || words[words.len() - kw.len()..] != kw[..] {
        return (t, false);
    }
    let body = kw.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join(r"[\s,.!?]+");
    let re = Regex::new(&format!(r"(?i)[\s,.!?]*(?:\band\s+)?{body}[\s,.!?]*$")).unwrap();
    (re.replace(&t, "").into_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::postfilter;

    #[test]
    fn symbols_become_characters() {
        assert_eq!(postfilter("open src slash main dot rs", "").0, "open src/main.rs");
        assert_eq!(postfilter("SEG underscore DEFAULT", "").0, "SEG_DEFAULT");
        assert_eq!(postfilter("feat forward slash my dash feat", "").0, "feat/my-feat");
        assert_eq!(postfilter("run dot slash build dot sh", "").0, "run ./build.sh");
        assert_eq!(postfilter("cd dot dot", "").0, "cd ..");
    }

    #[test]
    fn dot_as_a_word_stays() {
        assert_eq!(postfilter("show a red dot while recording", "").0, "show a red dot while recording");
    }

    #[test]
    fn enter_word_only_at_end() {
        assert_eq!(postfilter("Run the tests. Make it so.", "make it so"), ("Run the tests".into(), true));
        assert_eq!(postfilter("run the tests and make it so", "make it so"), ("run the tests".into(), true));
        assert_eq!(postfilter("make it so it works", "make it so"), ("make it so it works".into(), false));
        assert_eq!(postfilter("Make it so.", "make it so"), ("".into(), true));
    }

    #[test]
    fn empty_enter_word_disables() {
        assert_eq!(postfilter("make it so", ""), ("make it so".into(), false));
    }
}
