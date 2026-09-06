use super::{LanguagePreference, Message, UiLanguage};

#[test]
fn language_metadata_and_settings_values_remain_in_sync() {
    assert_eq!(UiLanguage::ALL.len(), nebula_settings::LanguagePref::LANGUAGES.len());
    for preference in LanguagePreference::ALL {
        assert_eq!(LanguagePreference::parse(preference.as_str()), Some(*preference));
        if let Some(language) = preference.explicit() {
            assert_eq!(language.code(), preference.as_str());
            assert_eq!(preference.resolved(), language);
        }
    }
}

#[test]
fn negotiates_locale_and_falls_back_to_english() {
    assert_eq!(UiLanguage::for_locale(Some("fr_CA.UTF-8")), UiLanguage::FrFr);
    assert_eq!(UiLanguage::for_locale(Some("zh-Hant-HK")), UiLanguage::ZhTw);
    assert_eq!(UiLanguage::for_locale(Some("unsupported")), UiLanguage::EnUs);
    assert_eq!(UiLanguage::for_locale(None), UiLanguage::EnUs);
}

#[test]
fn typed_and_compatibility_lookups_agree() {
    assert_eq!(UiLanguage::FrFr.text(Message::SettingsSidebarNetwork), "Réseau");
    assert_eq!(UiLanguage::JaJp.tr("settings.sidebar.network"), "ネットワーク");
    for language in UiLanguage::ALL {
        assert_eq!(
            language.tr("settings.sidebar.network"),
            language.text(Message::SettingsSidebarNetwork)
        );
        assert_eq!(language.tr("missing.message.id"), "missing.message.id");
    }
}

#[test]
fn inline_migration_preserves_bilingual_text_and_english_fallback() {
    assert_eq!(UiLanguage::ZhCn.pick("网络", "Network"), "网络");
    assert_eq!(UiLanguage::EnUs.pick("网络", "Network"), "Network");
    assert_eq!(UiLanguage::FrFr.pick("网络", "Network"), "Réseau");
    assert_eq!(UiLanguage::FrFr.pick("未迁移文案", "Unmigrated text"), "Unmigrated text");
}

#[test]
fn arguments_are_localized_without_losing_placeholders() {
    assert_eq!(
        UiLanguage::EnUs.tr_args("provider.test.success", &[("status", "200")]),
        "Connection succeeded (HTTP 200)"
    );
    assert_eq!(
        UiLanguage::FrFr.tr_args("provider.test.success", &[("status", "200")]),
        "Connexion réussie (HTTP 200)"
    );
    assert_eq!(
        UiLanguage::FrFr.format(Message::ProviderTestSuccess, &[("status", "200")]),
        "Connexion réussie (HTTP 200)"
    );
}

#[test]
#[ignore = "manual static translation microbenchmark"]
fn measure_static_catalog_costs() {
    use std::hint::black_box;
    use std::time::Instant;

    let count = 2_000_000u128;
    let started = Instant::now();
    for _ in 0..count {
        black_box(black_box(UiLanguage::FrFr).text(black_box(Message::SettingsSidebarNetwork)));
    }
    eprintln!(
        "typed lookup: {} ns/op; {} messages; {} locales; {} translated bytes",
        started.elapsed().as_nanos() / count,
        super::MESSAGE_COUNT,
        UiLanguage::ALL.len(),
        super::TRANSLATED_BYTES
    );
    let started = Instant::now();
    for _ in 0..count {
        black_box(black_box(UiLanguage::FrFr).tr(black_box("settings.sidebar.network")));
    }
    eprintln!("key lookup: {} ns/op", started.elapsed().as_nanos() / count);
}
