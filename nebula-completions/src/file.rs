use std::path::{Component, Path, PathBuf, MAIN_SEPARATOR as SEP, is_separator};
use std::fmt::Write;

use unicode_segmentation::UnicodeSegmentation;

use crate::options::{CompletionOptions, MatchAlgorithm};
use crate::matcher::{CandidateMatcher, IgnoreCaseExt};
use crate::span::Span;

// ---------------------------------------------------------------------------
// FileSuggestion
// ---------------------------------------------------------------------------

/// A single file/directory completion candidate produced by [`complete_item`].
pub struct FileSuggestion {
    pub span: Span,
    pub path: String,
    #[cfg(feature = "color")]
    pub style: Option<nu_ansi_term::Style>,
    pub is_dir: bool,
    pub display_override: Option<String>,
    pub match_indices: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Helper types for recursive path completion
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct PathBuiltFromString {
    cwd: PathBuf,
    parts: Vec<MatchedPart>,
    isdir: bool,
}

#[derive(Clone, Default)]
struct MatchedPart {
    text: String,
    match_indices: Vec<usize>,
}

#[derive(Debug)]
enum OriginalCwd {
    None,
    Home,
    Prefix(String),
}

/// Expand n-dots (`...` → `../..`, `....` → `../../..`, etc.).
fn expand_ndots(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c == '.' {
            let mut dot_count = 0;
            while chars.peek() == Some(&'.') {
                chars.next();
                dot_count += 1;
            }
            if dot_count > 2 {
                // n dots → n-1 repetitions of ".."
                for i in 0..dot_count - 1 {
                    if i > 0 {
                        result.push(SEP);
                    }
                    result.push_str("..");
                }
            } else {
                for _ in 0..dot_count {
                    result.push('.');
                }
            }
        } else {
            result.push(chars.next().unwrap());
        }
    }

    PathBuf::from(result)
}

/// Collapse consecutive `..` segments back into n-dots.
fn collapse_ndots(path: PathBuiltFromString) -> PathBuiltFromString {
    let mut result = PathBuiltFromString {
        parts: Vec::with_capacity(path.parts.len()),
        isdir: path.isdir,
        cwd: path.cwd,
    };
    let mut dot_count = 0;

    for part in path.parts {
        if part.text == ".." {
            dot_count += 1;
        } else {
            if dot_count > 0 {
                result.parts.push(MatchedPart {
                    text: ".".repeat(dot_count + 1),
                    match_indices: Vec::new(),
                });
                dot_count = 0;
            }
            result.parts.push(part);
        }
    }
    if dot_count > 0 {
        result.parts.push(MatchedPart {
            text: ".".repeat(dot_count + 1),
            match_indices: Vec::new(),
        });
    }
    result
}

/// Recursively walk directories and collect matching paths.
fn complete_rec(
    partial: &[&str],
    built_paths: &[PathBuiltFromString],
    options: &CompletionOptions,
    want_directory: bool,
    isdir: bool,
    enable_exact_match: bool,
) -> Vec<PathBuiltFromString> {
    let has_more = !partial.is_empty() && (partial.len() > 1 || isdir);

    if let Some((&base, rest)) = partial.split_first()
        && base.chars().all(|c| c == '.')
        && has_more
    {
        let built_paths: Vec<_> = built_paths
            .iter()
            .map(|built| {
                let mut built = built.clone();
                built.parts.push(MatchedPart {
                    text: base.to_string(),
                    match_indices: Vec::new(),
                });
                built.isdir = true;
                built
            })
            .collect();
        return complete_rec(
            rest,
            &built_paths,
            options,
            want_directory,
            isdir,
            enable_exact_match,
        );
    }

    let prefix = partial.first().unwrap_or(&"");
    let mut matcher = CandidateMatcher::new(prefix, options, true);

    let mut exact_match = None;
    let mut multiple_exact_matches = false;

    for built in built_paths {
        let mut path = built.cwd.clone();
        for part in &built.parts {
            path.push(part.text.as_str());
        }

        let Ok(result) = path.read_dir() else {
            continue;
        };

        for entry in result.filter_map(|e| e.ok()) {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let entry_isdir = entry.path().is_dir();
            let mut built = built.clone();
            built.isdir = entry_isdir;

            if !want_directory || entry_isdir {
                if enable_exact_match && !multiple_exact_matches && has_more {
                    let matches = if options.case_sensitive {
                        entry_name.eq(prefix)
                    } else {
                        entry_name.eq_ignore_case(prefix)
                    };
                    if matches {
                        if exact_match.is_none() {
                            let mut built_exact = built.clone();
                            let match_indices: Vec<usize> =
                                (0..entry_name.graphemes(true).count()).collect();
                            built_exact.parts.push(MatchedPart {
                                text: entry_name.clone(),
                                match_indices,
                            });
                            exact_match = Some(built_exact);
                        } else {
                            multiple_exact_matches = true;
                        }
                    }
                }

                matcher.add(entry_name.clone(), (built, entry_name));
            }
        }
    }

    // Single exact match → drill into it directly (hides sibling entries)
    if !multiple_exact_matches && let Some(built) = exact_match {
        return complete_rec(
            &partial[1..],
            &[built],
            options,
            want_directory,
            isdir,
            true,
        );
    }

    let completion_iter = matcher.results().into_iter().map(
        |((mut built, last_entry_name), last_match_indices)| {
            built.parts.push(MatchedPart {
                text: last_entry_name,
                match_indices: last_match_indices,
            });
            built
        },
    );

    if has_more {
        completion_iter
            .flat_map(|completion| {
                complete_rec(
                    &partial[1..],
                    &[completion],
                    options,
                    want_directory,
                    isdir,
                    false,
                )
            })
            .collect()
    } else {
        completion_iter.collect()
    }
}

