//! Case-insensitive, literal-word search for command lists and histories.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
};

/// One matcher per query. Spaces require every word to match, in any order;
/// punctuation is literal, so shell flags never become search operators.
pub struct CommandQuery {
    words: Vec<(String, Atom)>,
    matcher: Matcher,
    buffer: Vec<char>,
    valid: bool,
}

impl CommandQuery {
    pub fn new(query: &str) -> Self {
        let valid = query.len() <= 2048;
        let words = if valid {
            query
                .split_whitespace()
                .map(|word| {
                    (
                        word.to_lowercase(),
                        Atom::new(
                            word,
                            CaseMatching::Ignore,
                            Normalization::Smart,
                            AtomKind::Fuzzy,
                            false,
                        ),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        Self { words, matcher: Matcher::new(config), buffer: Vec::new(), valid }
    }

    pub fn score(&mut self, text: &str) -> Option<u32> {
        self.score_fields(&[text])
    }

    /// Earlier fields have priority (for example, a saved name before its body).
    pub fn score_fields(&mut self, fields: &[&str]) -> Option<u32> {
        if !self.valid {
            return None;
        }
        let mut total = 0;
        for (word, atom) in &self.words {
            let mut best = None;
            for (index, text) in fields.iter().enumerate() {
                let haystack = Utf32Str::new(text, &mut self.buffer);
                let Some(fuzzy) = atom.score(haystack, &mut self.matcher) else { continue };
                let folded = text.to_lowercase();
                let tier = if folded == *word {
                    4
                } else if folded.starts_with(word.as_str()) {
                    3
                } else if folded
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|part| part.starts_with(word.as_str()))
                {
                    2
                } else if folded.contains(word.as_str()) {
                    1
                } else {
                    0
                };
                let score = (fields.len() - index - 1) as u32 * 5_000
                    + tier * 1_000
                    + u32::from(fuzzy).min(999);
                best = Some(best.map_or(score, |previous: u32| previous.max(score)));
            }
            total += best?;
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_spaces_and_abbreviations_are_consistent() {
        for query in ["DOCKER COMPOSE", "compose docker", "dck cmp"] {
            assert!(CommandQuery::new(query).score("Docker Compose up").is_some());
        }
        assert!(
            CommandQuery::new("环境 CREATE").score_fields(&["创建环境", "conda create"]).is_some()
        );
        assert!(CommandQuery::new("docker missing").score("docker compose").is_none());
        assert!(CommandQuery::new("-xyz").score("docker compose").is_none());
        assert!(CommandQuery::new(&"a".repeat(2049)).score("anything").is_none());
    }

    #[test]
    fn exact_prefix_word_substring_and_fuzzy_have_stable_priority() {
        let mut query = CommandQuery::new("git");
        let scores =
            ["git", "gitk", "run git", "legit", "g-in-t"].map(|text| query.score(text).unwrap());
        assert!(scores.windows(2).all(|pair| pair[0] > pair[1]));
        assert!(
            query.score_fields(&["Git status", "echo hello"]).unwrap()
                > query.score_fields(&["Inspect repository", "git status"]).unwrap()
        );
    }
}
