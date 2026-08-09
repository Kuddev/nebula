//! 键位自定义模型（spec 002）。
//!
//! `nebula_settings.txt` 中每行 `keybind=<combo>:<action>` 构成用户自定义
//! 表；`input/keyboard.rs::process_key_bindings` 先查这张表再查 config 的
//! 默认/TOML 表，命中即短路——改键遮蔽默认、`none` 禁用键都由这一条
//! 优先级规则实现。设置页「按键映射」的行数据与捕获逻辑也在这里。

use std::sync::OnceLock;

use winit::event::KeyEvent;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::default_key_bindings;
use crate::config::{Action, BindingKey, BindingMode, KeyBinding, KeyLocation};

/// 设置页可编辑的动作（顺序即显示顺序）。数字系动作（SelectTab1..9、
/// LaunchProfile1..9）与 AI 贴入键属只读展示，不进这张表。
///
/// `QUICK_TERMINAL_ROW` 是设置页专用的全局快捷键行，不属于终端
/// `config::Action`；它放在动作行之前，保证用户在按键页打开时第一眼就能
/// 找到快速终端入口，同时仍复用同一套捕获/键帽交互。
pub(super) const QUICK_TERMINAL_ROW: usize = 0;
pub(super) const DEFAULT_QUICK_TERMINAL_HOTKEY: &str = "ctrl+`";

pub(super) fn editable_row_count() -> usize {
    EDITABLE_ACTIONS.len() + 1
}

pub(super) fn display_stored_combo(combo: &str) -> String {
    parse_combo(combo)
        .and_then(|(mods, key)| display_combo(mods, &key))
        .unwrap_or_else(|| combo.to_owned())
}

pub(super) const EDITABLE_ACTIONS: &[(Action, &str, &str)] = &[
    // 顺序 = 设置页展示顺序：按 GROUPS 的连续区间分组（2026-08-09 学原型
    // 分组裁定）。持久化按 Action 名存储，重排只影响展示，不动用户数据。
    // -- 全局（快速终端行占本组第 0 行，见 QUICK_TERMINAL_ROW）--
    (Action::ToggleCommandPalette, "命令面板", "Command palette"),
    (Action::OpenQuickJump, "快速跳转", "Quick jump"),
    (Action::ToggleShellPicker, "Shell 选择器", "Shell picker"),
    (Action::CreateNewWindow, "新建窗口", "New window"),
    (Action::ToggleFullscreen, "全屏", "Fullscreen"),
    // -- 标签页 --
    (Action::CreateNewTab, "新建标签页", "New tab"),
    (Action::CloseTab, "关闭标签页 / 分屏", "Close tab / pane"),
    (Action::SelectNextTab, "下一个标签页", "Next tab"),
    (Action::SelectPreviousTab, "上一个标签页", "Previous tab"),
    // -- 窗格 --
    (Action::SplitRight, "左右分屏", "Split right"),
    (Action::SplitDown, "上下分屏", "Split down"),
    (Action::ToggleZoom, "放大当前分屏", "Zoom current pane"),
    (Action::FocusPaneLeft, "焦点：左侧分屏", "Focus pane left"),
    (Action::FocusPaneRight, "焦点：右侧分屏", "Focus pane right"),
    (Action::FocusPaneUp, "焦点：上方分屏", "Focus pane up"),
    (Action::FocusPaneDown, "焦点：下方分屏", "Focus pane down"),
    // -- 侧栏面板 --
    (Action::ToggleFilesPanel, "目录树面板", "Files panel"),
    (Action::ToggleGitPanel, "Git 面板", "Git panel"),
    // -- 终端 --
    (Action::PromptJumpUp, "跳到上一个提示符", "Previous prompt"),
    (Action::PromptJumpDown, "跳到下一个提示符", "Next prompt"),
    (Action::SearchForward, "搜索（向前）", "Search forward"),
    (Action::SearchBackward, "搜索（向后）", "Search backward"),
    (Action::Copy, "复制", "Copy"),
    (Action::Paste, "粘贴", "Paste"),
    (Action::IncreaseFontSize, "增大字号", "Font size up"),
    (Action::DecreaseFontSize, "减小字号", "Font size down"),
    (Action::ResetFontSize, "重置字号", "Reset font size"),
];

