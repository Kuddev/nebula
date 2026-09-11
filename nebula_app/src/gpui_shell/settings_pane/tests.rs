use super::*;

#[cfg(feature = "gpui-test-support")]
#[gpui::test]
fn ai_toast_setting_is_searchable_and_has_a_visible_switch(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings = crate::gpui_shell::config::Settings::load(ThemeName::Nord);
        settings.ai_toasts = true;
        cx.set_global(settings);
    });
    let mut pane = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| SettingsPane::new(window, cx));
        view.update(cx, |pane, _| {
            pane.runtime = RuntimeSettings::from_raw(&nebula_settings::RawSettings::default());
        });
        pane = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let pane = pane.unwrap();
    cx.simulate_resize(gpui::size(px(1280.0), px(1600.0)));
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.settings_search_input
                .update(cx, |input, cx| input.replace_all("AI 消息弹窗", window, cx));
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(pane.read_with(cx, |pane, _| pane.active_section), 2);
    let bounds = cx.debug_bounds("nebula-switch-ai_toasts").expect("AI toast switch is rendered");
    assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0));
    assert!(bounds.origin.y >= px(0.0) && bounds.bottom() <= px(1600.0));
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.setting_override("ai_toasts")),
        Some((false, "1".to_owned()))
    );
}

#[test]
fn settings_nav_visibility_keeps_stable_routes_but_hides_two_entries() {
    let visibility: Vec<_> = (0..SECTION_IDS.len()).map(is_nav_section_visible).collect();
    assert_eq!(visibility, vec![true, true, true, false, true, true, true, true, true, false]);
    assert_eq!(
        SECTION_IDS,
        [
            "application",
            "appearance",
            "profiles",
            "providers",
            "ssh",
            "network",
            "interaction",
            "keymap",
            "advanced",
            "backup",
        ]
    );
}

#[test]
fn settings_nav_starts_with_application_then_frequent_options() {
    let visible: Vec<_> = visible_nav_sections().collect();
    assert_eq!(visible, vec![0, 1, 2, 6, 7, 4, 5, 8]);
    let zh_labels: Vec<_> = visible
        .iter()
        .map(|index| section_label(*index, crate::display::UiLanguage::ZhCn))
        .collect();
    assert_eq!(zh_labels, vec!["应用", "外观", "终端", "交互", "按键映射", "SSH", "网络", "高级"]);
    let en_labels: Vec<_> = visible
        .iter()
        .map(|index| section_label(*index, crate::display::UiLanguage::EnUs))
        .collect();
    assert_eq!(
        en_labels,
        vec![
            "Application",
            "Appearance",
            "Terminal",
            "Interaction",
            "Key Bindings",
            "SSH",
            "Network",
            "Advanced",
        ]
    );
}

#[test]
fn localized_select_labels_keep_stable_value_cardinality() {
    let cases: &[(&str, &[&str])] = &[
        ("language", nebula_settings::LanguagePref::VALUES),
        ("cursor_shape", &["beam", "underline", "block", "hollow"]),
        ("tabs_position", &["sidebar", "top"]),
        ("bell", &["off", "visual", "sound", "both"]),
    ];
    for (key, values) in cases {
        for language in crate::display::UiLanguage::ALL {
            assert_eq!(localized_select_labels(key, values, *language).len(), values.len());
        }
    }
    assert_eq!(
        localized_select_labels("tabs_position", cases[2].1, crate::display::UiLanguage::EnUs),
        vec![SharedString::from("Left sidebar"), SharedString::from("Top")]
    );
}

#[test]
fn language_picker_uses_native_names_and_translated_system_option() {
    let labels = localized_select_labels(
        "language",
        nebula_settings::LanguagePref::VALUES,
        crate::display::UiLanguage::FrFr,
    );
    assert_eq!(labels[0], SharedString::from("Suivre le système"));
    assert!(labels.contains(&SharedString::from("Français")));
    assert!(labels.contains(&SharedString::from("日本語")));
}

#[test]
fn cached_semantic_statuses_render_in_the_current_language() {
    let provider = ProviderStatus::Saved;
    assert_eq!(provider.text(crate::display::UiLanguage::ZhCn), "供应商配置已保存");
    assert_eq!(provider.text(crate::display::UiLanguage::EnUs), "Provider settings saved");

    let backup = BackupStatus::CredentialSaved;
    assert_eq!(backup.text(crate::display::UiLanguage::ZhCn), "凭据已写入系统凭据管理器");
    assert_eq!(
        backup.text(crate::display::UiLanguage::EnUs),
        "Credential saved to the system credential manager"
    );

    let ssh = SshStatus::Opening("server.example".to_owned());
    assert_eq!(ssh.text(crate::display::UiLanguage::ZhCn), "正在打开 server.example…");
    assert_eq!(ssh.text(crate::display::UiLanguage::EnUs), "Opening server.example…");
}

/// Every row, including the import action, has the same icon slot height.
/// The virtual list measures its first visible item; filtering must not change
/// the row geometry when that item changes from an action to a shell.
#[cfg(feature = "gpui-test-support")]
mod shell_row_geometry {
    use super::*;
    use gpui::TestAppContext;

    struct ShellRowProbe {
        rows: Vec<(&'static str, ShellSelectItem)>,
    }

    impl Render for ShellRowProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // 把继承字号压到图标槽以下，让行高只由图标槽决定。这样断言测的
            // 就是「行高与图标种类无关」这条不变量本身，而不是某套字体度量。
            // 真实弹出列表与搜索另由 shell_picker_tests 覆盖。
            v_flex().w(px(240.0)).text_size(px(8.0)).children(self.rows.iter().map(
                |(selector, item)| {
                    div()
                        .debug_selector(move || (*selector).to_owned())
                        .child(item.render(window, cx))
                },
            ))
        }
    }

    #[gpui::test]
    fn every_row_shares_one_icon_slot_height(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let rows = vec![
            (
                "shell-probe-import",
                ShellSelectItem::import_action(crate::display::UiLanguage::ZhCn),
            ),
            (
                "shell-probe-brand",
                ShellSelectItem::new("pwsh".to_owned(), "PowerShell 7".to_owned(), 1.0),
            ),
            ("shell-probe-glyph", ShellSelectItem::new("zsh".to_owned(), "Zsh".to_owned(), 1.0)),
        ];
        let (_, cx) = cx.add_window_view(|_, _| ShellRowProbe { rows });
        let heights: Vec<f32> = ["shell-probe-import", "shell-probe-brand", "shell-probe-glyph"]
            .iter()
            .map(|selector| {
                f32::from(cx.debug_bounds(selector).expect("shell row bounds").size.height)
            })
            .collect();
        assert_eq!(heights[1], heights[2], "品牌图标行与字形回落行必须等高: {heights:?}");
        assert_eq!(heights[1], SHELL_ROW_ICON_SIZE, "普通行的图标槽即整行高度: {heights:?}");
        assert_eq!(
            heights[0], heights[1],
            "导入行和 Shell 行必须等高，搜索改变首行时不得改变行距: {heights:?}"
        );
    }
}
