use crate::span::Span;

/// A completion suggestion, the core value-type for completions.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The text value to insert upon completion.
    pub value: String,
    /// Optional override for display (shown instead of `value`).
    pub display_override: Option<String>,
    /// Optional description / documentation.
    pub description: Option<String>,
    /// Arbitrary extra data.
    pub extra: Option<String>,
    /// Whether to append whitespace after the value.
    pub append_whitespace: bool,
    /// Indices in the displayed text that matched the query.
    pub match_indices: Option<Vec<usize>>,
    /// Styling for the suggestion.
    #[cfg(feature = "color")]
    pub style: Option<nu_ansi_term::Style>,
    /// The span in the input that this suggestion replaces.
    pub span: Span,
}

impl Default for Suggestion {
    fn default() -> Self {
        Self {
            value: String::new(),
            display_override: None,
            description: None,
            extra: None,
            append_whitespace: true,
            match_indices: None,
            #[cfg(feature = "color")]
            style: None,
            span: Span::new(0, 0),
        }
    }
}

impl Suggestion {
    /// The display value — either the override or the raw value.
    pub fn display_value(&self) -> &str {
        self.display_override.as_deref().unwrap_or(&self.value)
    }
}

/// Categorizes the kind of a completion.
#[derive(Debug, Clone, PartialEq)]
pub enum SuggestionKind {
    Command(String),
    Value(String),
    CellPath,
    Directory,
    File,
    Flag,
    Module,
    Operator,
    Variable,
}

/// A suggestion paired with its optional kind metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSuggestion {
    pub suggestion: Suggestion,
    pub kind: Option<SuggestionKind>,
}

impl SemanticSuggestion {
    pub fn new(suggestion: Suggestion) -> Self {
        Self {
            suggestion,
            kind: None,
        }
    }

    pub fn with_kind(suggestion: Suggestion, kind: SuggestionKind) -> Self {
        Self {
            suggestion,
            kind: Some(kind),
        }
    }
}

impl From<Suggestion> for SemanticSuggestion {
    fn from(suggestion: Suggestion) -> Self {
        Self {
            suggestion,
            ..Default::default()
        }
    }
}

impl Default for SemanticSuggestion {
    fn default() -> Self {
        Self {
            suggestion: Suggestion::default(),
            kind: None,
        }
    }
}