/// 设置页分组（无框裁定 2026-08-09：只用段标题+间距分组，不画容器）。
/// `usize` 是本组行数；区间连续覆盖全部可编辑行（含第 0 行快速终端）。
pub(super) const GROUPS: &[(&str, &str, usize)] = &[
    ("全局", "Global", 6),
    ("标签页", "Tabs", 4),
    ("窗格", "Panes", 7),
    ("侧栏面板", "Side panels", 2),
    ("终端", "Terminal", 9),
];

/// 只读展示行（无法在图形页编辑，TOML/settings 行仍可覆盖其中的表驱动键）。
pub(super) const READONLY_ROWS: &[(&str, &str, &str)] = &[
    ("切换到第 N 个标签页", "Select tab N", "Alt+1..9 / Ctrl+1..9"),
    ("启动 Profile N", "Launch Profile N", "Ctrl+Shift+1..9"),
    ("贴入 AI 修复建议", "Paste AI fix suggestion", "Ctrl+."),
];

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn groups_cover_every_editable_row_exactly_once() {
        let total: usize = GROUPS.iter().map(|(.., count)| count).sum();
        assert_eq!(total, editable_row_count(), "分组区间必须连续铺满全部可编辑行");
        assert_eq!(GROUPS.len(), 5, "settings::KeymapPaneState 的数组长度写死为 5");
    }
}

/// 内置默认表的进程级缓存：设置页每帧反查 25+ 个动作，重建整表太贵。
fn cached_defaults() -> &'static [KeyBinding] {
    static DEFAULTS: OnceLock<Vec<KeyBinding>> = OnceLock::new();
    DEFAULTS.get_or_init(default_key_bindings)
}

// ---- combo 文本 ↔ 结构 ----

/// 命名键与存储别名的映射（存储侧统一小写）。
const NAMED_ALIASES: &[(&str, NamedKey)] = &[
    ("enter", NamedKey::Enter),
    ("tab", NamedKey::Tab),
    ("escape", NamedKey::Escape),
    ("space", NamedKey::Space),
    ("backspace", NamedKey::Backspace),
    ("delete", NamedKey::Delete),
    ("insert", NamedKey::Insert),
    ("home", NamedKey::Home),
    ("end", NamedKey::End),
    ("pageup", NamedKey::PageUp),
    ("pagedown", NamedKey::PageDown),
    ("up", NamedKey::ArrowUp),
    ("down", NamedKey::ArrowDown),
    ("left", NamedKey::ArrowLeft),
    ("right", NamedKey::ArrowRight),
    ("f1", NamedKey::F1),
    ("f2", NamedKey::F2),
    ("f3", NamedKey::F3),
    ("f4", NamedKey::F4),
    ("f5", NamedKey::F5),
    ("f6", NamedKey::F6),
    ("f7", NamedKey::F7),
    ("f8", NamedKey::F8),
    ("f9", NamedKey::F9),
    ("f10", NamedKey::F10),
    ("f11", NamedKey::F11),
    ("f12", NamedKey::F12),
    ("f13", NamedKey::F13),
    ("f14", NamedKey::F14),
    ("f15", NamedKey::F15),
    ("f16", NamedKey::F16),
    ("f17", NamedKey::F17),
    ("f18", NamedKey::F18),
    ("f19", NamedKey::F19),
    ("f20", NamedKey::F20),
    ("f21", NamedKey::F21),
    ("f22", NamedKey::F22),
    ("f23", NamedKey::F23),
    ("f24", NamedKey::F24),
];

