//! 补全/建议引擎：ghost 余量与弹窗候选的计算核心。
//!
//! 从 `Display` 的方法下沉为自由函数：winit 壳把 `Display` 字段借成
//! [`SuggestSources`]，GPUI 壳借进程级单例（`gpui_shell::terminal::suggest`）。
//! 数据源与排序规则两个壳共用，避免第二套平行实现（与 `ssh_session` 的
//! `SshEventHost` 泛型下沉同一手法）。

use nebula_completions::file::complete_item;
use nebula_completions::{CompletionOptions, Span};

use super::state::{CompletionStyle, NebulaCompletionItem, NebulaCompletionKind, NebulaPaneState};
use super::{
    NEBULA_GHOST_MAX, nebula_command_hint, nebula_command_hints, nebula_debug_log,
    nebula_is_command_position, nebula_path_wants_directory,
};

/// 借用的共享数据源与运行时开关。生命周期只覆盖一次重算调用。
pub(crate) struct SuggestSources<'a> {
    pub history: &'a crate::nebula_history::NebulaHistory,
    pub directories: &'a crate::directory_history::DirectoryHistory,
    pub commands: &'a std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pub enabled: bool,
    pub style: CompletionStyle,
}

/// Refresh the inline ghost remainder or the popup candidate list for the
/// current prompt line. Cached on `cwd + line`; dismissal keeps the key so a
/// closed popup stays closed until the line itself changes.
pub(crate) fn suggest_update(
    sources: &SuggestSources<'_>,
    state: &mut NebulaPaneState,
    line_override: Option<String>,
) {
    let line = line_override.unwrap_or_else(|| state.line_buf.clone());
    if !sources.enabled || line.is_empty() {
        state.clear_completion_hints();
        nebula_debug_log(format!(
            "suggest_skip enabled={} cwd={:?} line={:?} line_buf={:?}",
            sources.enabled, state.cwd, line, state.line_buf
        ));
        return;
    }

    // 命令目录由后台 PowerShell 探针填充；目录从 0 变为完整集合时，即使
    // 用户没有继续输入，也必须让上一帧的“无候选”缓存失效。
    let command_generation = sources.commands.lock().map(|commands| commands.len()).unwrap_or(0);
    let key = format!("{}\u{0}{line}\u{0}{command_generation}", state.cwd);
    if state.completion_suppressed_line.as_deref() == Some(line.as_str()) {
        state.suggestion_key = key;
        state.suggestion.clear();
        state.completion_items.clear();
        state.completion_selected = 0;
        return;
    }
    state.completion_suppressed_line = None;
    if key == state.suggestion_key {
        // Cache hit also protects an Esc-dismissed popup: dismissal clears
        // the items but keeps the key, so nothing reopens until the line
        // actually changes.
        return;
    }
    state.suggestion_key = key;
    state.suggestion.clear();
    state.completion_items.clear();
    state.completion_selected = 0;

    nebula_debug_log(format!(
        "suggest_begin cwd={:?} line={:?} line_buf={:?}",
        state.cwd, line, state.line_buf
    ));

    // Popup style computes a multi-candidate list instead of the single
    // ghost remainder; the two are mutually exclusive per pane.
    if sources.style == CompletionStyle::Popup {
        suggest_collect(sources, state, &line);
        return;
    }

    // History first: newest command that extends the whole line (indexed
    // prefix lookup — scales with matches, not history size).
    if let Some(rem) = sources.history.hint(&line) {
        state.suggestion = clamp_ghost(rem);
        nebula_debug_log(format!("suggest_result kind=history rem={:?}", state.suggestion));
        return;
    }

    // Directory-history hint for cd-like commands. This normalizes Windows
    // slash style/case, so `cd D:\te` can pick a previous
    // `cd D:/temp_build/wuwei` before generic filesystem completion falls
    // back to alphabetic candidates like `D:\Telegram\`.
    if let Some(rem) = sources.directories.hint(&line, &state.cwd) {
        state.suggestion = clamp_ghost(&rem);
        nebula_debug_log(format!("suggest_result kind=dir rem={:?}", state.suggestion));
        return;
    }

    // First token with no path separators is a command position. Reuse the
    // process PATH inherited by the shell so typing `ca` can ghost `rgo`
    // even before that command has appeared in Nebula/Nushell history.
    if nebula_is_command_position(&line) {
        if let Ok(commands) = sources.commands.lock() {
            if let Some(rem) = nebula_command_hint(commands.as_slice(), &line) {
                state.suggestion = clamp_ghost(rem);
                nebula_debug_log(format!("suggest_result kind=command rem={:?}", state.suggestion));
                return;
            }
        }
    }

    // Otherwise complete the final path token. Absolute tokens (drive,
    // root or `~`) resolve without a cwd; relative ones need the tracked
    // cwd, so bail if it is unknown.
    let token = line.rsplit([' ', '\t']).next().unwrap_or("");
    if token.is_empty() {
        return;
    }
    let absolute =
        token.starts_with(['/', '\\', '~']) || token.as_bytes().get(1) == Some(&b':'); // Windows drive, e.g. `D:`
    if !absolute && state.cwd.is_empty() {
        return;
    }

    // Case-insensitive so `mor` completes `MoRealm` on Windows; prefer
    // directories for the common directory-changing commands.
    let options = CompletionOptions { case_sensitive: false, ..CompletionOptions::default() };
    let want_dir = nebula_path_wants_directory(&line);
    let span = Span::new(0, token.len());
    let cwd = state.cwd.clone();
    let cwd_slot = [cwd.as_str()];
    let cwds: &[&str] = if cwd.is_empty() { &[] } else { &cwd_slot };
    let matches = complete_item(want_dir, span, token, cwds, &options, false, None);
    let matches = if want_dir {
        sources.directories.rank_file_suggestions(matches, &state.cwd)
    } else {
        matches
    };
    let candidates: Vec<_> = matches
        .iter()
        .take(6)
        .map(|s| s.display_override.as_deref().unwrap_or(&s.path).to_owned())
        .collect();
    let remainder = matches.into_iter().find_map(|s| {
        let path = s.display_override.as_deref().unwrap_or(&s.path);
        // The match was case-insensitive, so the suggestion is the slice of
        // `path` past what the user typed. Compare the head ignoring ASCII
        // case (so `mor` matches `MoRealm`) and guard the byte split against
        // multibyte boundaries.
        if path.len() <= token.len() || !path.is_char_boundary(token.len()) {
            return None;
        }
        let (head, rem) = path.split_at(token.len());
        if !head.eq_ignore_ascii_case(token) {
            return None;
        }
        // Stop at the first separator so a single deep match doesn't drill
        // the whole tree into the ghost; suggest one segment.
        Some(match rem.find(['/', '\\']) {
            Some(i) => rem[..=i].to_owned(),
            None => rem.to_owned(),
        })
    });
    if let Some(rem) = remainder {
        state.suggestion = clamp_ghost(&rem);
        nebula_debug_log(format!(
            "suggest_result kind=path token={:?} candidates={:?} rem={:?}",
            token, candidates, state.suggestion
        ));
    } else {
        nebula_debug_log(format!(
            "suggest_result kind=none token={:?} candidates={:?}",
            token, candidates
        ));
    }
}

