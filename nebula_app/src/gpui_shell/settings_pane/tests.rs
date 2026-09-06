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