/// 物理数字键（Shift 组合下 logical key 随布局漂移，只能按 scancode 绑）。
const DIGIT_CODES: [(&str, KeyCode); 10] = [
    ("digit1", KeyCode::Digit1),
    ("digit2", KeyCode::Digit2),
    ("digit3", KeyCode::Digit3),
    ("digit4", KeyCode::Digit4),
    ("digit5", KeyCode::Digit5),
    ("digit6", KeyCode::Digit6),
    ("digit7", KeyCode::Digit7),
    ("digit8", KeyCode::Digit8),
    ("digit9", KeyCode::Digit9),
    ("digit0", KeyCode::Digit0),
];

/// `ctrl+shift+t` → (mods, key)。`plus` 是 `+` 键的别名（`+` 是分隔符）。
pub(super) fn parse_combo(combo: &str) -> Option<(ModifiersState, BindingKey)> {
    let mut mods = ModifiersState::empty();
    let mut key: Option<BindingKey> = None;
    for part in combo.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "shift" => mods |= ModifiersState::SHIFT,
            "alt" => mods |= ModifiersState::ALT,
            "win" | "super" | "cmd" => mods |= ModifiersState::SUPER,
            lower => {
                if key.is_some() {
                    return None;
                }
                key = Some(parse_key_name(&lower)?);
            },
        }
    }
    key.map(|key| (mods, key))
}

fn parse_key_name(lower: &str) -> Option<BindingKey> {
    if let Some((_, code)) = DIGIT_CODES.iter().find(|(name, _)| *name == lower) {
        return Some(BindingKey::Scancode(PhysicalKey::Code(*code)));
    }
    if let Some((_, named)) = NAMED_ALIASES.iter().find(|(name, _)| *name == lower) {
        return Some(BindingKey::Keycode { key: Key::Named(*named), location: KeyLocation::Any });
    }
    let key = match lower {
        "esc" => Key::Named(NamedKey::Escape),
        "plus" => Key::Character("+".into()),
        "minus" => Key::Character("-".into()),
        _ if lower.chars().count() == 1 => Key::Character(lower.into()),
        _ => return None,
    };
    Some(BindingKey::Keycode { key, location: KeyLocation::Any })
}

/// (mods, key) → 存储格式 `ctrl+shift+t`。捕获结果与 parse 往返一致。
pub(super) fn canonical_combo(mods: ModifiersState, key: &BindingKey) -> Option<String> {
    let mut out = String::new();
    if mods.control_key() {
        out.push_str("ctrl+");
    }
    if mods.shift_key() {
        out.push_str("shift+");
    }
    if mods.alt_key() {
        out.push_str("alt+");
    }
    if mods.super_key() {
        out.push_str("win+");
    }
    let name = key_storage_name(key)?;
    out.push_str(&name);
    Some(out)
}

fn key_storage_name(key: &BindingKey) -> Option<String> {
    match key {
        BindingKey::Scancode(physical) => DIGIT_CODES
            .iter()
            .find(|(_, code)| PhysicalKey::Code(*code) == *physical)
            .map(|(name, _)| (*name).to_owned()),
        BindingKey::Keycode { key: Key::Named(named), .. } => NAMED_ALIASES
            .iter()
            .find(|(_, candidate)| candidate == named)
            .map(|(name, _)| (*name).to_owned()),
        BindingKey::Keycode { key: Key::Character(c), .. } => {
            let c = c.as_str();
            match c {
                "+" => Some("plus".to_owned()),
                _ if c.chars().count() == 1 => Some(c.to_lowercase()),
                _ => None,
            }
        },
        BindingKey::Keycode { .. } => None,
    }
}

/// 展示格式：`Ctrl+Shift+T`、`Ctrl+Alt+Left`、`Ctrl+Shift+1`。
/// 修饰键前缀（"Ctrl+Shift+" 风格）。捕获态的实时回显也用它，保证与
/// 最终存储/展示的组合一字不差。
pub(crate) fn mods_prefix(mods: ModifiersState) -> String {
    let mut out = String::new();
    if mods.control_key() {
        out.push_str("Ctrl+");
    }
    if mods.shift_key() {
        out.push_str("Shift+");
    }
    if mods.alt_key() {
        out.push_str("Alt+");
    }
    if mods.super_key() {
        out.push_str("Win+");
    }
    out
}

