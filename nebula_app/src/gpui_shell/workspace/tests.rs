//! `workspace.rs` 单元测试。

use super::*;

#[cfg(feature = "gpui-test-support")]
mod title_bar_panel_control_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, point};

    #[derive(Default)]
    struct TitleBarPanelControlProbe {
        parent_mouse_downs: usize,
        tab_count: usize,
    }

    impl Render for TitleBarPanelControlProbe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.parent_mouse_downs += 1),
                )
                .child(div().w(px(64.0)).h(px(48.0)).flex().items_end().child(
                    super::top_tabs::top_new_tab_control(
                        cx.listener(|this, _, _, _| this.tab_count += 1),
                    ),
                ))
        }
    }

    #[gpui::test]
    fn top_new_tab_call_site_creates_tab_without_arming_title_bar(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (probe, cx) = cx.add_window_view(|_, _| TitleBarPanelControlProbe {
            tab_count: 1,
            ..Default::default()
        });
        cx.simulate_click(point(px(16.0), px(32.0)), Modifiers::default());

        let (parent_mouse_downs, tab_count) =
            probe.read_with(cx, |probe, _| (probe.parent_mouse_downs, probe.tab_count));
        assert_eq!(tab_count, 2, "顶部加号一次左键必须创建一个新标签");
        assert_eq!(parent_mouse_downs, 0, "面板按钮不得启动标题栏拖拽起手");
    }

    #[derive(Default)]
    struct TopTabGeometryProbe;

    impl Render for TopTabGeometryProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .w(px(400.0))
                .h(px(48.0))
                .items_end()
                .child(
                    div()
                        .debug_selector(|| "top-tab-geometry-probe".to_owned())
                        .w(px(160.0))
                        .h(px(super::top_tabs::TOP_TAB_H)),
                )
                .child(super::top_tabs::top_tab_action_slot(
                    super::top_tabs::top_new_tab_control(|_, _, _| {})
                        .debug_selector(|| "top-new-tab-geometry-probe".to_owned()),
                ))
                .child(super::top_tabs::top_tab_action_slot(
                    h_flex()
                        .debug_selector(|| "top-tabs-menu-geometry-probe".to_owned())
                        .child(super::top_tabs::top_tabs_menu_button(false)),
                ))
        }
    }

    fn vertical_center(bounds: Bounds<Pixels>) -> f32 {
        f32::from(bounds.origin.y + bounds.size.height * 0.5)
    }

    #[gpui::test]
    fn top_tab_action_buttons_share_the_tab_vertical_center(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| TopTabGeometryProbe);
        let tab = cx.debug_bounds("top-tab-geometry-probe").expect("tab bounds");
        let new_tab = cx.debug_bounds("top-new-tab-geometry-probe").expect("new-tab bounds");
        let menu = cx.debug_bounds("top-tabs-menu-geometry-probe").expect("menu bounds");

        assert_eq!(f32::from(tab.size.height), 34.0);
        assert_eq!(f32::from(new_tab.size.height), 32.0);
        assert_eq!(f32::from(menu.size.height), 32.0);
        assert_eq!(vertical_center(new_tab), vertical_center(tab));
        assert_eq!(vertical_center(menu), vertical_center(tab));
    }
}

#[cfg(feature = "gpui-test-support")]
mod sidebar_new_tab_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};

    #[derive(Default)]
    struct SidebarNewTabProbe {
        parent_mouse_downs: usize,
        parent_clicks: usize,
        new_tabs: usize,
    }

    impl Render for SidebarNewTabProbe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let hover_group: SharedString = "sidebar-new-tab-test-hover".into();
            h_flex()
                .id("sidebar-new-tab-test-parent")
                .group(hover_group.clone())
                .w(px(200.0))
                .h(px(34.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.parent_mouse_downs += 1),
                )
                .on_click(cx.listener(|this, _, _, _| this.parent_clicks += 1))
                .child(div().flex_1())
                .child(
                    super::sidebar::sidebar_new_tab_control(
                        hover_group,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.new_tabs += 1;
                        }),
                    )
                    .debug_selector(|| "sidebar-new-tab-probe".to_owned())
                    .child("+"),
                )
        }
    }

    #[gpui::test]
    fn sidebar_new_tab_click_after_header_hover_reaches_the_real_call_site(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (probe, cx) = cx.add_window_view(|_, _| SidebarNewTabProbe::default());
        let bounds = cx.debug_bounds("sidebar-new-tab-probe").expect("sidebar plus bounds");
        let center = gpui::point(
            bounds.origin.x + bounds.size.width * 0.5,
            bounds.origin.y + bounds.size.height * 0.5,
        );

        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.simulate_click(center, Modifiers::default());

        let (parent_mouse_downs, parent_clicks, new_tabs) = probe.read_with(cx, |probe, _| {
            (probe.parent_mouse_downs, probe.parent_clicks, probe.new_tabs)
        });
        assert_eq!(new_tabs, 1, "hover 后点击侧栏加号必须创建标签");
        assert_eq!(parent_mouse_downs, 0, "加号按下不得给 TABS 分区标题上膛");
        assert_eq!(parent_clicks, 0, "加号点击不得同时折叠 TABS 分区");
    }
}

