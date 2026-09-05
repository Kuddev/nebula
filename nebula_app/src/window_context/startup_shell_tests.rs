use nebula_terminal::tty::Shell;

use super::{
    TabPlacement, chrome_clock_interval, preferred_initial_cwd, preferred_tab_cwd,
    routed_input_pane, select_initial_shell, tab_insert_index, valid_new_tab_directory,
};
use crate::display::NewTabPosition;

fn shell(program: &str) -> Shell {
    Shell::new(program.to_owned(), Vec::new())
}

#[test]
fn created_tabs_land_after_the_active_tab_by_default() {
    let at = NewTabPosition::AfterCurrent;
    assert_eq!(tab_insert_index(TabPlacement::Created, at, 0, 3), 1);
    assert_eq!(tab_insert_index(TabPlacement::Created, at, 1, 3), 2);
    assert_eq!(tab_insert_index(TabPlacement::Created, at, 2, 3), 3);
}

#[test]
fn created_tabs_land_at_the_end_when_the_user_chose_end() {
    let end = NewTabPosition::End;
    assert_eq!(tab_insert_index(TabPlacement::Created, end, 0, 3), 3);
    assert_eq!(tab_insert_index(TabPlacement::Created, end, 1, 3), 3);
    assert_eq!(tab_insert_index(TabPlacement::Created, end, 2, 3), 3);
}

#[test]
fn restored_tabs_ignore_the_creation_strategy() {
    // 会话恢复与工作区导入保持既有的「当前标签之后」行为，即使用户把
    // 新标签插入策略选成了列表末尾——否则恢复会重排保存的顺序。
    assert_eq!(tab_insert_index(TabPlacement::AfterActive, NewTabPosition::End, 1, 4), 2);
    assert_eq!(tab_insert_index(TabPlacement::AfterActive, NewTabPosition::AfterCurrent, 1, 4), 2);
}

#[test]
fn insertion_never_runs_past_the_end_of_the_tab_list() {
    // 活动下标可能短暂领先于实际长度（关闭标签后的过渡态）；落点必须
    // 仍是合法的 Vec::insert 位置。
    assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::AfterCurrent, 9, 2), 2);
    assert_eq!(tab_insert_index(TabPlacement::AfterActive, NewTabPosition::End, 9, 2), 2);
    assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::AfterCurrent, 0, 0), 0);
    assert_eq!(tab_insert_index(TabPlacement::Created, NewTabPosition::End, 0, 0), 0);
}

/// 落点只能有一个来源。九处各自硬编码 `(active_tab + 1).min(len)` 是这个
/// 功能存在的原因——只靠约定「新入口记得读策略」，下一个入口就会漏掉。
///
/// 允许的两处：`insert_tab`（唯一的创建/恢复插入口）与 `move_tab`
/// （拖拽重排，落点由用户手势直接给出，不经策略）。新增第三处会让本测试
/// 变红——那说明它应该改走 `insert_tab`。
#[test]
fn tab_insertion_has_exactly_two_homes() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/window_context.rs"),
    )
    .expect("window_context.rs 必须可读，否则本约束静默放水");
    // 只扫产品代码。规则本身是用字符串字面量表达的，扫描自己会把规则
    // 连同它的失败信息一起算成违规。
    let production = source.split("#[cfg(test)]").next().unwrap_or_default();
    let insertions = production.matches("self.tabs.insert(").count();
    assert_eq!(
        insertions, 2,
        "找到 {insertions} 处 self.tabs.insert(，应为 2 处（insert_tab 与 move_tab）。\n\
             新的标签插入请调用 insert_tab(entry, placement)，由它决定落点。"
    );
}

#[test]
fn chrome_clock_bounds_running_tabs_to_chrome_cadence() {
    assert_eq!(
        chrome_clock_interval(true, true, false, false),
        std::time::Duration::from_millis(80)
    );
    assert_eq!(
        chrome_clock_interval(true, false, true, false),
        std::time::Duration::from_millis(80)
    );
    assert_eq!(
        chrome_clock_interval(true, false, false, true),
        std::time::Duration::from_millis(80)
    );
    assert_eq!(chrome_clock_interval(true, false, false, false), std::time::Duration::from_secs(1));
    assert_eq!(chrome_clock_interval(false, true, true, true), std::time::Duration::from_secs(1));
}

#[test]
fn startup_shell_uses_user_default_instead_of_the_base_pty_shell() {
    let selected =
        select_initial_shell(Some(shell("powershell.exe")), Some(shell("pwsh.exe")), None);
    assert_eq!(selected, Some(shell("pwsh.exe")));
}

#[test]
fn explicit_cli_command_still_wins_over_the_user_default() {
    let selected = select_initial_shell(
        Some(shell("powershell.exe")),
        Some(shell("pwsh.exe")),
        Some(shell("nu.exe")),
    );
    assert_eq!(selected, Some(shell("nu.exe")));
}

#[test]
fn tree_terminal_cwd_accepts_only_an_existing_directory() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("项目 空间");
    std::fs::create_dir(&directory).unwrap();
    let file = temp.path().join("not-a-directory.txt");
    std::fs::write(&file, b"x").unwrap();

    assert!(valid_new_tab_directory(&directory));
    assert!(!valid_new_tab_directory(&file));
    assert!(!valid_new_tab_directory(&temp.path().join("missing")));
}

#[test]
fn explicit_tab_directory_precedes_startup_and_focused_directories() {
    let explicit = std::path::PathBuf::from("D:/profile");
    let startup = std::path::PathBuf::from("D:/startup");
    let focused = std::path::PathBuf::from("D:/focused");

    assert_eq!(
        preferred_tab_cwd(Some(explicit.clone()), Some(startup.clone()), Some(focused.clone())),
        Some(explicit)
    );
    assert_eq!(preferred_tab_cwd(None, Some(startup.clone()), Some(focused)), Some(startup));
}

#[test]
fn initial_directory_precedence_keeps_cli_and_restore_before_startup() {
    let cli = std::path::PathBuf::from("D:/cli");
    let restored = std::path::PathBuf::from("D:/restore");
    let startup = std::path::PathBuf::from("D:/startup");
    let configured = std::path::PathBuf::from("D:/config");

    assert_eq!(
        preferred_initial_cwd(
            Some(cli.clone()),
            Some(restored.clone()),
            Some(startup.clone()),
            Some(configured.clone())
        ),
        Some(cli)
    );
    assert_eq!(
        preferred_initial_cwd(
            None,
            Some(restored.clone()),
            Some(startup.clone()),
            Some(configured)
        ),
        Some(restored)
    );
    assert_eq!(preferred_initial_cwd(None, None, Some(startup.clone()), None), Some(startup));
}

#[test]
fn multiline_paste_modal_routes_to_its_originating_pane_only() {
    let paste = crate::display::NebulaConfirm::Paste {
        pane_id: 7,
        text: "one\ntwo".to_owned(),
        bracketed: true,
        lines: 2,
    };
    assert_eq!(routed_input_pane(Some(&paste), 9, |pane_id| pane_id == 7), 7);
    assert_eq!(routed_input_pane(Some(&paste), 9, |_| false), 9);

    let close =
        crate::display::NebulaConfirm::ClosePane { pane_id: 7, process: "cargo".to_owned() };
    assert_eq!(routed_input_pane(Some(&close), 9, |_| true), 9);
}