pub(super) fn display_combo(mods: ModifiersState, key: &BindingKey) -> Option<String> {
    let mut out = mods_prefix(mods);
    let name = match key {
        BindingKey::Scancode(physical) => DIGIT_CODES
            .iter()
            .find(|(_, code)| PhysicalKey::Code(*code) == *physical)
            .map(|(name, _)| name.trim_start_matches("digit").to_owned())?,
        BindingKey::Keycode { key: Key::Named(named), .. } => {
            let alias = NAMED_ALIASES
                .iter()
                .find(|(_, candidate)| candidate == named)
                .map(|(name, _)| *name)?;
            match alias {
                "up" => "Up".to_owned(),
                "down" => "Down".to_owned(),
                "left" => "Left".to_owned(),
                "right" => "Right".to_owned(),
                "pageup" => "PageUp".to_owned(),
                "pagedown" => "PageDown".to_owned(),
                other => {
                    let mut chars = other.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                        .unwrap_or_default()
                },
            }
        },
        BindingKey::Keycode { key: Key::Character(c), .. } => c.to_uppercase(),
        BindingKey::Keycode { .. } => return None,
    };
    out.push_str(&name);
    Some(out)
}

/// `keybind=` 行的动作名 → Action。variant 名大小写不敏感（复用
/// ConfigDeserialize），`none`/`unbound` 显式禁用。
pub(super) fn parse_action(name: &str) -> Option<Action> {
    use serde::Deserialize as _;
    let name = name.trim();
    if name.eq_ignore_ascii_case("unbound") {
        return Some(Action::None);
    }
    Action::deserialize(toml::Value::String(name.to_owned())).ok()
}

/// Action → 存储名（Debug 名即 variant 名；仅 unit variants 会出现在
/// 自定义表里，Debug 输出必然干净）。
pub(super) fn action_storage_name(action: &Action) -> String {
    format!("{action:?}")
}

/// raw `keybind=` 行 → 自定义绑定表。非法行静默跳过（与该文件其余键的
/// 容错一致）；行序即优先级（后写的行先匹配）。
pub(super) fn build_bindings(raw: &[(String, String)]) -> Vec<KeyBinding> {
    let mut bindings = Vec::new();
    for (combo, action) in raw {
        let Some((mods, trigger)) = parse_combo(combo) else { continue };
        let Some(action) = parse_action(action) else { continue };
        bindings.push(KeyBinding {
            trigger,
            mods,
            mode: BindingMode::empty(),
            notmode: BindingMode::empty(),
            action,
        });
    }
    // 后写的行优先：倒序存放，匹配层顺序扫描第一条命中即用。
    bindings.reverse();
    bindings
}

/// 一个动作当前的有效键（设置页展示用）。
/// 返回 (展示文本, 是否来自用户自定义)；None = 未绑定。
pub(super) fn effective_combo(action: &Action, custom: &[KeyBinding]) -> Option<(String, bool)> {
    // custom 已是倒序（最新在前）。该动作的候选行必须没被更新的同键行
    // 遮蔽（手编文件可能给同一个键堆多行，运行时只有最前面那条生效——
    // 展示必须与匹配层口径一致，否则显示的键按下去没反应）。
    for (index, binding) in custom.iter().enumerate() {
        if binding.action != *action {
            continue;
        }
        let shadowed_by_newer =
            custom[..index].iter().any(|b| b.trigger == binding.trigger && b.mods == binding.mods);
        if !shadowed_by_newer {
            return display_combo(binding.mods, &binding.trigger).map(|text| (text, true));
        }
    }
    // 默认表：倒序扫描让 platform/nebula 键（靠后 extend）压过 vi/legacy
    // 键；跳过 Numpad 专属与 Copy/Paste 媒体键这类不适合展示的触发器。
    let shadowed = |candidate: &KeyBinding| {
        custom.iter().any(|b| b.trigger == candidate.trigger && b.mods == candidate.mods)
    };
    cached_defaults()
        .iter()
        .rev()
        .filter(|b| b.action == *action)
        .filter(|b| {
            !matches!(
                &b.trigger,
                BindingKey::Keycode { key: Key::Named(NamedKey::Copy | NamedKey::Paste), .. }
                    | BindingKey::Keycode { location: KeyLocation::Numpad, .. }
            )
        })
        .find(|b| !shadowed(b))
        .and_then(|b| display_combo(b.mods, &b.trigger).map(|text| (text, false)))
}