#[test]
fn gpui_binding_combo_maps_plus_minus_and_digits() {
    assert_eq!(gpui_binding_combo("ctrl+plus"), "ctrl-+");
    assert_eq!(gpui_binding_combo("ctrl+minus"), "ctrl--");
    assert_eq!(gpui_binding_combo("ctrl+digit1"), "ctrl-1");
    assert!(
        custom_workspace_binding("ctrl+shift+e", &crate::config::Action::CreateNewWindow).is_some()
    );
}

#[test]
fn pane_card_divider_reaches_the_window_top_without_moving_its_bottom() {
    let card = Bounds::new(
        gpui::point(px(230.0), px(48.0)),
        size(px(850.0), px(672.0)),
    );
    let divider = pane_card_divider_bounds(card, 1.0, 1.5).expect("侧栏存在时必须画分界线");

    assert_eq!(f32::from(divider.origin.x), 230.0);
    assert_eq!(f32::from(divider.origin.y), 0.0);
    assert_eq!(f32::from(divider.size.height), 720.0);
    assert_eq!(
        f32::from(divider.origin.y + divider.size.height),
        f32::from(card.origin.y + card.size.height),
        "向上延伸不能越过原来的正文底边"
    );
    assert!(
        (f32::from(divider.size.width) * 1.5 - 2.0).abs() < f32::EPSILON,
        "150% 缩放下 1px 线应吸附为两个物理像素"
    );

    let scale_one = pane_card_divider_bounds(card, 1.0, 1.0).unwrap();
    assert_eq!(f32::from(scale_one.size.width), 1.0);
}

#[test]
fn pane_card_divider_requires_both_width_and_a_real_sidebar_boundary() {
    let no_sidebar = Bounds::new(
        gpui::point(px(0.0), px(48.0)),
        size(px(1080.0), px(672.0)),
    );
    assert!(pane_card_divider_bounds(no_sidebar, 1.0, 1.5).is_none());

    let sidebar = Bounds::new(
        gpui::point(px(230.0), px(48.0)),
        size(px(850.0), px(672.0)),
    );
    assert!(pane_card_divider_bounds(sidebar, 0.0, 1.5).is_none());
}

#[test]
fn ai_hook_routing_never_falls_back_for_a_stale_exact_pane() {
    let pane_ids = [7u64, 11];
    assert_eq!(ai_hook_target_pane(&pane_ids, Some(11), Some(7)), Some(11));
    assert_eq!(ai_hook_target_pane(&pane_ids, Some(99), Some(7)), None);
    assert_eq!(ai_hook_target_pane(&pane_ids, None, Some(7)), Some(7));
    assert_eq!(ai_hook_target_pane(&pane_ids, None, None), None);
}

#[test]
fn restored_agent_command_honors_resume_ai_without_changing_session_data() {
    let agent = crate::session::AgentSession {
        source: "claude".to_owned(),
        session_id: Some("session-42".to_owned()),
    };

    assert_eq!(
        restored_agent_command(true, Some(&agent)),
        Some("claude --resume session-42".to_owned())
    );
    assert_eq!(restored_agent_command(false, Some(&agent)), None);
    assert_eq!(restored_agent_command(true, None), None);
}

