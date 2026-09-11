use super::*;

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

/// 回归锁：默认 Shell 下拉里，真实 shell 行共用同一个图标槽高度，只有置顶的
/// 「导入终端目录」动作行比它们多出一个行距。
///
/// `gpui-component` 的 `List` 只量**首行**的槽高，再拿这个高度排所有行，而它
/// 的虚拟列表又不支持条目 `gap_y`。所以两个不变量要一起锁住：品牌 PNG 行和
/// 字形回落行必须等高（否则高行溢出槽位、压住相邻行），动作行必须恰好高出
/// 一个 `SHELL_ROW_GAP`（否则行与行之间没有间隔）。
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
            // 走真实的 `SelectItem::render`，连带把动作行的行距一起量进去。
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
            heights[0],
            heights[1] + SHELL_ROW_GAP,
            "动作行必须恰好比普通行多一个行距，这个差值就是所有行的间距: {heights:?}"
        );
    }
}