// ---- 捕获 ----

/// 设置页捕获态下一次按键的解释结果。
pub(crate) enum CaptureOutcome {
    /// 纯修饰键或无法表示的键：保持捕获态继续等。
    Pending,
    /// Esc：取消捕获，不改绑定。
    Cancel,
    /// 裸 Backspace：删除该动作的自定义绑定（回落默认）。
    ClearCustom,
    /// 得到一个可存储的组合。
    Bind(String),
}

/// 把捕获态里按下的键翻译成存储 combo。
pub(crate) fn capture_combo(key: &KeyEvent, mods: ModifiersState) -> CaptureOutcome {
    match &key.logical_key {
        Key::Named(NamedKey::Escape) if mods.is_empty() => return CaptureOutcome::Cancel,
        Key::Named(NamedKey::Backspace) if mods.is_empty() => return CaptureOutcome::ClearCustom,
        Key::Named(
            NamedKey::Shift
            | NamedKey::Control
            | NamedKey::Alt
            | NamedKey::AltGraph
            | NamedKey::Super
            | NamedKey::Hyper
            | NamedKey::Meta
            | NamedKey::CapsLock
            | NamedKey::NumLock
            | NamedKey::ScrollLock
            | NamedKey::Fn
            | NamedKey::FnLock,
        ) => return CaptureOutcome::Pending,
        _ => {},
    }

    let binding_key = match key.key_without_modifiers() {
        Key::Named(named) => {
            BindingKey::Keycode { key: Key::Named(named), location: KeyLocation::Any }
        },
        Key::Character(c) => {
            let lower = c.to_lowercase();
            let single_letter = lower.chars().count() == 1
                && lower.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic());
            if mods.shift_key() && !single_letter {
                // Shift+数字/符号的 logical key 随布局漂移，只有物理数字键
                // 能稳定表达（LaunchProfile 系的既有裁定）。
                match key.physical_key {
                    PhysicalKey::Code(code)
                        if DIGIT_CODES.iter().any(|(_, digit)| *digit == code) =>
                    {
                        BindingKey::Scancode(key.physical_key)
                    },
                    _ => return CaptureOutcome::Pending,
                }
            } else {
                BindingKey::Keycode {
                    key: Key::Character(lower.into()),
                    location: KeyLocation::Any,
                }
            }
        },
        _ => return CaptureOutcome::Pending,
    };

    match canonical_combo(mods, &binding_key) {
        Some(combo) => CaptureOutcome::Bind(combo),
        None => CaptureOutcome::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_roundtrip() {
        for combo in
            ["ctrl+shift+t", "ctrl+tab", "alt+enter", "ctrl+alt+left", "f5", "ctrl+shift+digit1"]
        {
            let (mods, key) = parse_combo(combo).unwrap();
            assert_eq!(canonical_combo(mods, &key).as_deref(), Some(combo), "{combo}");
        }
    }

    #[test]
    fn quick_terminal_default_matches_global_hotkey_api() {
        let parsed = DEFAULT_QUICK_TERMINAL_HOTKEY
            .parse::<global_hotkey::hotkey::HotKey>()
            .expect("default quick-terminal shortcut must stay registrable");
        assert_eq!(parsed.key, global_hotkey::hotkey::Code::Backquote);
        assert!(parsed.mods.contains(global_hotkey::hotkey::Modifiers::CONTROL));
        assert_eq!(display_stored_combo(DEFAULT_QUICK_TERMINAL_HOTKEY), "Ctrl+`");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_combo("").is_none());
        assert!(parse_combo("ctrl+").is_none());
        assert!(parse_combo("ctrl+foo+t").is_none());
        assert!(parse_combo("notakey").is_none());
    }

    #[test]
    fn action_names_roundtrip() {
        for (action, ..) in EDITABLE_ACTIONS {
            let name = action_storage_name(action);
            assert_eq!(parse_action(&name).as_ref(), Some(action), "{name}");
        }
        assert_eq!(parse_action("none"), Some(Action::None));
        assert_eq!(parse_action("unbound"), Some(Action::None));
        assert!(parse_action("NotAnAction").is_none());
    }

    #[test]
    fn custom_binding_shadows_default_in_display() {
        // 自定义 Ctrl+Shift+T → SplitRight 后：SplitRight 显示新键（自定义），
        // CreateNewTab 的默认 Ctrl+Shift+T 被遮蔽 → 显示未绑定。
        let raw = vec![("ctrl+shift+t".to_owned(), "SplitRight".to_owned())];
        let custom = build_bindings(&raw);
        assert_eq!(
            effective_combo(&Action::SplitRight, &custom),
            Some(("Ctrl+Shift+T".to_owned(), true))
        );
        assert_eq!(effective_combo(&Action::CreateNewTab, &custom), None);
        // 未受影响的动作照常显示默认键。
        assert_eq!(
            effective_combo(&Action::CloseTab, &custom),
            Some(("Ctrl+Shift+W".to_owned(), false))
        );
    }

    #[test]
    fn none_disables_and_later_lines_win() {
        let raw = vec![
            ("ctrl+shift+w".to_owned(), "SplitDown".to_owned()),
            ("ctrl+shift+w".to_owned(), "none".to_owned()),
        ];
        let custom = build_bindings(&raw);
        // 后写的 none 生效：Ctrl+Shift+W 被禁用，SplitDown 回落默认键。
        assert_eq!(custom.first().map(|b| b.action.clone()), Some(Action::None));
        assert_eq!(
            effective_combo(&Action::SplitDown, &custom),
            Some(("Ctrl+Shift+S".to_owned(), false))
        );
    }

    #[test]
    fn hardcoded_migration_is_complete() {
        // 原 keyboard.rs 硬编码组合逐一在默认表有对应动作（防迁移漏键）。
        let expect = [
            ("ctrl+shift+t", Action::CreateNewTab),
            ("ctrl+shift+w", Action::CloseTab),
            ("ctrl+tab", Action::SelectNextTab),
            ("ctrl+shift+tab", Action::SelectPreviousTab),
            ("ctrl+shift+e", Action::CreateNewWindow),
            ("ctrl+shift+p", Action::ToggleCommandPalette),
            ("ctrl+shift+o", Action::OpenQuickJump),
            ("ctrl+k", Action::ToggleShellPicker),
            ("ctrl+shift+d", Action::SplitRight),
            ("ctrl+shift+s", Action::SplitDown),
            ("ctrl+shift+enter", Action::ToggleZoom),
            ("ctrl+alt+left", Action::FocusPaneLeft),
            ("ctrl+alt+right", Action::FocusPaneRight),
            ("ctrl+alt+up", Action::FocusPaneUp),
            ("ctrl+alt+down", Action::FocusPaneDown),
            ("ctrl+shift+f", Action::ToggleFilesPanel),
            ("ctrl+shift+g", Action::ToggleGitPanel),
            ("ctrl+shift+up", Action::PromptJumpUp),
            ("ctrl+shift+down", Action::PromptJumpDown),
            ("ctrl+1", Action::SelectTab1),
            ("alt+9", Action::SelectTab9),
            ("ctrl+shift+digit1", Action::LaunchProfile1),
            ("ctrl+shift+digit9", Action::LaunchProfile9),
        ];
        let defaults = cached_defaults();
        for (combo, action) in expect {
            let (mods, key) = parse_combo(combo).unwrap();
            assert!(
                defaults.iter().any(|b| b.action == action && b.mods == mods && b.trigger == key),
                "default table is missing {combo} → {action:?}"
            );
        }
    }
}
