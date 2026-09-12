//! Keep the commit draft while refreshing its placeholder after a language change.

use gpui::{App, AppContext as _, Entity, Window};
use gpui_component::input::InputState;

use crate::gpui_shell::config::ui_language;
use crate::i18n::{Message, UiLanguage};

pub(in crate::gpui_shell::workspace) struct CommitInput {
    pub(in crate::gpui_shell::workspace) input: Entity<InputState>,
    language: UiLanguage,
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests {
    use super::*;
    use gpui::{Element as _, IntoElement as _, Render, RenderOnce as _, div};
    use gpui_component::{ElementExt as _, input::Input};
    use std::sync::{Arc, Mutex};

    struct CommitInputProbe {
        commit: CommitInput,
        placeholder: Arc<Mutex<Option<String>>>,
    }

    impl Render for CommitInputProbe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            self.commit.sync_language(window, cx);
            let input = self.commit.input.clone();
            let placeholder = self.placeholder.clone();
            div().on_prepaint(move |_, window, cx| {
                let element = Input::new(&input).render(window, cx).into_element();
                let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::TextInput);
                element.write_a11y_info(&mut node);
                *placeholder.lock().unwrap() = node.placeholder().map(str::to_owned);
            })
        }
    }

    #[gpui::test]
    fn commit_placeholder_tracks_language_without_replacing_the_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let placeholder = Arc::new(Mutex::new(None));
        let captured = placeholder.clone();
        // The early-startup fallback is English even before Settings is registered.
        let (probe, cx) = cx.add_window_view(|window, cx| CommitInputProbe {
            commit: CommitInput::new(window, cx),
            placeholder,
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(captured.lock().unwrap().as_deref(), Some("Commit message..."));
        let input = probe.read_with(cx, |probe, _| probe.commit.input.clone());
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.set_value("fix: 保留 draft", window, cx);
                input.set_selected_range(0..3, cx);
            });
        });
        let original_id = input.entity_id();
        let original_cursor = input.read_with(cx, |input, _| input.cursor_position());
        let original_selection = input.read_with(cx, |input, _| input.selected_value());
        assert_eq!(original_selection, "fix");
        for (language, expected) in [
            (UiLanguage::ZhCn, "提交信息…"),
            (UiLanguage::for_locale(Some("en-GB")), "Commit message..."),
            (UiLanguage::FrFr, "Commit message..."),
            (UiLanguage::ZhCn, "提交信息…"),
        ] {
            cx.update(|window, cx| {
                let mut settings =
                    crate::gpui_shell::config::Settings::load(nebula_settings::ThemeName::Nord);
                settings.ui_language = language;
                cx.set_global(settings);
                probe.update(cx, |_, cx| cx.notify());
                let _ = window.draw(cx);
            });
            assert_eq!(captured.lock().unwrap().as_deref(), Some(expected));
            assert_eq!(probe.read_with(cx, |probe, _| probe.commit.input.entity_id()), original_id);
            assert_eq!(input.read_with(cx, |input, _| input.value()), "fix: 保留 draft");
            assert_eq!(input.read_with(cx, |input, _| input.cursor_position()), original_cursor);
            assert_eq!(input.read_with(cx, |input, _| input.selected_value()), original_selection);
        }
    }
}

impl CommitInput {
    pub(in crate::gpui_shell::workspace) fn new(window: &mut Window, cx: &mut App) -> Self {
        let language = ui_language(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(language.text(Message::VcsCommitPlaceholder))
        });
        Self { input, language }
    }

    pub(super) fn sync_language(&mut self, window: &mut Window, cx: &mut App) {
        let language = ui_language(cx);
        if self.language == language {
            return;
        }
        self.input.update(cx, |input, cx| {
            input.set_placeholder(language.text(Message::VcsCommitPlaceholder), window, cx);
        });
        self.language = language;
    }
}
