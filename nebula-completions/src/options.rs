use std::fmt::Display;

/// Algorithm used to match completions against the input prefix.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MatchAlgorithm {
    /// Only show suggestions beginning with the input.
    ///
    /// Example: `"git switch"` is matched by `"git sw"`
    Prefix,

    /// Only show suggestions containing the input as a substring.
    ///
    /// Example: `"git checkout"` is matched by `"checkout"`
    Substring,

    /// Fuzzy matching — characters can appear anywhere, in order.
    ///
    /// Example: `"git checkout"` is matched by `"gco"`
    Fuzzy,
}

/// Sorting strategy for completion results.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum CompletionSort {
    /// Sort alphabetically.
    #[default]
    Alphabetical,
    /// Smart sort: by relevance score first, then alphabetically.
    Smart,
}

/// Configuration for how completions are matched and sorted.
#[derive(Clone)]
pub struct CompletionOptions {
    pub case_sensitive: bool,
    pub match_algorithm: MatchAlgorithm,
    pub sort: CompletionSort,
    /// Whether to also match against the suggestion description.
    pub match_description: bool,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            match_algorithm: MatchAlgorithm::Prefix,
            sort: CompletionSort::default(),
            match_description: false,
        }
    }
}

#[derive(Debug)]
pub enum InvalidMatchAlgorithm {
    Unknown,
}

impl Display for InvalidMatchAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown match algorithm")
    }
}

impl std::error::Error for InvalidMatchAlgorithm {}

impl TryFrom<String> for MatchAlgorithm {
    type Error = InvalidMatchAlgorithm;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "prefix" => Ok(Self::Prefix),
            "substring" => Ok(Self::Substring),
            "fuzzy" => Ok(Self::Fuzzy),
            _ => Err(InvalidMatchAlgorithm::Unknown),
        }
    }
}
