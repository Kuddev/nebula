//! Command palette (`Ctrl+Shift+P`): a fuzzy-searchable launcher for every
//! Nebula action — the discoverable entry point for features whose shortcuts
//! are hard to remember.
//!
//! This module owns only the *model*: the action list, the query/selection
//! state, fuzzy filtering, and the popup layout maths. Rendering lives in
//! `display::mod` (it mirrors the settings modal), and execution is dispatched
//! by the input layer, which is the only place that can reach both the display
//! and the window context. Keeping the model here makes it self-contained and
//! keeps the giant `mod.rs` free of the item table.

use super::NebulaTheme;

/// A single executable action reachable from the command palette.
///
/// Deliberately flat and `Copy` so the input layer can match on it after the
/// palette closes, without holding any borrow. Each variant maps onto either a
/// `TabRequest` (tab / split / window operations) or a `Display` method
/// (theme / settings / appearance) — see `keyboard.rs::run_palette_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    NewWindow,
    SplitRight,
    SplitDown,
    OpenSettings,
    OpenSettingsFile,
    ToggleGhost,
    CycleAccept,
    PickBackgroundImage,
    CycleBackground,
    ResetAppearance,
    SelectTheme(NebulaTheme),
}

/// One palette row.
///
/// * `label`  — localized text shown on the left.
/// * `hint`   — optional shortcut / aux text, dimmed and right-aligned (ASCII,
///   so its on-screen width equals its `char` count).
/// * `search` — the haystack matched against the query. Includes the label plus
///   latin aliases (pinyin / English) so the palette is reachable even when the
///   IME can't feed CJK into it.
/// * `action` — what to run on confirm.
struct PaletteItem {
    label: &'static str,
    hint: &'static str,
    search: &'static str,
    action: PaletteAction,
}

/// The full action table, in declaration order (also the tie-break order when
/// fuzzy scores are equal, and the order shown for an empty query).
const ITEMS: &[PaletteItem] = &[
    PaletteItem {
        label: "新建标签页",
        hint: "Ctrl+Shift+T",
        search: "新建标签页 new tab xinjian biaoqianye",
        action: PaletteAction::NewTab,
    },
    PaletteItem {
        label: "关闭标签页",
        hint: "Ctrl+Shift+W",
        search: "关闭标签页 close tab guanbi",
        action: PaletteAction::CloseTab,
    },
    PaletteItem {
        label: "下一个标签页",
        hint: "Ctrl+Tab",
        search: "下一个标签页 next tab xiayige",
        action: PaletteAction::NextTab,
    },
    PaletteItem {
        label: "上一个标签页",
        hint: "Ctrl+Shift+Tab",
        search: "上一个标签页 previous prev tab shangyige",
        action: PaletteAction::PrevTab,
    },
    PaletteItem {
        label: "新建窗口",
        hint: "Ctrl+Shift+E",
        search: "新建窗口 new window xinjian chuangkou",
        action: PaletteAction::NewWindow,
    },
    PaletteItem {
        label: "左右分屏",
        hint: "Ctrl+Shift+D",
        search: "左右分屏 split right vertical zuoyou fenping",
        action: PaletteAction::SplitRight,
    },
    PaletteItem {
        label: "上下分屏",
        hint: "Ctrl+Shift+S",
        search: "上下分屏 split down horizontal shangxia fenping",
        action: PaletteAction::SplitDown,
    },
    PaletteItem {
        label: "打开设置",
        hint: "",
        search: "打开设置 open settings preferences dakai shezhi",
        action: PaletteAction::OpenSettings,
    },
    PaletteItem {
        label: "打开配置文件",
        hint: "",
        search: "打开配置文件 open config file dakai peizhi wenjian",
        action: PaletteAction::OpenSettingsFile,
    },
    PaletteItem {
        label: "切换行内补全 (Ghost)",
        hint: "",
        search: "切换行内补全 toggle ghost completion qiehuan buquan",
        action: PaletteAction::ToggleGhost,
    },
    PaletteItem {
        label: "切换补全接受键",
        hint: "",
        search: "切换补全接受键 cycle accept key completion jieshou",
        action: PaletteAction::CycleAccept,
    },
    PaletteItem {
        label: "选择背景图片…",
        hint: "",
        search: "选择背景图片 background image picture xuanze beijing tupian",
        action: PaletteAction::PickBackgroundImage,
    },
    PaletteItem {
        label: "切换背景色",
        hint: "",
        search: "切换背景色 cycle background color qiehuan beijingse",
        action: PaletteAction::CycleBackground,
    },
    PaletteItem {
        label: "恢复外观默认",
        hint: "",
        search: "恢复外观默认 reset appearance default huifu waiguan moren",
        action: PaletteAction::ResetAppearance,
    },
    PaletteItem {
        label: "主题：Nebula",
        hint: "",
        search: "主题 nebula theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::Nebula),
    },
    PaletteItem {
        label: "主题：Silver Light",
        hint: "",
        search: "主题 silver light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::SilverLight),
    },
    PaletteItem {
        label: "主题：Steel Dark",
        hint: "",
        search: "主题 steel dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::SteelDark),
    },
    PaletteItem {
        label: "主题：Limestone",
        hint: "",
        search: "主题 limestone light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::LimestoneLight),
    },
    PaletteItem {
        label: "主题：Coal Dark",
        hint: "",
        search: "主题 coal dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::CoalDark),
    },
    PaletteItem {
        label: "主题：Linen Light",
        hint: "",
        search: "主题 linen light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::LinenLight),
    },
    PaletteItem {
        label: "主题：Moss Dark",
        hint: "",
        search: "主题 moss dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::MossDark),
    },
];

