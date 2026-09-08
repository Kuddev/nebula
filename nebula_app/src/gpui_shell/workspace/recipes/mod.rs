//! Save and restore named window/tab layouts, using the existing session restore authority.

mod store;

use super::*;
use crate::i18n::Message;
use crate::session::Session;
use gpui::EventEmitter;

gpui::actions!(layout_recipes, [OpenLayoutRecipes]);

pub(super) fn palette_row(language: crate::display::UiLanguage) -> WorkspacePaletteRow {
    WorkspacePaletteRow {
        group_order: 2,
        group: language.text(Message::RecipeTitle).to_owned(),
        label: language.text(Message::RecipeOpenLibrary).to_owned(),
        hint: "Ctrl+Alt+Shift+S".into(),
        hint_style: WorkspacePaletteHintStyle::Shortcut,
        search: "recipe layout save restore 布局 配方 保存 恢复".into(),
        action: WorkspacePaletteAction::LayoutRecipes,
        icon: None,
        icon_glyph: None,
        icon_path: None,
    }
}

impl NebulaWorkspace {
    pub(super) fn open_layout_recipes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot_session(cx);
        let tab = self.tabs.get(self.active).filter(|tab| tab.is_terminal()).and_then(|_| {
            let index = self.tabs.iter().take(self.active).filter(|tab| tab.is_terminal()).count();
            snapshot.tabs.get(index).cloned().map(|tab| Session::new(0, vec![tab]))
        });
        let library = cx.new(|cx| RecipeLibrary::new(snapshot, tab, window, cx));
        cx.subscribe_in(&library, window, |_, _, session: &Session, window, cx| {
            window.close_dialog(cx);
            windowing::open_recipe_window(session.clone(), cx);
        })
        .detach();
        let language = crate::gpui_shell::config::ui_language(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(language.text(Message::RecipeTitle)).w(px(600.0)).child(library.clone())
        });
    }
}

struct RecipeLibrary {
    name: Entity<InputState>,
    window_snapshot: Session,
    tab_snapshot: Option<Session>,
    current_tab: bool,
    entries: Vec<store::Recipe>,
    busy: bool,
    error: Option<String>,
}

impl EventEmitter<Session> for RecipeLibrary {}

impl RecipeLibrary {
    fn new(
        window_snapshot: Session,
        tab_snapshot: Option<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = crate::gpui_shell::config::ui_language(cx);
        let name = cx
            .new(|cx| InputState::new(window, cx).placeholder(language.text(Message::RecipeName)));
        let mut this = Self {
            name,
            window_snapshot,
            tab_snapshot,
            current_tab: false,
            entries: vec![],
            busy: false,
            error: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let task = cx.background_executor().spawn(async { store::list_at(&store::directory()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(entries) => this.entries = entries,
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let session = if self.current_tab {
            self.tab_snapshot.clone()
        } else {
            Some(self.window_snapshot.clone())
        };
        let Some(session) = session else { return };
        let recipe = match store::Recipe::new(self.name.read(cx).value().to_string(), session) {
            Ok(recipe) => recipe,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            },
        };
        self.busy = true;
        self.error = None;
        let task = cx
            .background_executor()
            .spawn(async move { store::save_at(&store::directory(), &recipe) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.refresh(cx),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn delete(&mut self, recipe: store::Recipe, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let language = crate::gpui_shell::config::ui_language(cx);
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            language.text(Message::RecipeDeleteTitle),
            Some(&recipe.name),
            &[language.text(Message::EditorCancel), language.text(Message::RecipeDelete)],
            cx,
        );
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let result = executor.spawn(async move { std::fs::remove_file(recipe.path) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.refresh(cx),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for RecipeLibrary {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let entries = self.entries.clone();
        v_flex()
            .w_full()
            .gap_3()
            .child(
                div().text_sm().text_color(muted).child(language.text(Message::RecipeDescription)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("recipe-scope-window")
                            .small()
                            .ghost()
                            .selected(!self.current_tab)
                            .label(language.text(Message::RecipeWindow))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("recipe-scope-tab")
                            .small()
                            .ghost()
                            .selected(self.current_tab)
                            .disabled(self.tab_snapshot.is_none())
                            .label(language.text(Message::RecipeTab))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = true;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.name).w_full()))
                    .child(
                        Button::new("recipe-save")
                            .label(language.text(Message::RecipeSave))
                            .disabled(self.busy || self.window_snapshot.tabs.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .when_some(self.error.clone(), |root, error| {
                root.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(language.format(Message::RecipeFailed, &[("error", &error)])),
                )
            })
            .child(
                v_flex()
                    .id("recipe-list")
                    .w_full()
                    .h(px(300.0))
                    .overflow_y_scroll()
                    .gap_2()
                    .when(self.busy, |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(language.text(Message::RecipeLoading)),
                        )
                    })
                    .when(!self.busy && entries.is_empty(), |list| {
                        list.child(
                            div()
                                .py_5()
                                .text_sm()
                                .text_color(muted)
                                .child(language.text(Message::RecipeEmpty)),
                        )
                    })
                    .children(entries.into_iter().enumerate().map(|(index, recipe)| {
                        let open = recipe.session.clone();
                        h_flex()
                            .w_full()
                            .gap_2()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_sm().truncate().child(recipe.name.clone()))
                                    .child(div().text_xs().text_color(muted).child(
                                        language.format(
                                            Message::RecipeCount,
                                            &[
                                                ("tabs", &recipe.session.tabs.len().to_string()),
                                                ("panes", &recipe.panes().to_string()),
                                            ],
                                        ),
                                    )),
                            )
                            .child(
                                Button::new(("recipe-open", index))
                                    .small()
                                    .label(language.text(Message::RecipeOpen))
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(move |_, _, _, cx| cx.emit(open.clone())),
                                    ),
                            )
                            .child(
                                Button::new(("recipe-delete", index))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Delete)
                                    .disabled(self.busy)
                                    .tooltip(language.text(Message::RecipeDelete))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.delete(recipe.clone(), window, cx)
                                    })),
                            )
                    })),
            )
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;
    #[test]
    fn layout_shortcut_preserves_the_existing_split_shortcut() {
        let keymap = gpui::Keymap::new(super::super::default_workspace_bindings());
        let contexts = [gpui::KeyContext::parse("Root").unwrap()];
        let (split, _) = keymap
            .bindings_for_input(&[gpui::Keystroke::parse("ctrl-shift-s").unwrap()], &contexts);
        assert!(split[0].action().as_any().is::<super::super::SplitDown>());
        let (recipe, _) = keymap
            .bindings_for_input(&[gpui::Keystroke::parse("ctrl-alt-shift-s").unwrap()], &contexts);
        assert!(recipe[0].action().as_any().is::<OpenLayoutRecipes>());
    }
}
