use std::borrow::Cow;

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::options::{CompletionOptions, MatchAlgorithm};
use crate::suggestion::SemanticSuggestion;

// ---------------------------------------------------------------------------
// IgnoreCaseExt — case folding via unicase
// ---------------------------------------------------------------------------

pub trait IgnoreCaseExt {
    /// Case-folded equivalent of this string (for comparison, not display).
    fn to_folded_case(&self) -> String;
    /// Case-insensitive equality check.
    fn eq_ignore_case(&self, other: &str) -> bool;
}

impl IgnoreCaseExt for str {
    fn to_folded_case(&self) -> String {
        unicase::UniCase::new(self).to_folded_case()
    }

    fn eq_ignore_case(&self, other: &str) -> bool {
        unicase::UniCase::new(self) == unicase::UniCase::new(other)
    }
}

// ---------------------------------------------------------------------------
// CandidateMatcher — the core fuzzy / prefix / substring matching engine
// ---------------------------------------------------------------------------

const QUOTES: [char; 3] = ['"', '\'', '`'];

/// Internal data for an unscored (prefix / substring) match.
struct UnscoredMatch<T> {
    item: T,
    haystack: String,
    match_indices: Vec<usize>,
}

/// Internal data for a fuzzy (scored) match.
struct FuzzyMatch<T> {
    item: T,
    haystack: String,
    score: u16,
    match_indices: Vec<usize>,
}

enum State<T> {
    Unscored(Vec<UnscoredMatch<T>>),
    Fuzzy {
        matcher: Matcher,
        atom: Atom,
        matches: Vec<FuzzyMatch<T>>,
    },
}

/// Filters and sorts candidate completions against a needle using the
/// configured [`MatchAlgorithm`].
///
/// Supports three algorithms:
/// * [`Prefix`](MatchAlgorithm::Prefix) — `starts_with`
/// * [`Substring`](MatchAlgorithm::Substring) — `contains`
/// * [`Fuzzy`](MatchAlgorithm::Fuzzy) — character-skip matching via `nucleo_matcher`
pub struct CandidateMatcher<'a, T> {
    options: &'a CompletionOptions,
    should_sort: bool,
    needle: String,
    state: State<T>,
}

impl<T> CandidateMatcher<'_, T> {
    /// Create a new matcher.
    ///
    /// * `needle` — The text to search for. Leading/trailing quotes are stripped.
    /// * `options` — Matching configuration (algorithm, case sensitivity, etc.).
    /// * `should_sort` — Whether results should be sorted before returning.
    pub fn new(
        needle: impl AsRef<str>,
        options: &CompletionOptions,
        should_sort: bool,
    ) -> CandidateMatcher<'_, T> {
        let needle = needle.as_ref().trim_matches(QUOTES);
        match options.match_algorithm {
            MatchAlgorithm::Prefix | MatchAlgorithm::Substring => {
                let lowercase_needle = if options.case_sensitive {
                    needle.to_owned()
                } else {
                    needle.to_folded_case()
                };
                CandidateMatcher {
                    options,
                    should_sort,
                    needle: lowercase_needle,
                    state: State::Unscored(Vec::new()),
                }
            }
            MatchAlgorithm::Fuzzy => {
                let atom = Atom::new(
                    needle,
                    if options.case_sensitive {
                        CaseMatching::Respect
                    } else {
                        CaseMatching::Ignore
                    },
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                );
                CandidateMatcher {
                    options,
                    should_sort,
                    needle: needle.to_owned(),
                    state: State::Fuzzy {
                        matcher: Matcher::new({
                            let mut cfg = Config::DEFAULT;
                            cfg.prefer_prefix = true;
                            cfg
                        }),
                        atom,
                        matches: Vec::new(),
                    },
                }
            }
        }
    }

    /// Internal: test `haystack` against the needle and optionally store the item.
    fn matches_aux(&mut self, orig_haystack: &str, item: Option<T>) -> Option<Vec<usize>> {
        let haystack = orig_haystack.trim_start_matches(QUOTES);
        let offset = orig_haystack.len() - haystack.len();
        let haystack = haystack.trim_end_matches(QUOTES);
        match &mut self.state {
            State::Unscored(matches) => {
                let haystack_folded = if self.options.case_sensitive {
                    Cow::Borrowed(haystack)
                } else {
                    Cow::Owned(haystack.to_folded_case())
                };
                let match_start = match self.options.match_algorithm {
                    MatchAlgorithm::Prefix => {
                        if haystack_folded.starts_with(self.needle.as_str()) {
                            Some(0)
                        } else {
                            None
                        }
                    }
                    MatchAlgorithm::Substring => haystack_folded.find(self.needle.as_str()),
                    _ => unreachable!("Only prefix and substring algorithms don't use score"),
                };
                match_start.map(|byte_start| {
                    let grapheme_start = haystack_folded[0..byte_start].graphemes(true).count();
                    let grapheme_len = self.needle.graphemes(true).count();
                    let match_indices: Vec<usize> =
                        (offset + grapheme_start..offset + grapheme_start + grapheme_len).collect();
                    if let Some(item) = item {
                        matches.push(UnscoredMatch {
                            item,
                            haystack: haystack.to_string(),
                            match_indices: match_indices.clone(),
                        });
                    }
                    match_indices
                })
            }
            State::Fuzzy {
                matcher,
                atom,
                matches,
            } => {
                let mut haystack_buf = Vec::new();
                let haystack_utf32 = Utf32Str::new(haystack, &mut haystack_buf);
                let mut indices = Vec::new();
                let score = atom.indices(haystack_utf32, matcher, &mut indices)?;
                let indices: Vec<usize> = indices
                    .iter()
                    .map(|i| {
                        offset
                            + usize::try_from(*i)
                                .expect("index should fit in usize on 32+ bit systems")
                    })
                    .collect();
                if let Some(item) = item {
                    matches.push(FuzzyMatch {
                        item,
                        haystack: haystack.to_string(),
                        score,
                        match_indices: indices.clone(),
                    });
                }
                Some(indices)
            }
        }
    }

    /// Add `item` if `haystack` matches the needle.
    ///
    /// Returns `true` if the item was added.
    pub fn add(&mut self, haystack: impl AsRef<str>, item: T) -> bool {
        self.matches_aux(haystack.as_ref(), Some(item)).is_some()
    }

    /// Check whether `haystack` matches the needle without inserting.
    ///
    /// Returns `Some(match_indices)` if it matched.
    pub fn check_match(&mut self, haystack: &str) -> Option<Vec<usize>> {
        self.matches_aux(haystack, None)
    }

    fn sort(&mut self) {
        match &mut self.state {
            State::Unscored(matches) => {
                matches.sort_by(|a, b| {
                    let cmp_sensitive = a.haystack.cmp(&b.haystack);
                    if self.options.case_sensitive {
                        cmp_sensitive
                    } else {
                        a.haystack
                            .to_folded_case()
                            .cmp(&b.haystack.to_folded_case())
                            .then(cmp_sensitive)
                    }
                });
            }
            State::Fuzzy { matches, .. } => match self.options.sort {
                crate::options::CompletionSort::Alphabetical => {
                    matches.sort_by(|a, b| a.haystack.cmp(&b.haystack));
                }
                crate::options::CompletionSort::Smart => {
                    matches.sort_by(|a, b| {
                        b.score
                            .cmp(&a.score)
                            .then(a.haystack.cmp(&b.haystack))
                    });
                }
            },
        }
    }

    /// Sort (if configured) and return all matched items with their match
    /// indices.
    pub fn results(mut self) -> Vec<(T, Vec<usize>)> {
        if self.should_sort {
            self.sort();
        }
        match self.state {
            State::Unscored(matches) => matches
                .into_iter()
                .map(|m| (m.item, m.match_indices))
                .collect(),
            State::Fuzzy { matches, .. } => matches
                .into_iter()
                .map(|m| (m.item, m.match_indices))
                .collect(),
        }
    }
}