/// How many recently-run actions are remembered for the empty-query ordering.
const RECENT_MAX: usize = 6;

/// Command palette UI + filtering state, embedded in `Display`.
pub struct CommandPalette {
    open: bool,
    query: String,
    /// Indices into `ITEMS`, best match first. Rebuilt on every query change.
    filtered: Vec<usize>,
    /// Selected row *within `filtered`*.
    selected: usize,
    /// Recently-run `ITEMS` indices, most-recent first (deduped, capped at
    /// `RECENT_MAX`). Lifts frequent actions to the top of an empty query.
    recent: Vec<usize>,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut palette = Self {
            open: false,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            recent: Vec::new(),
        };
        palette.refilter();
        palette
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open (or re-open) the palette with a cleared query and the full list.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// Append a typed character (control chars ignored) and re-filter.
    pub fn input_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.query.push(c);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }

    /// Move the selection by `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
    }

    /// Confirm the current selection: records it as recent, closes the palette,
    /// and returns the action to run, or `None` when nothing matches.
    pub fn confirm(&mut self) -> Option<PaletteAction> {
        let idx = *self.filtered.get(self.selected)?;
        self.record_recent(idx);
        self.close();
        Some(ITEMS[idx].action)
    }

    /// Remember `idx` as the most-recently run command (deduped, capped), so a
    /// freshly-opened (empty-query) palette lists frequent actions first.
    fn record_recent(&mut self, idx: usize) {
        self.recent.retain(|&i| i != idx);
        self.recent.insert(0, idx);
        self.recent.truncate(RECENT_MAX);
    }

    /// Re-score every item against the query and rebuild `filtered`. With a
    /// query: fuzzy score, best first, ties in declaration order. Empty query:
    /// recently-run first, then declaration order (a stable sort keeps the
    /// declared order for the un-recent tail). Resets the selection to the top.
    fn refilter(&mut self) {
        let query = self.query.trim();
        if query.is_empty() {
            let mut order: Vec<usize> = (0..ITEMS.len()).collect();
            order.sort_by_key(|&i| self.recent.iter().position(|&r| r == i).unwrap_or(usize::MAX));
            self.filtered = order;
        } else {
            let mut scored: Vec<(i32, usize)> = ITEMS
                .iter()
                .enumerate()
                .filter_map(|(i, item)| fuzzy_score(query, item.search).map(|score| (score, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = 0;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// The at-most `max_rows` visible rows, scrolled so the selection stays in
    /// view, plus the selected row's index *within that window* (`None` when the
    /// list is empty). Collected so the result borrows nothing (all `&'static`).
    pub fn visible(&self, max_rows: usize) -> (Vec<(&'static str, &'static str)>, Option<usize>) {
        if self.filtered.is_empty() || max_rows == 0 {
            return (Vec::new(), None);
        }
        // Keep the selection visible: once it passes the last row, scroll so it
        // sits on the bottom line of the window.
        let start = self.selected.saturating_sub(max_rows - 1);
        let rows = self
            .filtered
            .iter()
            .skip(start)
            .take(max_rows)
            .map(|&idx| (ITEMS[idx].label, ITEMS[idx].hint))
            .collect();
        (rows, Some(self.selected - start))
    }
}

/// Subsequence fuzzy score, or `None` if the needle isn't a subsequence of the
/// haystack. Consecutive runs and word-start matches are rewarded so intuitive
/// queries rank first (e.g. "nt" prefers "new tab" over "next"). An empty
/// needle matches everything with score 0, preserving declaration order.
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let mut next = 0usize;
    let mut score = 0i32;
    let mut run = 0i32;
    let mut prev = ' ';
    for hc in haystack.chars().flat_map(char::to_lowercase) {
        if next < needle.len() && hc == needle[next] {
            score += 1 + run * 5; // consecutive-match run bonus (dominant)
            if !prev.is_alphanumeric() {
                score += 4; // word / segment start
            }
            run += 1;
            next += 1;
        } else {
            run = 0;
        }
        prev = hc;
    }
    (next == needle.len()).then_some(score)
}

/// Popup layout rectangles, all in physical pixels for the given `scale`.
pub struct PaletteLayout {
    /// Outer panel `(x, y, w, h)`.
    pub panel: (f32, f32, f32, f32),
    /// Query input box `(x, y, w, h)`.
    pub input: (f32, f32, f32, f32),
    /// Height of one result row.
    pub row_h: f32,
    /// Top Y of the first result row.
    pub list_y: f32,
    /// Maximum rows drawn before the list scrolls.
    pub max_rows: usize,
}

/// Compute the centered popup layout for a window of `win_w` × `win_h`. The
/// panel height is fixed (sized for `max_rows`) so it doesn't jump as the match
/// count changes while typing.
pub fn palette_layout(win_w: f32, win_h: f32, scale: f32) -> PaletteLayout {
    let s = |v: f32| v * scale;
    let margin = s(8.0);
    let pad = s(12.0);
    let input_h = s(50.0);
    let row_h = s(38.0);
    let max_rows = 8usize;

    let pw = s(640.0).min(win_w - 2.0 * margin);
    let ph = pad + input_h + s(8.0) + max_rows as f32 * row_h + pad;
    let px = ((win_w - pw) * 0.5).max(margin);
    let py = ((win_h - ph) * 0.5).max(s(48.0));

    let input = (px + pad, py + pad, pw - 2.0 * pad, input_h);
    let list_y = py + pad + input_h + s(8.0);

    PaletteLayout { panel: (px, py, pw, ph), input, row_h, list_y, max_rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_all_in_declaration_order() {
        let palette = CommandPalette::new();
        assert_eq!(palette.filtered.len(), ITEMS.len());
        assert_eq!(palette.filtered[0], 0, "first declared item leads by default");
    }

    #[test]
    fn recent_actions_surface_first_on_empty_query() {
        let mut palette = CommandPalette::new();
        palette.record_recent(3);
        palette.record_recent(7);
        palette.refilter();
        // Most-recent first, then the previous recent, then the declared rest.
        assert_eq!(palette.filtered[0], 7);
        assert_eq!(palette.filtered[1], 3);
        assert_eq!(palette.filtered[2], 0);
    }

    #[test]
    fn record_recent_dedups_and_caps() {
        let mut palette = CommandPalette::new();
        for i in 0..(RECENT_MAX + 3) {
            palette.record_recent(i);
        }
        assert_eq!(palette.recent.len(), RECENT_MAX);
        // Re-running an existing action moves it to the front without growing.
        palette.record_recent(2);
        assert_eq!(palette.recent.first(), Some(&2));
        assert_eq!(palette.recent.len(), RECENT_MAX);
    }

    #[test]
    fn fuzzy_matches_subsequence_and_rejects_the_rest() {
        assert!(fuzzy_score("nt", "new tab").is_some());
        assert!(fuzzy_score("newtab", "new tab").is_some());
        assert!(fuzzy_score("xyz", "new tab").is_none());
        assert!(fuzzy_score("", "anything").is_some(), "empty query matches everything");
    }

    #[test]
    fn fuzzy_rewards_consecutive_and_word_start() {
        // A consecutive run beats the same letters scattered across separators.
        let consecutive = fuzzy_score("tab", "xtab").unwrap();
        let scattered = fuzzy_score("tab", "t-a-b").unwrap();
        assert!(consecutive > scattered, "consecutive {consecutive} vs scattered {scattered}");
        // A word-start match beats a mid-word match of the same length.
        let word_start = fuzzy_score("t", "x t").unwrap();
        let mid_word = fuzzy_score("t", "xt").unwrap();
        assert!(word_start > mid_word, "word-start {word_start} vs mid-word {mid_word}");
    }

    #[test]
    fn confirm_records_recent_and_closes() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.selected = 2;
        let picked = palette.filtered[2];
        let action = palette.confirm();
        assert!(action.is_some());
        assert!(!palette.is_open());
        assert_eq!(palette.recent.first(), Some(&picked));
    }

    #[test]
    fn typing_filters_then_backspace_restores() {
        let mut palette = CommandPalette::new();
        palette.open();
        let full = palette.filtered.len();
        for ch in "zqxjk".chars() {
            palette.input_char(ch);
        }
        assert!(palette.filtered.len() < full, "gibberish should filter most out");
        for _ in 0.."zqxjk".len() {
            palette.backspace();
        }
        assert_eq!(palette.filtered.len(), full, "clearing the query restores the full list");
    }

    #[test]
    fn move_selection_wraps_both_ends() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.selected, 0);
        palette.move_selection(-1);
        assert_eq!(palette.selected, palette.filtered.len() - 1, "up from top wraps to bottom");
        palette.move_selection(1);
        assert_eq!(palette.selected, 0, "down from bottom wraps to top");
    }

    #[test]
    fn visible_window_scrolls_to_keep_selection_in_view() {
        let mut palette = CommandPalette::new();
        palette.open();
        let max = 5;
        // Selection at the top: window starts at 0, selection on row 0.
        let (rows, sel) = palette.visible(max);
        assert_eq!(rows.len(), max);
        assert_eq!(sel, Some(0));
        // Move past the window; the selection pins to the bottom visible row.
        for _ in 0..7 {
            palette.move_selection(1);
        }
        assert_eq!(palette.selected, 7);
        let (rows, sel) = palette.visible(max);
        assert_eq!(rows.len(), max);
        assert_eq!(sel, Some(max - 1), "selection pinned to bottom row when scrolled");
        // The bottom visible row is the actually-selected item.
        assert_eq!(rows[max - 1].0, ITEMS[palette.filtered[7]].label);
    }
}
