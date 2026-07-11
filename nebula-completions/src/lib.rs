//! `nebula-completions` — A lightweight, standalone completion engine.
//!
//! Extracted from Nushell's `nu-cli` completions framework, this crate
//! provides:
//!
//! * A [`CandidateMatcher`](matcher::CandidateMatcher) engine supporting
//!   prefix, substring, and fuzzy matching via `nucleo-matcher`.
//! * A [`Completer`](completer::Completer) trait for pluggable completion
//!   sources.
//! * Built-in completers for files, directories, and static string lists.
//! * `Span`, `Suggestion`, `SemanticSuggestion`, and `SuggestionKind` types.

pub mod completer;
pub mod file;
pub mod matcher;
pub mod options;
pub mod span;
pub mod suggestion;

mod directory;
mod static_completion;

#[cfg(feature = "color")]
mod color;

// -- Re-exports -----------------------------------------------------------

pub use completer::Completer;
pub use directory::DirectoryCompletion;
pub use matcher::{CandidateMatcher, IgnoreCaseExt};
pub use options::{CompletionOptions, CompletionSort, InvalidMatchAlgorithm, MatchAlgorithm};
pub use span::Span;
pub use static_completion::StaticCompletion;
pub use suggestion::{SemanticSuggestion, Suggestion, SuggestionKind};
