//! `side_panel` 的单元测试。从 `side_panel.rs` 拆出（2026-08-31）。

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_switches_views_without_closing() {
        let mut p = SidePanel::new();
        p.toggle(PanelView::Files);
        assert!(p.open);
        p.toggle(PanelView::Git);
        assert!(p.open, "switching views keeps the drawer open");
        assert_eq!(p.view, PanelView::Git);
        p.toggle(PanelView::Git);
        assert!(!p.open, "re-toggling the current view closes");
    }

    #[test]
    fn sync_noops_while_closed() {
        let mut p = SidePanel::new();
        assert!(!p.sync(Some(std::env::temp_dir())));
    }

    #[test]
    fn custom_root_survives_same_cwd_but_releases_when_terminal_moves() {
        let base =
            std::env::temp_dir().join(format!("nebula-panel-root-test-{}", std::process::id()));
        let cwd = base.join("cwd");
        let custom = base.join("custom");
        let next_cwd = base.join("next-cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::create_dir_all(&next_cwd).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(cwd.clone())));
        assert!(panel.set_custom_root(custom.clone()));
        assert!(panel.custom_root_active());

        panel.sync(Some(cwd));
        assert_eq!(panel.root(), Some(custom.as_path()));
        assert!(panel.custom_root_active());

        assert!(panel.sync(Some(next_cwd.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(next_cwd.as_path()));

        panel.wait_snapshot();
        panel.file_index.release_for_test();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_custom_root_returns_to_latest_cwd_with_visible_feedback() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-missing-root-test-{}", std::process::id()));
        let cwd = base.join("cwd");
        let custom = base.join("custom");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&custom).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        panel.sync(Some(cwd.clone()));
        assert!(panel.set_custom_root(custom.clone()));
        std::fs::remove_dir_all(&custom).unwrap();

        assert!(panel.sync(Some(cwd.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(cwd.as_path()));
        assert_eq!(panel.root_notice(), Some("所选目录不可用，已跟随当前目录"));

        panel.wait_snapshot();
        panel.file_index.release_for_test();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn invalid_custom_root_refreshes_notice_when_followed_cwd_is_the_same_path() {
        let root = std::env::temp_dir()
            .join(format!("nebula-panel-same-missing-root-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(root.clone())));
        assert!(panel.set_custom_root(root.clone()));
        std::fs::remove_dir_all(&root).unwrap();

        assert!(panel.sync(Some(root.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(root.as_path()));
        assert_eq!(panel.root_notice(), Some("所选目录不可用，已跟随当前目录"));
    }

    #[test]
    fn native_wsl_cwd_replaces_the_stale_host_root() {
        let host = std::env::temp_dir();
        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        // 这里只验证状态切换，不让单测启动真实 WSL/文件快照工人。
        panel.snapshot_running.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(panel.sync(Some(host)));

        let located = crate::shell_detect::WslCwd {
            distro: "Debian".to_owned(),
            guest: "/home/hello".to_owned(),
        };
        assert!(panel.sync_at(None, Some(located.clone())));
        assert_eq!(panel.root(), Some(Path::new("/home/hello")));
        assert_eq!(panel.file_wsl_root(), Some(&located));
        assert!(panel.followed_cwd.is_none(), "旧宿主根不能盖住来宾 cwd");
    }

    #[test]
    fn new_wsl_cwd_releases_a_browsed_guest_root() {
        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        panel.snapshot_running.store(true, std::sync::atomic::Ordering::Relaxed);
        let home =
            crate::shell_detect::WslCwd { distro: "Debian".to_owned(), guest: "/home".to_owned() };
        panel.sync_at(None, Some(home.clone()));
        assert!(panel.set_custom_wsl_root(crate::shell_detect::WslCwd {
            distro: "Debian".to_owned(),
            guest: "/".to_owned(),
        }));
        assert!(panel.custom_root_active());

        let etc =
            crate::shell_detect::WslCwd { distro: "Debian".to_owned(), guest: "/etc".to_owned() };
        assert!(panel.sync_at(None, Some(etc.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(Path::new("/etc")));
        assert_eq!(panel.file_wsl_root(), Some(&etc));
    }

    #[test]
    fn wsl_find_records_preserve_newlines_and_drop_partial_tails() {
        let parsed = parse_wsl_find_pairs(b"d\0hello\0f\0line\nname.txt\0f\0partial");
        assert_eq!(parsed, vec![(b'd', "hello".to_owned()), (b'f', "line\nname.txt".to_owned())]);
    }

    /// WSL 来宾根的枚举回归。依赖本机真实 Debian，所以默认忽略；
    /// 用 `cargo test -p nebula --features gpui-shell -- --ignored` 手动跑。
    #[test]
    #[ignore = "需要本机注册 Debian 发行版"]
    fn wsl_guest_root_enumerates_entries() {
        let located =
            crate::shell_detect::WslCwd { distro: "Debian".to_owned(), guest: "/".to_owned() };
        let (rows, ok) = SidePanel::tree_rows_wsl(&located, &HashSet::new());
        assert!(ok, "枚举必须成功（失败通常是 WSL 冷启动耗尽预算）");
        assert!(!rows.is_empty(), "WSL `/` 必须枚举出来宾条目");
        assert!(rows.iter().any(|row| row.name == "home"), "应当含 home：{rows:?}");
        assert!(rows.iter().all(|row| !row.is_parent), "根目录不该有 `..` 行");
    }

    /// 展开多层的代价必须**不随层数增长**。
    ///
    /// 旧实现在递归里每层 fork 一次 `wsl.exe`（冷启动实测 7.5 秒），展开 5 层
    /// 就是 5 次串行往返——这是"WSL 打开路径非常卡"的根因。现在整棵树只发一条
    /// 多起点 `find`，所以展开若干层的耗时应当和只读根同量级。
    ///
    /// 判据用倍数而不是绝对秒数：`wsl.exe` 的耗时高度可变（346ms / 7.5s / >20s，
    /// 见 [`WSL_COMMAND_TIMEOUT`] 的实测记录），任何硬编码的墙钟阈值都会在某些
    /// 时刻误报。基准跑在被测调用**之前**，把冷启动的一次性代价算进基准里。
    #[test]
    #[ignore = "需要本机注册 Debian 发行版"]
    fn expanding_many_levels_costs_one_find_not_one_per_level() {
        let located =
            crate::shell_detect::WslCwd { distro: "Debian".to_owned(), guest: "/".to_owned() };

        let baseline_started = Instant::now();
        let (root_rows, ok) = SidePanel::tree_rows_wsl(&located, &HashSet::new());
        let baseline = baseline_started.elapsed();
        assert!(ok, "基准枚举必须成功");

        // 从根的真实条目里挑目录来展开，避免钉死在某个发行版才有的路径上。
        let expanded: HashSet<PathBuf> =
            root_rows.iter().filter(|row| row.is_dir).take(5).map(|row| row.path.clone()).collect();
        assert!(expanded.len() >= 2, "根下至少要有两个目录才测得出层数效应");

        let started = Instant::now();
        let (rows, ok) = SidePanel::tree_rows_wsl(&located, &expanded);
        let elapsed = started.elapsed();
        assert!(ok, "多层枚举必须成功");
        assert!(rows.len() > root_rows.len(), "展开后应当多出子层的行");

        let ceiling = baseline.mul_f32(1.8);
        assert!(
            elapsed <= ceiling,
            "展开 {} 层花了 {elapsed:?}，基准（只读根）是 {baseline:?}——按每层一次 \
             子进程算本该接近 {:?}，说明合并 find 没有生效",
            expanded.len(),
            baseline * (expanded.len() as u32 + 1)
        );
    }

    #[test]
    fn partial_file_snapshot_keeps_vcs_until_the_final_snapshot() {
        let located =
            crate::shell_detect::WslCwd { distro: "Debian".to_owned(), guest: "/".to_owned() };
        let root = PathBuf::from("/");
        let mut panel = SidePanel::new();
        panel.open = true;
        panel.root = Some(root.clone());
        panel.followed_wsl = Some(located.clone());
        panel.git = Some(GitInfo { branch: "keep-until-ready".to_owned(), ..Default::default() });

        *panel.snapshot_slot.lock().unwrap() = Some(PanelSnapshot {
            root: root.clone(),
            files_wsl: Some(located.clone()),
            rows: Vec::new(),
            enumeration_ok: true,
            git: None,
        });
        assert!(panel.harvest_snapshot());
        assert_eq!(panel.git().map(|git| git.branch.as_str()), Some("keep-until-ready"));

        *panel.snapshot_slot.lock().unwrap() = Some(PanelSnapshot {
            root,
            files_wsl: Some(located),
            rows: Vec::new(),
            enumeration_ok: true,
            git: Some(None),
        });
        assert!(panel.harvest_snapshot());
        assert!(panel.git().is_none());
    }

    #[test]
    fn wsl_guest_path_helpers_keep_root_and_parent_boundaries() {
        assert_eq!(normalize_wsl_guest_path("/home/hello/"), "/home/hello");
        assert_eq!(wsl_guest_parent("/home/hello"), Some("/home".to_owned()));
        assert_eq!(wsl_guest_parent("/home"), Some("/".to_owned()));
        assert_eq!(wsl_guest_parent("/"), None);
    }

    /// 起点集合决定这一趟 `find` 的覆盖面，所以边界只能是"严格后代"：把兄弟
    /// 目录（`/home/kudo` 之于 `/home/kud`）也塞进去会枚举出树上根本不存在的
    /// 层，漏掉真后代则让那层显示成空目录。
    #[test]
    fn only_strict_descendants_of_the_root_join_the_find() {
        assert!(wsl_path_is_descendant("/", "/home"));
        assert!(!wsl_path_is_descendant("/", "/"), "根不是自己的后代");
        assert!(wsl_path_is_descendant("/home/kud", "/home/kud/src"));
        assert!(!wsl_path_is_descendant("/home/kud", "/home/kud"));
        assert!(!wsl_path_is_descendant("/home/kud", "/home/kudo"), "前缀相同但不是子目录");
        assert!(!wsl_path_is_descendant("/home/kud", "/etc"));
    }

    /// 根必须排在起点表首位：`expanded` 是 `HashSet`，遍历顺序不定，而超出上限
    /// 的截断一旦把根切掉，整棵树就只剩 `..` 一行。
    #[test]
    fn the_root_always_survives_the_start_point_cap() {
        let expanded: HashSet<PathBuf> = ["/home/kud/z", "/home/kud/a", "/elsewhere", "/home/kud"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let dirs = wsl_dirs_to_list("/home/kud", &expanded);
        assert_eq!(dirs.first().map(String::as_str), Some("/home/kud"));
        assert_eq!(dirs, vec!["/home/kud", "/home/kud/a", "/home/kud/z"], "根外的路径不参与");
    }

    /// 多起点的输出是混在一起的，只有全路径（`%p`）能分桶还原成树。
    #[test]
    fn a_single_find_output_splits_back_into_per_directory_buckets() {
        let mut output = Vec::new();
        for (kind, path) in [
            (b'd', "/home/kud/src"),
            (b'f', "/home/kud/a.txt"),
            (b'f', "/home/kud/src/main.rs"),
            (b'd', "/home/kud/.git"),
            (b'd', "/home"),
        ] {
            output.push(kind);
            output.push(0);
            output.extend_from_slice(path.as_bytes());
            output.push(0);
        }
        let by_dir = bucket_wsl_find_output(&output);
        assert_eq!(
            by_dir["/home/kud"],
            vec![
                (true, "src".to_owned(), "/home/kud/src".to_owned()),
                (false, "a.txt".to_owned(), "/home/kud/a.txt".to_owned()),
            ],
            "同层目录在前，`.git` 不进树：{by_dir:?}"
        );
        assert_eq!(by_dir["/home/kud/src"].len(), 1, "子层单独成桶：{by_dir:?}");
        assert_eq!(by_dir["/"].len(), 1, "根下的条目父是 `/` 而不是空串：{by_dir:?}");
    }

    /// The per-directory cap must be applied *after* ordering. Capping the
    /// `read_dir` iterator instead samples filesystem order, which in a
    /// dot-heavy repo root pushes the real source directories past the cap and
    /// leaves the tree showing nothing but `.tmp-*` scratch dirs.
    #[test]
    fn per_directory_cap_keeps_the_ordered_head() {
        let e = |dir: bool, name: &str| (dir, name.to_owned(), PathBuf::from(name));
        let raw = vec![
            e(true, "zz-last-dir"),
            e(true, ".tmp-scratch"),
            e(false, "a.txt"),
            e(true, "nebula_app"),
        ];
        let kept = SidePanel::ordered_entries(raw, 2);
        let names: Vec<_> = kept.iter().map(|(_, name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [".tmp-scratch", "nebula_app"],
            "cap keeps the ordered head (dirs first, alphabetical), not read_dir order"
        );
        // Files still sort behind every directory, and the cap never reorders.
        let all = SidePanel::ordered_entries(vec![e(false, "a.txt"), e(true, "zz-last-dir")], 10);
        assert_eq!(all[0].1, "zz-last-dir", "directories precede files");
    }

    #[test]
    fn tree_lists_dirs_first_and_expands_on_click() {
        let base = std::env::temp_dir().join(format!("nebula-panel-test-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("a.txt"), "x").unwrap();
        std::fs::write(sub.join("inner.txt"), "y").unwrap();

        let mut p = SidePanel::new();
        p.toggle(PanelView::Files);
        assert!(p.sync(Some(base.clone())));
        p.wait_snapshot();
        let rows = p.file_rows();
        assert_eq!(rows[0].name, "..", "parent navigation stays at the top");
        assert!(rows[0].is_parent);
        assert_eq!(rows[1].name, "sub", "directory sorts before file");
        assert!(rows[1].is_dir);
        assert_eq!(rows.len(), 3, "collapsed dir hides children");

        assert!(p.click_row(1), "clicking a dir toggles expansion");
        let rows = p.file_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2].name, "inner.txt");
        assert_eq!(rows[2].depth, 1);

        p.file_index.release_for_test();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn ignored_state_does_not_change_existing_tree_order() {
        let mut rows = vec![
            FileRow {
                path: PathBuf::from("src"),
                guest_path: None,
                name: "src".into(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: false,
                ignored: false,
            },
            FileRow {
                path: PathBuf::from("target"),
                guest_path: None,
                name: "target".into(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: false,
                ignored: false,
            },
        ];
        let before: Vec<_> = rows.iter().map(|row| row.name.clone()).collect();
        rows[1].ignored = true;
        let after: Vec<_> = rows.iter().map(|row| row.name.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn parent_row_navigates_the_tree_root_without_becoming_draggable() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-parent-row-test-{}", std::process::id()));
        let child = base.join("child");
        std::fs::create_dir_all(&child).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(child.clone())));
        panel.wait_snapshot();
        let parent = panel.file_rows().first().expect("parent row").clone();
        assert_eq!(parent.name, "..");
        assert!(parent.is_parent);
        assert!(parent.is_dir);
        assert_eq!(parent.path, base);

        let drag = FileDrag::new(parent.path.clone(), parent.name, true, 0, (10.0, 10.0));
        assert!(!panel.click_drag_source(&drag), "the parent row never enters drag dispatch");
        assert!(panel.click_row(0));
        assert_eq!(panel.root(), Some(base.as_path()));
        assert!(panel.custom_root_active(), "upward navigation is window-local");

        panel.file_index.release_for_test();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn directory_drag_defers_and_validates_the_plain_click() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-directory-drag-test-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(base.clone())));
        panel.wait_snapshot();
        let drag = FileDrag::new(sub, "sub".into(), true, 1, (10.0, 10.0));

        assert!(panel.click_drag_source(&drag), "a non-drag release keeps directory click");
        assert!(panel.file_rows()[1].expanded);

        let mut active = drag.clone();
        active.update_position((18.0, 10.0));
        assert!(active.active, "eight physical pixels arm the drag");
        assert!(!panel.click_drag_source(&active), "an active drag must not toggle the tree");
        assert!(panel.file_rows()[1].expanded);

        let mut stale = drag;
        stale.source_row = 2;
        assert!(!panel.click_drag_source(&stale), "a changed source row must be ignored");

        panel.file_index.release_for_test();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn terminal_drop_text_requires_an_active_terminal_drop_and_quotes_unicode_whitespace() {
        let path = PathBuf::from("D:/项目 空间");
        let mut drag = FileDrag::new(path, "项目 空间".into(), true, 0, (0.0, 0.0));

        assert_eq!(drag.terminal_drop_text(true), None, "plain clicks never paste");
        drag.update_position((7.0, 7.0));
        assert!(!drag.active, "diagonal motion below each axis threshold remains a click");
        drag.update_position((8.0, 7.0));
        assert!(drag.active);
        assert_eq!(drag.terminal_drop_text(false), None, "dropping back on the drawer is inert");
        assert_eq!(
            String::from_utf8(drag.terminal_drop_text(true).unwrap()).unwrap(),
            "\"D:/项目 空间\" "
        );

        let mut control =
            FileDrag::new(PathBuf::from("unsafe\npath"), "unsafe".into(), true, 0, (0.0, 0.0));
        control.update_position((8.0, 0.0));
        assert_eq!(control.terminal_drop_text(true), None, "a drop must never inject Enter");
    }

    #[test]
    fn hit_test_maps_header_and_rows() {
        let l = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0, PANEL_W_LOGICAL);
        let (px, py, pw, _) = l.panel;
        assert_eq!(panel_hit(&l, px - 1.0, py + 5.0), PanelHit::None);
        assert_eq!(panel_hit(&l, px + 5.0, py + 5.0), PanelHit::Inside);
        assert_eq!(panel_hit(&l, px + 20.0, py + 20.0), PanelHit::ViewFiles);
        assert_eq!(panel_hit(&l, px + pw - 20.0, py + 20.0), PanelHit::ViewGit);
        assert_eq!(panel_hit(&l, px + 5.0, l.list_y + l.row_h * 1.5), PanelHit::Row(1));
    }

    #[test]
    fn git_hover_only_accepts_real_file_rows() {
        let mut panel = SidePanel::new();
        panel.view = PanelView::Git;
        panel.git = Some(GitInfo {
            vcs: VcsKind::Git,
            branch: "main".into(),
            plus: 0,
            minus: 0,
            ahead: 0,
            unstaged: vec![('?', "one.txt".into()), ('M', "two.txt".into())],
            staged: vec![('A', "three.txt".into())],
            conflicts: Vec::new(),
            history: Vec::new(),
            repository_root: None,
            repository: None,
        });

        assert!(!panel.git_row_is_file(0), "未暂存标题");
        assert!(panel.git_row_is_file(1));
        assert!(panel.git_row_is_file(2));
        assert!(!panel.git_row_is_file(3), "已暂存标题");
        assert!(panel.git_row_is_file(4));
        assert!(!panel.git_row_is_file(5), "列表末尾空白行");

        panel.scroll = 2;
        assert!(panel.git_row_is_file(0), "滚动后的真实文件行");
        assert!(!panel.git_row_is_file(1), "滚动后的已暂存标题");
    }

    #[test]
    fn files_summary_actions_have_distinct_exact_hit_targets() {
        let layout = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0, PANEL_W_LOGICAL);
        let actions: Vec<_> = panel_action_rects(&layout, true, true).collect();
        let reveal = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::RevealDirectory)
            .expect("reveal-directory action");
        let follow = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::FollowCurrentDirectory)
            .expect("follow-current-directory action");
        let terminal = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::NewTerminalHere)
            .expect("new-terminal-here action");
        let center = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        let (reveal_x, reveal_y) = center(reveal.1);
        let (follow_x, follow_y) = center(follow.1);
        let (terminal_x, terminal_y) = center(terminal.1);
        let (directory_x, directory_y) = center(panel_tools_layout(&layout).directory);

        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, directory_x, directory_y),
            PanelHit::OpenDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, reveal_x, reveal_y),
            PanelHit::RevealDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, terminal_x, terminal_y),
            PanelHit::NewTerminalHere
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, follow_x, follow_y),
            PanelHit::FollowCurrentDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, false, true, follow_x, follow_y),
            PanelHit::FollowCurrentDirectory,
            "the active follow control keeps its stable hit target"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, false, false, terminal_x, terminal_y),
            PanelHit::Inside,
            "the terminal action must not exist without a tree root"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Git, true, true, reveal_x, reveal_y),
            PanelHit::Inside,
            "Files-only actions must not create invisible Git hit targets"
        );

        for (index, (_, a)) in actions.iter().enumerate() {
            for (_, b) in actions.iter().skip(index + 1) {
                let overlaps =
                    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1;
                assert!(!overlaps, "summary action hit targets must not overlap");
            }
        }
    }

    #[test]
    fn self_drawn_fields_replace_select_all_on_paste() {
        let mut panel = SidePanel::new();
        panel.search_input("old");
        panel.search_select_all();
        assert_eq!(panel.search_selected_text().as_deref(), Some("old"));
        panel.search_input("new\nvalue");
        assert_eq!(panel.search, "newvalue");

        panel.commit_input("old commit");
        panel.commit_select_all();
        assert_eq!(panel.commit_selected_text().as_deref(), Some("old commit"));
        panel.commit_input("new commit");
        assert_eq!(panel.commit_msg, "new commit");
    }

    #[test]
    #[cfg(feature = "legacy-shell")]
    fn git_action_strip_has_four_equal_buttons() {
        let rects = git_button_rects(10.0, 430.0, 10.0);
        assert_eq!(rects.len(), 4);
        assert!(rects.windows(2).all(|pair| (pair[0].1 - pair[1].1).abs() < f32::EPSILON));
        let last = rects.last().expect("at least one git action");
        assert!((last.0 + last.1 - 440.0).abs() < f32::EPSILON);
    }

    #[test]
    fn git_pull_is_fast_forward_only() {
        assert_eq!(git_pull_args(), vec!["pull", "--ff-only"]);
    }

    #[test]
    fn svn_status_parses_item_state_and_normalizes_separators() {
        let status = "M       src\\main.rs\nA       docs/new.md\n?       target\n!       gone.rs\n        props-only-ignored\n";
        let changes = parse_svn_status(status);
        assert_eq!(
            changes,
            vec![
                ('M', "src/main.rs".to_owned()),
                ('A', "docs/new.md".to_owned()),
                ('?', "target".to_owned()),
                ('!', "gone.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn svn_info_revision_parser_accepts_standard_output() {
        let info = "Path: .\r\nWorking Copy Root Path: D:\\checkout\r\nRevision: 42\r\nNode Kind: directory\r\n";
        assert_eq!(parse_svn_revision(info).as_deref(), Some("42"));
        assert_eq!(parse_svn_revision("Path: .\nNode Kind: directory\n"), None);
    }

    #[test]
    fn svn_snapshot_disables_stage_and_push_semantics() {
        // SVN 没有暂存区、没有 push：快照层用 staged 恒空 + ahead 恒 0 编码，
        // 两壳按钮的既有 gating（staged/ahead）不改一行就得到正确禁用。
        let info = GitInfo {
            vcs: VcsKind::Svn,
            branch: "r42".into(),
            unstaged: vec![('M', "a.rs".into())],
            ..GitInfo::default()
        };
        assert!(info.staged.is_empty());
        assert_eq!(info.ahead, 0);
    }

    #[test]
    fn svn_snapshot_separates_addable_and_committable_changes() {
        let only_unversioned = GitInfo {
            vcs: VcsKind::Svn,
            unstaged: vec![('?', "new.txt".into())],
            ..GitInfo::default()
        };
        assert!(only_unversioned.svn_add_ready());
        assert!(!only_unversioned.svn_commit_ready());

        let versioned = GitInfo {
            vcs: VcsKind::Svn,
            unstaged: vec![('M', "tracked.txt".into()), ('C', "conflict.txt".into())],
            ..GitInfo::default()
        };
        assert!(!versioned.svn_add_ready());
        assert!(versioned.svn_commit_ready());
    }

    #[test]
    fn svn_repository_snapshot_keeps_the_ancestor_root() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        for directory in ["conf", "db/revs", "hooks"] {
            std::fs::create_dir_all(repository.join(directory)).unwrap();
        }
        std::fs::write(repository.join("format"), "8\n").unwrap();

        let info = read_svn(&repository.join("db/revs")).expect("repository snapshot");
        assert_eq!(info.vcs, VcsKind::SvnRepository);
        assert_eq!(info.repository_root.as_deref(), Some(repository.as_path()));
        assert!(info.unstaged.is_empty());
    }

    #[test]
    fn svn_repository_snapshot_carries_the_summary_into_the_branch_slot() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        for directory in ["conf", "db/revs/0", "db/revprops/0", "hooks"] {
            std::fs::create_dir_all(repository.join(directory)).unwrap();
        }
        std::fs::write(repository.join("format"), "5\n").unwrap();
        std::fs::write(repository.join("db/format"), "8\nlayout sharded 1000\n").unwrap();
        std::fs::write(repository.join("db/current"), "7\n").unwrap();
        std::fs::write(repository.join("db/revs/0/7"), "cpath: /trunk\ncpath: /tags\n").unwrap();

        let info = read_svn(&repository).expect("repository snapshot");
        let summary = info.repository.expect("摘要必须随快照一起送到 UI");
        assert_eq!(summary.head, Some(7));
        assert_eq!(summary.top_level, vec!["tags", "trunk"]);
        assert!(!summary.has_standard_layout(), "缺 branches 就不算标准布局");
        // 分支位是 UI 的主标题：版本库没有工作副本修订，放 HEAD 才有信息量。
        assert_eq!(info.branch, "版本库 · HEAD r7");
    }

    /// 侧栏定位必须接管 VCS 视图。
    ///
    /// 这是"小乌龟建的目录识别不上"的直接原因：`custom_root` 原先排在
    /// `followed_cwd` 之后，于是在侧栏打开一个 SVN 目录对 VCS 面板毫无作用，
    /// 面板照旧显示终端 cwd 所在的仓库。
    #[test]
    fn vcs_root_prefers_explicit_browsing_over_the_terminal_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let terminal_cwd = temp.path().join("terminal");
        let browsed = temp.path().join("browsed");
        std::fs::create_dir_all(&terminal_cwd).unwrap();
        std::fs::create_dir_all(&browsed).unwrap();

        let mut panel = SidePanel::new();
        panel.followed_cwd = Some(terminal_cwd.clone());
        panel.root = Some(terminal_cwd.clone());
        assert_eq!(panel.vcs_root(), Some(terminal_cwd.as_path()), "没有浏览覆盖时跟终端");

        assert!(panel.set_custom_root(browsed.clone()));
        assert_eq!(
            panel.vcs_root(),
            Some(browsed.as_path()),
            "在侧栏定位到别处之后，VCS 视图必须跟过去"
        );

        assert!(panel.clear_custom_root());
        assert_eq!(panel.vcs_root(), Some(terminal_cwd.as_path()), "回头路把作用域交还终端");
    }

    #[test]
    fn svn_commands_keep_paths_and_messages_as_separate_arguments() {
        let path = PathBuf::from(r"D:\工作副本\src\main.rs");
        let cli = SvnMutation::Resolve(path.clone()).cli_args();
        assert_eq!(
            cli,
            vec![
                OsString::from("resolve"),
                OsString::from("--accept"),
                OsString::from("working"),
                OsString::from("--"),
                path.as_os_str().to_owned(),
            ]
        );

        let commit = SvnMutation::Commit("修复空格 ; $(echo nope)".into())
            .tortoise_args(Path::new(r"D:\工作副本"));
        assert_eq!(commit[0], OsString::from("/command:commit"));
        assert_eq!(commit[1], OsString::from(r"/path:D:\工作副本"));
        assert_eq!(commit[2], OsString::from("/logmsg:修复空格 ; $(echo nope)"));
    }

    #[test]
    fn tortoise_checkout_uses_an_encoded_local_repository_url() {
        let repository = PathBuf::from("D:/新建 文件夹");
        assert_eq!(
            local_repository_url(&repository),
            "file:///D:/%E6%96%B0%E5%BB%BA%20%E6%96%87%E4%BB%B6%E5%A4%B9"
        );
        assert_eq!(
            SvnVisual::Repository {
                command: "checkout",
                root: repository.clone(),
                url_key: "/url:",
            }
            .tortoise_args(),
            vec![
                OsString::from("/command:checkout"),
                OsString::from("/url:file:///D:/%E6%96%B0%E5%BB%BA%20%E6%96%87%E4%BB%B6%E5%A4%B9"),
            ]
        );
        // 版本库浏览器收的是 `/path:`，值仍是 URL——两个键不能互换。
        assert_eq!(
            SvnVisual::Repository { command: "repobrowser", root: repository, url_key: "/path:" }
                .tortoise_args()[1],
            OsString::from("/path:file:///D:/%E6%96%B0%E5%BB%BA%20%E6%96%87%E4%BB%B6%E5%A4%B9"),
        );
    }

    #[test]
    fn working_copy_dialogs_pass_the_path_and_any_fixed_switches() {
        let path = PathBuf::from(r"D:\工作副本\art\hero.psd");
        // 最常见的形状：命令 + 路径，没有别的。
        assert_eq!(
            SvnVisual::WorkingCopy { command: "lock", path: path.clone(), extra: &[] }
                .tortoise_args(),
            vec![
                OsString::from("/command:lock"),
                OsString::from(r"/path:D:\工作副本\art\hero.psd")
            ]
        );
        // blame 缺了修订区间会让 TortoiseBlame 静默退出，所以区间必须传到。
        assert_eq!(
            SvnVisual::WorkingCopy {
                command: "blame",
                path,
                extra: &["/startrev:1", "/endrev:HEAD"],
            }
            .tortoise_args(),
            vec![
                OsString::from("/command:blame"),
                OsString::from(r"/path:D:\工作副本\art\hero.psd"),
                OsString::from("/startrev:1"),
                OsString::from("/endrev:HEAD"),
            ]
        );
    }

    #[test]
    fn svn_relative_targets_cannot_escape_the_visible_root() {
        let root = Path::new(r"D:\checkout");
        assert_eq!(svn_relative_target(root, "src/main.rs"), Some(root.join("src/main.rs")));
        assert!(svn_relative_target(root, "../outside.txt").is_none());
        assert!(svn_relative_target(root, r"D:\outside.txt").is_none());
    }

    // ---- 手动忽略 ----

    #[test]
    fn gitignore_entries_are_anchored_typed_and_escaped() {
        let root = Path::new(r"D:\repo");
        // 锚定：不带前导 `/` 的规则会匹配仓库里每一个同名文件，而用户点的是
        // 这一个。
        assert_eq!(
            gitignore_entry(root, &root.join("src").join("main.rs"), false).as_deref(),
            Some("/src/main.rs")
        );
        // 目录带尾斜杠，才不会连同名文件一起吃掉。
        assert_eq!(gitignore_entry(root, &root.join("target"), true).as_deref(), Some("/target/"));
        // glob 元字符必须转义，否则 `a[1].psd` 变成字符集：既漏本文件又误伤 a1。
        assert_eq!(
            gitignore_entry(root, &root.join("art").join("a[1].psd"), false).as_deref(),
            Some("/art/a\\[1\\].psd")
        );
        assert_eq!(
            gitignore_entry(root, &root.join("weird*name?.txt"), false).as_deref(),
            Some("/weird\\*name\\?.txt")
        );
        // 行尾空格会被 gitignore 丢掉，得转义。
        assert_eq!(
            gitignore_entry(root, &root.join("trailing "), false).as_deref(),
            Some("/trailing\\ ")
        );
        // 仓库根自己、以及跑到仓库外面的路径，都不该产出规则。
        assert!(gitignore_entry(root, root, true).is_none());
        assert!(gitignore_entry(root, Path::new(r"D:\elsewhere\x.txt"), false).is_none());
    }

    #[test]
    fn appending_to_gitignore_creates_dedupes_and_keeps_the_line_ending() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("target")).unwrap();
        let noise = repo.join("target").join("debug.log");
        std::fs::write(&noise, "noise").unwrap();

        // 没有 .gitignore 时新建。
        let outcome = append_to_gitignore(&noise, false).unwrap();
        assert_eq!(
            outcome,
            IgnoreOutcome::Added {
                entry: "/target/debug.log".to_owned(),
                file: repo.join(".gitignore"),
            }
        );
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "/target/debug.log\n"
        );

        // 同一条不重复写。
        assert_eq!(
            append_to_gitignore(&noise, false).unwrap(),
            IgnoreOutcome::AlreadyPresent { entry: "/target/debug.log".to_owned() }
        );

        // 目录规则是另一条，照常追加。
        append_to_gitignore(&repo.join("target"), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "/target/debug.log\n/target/\n"
        );
    }

    #[test]
    fn appending_follows_crlf_and_repairs_a_missing_final_newline() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // 已有 CRLF 且末行没有换行——两个坑同时踩：混行尾会让 git diff 把整个
        // 文件报成改动，缺换行会让新规则和末行拼成一行。
        std::fs::write(repo.join(".gitignore"), "/build\r\n/dist").unwrap();
        std::fs::write(repo.join("secret.env"), "x").unwrap();

        append_to_gitignore(&repo.join("secret.env"), false).unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "/build\r\n/dist\r\n/secret.env\r\n"
        );
    }

    #[test]
    fn repository_root_walks_up_and_accepts_a_gitdir_file() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(git_repository_root(&nested).as_deref(), Some(repo.as_path()));

        let file = nested.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        assert_eq!(git_repository_root(&file).as_deref(), Some(repo.as_path()), "文件从父目录起步");

        // worktree / submodule 的 `.git` 是文件而不是目录，同样算仓库。
        let linked = temp.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: ../repo/.git/worktrees/linked").unwrap();
        assert_eq!(git_repository_root(&linked).as_deref(), Some(linked.as_path()));
    }
}

#[test]
fn git_history_parser_keeps_commit_identity_parents_and_fields() {
    let output = concat!(
        "\u{1e}aaaaaaaa\u{1f}a1b2c3d\u{1f}HEAD -> refs/heads/main, tag: refs/tags/v1\u{1f}ship it\u{1f}Alice\u{1f}1700000000\u{1f}1111111 2222222\n",
        "\u{1e}dddddddd\u{1f}d4e5f6a\u{1f}refs/heads/topic\u{1f}side work\u{1f}Bob\u{1f}1699999000\u{1f}3333333\n",
    );
    let commits = parse_git_history(output);
    assert_eq!(commits.len(), 2);
    let first = &commits[0];
    assert_eq!(first.full_hash, "aaaaaaaa");
    assert_eq!(first.short_hash, "a1b2c3d");
    assert_eq!(first.decorations, "HEAD -> refs/heads/main, tag: refs/tags/v1");
    assert_eq!(first.subject, "ship it");
    assert_eq!(first.author, "Alice");
    assert_eq!(first.timestamp, 1_700_000_000);
    assert_eq!(first.parent_hashes, ["1111111", "2222222"]);
    assert_eq!(commits[1].subject, "side work");
}
