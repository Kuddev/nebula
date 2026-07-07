use crate::options::CompletionOptions;
use crate::span::Span;
use crate::suggestion::SemanticSuggestion;

/// Trait for types that can produce completion suggestions.
pub trait Completer {
    /// Fetch, filter, and sort completions for the given `prefix`.
    ///
    /// * `cwd` — Current working directory (used by file-based completers).
    /// * `prefix` — The partial text the user has typed.
    /// * `span` — The span in the original input that `prefix` covers.
    /// * `offset` — Offset of the span relative to the start of the line
    ///   (for adjusting span values in suggestions).
    /// * `options` — Matching / sorting configuration.
    fn fetch(
        &mut self,
        cwd: &str,
        prefix: impl AsRef<str>,
        span: Span,
        offset: usize,
        options: &CompletionOptions,
    ) -> Vec<SemanticSuggestion>;
}