#[test]
fn new_tab_position_uses_the_shared_runtime_semantics() {
    use nebula_settings::NewTabPositionName::{AfterCurrent, End};

    assert_eq!(new_tab_insert_index(AfterCurrent, 1, 4), 2);
    assert_eq!(new_tab_insert_index(End, 1, 4), 4);
    assert_eq!(new_tab_insert_index(AfterCurrent, 9, 2), 2);
    assert_eq!(new_tab_insert_index(AfterCurrent, 0, 0), 0);
}

/// 标签位置左右移动的落点：不环绕，两端各自到头就是 no-op。环绕会让"按住
/// 键把标签推到最左"突然跳到最右端，而标签栏此时正滚在另一头，人就找不着
/// 自己的标签了。
#[test]
fn moving_a_tab_stops_at_both_ends_instead_of_wrapping() {
    use super::key_actions::move_target;

    assert_eq!(move_target(1, 4, true), Some(2));
    assert_eq!(move_target(1, 4, false), Some(0));
    // 两端到头。
    assert_eq!(move_target(0, 4, false), None);
    assert_eq!(move_target(3, 4, true), None);
    // 退化输入：单标签、空集合、越界 active 都不动。
    assert_eq!(move_target(0, 1, true), None);
    assert_eq!(move_target(0, 0, false), None);
    assert_eq!(move_target(9, 4, true), None);
}

#[test]
fn default_launch_tag_does_not_follow_later_default_shell_changes() {
    use crate::session::LaunchSession;

    // `Default` 的实际 PTY 启动是引擎默认 PowerShell；标签必须只由这个
    // 已保存身份决定，不能在恢复时重新采样可变的用户设置。引擎默认的
    // 那台是系统自带的 Windows PowerShell 5.1，短标 `ps`（7 才是 pwsh）。
    assert_eq!(
        NebulaWorkspace::launch_shell_tag(&LaunchSession::Default).map(|tag| tag.to_string()),
        Some("ps".to_owned())
    );
}

/// 分屏生命周期合同的树侧不变式：split 后叶集扩张、关到最后一个叶
/// 判 WasRoot（宿主关整 tab）、中途摘叶塌缩并移交焦点。
#[test]
fn split_tree_lifecycle_matches_pane_contract() {
    let mut tree = SplitTree::leaf(1u64);
    assert!(tree.split_leaf(1, 2, SplitDirection::LeftRight, 0.5));
    assert!(tree.split_leaf(2, 3, SplitDirection::TopBottom, 0.5));
    assert_eq!(tree.leaves(), vec![1, 2, 3]);

    // 摘中间叶：父节点塌缩，焦点移交幸存子树首叶。
    assert_eq!(tree.remove_leaf(2), RemoveOutcome::Collapsed(3));
    assert_eq!(tree.leaves(), vec![1, 3]);
    // 摘到只剩一个：WasRoot=宿主应关整个 tab，树不再可用。
    assert_eq!(tree.remove_leaf(1), RemoveOutcome::Collapsed(3));
    assert_eq!(tree.remove_leaf(3), RemoveOutcome::WasRoot);
    // 不存在的叶子不产生副作用。
    let mut single = SplitTree::leaf(9u64);
    assert_eq!(single.remove_leaf(4), RemoveOutcome::NotFound);
}

#[test]
fn dock_tree_places_the_source_tree_on_the_nav_side() {
    let docked = dock_tree(SplitTree::leaf(1u64), SplitTree::leaf(9u64), SplitNav::Left);
    assert_eq!(docked.leaves(), vec![9, 1], "Left：source 树进 first 槽");
    match &docked {
        SplitTree::Split { direction: SplitDirection::LeftRight, ratio, .. } => {
            assert!((ratio - 0.5).abs() < f32::EPSILON, "dock 恒为 50/50 根分割");
        },
        _ => panic!("dock 应产生根级左右分割"),
    }

    // 多叶源树整树挂入，叶序保持源树内部顺序。
    let mut source = SplitTree::leaf(7u64);
    assert!(source.split_leaf(7, 8, SplitDirection::TopBottom, 0.5));
    let docked = dock_tree(SplitTree::leaf(1u64), source, SplitNav::Down);
    assert_eq!(docked.leaves(), vec![1, 7, 8], "Down：source 树进 second 槽");
    match &docked {
        SplitTree::Split { direction: SplitDirection::TopBottom, .. } => {},
        _ => panic!("Down 应产生上下分割"),
    }
}

