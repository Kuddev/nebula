use super::*;
use gpui::Focusable as _;

const EDITOR_WIDTH: f32 = 512.0;
const EDITOR_HEIGHT: f32 = 720.0;
const EDITOR_PADDING: f32 = 28.0;

pub(super) fn editor_input(
    state: &Entity<InputState>,
    label: &'static str,
    window: &Window,
    cx: &gpui::App,
) -> Input {
    let theme = cx.theme();
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    gpui::Styled::h(Input::new(state), px(SSH_EDITOR_CTL_H))
        .text_size(px(13.0))
        .px(px(12.0))
        .rounded(px(7.0))
        .bg(theme.popover)
        .focus_bordered(false)
        .border_color(if focused { theme.ring.opacity(0.8) } else { theme.input })
        .when(focused, |input| {
            input.shadow(vec![gpui::BoxShadow {
                inset: false,
                color: theme.ring.opacity(0.13),
                offset: gpui::point(px(0.0), px(0.0)),
                blur_radius: px(0.0),
                spread_radius: px(3.0),
            }])
        })
        .aria_label(label)
}

pub(super) fn editor_field(label: &'static str, control: impl IntoElement) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(7.0))
        .child(div().text_xs().font_medium().child(label))
        .child(control)
}

pub(super) fn editor_hint(text: impl Into<SharedString>, cx: &gpui::App) -> gpui::Div {
    div()
        .text_xs()
        .line_height(gpui::relative(1.6))
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

impl SettingsPane {
    pub(in crate::gpui_shell) fn ssh_editor_modal(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let language = crate::gpui_shell::config::ui_language(cx);
        let editor = self.ssh_editor.as_ref()?.clone();
        let icon = crate::display::ui::os_icons::resolve(editor.icon.as_deref());
        let avatar = self.ssh_avatar(icon, cx);
        let icon_popup = self.ssh_icon_popup(cx);
        let username_popup = self.ssh_username_popup(cx);
        let content = if editor.advanced {
            self.ssh_editor_advanced(window, cx)
        } else {
            self.ssh_editor_basic(window, cx)
        };
        let theme = cx.theme();
        let title = if editor.original_destination.is_some() {
            language.pick("编辑 SSH 主机", "Edit SSH host")
        } else {
            language.pick("添加 SSH 主机", "Add SSH host")
        };
        let status = match self.ssh_status.as_ref() {
            Some(SshStatus::Validation(_)) => None,
            Some(status) => Some((status.text(language), status.is_error())),
            None => {
                editor.test_status.as_ref().map(|status| (status.text(language), status.is_error()))
            },
        };
        let status_connecting = self.ssh_status.is_none() && editor.testing();
        let page_tab = |id, label, advanced| {
            let selected = editor.advanced == advanced;
            Button::new(id)
                .label(label)
                .ghost()
                .small()
                .h(px(38.0))
                .px_1()
                .rounded_none()
                .text_color(if selected { theme.foreground } else { theme.muted_foreground })
                .when(selected, |button| {
                    button.font_medium().border_b_2().border_color(theme.primary)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(editor) = this.ssh_editor.as_mut() {
                        editor.advanced = advanced;
                        editor.jump_picker_open = false;
                    }
                    this.ssh_username_picker_open = false;
                    this.ssh_icon_picker_open = false;
                    window.focus(&this.ssh_editor_focus_handle, cx);
                    cx.notify();
                }))
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .track_focus(&self.ssh_editor_focus_handle)
                .occlude()
                .bg(theme.overlay)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        if this.ssh_icon_picker_open {
                            this.toggle_ssh_icon_picker(window, cx);
                        } else if this.ssh_username_picker_open {
                            this.ssh_username_picker_open = false;
                            cx.notify();
                        }
                    }),
                )
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if !event.keystroke.key.eq_ignore_ascii_case("escape") {
                        return;
                    }
                    cx.stop_propagation();
                    if this.ssh_icon_picker_open {
                        this.toggle_ssh_icon_picker(window, cx);
                    } else if this.ssh_username_picker_open {
                        this.ssh_username_picker_open = false;
                        cx.notify();
                    } else if this.ssh_editor.as_ref().is_some_and(|editor| editor.jump_picker_open)
                    {
                        if let Some(editor) = this.ssh_editor.as_mut() {
                            editor.jump_picker_open = false;
                        }
                        cx.notify();
                    } else {
                        this.close_ssh_editor(window, cx);
                    }
                }))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(20.0))
                        .child(
                            v_flex()
                                .id("ssh-editor-dialog")
                                .debug_selector(|| "ssh-editor-dialog".to_owned())
                                .w(px(EDITOR_WIDTH))
                                .h(px(EDITOR_HEIGHT))
                                .max_w(gpui::relative(1.0))
                                .max_h(gpui::relative(1.0))
                                .flex_none()
                                .rounded(px(13.0))
                                .border_1()
                                .border_color(theme.border.opacity(0.7))
                                .bg(theme.popover)
                                .text_color(theme.foreground)
                                .shadow_lg()
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .h(px(94.0))
                                        .flex_shrink_0()
                                        .px(px(EDITOR_PADDING))
                                        .gap_3()
                                        .items_center()
                                        .child(avatar)
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_size(px(19.0))
                                                        .font_semibold()
                                                        .child(title),
                                                )
                                                .child(editor_hint(
                                                    language.pick(
                                                        "配置连接信息，随时快速访问。",
                                                        "Save connection details for quick access.",
                                                    ),
                                                    cx,
                                                )),
                                        )
                                        .child(
                                            Button::new("ssh-editor-close")
                                                .icon(IconName::Close)
                                                .ghost()
                                                .small()
                                                .tooltip(language.pick("关闭", "Close"))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.close_ssh_editor(window, cx);
                                                })),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .h(px(39.0))
                                        .flex_shrink_0()
                                        .px(px(EDITOR_PADDING))
                                        .gap_6()
                                        .border_b_1()
                                        .border_color(theme.border)
                                        .child(page_tab(
                                            "ssh-editor-basic",
                                            language.pick("基本", "General"),
                                            false,
                                        ))
                                        .child(page_tab(
                                            "ssh-editor-advanced",
                                            language.pick("高级", "Advanced"),
                                            true,
                                        )),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scrollbar()
                                        .px(px(EDITOR_PADDING))
                                        .py_5()
                                        .child(content)
                                        .when_some(status, |body, (message, error)| {
                                            let color = if error {
                                                theme.danger
                                            } else if status_connecting {
                                                theme.muted_foreground
                                            } else {
                                                theme.success
                                            };
                                            body.child(
                                                h_flex()
                                                    .mt_4()
                                                    .p_3()
                                                    .gap_2()
                                                    .items_start()
                                                    .rounded_md()
                                                    .bg(color.opacity(0.08))
                                                    .text_color(color)
                                                    .child(if status_connecting {
                                                        Spinner::new().small().into_any_element()
                                                    } else {
                                                        Icon::new(if error {
                                                            IconName::CircleX
                                                        } else {
                                                            IconName::CircleCheck
                                                        })
                                                        .xsmall()
                                                        .into_any_element()
                                                    })
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .text_xs()
                                                            .child(message),
                                                    ),
                                            )
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .h(px(71.0))
                                        .flex_shrink_0()
                                        .px(px(EDITOR_PADDING))
                                        .items_center()
                                        .justify_between()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .bg(theme.muted)
                                        .rounded_b(px(13.0))
                                        .child(
                                            Button::new("ssh-editor-test")
                                                .debug_selector(|| "ssh-editor-test".to_owned())
                                                .label(if editor.testing() {
                                                    language.pick("测试中…", "Testing...")
                                                } else {
                                                    language.pick("测试连接", "Test connection")
                                                })
                                                .ghost()
                                                .small()
                                                .loading(editor.testing())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.test_ssh_editor(cx)
                                                })),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    Button::new("ssh-editor-cancel")
                                                        .label(language.pick("取消", "Cancel"))
                                                        .ghost()
                                                        .small()
                                                        .h(px(35.0))
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.close_ssh_editor(window, cx)
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new("ssh-editor-save")
                                                        .debug_selector(|| {
                                                            "ssh-editor-save".to_owned()
                                                        })
                                                        .label(language.pick("保存", "Save"))
                                                        .primary()
                                                        .small()
                                                        .h(px(35.0))
                                                        .min_w(px(74.0))
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.save_ssh_editor(window, cx)
                                                            },
                                                        )),
                                                ),
                                        ),
                                ),
                        ),
                )
                .children(icon_popup)
                .children(username_popup)
                .into_any_element(),
        )
    }

    fn ssh_editor_basic(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let username = self.ssh_username_control(window, cx);
        let authentication = self.ssh_editor_authentication(window, cx);
        let theme = cx.theme();
        let validation = self.ssh_status.as_ref().and_then(|status| match status {
            SshStatus::Validation(error) => Some(*error),
            _ => None,
        });
        let port_error = matches!(validation, Some(SshValidationError::InvalidPort));
        let address_error = validation.is_some() && !port_error;
        let destination = self.ssh_destination_from_draft(cx).ok();
        v_flex()
            .w_full()
            .child(
                v_flex()
                    .gap(px(7.0))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .child(language.pick("主机名称", "Host name")),
                            )
                            .child(editor_hint(language.pick("可选", "Optional"), cx)),
                    )
                    .child(editor_input(
                        &self.ssh_label_input,
                        language.pick("主机名称", "Host name"),
                        window,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .mt_5()
                    .gap_3()
                    .items_start()
                    .child(
                        div().flex_1().min_w_0().child(editor_field(
                            language.pick("主机地址 *", "Host address *"),
                            editor_input(
                                &self.ssh_destination_input,
                                language.pick("主机地址", "Host address"),
                                window,
                                cx,
                            )
                            .when(address_error, |input| input.border_color(theme.danger)),
                        )),
                    )
                    .child(
                        div().w(px(84.0)).flex_shrink_0().child(editor_field(
                            language.pick("端口", "Port"),
                            editor_input(
                                &self.ssh_port_input,
                                language.pick("端口", "Port"),
                                window,
                                cx,
                            )
                            .when(port_error, |input| input.border_color(theme.danger)),
                        )),
                    ),
            )
            .child(editor_hint(language.tr("settings.ssh.address_hint"), cx).mt_1())
            .when_some(validation, |body, error| {
                body.child(
                    div().mt_1().text_xs().text_color(theme.danger).child(error.text(language)),
                )
            })
            .child(div().mt_4().child(editor_field(language.pick("用户名", "Username"), username)))
            .when_some(destination, |body, destination| {
                body.child(editor_hint(destination, cx).mt_2().truncate())
            })
            .child(
                v_flex()
                    .mt_5()
                    .pt_5()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(authentication),
            )
    }

    fn ssh_editor_authentication(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        use crate::ssh_profiles::SshAuthMode;

        let language = crate::gpui_shell::config::ui_language(cx);
        let editor = self.ssh_editor.as_ref().expect("open SSH editor");
        let theme = cx.theme();
        let (shows_password, shows_keys) = crate::display::auth_sections(editor.auth);
        let modes = [
            ("ssh-auth-password", language.pick("密码", "Password"), SshAuthMode::Password),
            ("ssh-auth-key", language.pick("密钥", "Key"), SshAuthMode::PublicKey),
            ("ssh-auth-auto", language.pick("自动", "Auto"), SshAuthMode::Auto),
            (
                "ssh-auth-interactive",
                language.pick("交互式", "Interactive"),
                SshAuthMode::KeyboardInteractive,
            ),
        ];
        let controls = modes.into_iter().map(|(id, label, mode)| {
            Button::new(id)
                .label(label)
                .ghost()
                .small()
                .flex_1()
                .min_w_0()
                .h(px(31.0))
                .rounded(gpui_component::button::ButtonRounded::Size(px(5.0)))
                .px_1()
                .toggled(editor.auth == mode)
                .when(editor.auth == mode, |button| {
                    button.bg(theme.popover).font_medium().shadow_sm()
                })
                .when(editor.auth != mode, |button| button.text_color(theme.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.ssh_editor.as_mut() {
                        editor.auth = mode;
                    }
                    this.touch_ssh_editor(cx);
                }))
        });
        v_flex()
            .w_full()
            .child(div().text_xs().font_medium().child(language.pick("身份验证", "Authentication")))
            .child(
                h_flex()
                    .mt_2()
                    .p(px(3.0))
                    .gap(px(3.0))
                    .rounded(px(8.0))
                    .bg(theme.muted)
                    .children(controls),
            )
            .when(shows_password, |body| {
                body.child(
                    div().mt_4().child(editor_field(
                        language.pick("密码", "Password"),
                        editor_input(
                            &self.ssh_password_input,
                            language.pick("密码", "Password"),
                            window,
                            cx,
                        )
                        .mask_toggle(),
                    )),
                )
                .child(
                    div().mt_3().child(
                        Checkbox::new("ssh-save-password")
                            .small()
                            .disabled(!crate::platform::credentials::can_store())
                            .checked(editor.save_password)
                            .label(if cfg!(windows) {
                                language.pick(
                                    "保存到 Windows 凭据管理器",
                                    "Save in Windows Credential Manager",
                                )
                            } else {
                                language.pick("保存到系统凭据库", "Save in system credential store")
                            })
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                if let Some(editor) = this.ssh_editor.as_mut() {
                                    editor.save_password = *value;
                                }
                                this.touch_ssh_editor(cx);
                            })),
                    ),
                )
            })
            .when(shows_keys, |body| {
                let rows = editor.private_keys.iter().enumerate().map(|(index, path)| {
                    h_flex()
                        .w_full()
                        .h(px(32.0))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .truncate()
                                .child(ssh_key_path_tail(path, 64)),
                        )
                        .child(
                            Button::new(SharedString::from(format!("ssh-key-remove-{index}")))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip(language.pick("移除私钥", "Remove private key"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(editor) = this.ssh_editor.as_mut() {
                                        if index < editor.private_keys.len() {
                                            editor.private_keys.remove(index);
                                        }
                                    }
                                    this.touch_ssh_editor(cx);
                                })),
                        )
                });
                body.child(
                    v_flex()
                        .mt_4()
                        .gap_2()
                        .when(!editor.private_keys.is_empty(), |list| {
                            list.child(
                                v_flex()
                                    .h(px((editor.private_keys.len() as f32 * 38.0).min(140.0)))
                                    .overflow_y_scrollbar()
                                    .child(v_flex().gap(px(6.0)).children(rows)),
                            )
                        })
                        .child(
                            Button::new("ssh-add-private-key")
                                .label(language.pick("选择私钥文件", "Choose private key"))
                                .icon(IconName::Plus)
                                .outline()
                                .small()
                                .w_full()
                                .h(px(40.0))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_ssh_private_key(window, cx)
                                })),
                        )
                        .child(editor_hint(language.tr("settings.ssh.default_keys"), cx)),
                )
            })
            .when(!shows_password && !shows_keys, |body| {
                body.child(
                    editor_hint(
                        language.tr(if editor.auth == SshAuthMode::Auto {
                            "settings.ssh.auth_auto_description"
                        } else {
                            "settings.ssh.auth_interactive_description"
                        }),
                        cx,
                    )
                    .mt_4(),
                )
            })
    }
}