/// Cap ghost length so a long path/command can't spill into the chrome.
fn clamp_ghost(rem: &str) -> String {
    rem.chars().take(NEBULA_GHOST_MAX).collect()
}

/// Elide long popup labels from the LEFT (paths keep their informative
/// tail; the head the user already typed is the expendable part).
pub(crate) fn elide_left(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count + 1 - max_chars).collect();
    format!("…{tail}")
}

/// Fill `state.completion_items` for the popup style: the same sources as
/// the ghost hint (history → directory history → PATH commands → file
/// system), but keeping several candidates each instead of the first hit.
fn suggest_collect(sources: &SuggestSources<'_>, state: &mut NebulaPaneState, line: &str) {
    // 8 是视口行数，不是数据上限。旧实现把两者混成一个常量，候选在收集
    // 阶段就被截断，因而既画不出滚动条，Tab 也永远走不到第九项以后。
    const POPUP_LIMIT: usize = 256;
    const LABEL_MAX: usize = 44;

    let mut items: Vec<NebulaCompletionItem> = Vec::new();
    let push = |items: &mut Vec<NebulaCompletionItem>, item: NebulaCompletionItem| {
        if item.insert.is_empty() {
            return;
        }
        if items.iter().any(|seen| seen.insert == item.insert && seen.kind == item.kind) {
            return;
        }
        if items.len() < POPUP_LIMIT {
            items.push(item);
        }
    };

    // Whole-line history matches, newest first.
    for (full, rem) in sources.history.hints(line, 3) {
        push(&mut items, NebulaCompletionItem {
            label: elide_left(full, LABEL_MAX),
            insert: rem.to_owned(),
            kind: NebulaCompletionKind::History,
        });
    }

    let token = line.rsplit([' ', '\t']).next().unwrap_or("");

    // Frecency-ranked directory completion for cd-like commands.
    if let Some(rem) = sources.directories.hint(line, &state.cwd) {
        push(&mut items, NebulaCompletionItem {
            label: elide_left(&format!("{token}{rem}"), LABEL_MAX),
            insert: rem,
            kind: NebulaCompletionKind::Dir,
        });
    }

    // PATH executables while the first token is being typed.
    if nebula_is_command_position(line) {
        if let Ok(commands) = sources.commands.lock() {
            for command in nebula_command_hints(commands.as_slice(), line, POPUP_LIMIT) {
                // 精确命令也必须成为可接受项；补一个空格既给用户明确反馈，
                // 又让 Enter 只完成选择而不立刻执行命令。
                let insert = if command.len() == line.len() {
                    " ".to_owned()
                } else {
                    command[line.len()..].to_owned()
                };
                push(&mut items, NebulaCompletionItem {
                    label: elide_left(command, LABEL_MAX),
                    insert,
                    kind: NebulaCompletionKind::Command,
                });
            }
        }
    }

    // Filesystem candidates for the final token (same gating as the ghost
    // path: absolute tokens work without a cwd, relative ones need one).
    if !token.is_empty() {
        let absolute =
            token.starts_with(['/', '\\', '~']) || token.as_bytes().get(1) == Some(&b':');
        if absolute || !state.cwd.is_empty() {
            let options =
                CompletionOptions { case_sensitive: false, ..CompletionOptions::default() };
            let want_dir = nebula_path_wants_directory(line);
            let span = Span::new(0, token.len());
            let cwd = state.cwd.clone();
            let cwd_slot = [cwd.as_str()];
            let cwds: &[&str] = if cwd.is_empty() { &[] } else { &cwd_slot };
            let matches = complete_item(want_dir, span, token, cwds, &options, false, None);
            let matches = if want_dir {
                sources.directories.rank_file_suggestions(matches, &state.cwd)
            } else {
                matches
            };
            for candidate in matches.iter().take(POPUP_LIMIT) {
                let path = candidate.display_override.as_deref().unwrap_or(&candidate.path);
                if path.len() <= token.len() || !path.is_char_boundary(token.len()) {
                    continue;
                }
                let (head, rem) = path.split_at(token.len());
                if !head.eq_ignore_ascii_case(token) {
                    continue;
                }
                // One segment at a time, like the ghost: a deep match
                // drills the tree one directory per acceptance.
                let (insert, cut_at_dir) = match rem.find(['/', '\\']) {
                    Some(i) => (&rem[..=i], true),
                    None => (rem, false),
                };
                let kind = if cut_at_dir || candidate.is_dir {
                    NebulaCompletionKind::Dir
                } else {
                    NebulaCompletionKind::File
                };
                push(&mut items, NebulaCompletionItem {
                    label: elide_left(&format!("{token}{insert}"), LABEL_MAX),
                    insert: insert.to_owned(),
                    kind,
                });
            }
        }
    }

    nebula_debug_log(format!("suggest_result kind=popup line={:?} items={}", line, items.len()));
    state.completion_items = items;
    state.completion_selected = 0;
}