// — SemanticSuggestion convenience helpers --------------------------------

impl CandidateMatcher<'_, SemanticSuggestion> {
    /// Add a `SemanticSuggestion` (matched by its display value).
    pub fn add_suggestion(&mut self, sugg: SemanticSuggestion) -> bool {
        let value = sugg.suggestion.display_value().to_string();
        self.add(value, sugg)
    }

    /// Return all matched suggestions sorted, with match indices set.
    pub fn suggestion_results(self) -> Vec<SemanticSuggestion> {
        self.results()
            .into_iter()
            .map(|(mut sugg, indices)| {
                sugg.suggestion.match_indices = Some(indices);
                sugg
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests (ported from nushell `completion_options.rs`, all names stripped)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Prefix,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("examp", &opts, true);
        assert!(m.add("example text", "example text"));
        assert!(!m.add("text", "text"));
        let results: Vec<_> = m.results().iter().map(|r| r.0).collect();
        assert_eq!(vec!["example text"], results);
    }

    #[test]
    fn prefix_no_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Prefix,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("text", &opts, true);
        assert!(!m.add("example text", "example text"));
        let results: Vec<_> = m.results().iter().map(|r| r.0).collect();
        let empty: Vec<&str> = Vec::new();
        assert_eq!(empty, results);
    }

    #[test]
    fn substring_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Substring,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("text", &opts, true);
        assert!(m.add("example text", "example text"));
        let results: Vec<_> = m.results().iter().map(|r| r.0).collect();
        assert_eq!(vec!["example text"], results);
    }

    #[test]
    fn substring_no_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Substring,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("mplxt", &opts, true);
        assert!(!m.add("example text", "example text"));
    }

    #[test]
    fn fuzzy_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Fuzzy,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("ext", &opts, true);
        assert!(m.add("example text", "example text"));
        let results: Vec<_> = m.results().iter().map(|r| r.0).collect();
        assert_eq!(vec!["example text"], results);
    }

    #[test]
    fn fuzzy_no_match() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Fuzzy,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("mpp", &opts, true);
        assert!(!m.add("example text", "example text"));
    }

    #[test]
    fn fuzzy_sort_by_score() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Fuzzy,
            sort: crate::options::CompletionSort::Smart,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("fob", &opts, true);
        for item in ["foo/bar", "fob", "foo bar"] {
            m.add(item, item);
        }
        let results = m.results();
        assert_eq!(3, results.len());
        // Best score first, alphabetical tie-break for equal scores
        assert_eq!("fob", results[0].0);
        assert_eq!("foo bar", results[1].0);
        assert_eq!("foo/bar", results[2].0);
    }

    #[test]
    fn fuzzy_strip_quotes() {
        let opts = CompletionOptions {
            match_algorithm: MatchAlgorithm::Fuzzy,
            ..Default::default()
        };
        let mut m = CandidateMatcher::new("'love spaces' ", &opts, true);
        for item in [
            "'i love spaces'",
            "'i love spaces' so much",
            "'lovespaces' ",
        ] {
            m.add(item, item);
        }
        let results = m.results();
        // Only "'i love spaces' so much" should match because the query
        // includes a space after "spaces"
        assert_eq!(1, results.len());
        assert_eq!("'i love spaces' so much", results[0].0);
    }
}