#[test]
fn cwd_palette_actions_are_available_only_for_local_terminal_tabs() {
    use crate::display::command_palette::PaletteAction;

    assert!(!NebulaWorkspace::palette_action_available(&PaletteAction::CopyCwd, false,));
    assert!(!NebulaWorkspace::palette_action_available(&PaletteAction::RevealCwd, false,));
    assert!(NebulaWorkspace::palette_action_available(&PaletteAction::CopyCwd, true,));
    assert!(NebulaWorkspace::palette_action_available(&PaletteAction::RevealCwd, true,));
    assert!(NebulaWorkspace::palette_action_available(&PaletteAction::NewTab, false));
}

/// 新建终端弹窗：默认 shell 必须占首行，因为「开弹窗 → 回车」是旧壳
/// 点 "+" 后最常走的一条路，它必须等于「开一个默认终端」。
#[test]
fn shell_palette_puts_the_default_shell_first() {
    let shells = ["pwsh", "powershell", "cmd", "nu"]
        .into_iter()
        .map(|id| crate::shell_detect::DetectedShell {
            name: crate::shell_detect::display_name_for_id(id),
            id: id.to_owned(),
            program: format!("{id}.exe"),
            args: Vec::new(),
        })
        .collect::<Vec<_>>();
    let language = crate::display::UiLanguage::ZhCn;

    // 检测顺序里 nu 在最后，设成默认后必须提到首行。
    let rows = shell_palette_rows(
        shells.clone(),
        Vec::new(),
        ["box.example".to_owned()],
        "nu",
        language,
        1.0,
    );
    assert_eq!(rows.len(), 5, "四台 shell + 一台 SSH");
    assert!(matches!(
        &rows[0].action,
        WorkspacePaletteAction::LaunchShell(shell) if shell.id == "nu"
    ));
    assert_eq!(rows[0].group, "推荐");
    let rest: Vec<_> = rows[1..4]
        .iter()
        .map(|row| match &row.action {
            WorkspacePaletteAction::LaunchShell(shell) => shell.id.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(rest, ["pwsh", "powershell", "cmd"]);
    assert!(rows[1..4].iter().all(|row| row.group == "所有 Shell"));
    assert!(matches!(
        &rows[4].action,
        WorkspacePaletteAction::LaunchSshHost(host) if host == "box.example"
    ));
    assert_eq!(rows[4].group, "SSH 主机");

    // 默认 id 没在检测结果里（WSL 发行版被卸载等）：不置顶也不 panic。
    let rows = shell_palette_rows(shells, Vec::new(), None::<String>, "wsl:Ghost", language, 1.0);
    assert!(matches!(
        &rows[0].action,
        WorkspacePaletteAction::LaunchShell(shell) if shell.id == "pwsh"
    ));
    assert_eq!(rows[0].group, "所有 Shell");
}

#[test]
fn shell_palette_includes_imported_terminal_profiles() {
    let profile = crate::config::ui_config::Profile {
        name: "Portable PowerShell".to_owned(),
        command: r"D:\Tools\pwsh.exe".to_owned(),
        args: vec!["-NoLogo".to_owned()],
        cwd: None,
        shell_id: Some("pwsh".to_owned()),
        terminal_profile_id: Some("pwsh-deadbeef".to_owned()),
    };
    let id = profile.settings_id().expect("imported profile id");
    let rows = shell_palette_rows(
        Vec::new(),
        vec![profile],
        None::<String>,
        &id,
        crate::display::UiLanguage::ZhCn,
        1.0,
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].group, "推荐");
    assert!(matches!(
        &rows[0].action,
        WorkspacePaletteAction::LaunchProfile(profile)
            if profile.command == r"D:\Tools\pwsh.exe"
    ));
}

#[test]
fn wsl_file_tree_terminal_changes_directory_without_forcing_bash() {
    let launch = super::file_tree::wsl_terminal_launch_at(
        "WSL · Ubuntu".to_owned(),
        "wsl.exe".to_owned(),
        "Ubuntu".to_owned(),
        "/home/user/project".to_owned(),
    );
    let crate::session::LaunchSession::Shell { program, args, .. } = launch else {
        panic!("WSL file-tree launch must remain a shell session");
    };

    assert_eq!(program, "wsl.exe");
    assert_eq!(args, ["-d", "Ubuntu", "--cd", "/home/user/project"]);
    assert!(!args.iter().any(|arg| arg == "--exec" || arg.eq_ignore_ascii_case("bash")));
}

#[test]
fn escape_does_not_bind_globally_when_the_terminal_is_focused() {
    use gpui::{KeyContext, Keymap, Keystroke};

    let keymap = Keymap::new(vec![KeyBinding::new(
        "escape",
        CloseCommandPalette,
        Some(PALETTE_KEY_CONTEXT),
    )]);
    let terminal =
        [KeyContext::parse("Root").unwrap(), KeyContext::parse("NebulaTerminal").unwrap()];
    let (bindings, pending) =
        keymap.bindings_for_input(&[Keystroke::parse("escape").unwrap()], &terminal);
    assert!(!pending);
    assert!(
        bindings.iter().all(|binding| !binding.action().as_any().is::<CloseCommandPalette>()),
        "终端聚焦时 Esc 不得命中 CloseCommandPalette"
    );

    let palette =
        [KeyContext::parse("Root").unwrap(), KeyContext::parse(PALETTE_KEY_CONTEXT).unwrap()];
    let (bindings, pending) =
        keymap.bindings_for_input(&[Keystroke::parse("escape").unwrap()], &palette);
    assert!(!pending);
    assert!(bindings[0].action().as_any().is::<CloseCommandPalette>());
}

#[test]
fn palette_now_exposes_export_workspace_and_git_panel() {
    use crate::display::command_palette::PaletteAction;

    assert!(NebulaWorkspace::palette_action_supported(&PaletteAction::ExportWorkspace));
    assert!(NebulaWorkspace::palette_action_supported(&PaletteAction::ToggleGitPanel));
}

#[test]
fn ai_session_palette_exposes_verified_resume_and_fork_commands() {
    let sessions = vec![
        crate::ai_sessions::AiSession::test_session(
            crate::ai_agents::AgentKind::Claude,
            "claude-42",
            "Fix resize",
        ),
        crate::ai_sessions::AiSession::test_session(
            crate::ai_agents::AgentKind::Aider,
            "aider-42",
            "Unsupported",
        ),
    ];
    let rows = ai_session_palette_rows(sessions);
    assert_eq!(rows.len(), 2, "Claude supports resume and fork; Aider supports neither");
    assert!(matches!(
        &rows[0].action,
        WorkspacePaletteAction::RunAiSession { command, .. }
            if command == "claude --resume claude-42"
    ));
    assert!(matches!(
        &rows[1].action,
        WorkspacePaletteAction::RunAiSession { command, .. }
            if command == "claude --resume claude-42 --fork-session"
    ));
}

/// 旧壳 CommitRename：trim 后空串清掉 custom_name，恢复 cwd 自动名。
#[test]
fn tab_chrome_commit_empty_clears_custom_name() {
    let mut meta = TabMeta { custom_name: Some("manual".to_owned()), ..TabMeta::default() };
    apply_commit_rename(&mut meta, "   \t  ");
    assert_eq!(meta.custom_name, None);

    apply_commit_rename(&mut meta, "  my-tab  ");
    assert_eq!(meta.custom_name.as_deref(), Some("my-tab"));
}

/// 旧壳 CancelRename：丢掉缓冲，custom_name 原样保留（含启动时写入的「分叉」）。
#[test]
fn tab_chrome_cancel_leaves_custom_name() {
    let mut named = TabMeta { custom_name: Some("Claude 分叉".to_owned()), ..TabMeta::default() };
    apply_cancel_rename(&mut named);
    assert_eq!(named.custom_name.as_deref(), Some("Claude 分叉"));

    let mut auto = TabMeta::default();
    apply_cancel_rename(&mut auto);
    assert_eq!(auto.custom_name, None);
}

/// 旧壳 `chrome_tab_label`：有 cwd 就用末级目录名，不用 OSC 标题 / `NEBULA|` 整串。
#[test]
fn tab_chrome_label_uses_cwd_last_component() {
    use crate::gpui_shell::terminal::view::last_path_component;

    assert_eq!(last_path_component(r"D:\work\nebula").as_deref(), Some("nebula"));
    assert_eq!(last_path_component("/home/me/src/").as_deref(), Some("src"));
    assert_eq!(last_path_component(r"C:\Users\me\").as_deref(), Some("me"));
    assert_eq!(last_path_component("").as_deref(), None);
    assert_eq!(last_path_component("///").as_deref(), None);
}
