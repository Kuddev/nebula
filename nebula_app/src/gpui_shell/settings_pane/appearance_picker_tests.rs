use super::appearance_picker::{AppearanceSelection, picker_columns, picker_next_index};
use super::*;
use nebula_settings::AppIconName;

#[test]
fn appearance_theme_catalog_matches_the_approved_gallery_order() {
    let themes = AppearanceSelection::Theme(ThemeName::SilverLight);
    let labels: Vec<_> = themes
        .choices(0)
        .into_iter()
        .map(|choice| choice.label(crate::display::UiLanguage::EnUs))
        .collect();
    assert_eq!(
        labels,
        [
            "Breeze Light",
            "Breeze Dark",
            "Mint Light",
            "Mint Dark",
            "Silver",
            "Nord",
            "Paper",
            "Limestone",
            "Linen",
            "Catppuccin Mocha",
            "Catppuccin Latte",
            "Catppuccin Frappé",
            "Catppuccin Macchiato",
            "Glass Light",
            "Glass Dark"
        ]
    );
    assert_eq!(themes.choices(1).len(), 8);
    assert_eq!(themes.choices(2).len(), 7);
    for choice in themes.choices(0) {
        assert_ne!(themes.choices(1).contains(&choice), themes.choices(2).contains(&choice));
    }
}

#[test]
fn appearance_icon_filters_cover_each_catalog_entry_once() {
    let icons = AppearanceSelection::Icon(AppIconName::Titanium);
    assert_eq!(icons.choices(0).len(), 25);
    assert_eq!((1..=4).map(|filter| icons.choices(filter).len()).collect::<Vec<_>>(), [4, 8, 5, 8]);
    for choice in icons.choices(0) {
        assert_eq!((1..=4).filter(|filter| icons.choices(*filter).contains(&choice)).count(), 1);
    }
}

#[test]
fn appearance_theme_confirmation_preserves_icon_and_other_settings() {
    let initial = "theme=Nebula\nfollow_system_theme=1\napp_icon=graphite-violet\nbackground=#111111\nfont_size=17\nshell=cmd\n";
    for theme in
        [ThemeName::LinenLight, ThemeName::CatppuccinFrappe, ThemeName::CatppuccinMacchiato]
    {
        let updates = AppearanceSelection::Theme(theme).updates();
        let updated = nebula_settings::apply_updates(initial, &updates);
        let runtime = RuntimeSettings::from_raw(&nebula_settings::RawSettings::from_text(&updated));
        assert_eq!(runtime.theme, theme);
        assert!(!runtime.follow_system_theme);
        assert_eq!(runtime.background, Some(theme.term_theme().background));
        assert_eq!(runtime.app_icon, AppIconName::GraphiteViolet);
        assert!(updated.contains("font_size=17"));
        assert!(updated.contains("shell=cmd"));
    }
}

#[test]
fn catppuccin_system_appearance_restores_the_saved_dark_flavor() {
    use crate::gpui_shell::theme::resolve_theme_name;

    for theme in
        [ThemeName::CatppuccinMocha, ThemeName::CatppuccinFrappe, ThemeName::CatppuccinMacchiato]
    {
        let initial = format!("theme={}\nfollow_system_theme=1\n", theme.prompt_name());
        let runtime = RuntimeSettings::from_raw(&nebula_settings::RawSettings::from_text(&initial));
        for is_light in [false, true, false] {
            assert_eq!(
                resolve_theme_name(runtime.theme, runtime.follow_system_theme, is_light),
                if is_light { ThemeName::CatppuccinLatte } else { theme }
            );
            assert_eq!(runtime.theme, theme);
        }
        assert_eq!(resolve_theme_name(runtime.theme, false, true), theme);
    }
    assert_eq!(
        resolve_theme_name(ThemeName::CatppuccinLatte, true, false),
        ThemeName::CatppuccinMocha
    );
}

#[test]
fn appearance_icon_confirmation_never_changes_the_theme() {
    let initial = "theme=SteelDark\nfollow_system_theme=1\nbackground=#123456\nfont_size=17\n";
    let updates = AppearanceSelection::Icon(AppIconName::SilverViolet).updates();
    assert_eq!(updates, vec![("app_icon", "silver-violet".to_owned())]);
    let updated = nebula_settings::apply_updates(initial, &updates);
    let runtime = RuntimeSettings::from_raw(&nebula_settings::RawSettings::from_text(&updated));
    assert_eq!(runtime.theme, ThemeName::SteelDark);
    assert!(runtime.follow_system_theme);
    assert_eq!(runtime.background, Some([0x12, 0x34, 0x56]));
    assert_eq!(runtime.app_icon, AppIconName::SilverViolet);
}

#[test]
fn appearance_grid_keyboard_navigation_wraps_and_respects_columns() {
    assert_eq!(picker_next_index("right", 24, 25, 5), Some(0));
    assert_eq!(picker_next_index("left", 0, 25, 5), Some(24));
    assert_eq!(picker_next_index("up", 0, 25, 5), Some(20));
    assert_eq!(picker_next_index("down", 21, 25, 5), Some(1));
    assert_eq!(picker_next_index("down", 0, 9, 3), Some(3));
    assert_eq!(picker_next_index("down", 0, 9, 2), Some(2));
    assert_eq!(picker_next_index("up", 0, 1, 5), Some(0));
    assert_eq!(picker_next_index("home", 8, 9, 3), Some(0));
    assert_eq!(picker_next_index("end", 0, 9, 3), Some(8));
    assert_eq!(picker_next_index("enter", 0, 9, 3), None);
    assert_eq!(picker_next_index("right", 0, 0, 5), None);
}

#[test]
fn appearance_grid_columns_follow_the_html_breakpoint() {
    assert_eq!(picker_columns(true, 1440.0), 3);
    assert_eq!(picker_columns(true, 390.0), 2);
    assert_eq!(picker_columns(false, 1440.0), 5);
    assert_eq!(picker_columns(false, 390.0), 4);
}