/// Escape special characters in a path for safe insertion.
///
/// Returns `Some(escaped)` if escaping was needed, `None` if the path
/// is safe as-is.
pub fn escape_path(path: &str) -> Option<String> {
    // Check for glob-like characters or backticks
    let has_glob = path.contains(|c: char| matches!(c, '*' | '?' | '[' | ']'));
    if has_glob || path.contains('`') {
        // Simple tilde expansion
        let expanded = if path.starts_with('~') {
            dirs_next_home().map_or_else(
                || path.to_string(),
                |home| {
                    let rest = &path[1..];
                    if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
                        format!("{}{}", home.display(), rest)
                    } else {
                        // ~user form — just leave it
                        path.to_string()
                    }
                },
            )
        } else {
            path.to_string()
        };

        if expanded.contains('\'') {
            Some(format!("{expanded:?}")) // debug escapes quotes
        } else {
            Some(format!("'{expanded}'"))
        }
    } else {
        let contaminated =
            path.contains(['\'', '"', ' ', '#', '(', ')', '{', '}', '[', ']', '|', ';']);
        let maybe_flag = path.starts_with('-');
        let maybe_variable = path.starts_with('$');
        let maybe_number = path.parse::<f64>().is_ok();
        if contaminated || maybe_flag || maybe_variable || maybe_number {
            Some(format!("`{path}`"))
        } else {
            None
        }
    }
}

/// Get the home directory (small helper to avoid pulling in `dirs` crate
/// if we can use `std`).
fn dirs_next_home() -> Option<PathBuf> {
    // Try standard env vars first, then fall back to home_dir (deprecated but works)
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOMEDRIVE").and_then(|drive| {
            std::env::var_os("HOMEPATH").map(|path| {
                let mut p = PathBuf::from(drive);
                p.push(path);
                p
            })
        }))
}

/// Remove surrounding quotes from a partial path.
pub fn surround_remove(partial: &str) -> String {
    for c in ['`', '"', '\''] {
        if partial.starts_with(c) {
            let ret = partial.strip_prefix(c).unwrap_or(partial);
            return match ret.split(c).collect::<Vec<_>>()[..] {
                [inside] => inside.to_string(),
                [inside, outside] if inside.ends_with(is_separator) => {
                    format!("{inside}{outside}")
                }
                _ => ret.to_string(),
            };
        }
    }
    partial.to_string()
}

