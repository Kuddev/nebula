//! macOS menu bar: native presentation of the existing workspace actions.
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};
use gpui_component::input;

use super::{config, workspace::*};
use crate::i18n::{Message, UiLanguage};

gpui::actions!(native_menu, [Quit, Hide, HideOthers, ShowAll]);

pub(super) fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // Inputs/editors consume these first. Only unhandled actions reach the terminal.
    cx.on_action(|_: &input::Copy, cx| cx.defer(|cx| cx.dispatch_action(&CopySelection)));
    cx.on_action(|_: &input::Paste, cx| cx.defer(|cx| cx.dispatch_action(&PasteClipboard)));
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
    ]);
    let mut language = config::ui_language(cx);
    cx.set_menus(menus(language));
    cx.observe_global::<config::Settings>(move |cx| {
        let next = config::ui_language(cx);
        if language != next {
            language = next;
            cx.set_menus(menus(language));
        }
    })
    .detach();
}

fn menus(language: UiLanguage) -> Vec<Menu> {
    let text = |message| language.text(message);
    vec![
        Menu::new("Pebrel").items([
            MenuItem::action(text(Message::CommonSettings), OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu(text(Message::MenuServices), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuHide), Hide),
            MenuItem::action(text(Message::MenuHideOthers), HideOthers),
            MenuItem::action(text(Message::MenuShowAll), ShowAll),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuQuit), Quit),
        ]),
        Menu::new(text(Message::MenuFile)).items([
            MenuItem::action(text(Message::CommonNewWindow), NewWindow),
            MenuItem::action(text(Message::CommonNewTab), NewTerminal),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuSplitRight), SplitRight),
            MenuItem::action(text(Message::MenuSplitDown), SplitDown),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuClosePane), CloseActiveTerminal),
        ]),
        Menu::new(text(Message::CommonEdit)).items([
            MenuItem::os_action(text(Message::CommonCut), input::Cut, OsAction::Cut),
            MenuItem::os_action(text(Message::CommonCopy), input::Copy, OsAction::Copy),
            MenuItem::os_action(text(Message::CommonPaste), input::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action(text(Message::CommonSelectAll), input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new(text(Message::MenuView)).items([
            MenuItem::action(text(Message::MenuCommandPalette), ToggleCommandPalette),
            MenuItem::action(text(Message::MenuToggleSidebar), ToggleSidebar),
            MenuItem::action(text(Message::MenuToggleFiles), ToggleFileTree),
            MenuItem::action(text(Message::MenuToggleGit), ToggleGitPanel),
            MenuItem::separator(),
            MenuItem::action(text(Message::CommonIncreaseFont), IncreaseFontSize),
            MenuItem::action(text(Message::CommonDecreaseFont), DecreaseFontSize),
            MenuItem::action(text(Message::MenuResetFont), ResetFontSize),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuToggleFullscreen), ToggleFullscreen),
        ]),
        Menu::new(text(Message::MenuWindow)).items([
            MenuItem::action(text(Message::MenuNextTab), SelectNextTab),
            MenuItem::action(text(Message::MenuPreviousTab), SelectPreviousTab),
            MenuItem::separator(),
            MenuItem::action(text(Message::MenuZoomPane), ToggleZoom),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_titles_follow_the_selected_language() {
        let english = menus(UiLanguage::EnUs);
        let chinese = menus(UiLanguage::ZhCn);
        assert_eq!(english[1].name.as_ref(), "File");
        assert_eq!(chinese[1].name.as_ref(), "文件");
        assert_eq!(english.len(), chinese.len());
        assert!(english.iter().all(|menu| !menu.items.is_empty()));
    }
}
