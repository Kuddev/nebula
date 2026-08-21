//! 补全引擎的 GPUI 接线：进程级共享数据源。
//!
//! 计算核心在 `display::suggest_engine`（两壳同源）；这里只解决"GPUI 壳没有
//! `Display` 可借"的所有权问题：历史/目录/PATH 三个数据源在进程内各持一份
//! 单例，所有 `TerminalView` 共用——多 pane 各自 load 会在退出时互相覆盖
//! 历史文件，单例还顺带免掉每次 spawn 的重复读盘。
//!
//! 锁序：本模块的锁内不再碰 `Term` 锁与 GPUI 实体；调用方先读完 grid 行、
//! 放掉终端锁，再进这里算建议（文件系统 IO 只发生在补全源里）。

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::directory_history::DirectoryHistory;
use crate::display::suggest_engine::{SuggestSources, suggest_update};
use crate::display::{AcceptKey, CompletionStyle, NebulaPaneState};
use crate::nebula_history::NebulaHistory;

/// 历史是唯一需要独占可变借用的源（`record` 追加 + 落盘）。目录/PATH
/// 内部自带共享语义（`DirectoryHistory` 克隆句柄、commands 是 `Arc<Mutex>`）。
struct Shared {
    history: NebulaHistory,
    directories: DirectoryHistory,
    commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

fn shared() -> MutexGuard<'static, Shared> {
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                history: NebulaHistory::load(),
                directories: crate::directory_history::global(),
                commands: crate::display::nebula_commands_handle(),
            })
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 重算一个 pane 的 ghost/弹窗建议。`line_override` 是 grid 读出的屏幕真值
/// （Windows 唯一行来源，见旧壳 `nebula_input_from_raw_grid` 的契约）。
pub fn update(
    state: &mut NebulaPaneState,
    line_override: Option<String>,
    enabled: bool,
    style: CompletionStyle,
) {
    let guard = shared();
    suggest_update(
        &SuggestSources {
            history: &guard.history,
            directories: &guard.directories,
            commands: &guard.commands,
            enabled,
            style,
        },
        state,
        line_override,
    );
}

/// Enter 提交：命令进共享历史（与旧壳 `nebula_commit_line` 同一落点），
/// pane 状态清空等下一行。空行只清不记。
pub fn commit_line(state: &mut NebulaPaneState) {
    let line = state.screen_line.trim().to_owned();
    if !line.is_empty() {
        shared().history.record(&line, &state.cwd);
    }
    // 旧壳 `nebula_commit_line` 同一条：OSC 133;C 到达时 PTY 已把行缓冲清
    // 空，程序身份（侧栏 tab 图标）必须在 Enter 这一刻从屏幕真值捕获。
    // grid 读失败时退回按键镜像——取首 token 做身份已足够。
    state.last_committed = if line.is_empty() { state.line_buf.trim().to_owned() } else { line };
    crate::display::Display::nebula_clear_line(state);
}

/// shell 集成上报 cwd 时喂目录 frecency（旧壳 `nebula_record_directory`）。
pub fn record_directory(cwd: &str) {
    if !cwd.is_empty() {
        shared().directories.record(cwd);
    }
}

/// 弹窗列表是否正显示。
pub fn popup_active(state: &NebulaPaneState) -> bool {
    !state.completion_items.is_empty()
}

/// 弹窗高亮行循环移动。初始没有选中项；首次向任一方向导航都从首项进入，
/// 避免 Up 在无选择态直接跳到列表末尾。
pub fn popup_move(state: &mut NebulaPaneState, delta: isize) {
    let len = state.completion_items.len();
    if len == 0 {
        return;
    }
    state.completion_selected = Some(match state.completion_selected {
        Some(current) => (current as isize + delta).rem_euclid(len as isize) as usize,
        None => 0,
    });
}

/// 取走选中候选要键入的余量并关闭列表。
pub fn popup_take(state: &mut NebulaPaneState) -> Option<String> {
    let index = state.completion_selected?;
    let insert = state.completion_items.get(index)?.insert.clone();
    state.completion_items.clear();
    state.completion_selected = None;
    Some(insert)
}

/// Esc 关闭列表；候选清空但重算键保留，列表在行变化前不会复开（与旧壳
/// `nebula_completion_popup_dismiss` 的缓存约定一致）。返回是否真的关了。
pub fn popup_dismiss(state: &mut NebulaPaneState) -> bool {
    if state.completion_items.is_empty() {
        return false;
    }
    let line = if state.screen_line.is_empty() { &state.line_buf } else { &state.screen_line };
    if !line.is_empty() {
        state.completion_suppressed_line = Some(line.clone());
    }
    state.completion_items.clear();
    state.completion_selected = None;
    true
}

#[cfg(test)]
mod tests {
    use super::{popup_active, popup_move, popup_take};
    use crate::display::{NebulaCompletionItem, NebulaCompletionKind, NebulaPaneState};

    fn popup_state() -> NebulaPaneState {
        let mut state = NebulaPaneState::default();
        state.completion_items = vec![
            NebulaCompletionItem {
                label: "git pull upstream".to_owned(),
                insert: " upstream".to_owned(),
                kind: NebulaCompletionKind::History,
            },
            NebulaCompletionItem {
                label: "git pull --rebase".to_owned(),
                insert: " --rebase".to_owned(),
                kind: NebulaCompletionKind::Command,
            },
        ];
        state
    }

    #[test]
    fn popup_does_not_accept_before_explicit_navigation() {
        let mut state = popup_state();
        assert!(popup_active(&state));
        assert_eq!(state.completion_selected, None);
        assert_eq!(popup_take(&mut state), None);
        assert!(popup_active(&state));
    }

    #[test]
    fn popup_first_navigation_enters_at_the_first_item() {
        for delta in [-1, 1] {
            let mut state = popup_state();
            popup_move(&mut state, delta);
            assert_eq!(state.completion_selected, Some(0));
            assert_eq!(popup_take(&mut state).as_deref(), Some(" upstream"));
            assert!(!popup_active(&state));
        }
    }
}

/// 接受键（Tab/Right/Both）的判定复用旧壳 [`AcceptKey`]。
pub fn accepts(accept: AcceptKey, key: &str) -> bool {
    match key {
        "tab" => accept.accepts_tab(),
        "right" => accept.accepts_right(),
        _ => false,
    }
}