/// Complete files / directories recursively.
///
/// * `want_directory` — only return directories if `true`.
/// * `span` — span of the partial in the input.
/// * `partial` — the partial path the user typed.
/// * `cwds` — one or more directories to search from.
/// * `options` — matching configuration.
/// * `use_ls_colors` — whether to compute ANSI styles from `LS_COLORS`.
/// * `ls_colors_env` — optional `LS_COLORS` environment variable value.
#[allow(unused_variables)]
pub fn complete_item(
    want_directory: bool,
    span: Span,
    partial: &str,
    cwds: &[impl AsRef<str>],
    options: &CompletionOptions,
    use_ls_colors: bool,
    ls_colors_env: Option<&str>,
) -> Vec<FileSuggestion> {
    let cleaned_partial = surround_remove(partial);
    let isdir = cleaned_partial.ends_with(is_separator);
    let expanded_partial = expand_ndots(Path::new(&cleaned_partial));
    let should_collapse_dots = expanded_partial != Path::new(&cleaned_partial);
    let mut partial = expanded_partial.to_string_lossy().to_string();

    #[cfg(unix)]
    let path_separator = SEP;
    #[cfg(windows)]
    let path_separator = cleaned_partial
        .chars()
        .rfind(|c: &char| is_separator(*c))
        .unwrap_or(SEP);

    // Handle trailing dot case
    if cleaned_partial.ends_with(&format!("{path_separator}.")) {
        write!(partial, "{path_separator}.").expect("write to String is infallible");
    }

    let cwd_pathbufs: Vec<_> = cwds
        .iter()
        .map(|cwd| Path::new(cwd.as_ref()).to_path_buf())
        .collect();

    #[cfg(feature = "color")]
    let ls_colors = if use_ls_colors {
        crate::color::get_ls_colors(ls_colors_env)
    } else {
        None
    };

let mut cwds = cwd_pathbufs.clone();
    let mut prefix_len = 0;
    let mut original_cwd = OriginalCwd::None;

    let mut components = Path::new(&partial).components().peekable();
    match components.peek().cloned() {
        Some(c @ Component::Prefix(..)) => {
            // Windows prefix (e.g., `C:`)
            cwds = vec![[c, Component::RootDir].iter().collect()];
            prefix_len = c.as_os_str().len();
            original_cwd = OriginalCwd::Prefix(c.as_os_str().to_string_lossy().into_owned());
        }
        Some(c @ Component::RootDir) => {
            cwds = vec![PathBuf::from(c.as_os_str())];
            prefix_len = 1;
            original_cwd = OriginalCwd::Prefix(String::new());
        }
        Some(Component::Normal(home)) if home.to_string_lossy() == "~" => {
            cwds = dirs_next_home()
                .map(|dir| vec![dir])
                .unwrap_or(cwd_pathbufs);
            prefix_len = 1;
            original_cwd = OriginalCwd::Home;
        }
        _ => {}
    };

    let after_prefix = &partial[prefix_len..];
    let partial: Vec<_> = after_prefix
        .strip_prefix(is_separator)
        .unwrap_or(after_prefix)
        .split(is_separator)
        .filter(|s| !s.is_empty())
        .collect();

    complete_rec(
        partial.as_slice(),
        &cwds
            .into_iter()
            .map(|cwd| PathBuiltFromString {
                cwd,
                parts: Vec::new(),
                isdir: false,
            })
            .collect::<Vec<_>>(),
        options,
        want_directory,
        isdir,
        options.match_algorithm == MatchAlgorithm::Prefix,
    )
    .into_iter()
    .map(|mut p| {
        if should_collapse_dots {
            p = collapse_ndots(p);
        }
        let is_dir = p.isdir;

        let mut path = match &original_cwd {
            OriginalCwd::None => String::new(),
            OriginalCwd::Home => format!("~{path_separator}"),
            OriginalCwd::Prefix(s) => format!("{s}{path_separator}"),
        };
        let mut match_index_offset = path.graphemes(true).count();
        let mut match_indices = Vec::new();
        for (i, part) in p.parts.iter().enumerate() {
            path.push_str(&part.text);
            for ind in &part.match_indices {
                match_indices.push(ind + match_index_offset);
            }
            match_index_offset += part.text.graphemes(true).count();
            if i != p.parts.len() - 1 {
                path.push(path_separator);
                match_index_offset += path_separator.len_utf8();
            }
        }
        if p.isdir {
            path.push(path_separator);
        }

        #[cfg(feature = "color")]
        let style = ls_colors.as_ref().and_then(|lsc| {
            let real_path = std::path::absolute(&path).ok().unwrap_or_else(|| PathBuf::from(&path));
            lsc.style_for_path_with_metadata(&real_path, None)
                .map(|s| s.to_nu_ansi_term_style())
        });

        let (value, display_override) = if let Some(escaped) = escape_path(&path) {
            (escaped, Some(path))
        } else {
            (path, None)
        };
        FileSuggestion {
            span,
            path: value,
            #[cfg(feature = "color")]
            style,
            is_dir,
            display_override,
            match_indices,
        }
    })
    .collect()
}
