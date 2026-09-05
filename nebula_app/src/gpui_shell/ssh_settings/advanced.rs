use super::editor::{editor_field, editor_hint, editor_input};
use super::*;
use crate::ssh_profiles::{SshConnectionOptions, SshHostJumpMode, SshHostProxyMode};

fn same_proxy_endpoint(previous: &SshConnectionOptions, connection: &SshConnectionOptions) -> bool {
    previous.proxy_mode == connection.proxy_mode
        && previous.normalized_proxy_host() == connection.normalized_proxy_host()
        && previous.effective_proxy_port() == connection.effective_proxy_port()
        && previous.proxy_username.trim() == connection.proxy_username.trim()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    fn proxy() -> SshConnectionOptions {
        SshConnectionOptions {
            proxy_mode: SshHostProxyMode::Socks5,
            proxy_host: "proxy.example".into(),
            proxy_username: "alice".into(),
            ..Default::default()
        }
    }

    #[test]
    fn ssh_proxy_rename_preserves_credentials_for_equivalent_endpoints() {
        let previous = proxy();
        let mut changed = previous.clone();
        changed.proxy_host = "PROXY.EXAMPLE".into();
        changed.proxy_username = " alice ".into();
        changed.proxy_port = Some(1080);
        let old_key = previous.proxy_credential_target("old-host").unwrap();
        let new_key = changed.proxy_credential_target("new-host").unwrap();
        let secrets = RefCell::new(HashMap::from([(old_key.clone(), b"saved-secret".to_vec())]));
        save_proxy_credential_with(
            "new-host",
            &changed,
            Some(("old-host", &previous)),
            None,
            |target| Ok(secrets.borrow().get(target).cloned()),
            |target, value| {
                secrets.borrow_mut().insert(target.to_owned(), value.to_vec());
                Ok(())
            },
            |target| {
                secrets.borrow_mut().remove(target);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(secrets.borrow().get(&new_key).unwrap(), b"saved-secret");
        assert!(!secrets.borrow().contains_key(&old_key));
    }

    #[test]
    fn ssh_proxy_different_endpoint_never_receives_the_old_password() {
        let previous = proxy();
        let mut changed = previous.clone();
        changed.proxy_host = "other.example".into();
        let deleted = RefCell::new(Vec::new());
        save_proxy_credential_with(
            "new-host",
            &changed,
            Some(("old-host", &previous)),
            None,
            |_| panic!("must not read another endpoint's password"),
            |_, _| panic!("must not copy another endpoint's password"),
            |target| {
                deleted.borrow_mut().push(target.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*deleted.borrow(), vec![previous.proxy_credential_target("old-host").unwrap()]);
    }

    #[test]
    fn ssh_proxy_failed_migration_keeps_the_original_credential() {
        let previous = proxy();
        save_proxy_credential_with(
            "new-host",
            &previous,
            Some(("old-host", &previous)),
            None,
            |_| Ok(Some(b"saved-secret".to_vec())),
            |_, _| Err(std::io::Error::other("credential store unavailable")),
            |_| panic!("must not delete before the new credential was stored"),
        )
        .unwrap_err();
    }

    #[test]
    fn ssh_proxy_clear_does_not_reload_a_saved_password() {
        let previous = proxy();
        let deleted = RefCell::new(Vec::new());
        save_proxy_credential_with(
            "host",
            &previous,
            Some(("host", &previous)),
            Some(String::new()),
            |_| panic!("clear must not load a password"),
            |_, _| panic!("clear must not store a password"),
            |target| {
                deleted.borrow_mut().push(target.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*deleted.borrow(), vec![previous.proxy_credential_target("host").unwrap()]);
    }
}

pub(super) fn save_proxy_credential(
    destination: &str,
    connection: &SshConnectionOptions,
    previous: Option<(&str, &SshConnectionOptions)>,
    draft: Option<String>,
) -> std::io::Result<()> {
    save_proxy_credential_with(
        destination,
        connection,
        previous,
        draft,
        crate::ssh_credentials::load_generic_secret,
        crate::ssh_credentials::store_generic_secret,
        crate::ssh_credentials::delete_generic_secret,
    )
}

fn save_proxy_credential_with(
    destination: &str,
    connection: &SshConnectionOptions,
    previous: Option<(&str, &SshConnectionOptions)>,
    draft: Option<String>,
    mut load: impl FnMut(&str) -> std::io::Result<Option<Vec<u8>>>,
    mut store: impl FnMut(&str, &[u8]) -> std::io::Result<()>,
    mut delete: impl FnMut(&str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = connection.proxy_credential_target(destination);
    let previous_target = previous
        .and_then(|(destination, connection)| connection.proxy_credential_target(destination));
    if let Some(target) = target.as_deref() {
        if let Some(password) = draft {
            let password = zeroize::Zeroizing::new(password);
            if password.is_empty() {
                delete(target)?;
            } else {
                store(target, password.as_bytes())?;
            }
        } else if let Some((_, previous)) = previous {
            if same_proxy_endpoint(previous, connection)
                && previous_target.as_deref() != Some(target)
            {
                if let Some(previous_target) = previous_target.as_deref() {
                    if let Some(password) = load(previous_target)? {
                        let password = zeroize::Zeroizing::new(password);
                        store(target, &password)?;
                    }
                }
            }
        }
    }
    if previous_target != target {
        if let Some(previous_target) = previous_target {
            delete(&previous_target)?;
        }
    }
    Ok(())
}

impl SettingsPane {
    pub(super) fn ssh_connection_from_draft(
        &self,
        destination: &str,
        cx: &gpui::App,
    ) -> Result<SshConnectionOptions, String> {
        let language = crate::gpui_shell::config::ui_language(cx);
        let Some(editor) = self.ssh_editor.as_ref() else {
            return Err(language.pick("SSH 编辑器已关闭", "The SSH editor is closed").to_owned());
        };
        let mut connection = editor.connection.clone();
        connection.proxy_host = self.ssh_proxy_host_input.read(cx).value().trim().to_owned();
        connection.proxy_username =
            self.ssh_proxy_username_input.read(cx).value().trim().to_owned();
        connection.jump_host = self.ssh_jump_host_input.read(cx).value().trim().to_owned();
        let port = self.ssh_proxy_port_input.read(cx).value();
        connection.proxy_port = if port.trim().is_empty() {
            None
        } else {
            match port.trim().parse::<u16>() {
                Ok(port) if port > 0 => Some(port),
                _ if matches!(
                    connection.proxy_mode,
                    SshHostProxyMode::Socks5 | SshHostProxyMode::Http
                ) =>
                {
                    return Err(language
                        .pick(
                            "代理端口需要是 1–65535 之间的数字",
                            "The proxy port must be a number from 1 to 65535",
                        )
                        .to_owned());
                },
                _ => None,
            }
        };
        connection.validate(destination)?;
        let password = self.ssh_proxy_password_input.read(cx).value();
        if connection.has_custom_proxy() && !editor.clear_proxy_password && !password.is_empty() {
            if connection.proxy_username.is_empty() {
                return Err(language
                    .pick(
                        "填写代理密码时也需要填写代理用户名",
                        "Enter a proxy username when providing a proxy password",
                    )
                    .to_owned());
            }
            if connection.proxy_mode == SshHostProxyMode::Socks5 && password.len() > 255 {
                return Err(language
                    .pick(
                        "SOCKS5 代理密码不能超过 255 字节",
                        "The SOCKS5 proxy password cannot exceed 255 bytes",
                    )
                    .to_owned());
            }
        }
        Ok(connection)
    }

    pub(super) fn ssh_test_passwords(
        &self,
        destination: &str,
        connection: &SshConnectionOptions,
        cx: &gpui::App,
    ) -> std::io::Result<(Option<String>, Option<String>)> {
        let Some(editor) = self.ssh_editor.as_ref() else { return Ok((None, None)) };
        let mut proxy_password = self.ssh_proxy_password_from_draft(cx);
        let mut password = crate::display::auth_sections(editor.auth)
            .0
            .then(|| self.ssh_password_input.read(cx).value().to_string())
            .filter(|password| !password.is_empty());
        if let Some(original) =
            editor.original_destination.as_deref().filter(|old| *old != destination)
        {
            let decode = |secret: Vec<u8>| -> std::io::Result<String> {
                let secret = zeroize::Zeroizing::new(secret);
                std::str::from_utf8(&secret)
                    .map(str::to_owned)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            };
            if proxy_password.is_none()
                && same_proxy_endpoint(&editor.original_connection, connection)
            {
                if let Some(target) = editor.original_connection.proxy_credential_target(original) {
                    proxy_password = crate::ssh_credentials::load_generic_secret(&target)?
                        .map(decode)
                        .transpose()?;
                }
            }
            if password.is_none()
                && editor.save_password
                && crate::display::auth_sections(editor.auth).0
            {
                password = crate::ssh_credentials::load_stored_password(original)?
                    .map(decode)
                    .transpose()?;
            }
        }
        Ok((proxy_password, password))
    }

    pub(super) fn ssh_proxy_password_from_draft(&self, cx: &gpui::App) -> Option<String> {
        let editor = self.ssh_editor.as_ref()?;
        if !matches!(
            editor.connection.proxy_mode,
            SshHostProxyMode::Socks5 | SshHostProxyMode::Http
        ) {
            return None;
        }
        if editor.clear_proxy_password {
            return Some(String::new());
        }
        let password = self.ssh_proxy_password_input.read(cx).value();
        (!password.is_empty()).then(|| password.to_string())
    }

    pub(super) fn ssh_editor_advanced(&self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let editor = self.ssh_editor.as_ref().expect("open SSH editor");
        let theme = cx.theme();
        let custom_proxy = matches!(
            editor.connection.proxy_mode,
            SshHostProxyMode::Socks5 | SshHostProxyMode::Http
        );
        let proxy_modes = [
            ("ssh-proxy-inherit", language.pick("跟随全局", "Inherit"), SshHostProxyMode::Inherit),
            ("ssh-proxy-direct", language.pick("不使用", "None"), SshHostProxyMode::Direct),
            ("ssh-proxy-socks", "SOCKS5", SshHostProxyMode::Socks5),
            ("ssh-proxy-http", "HTTP", SshHostProxyMode::Http),
        ];
        let proxy_buttons = proxy_modes.into_iter().map(|(id, label, mode)| {
            Button::new(id)
                .label(label)
                .ghost()
                .small()
                .flex_1()
                .min_w_0()
                .h(px(30.0))
                .px_1()
                .toggled(editor.connection.proxy_mode == mode)
                .when(editor.connection.proxy_mode == mode, |button| {
                    button.bg(theme.popover).font_medium().shadow_sm()
                })
                .when(editor.connection.proxy_mode != mode, |button| {
                    button.text_color(theme.muted_foreground)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(editor) = this.ssh_editor.as_mut() {
                        editor.connection.proxy_mode = mode;
                        let placeholder = editor.connection.effective_proxy_port().to_string();
                        this.ssh_proxy_port_input
                            .update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
                    }
                    this.touch_ssh_editor(cx);
                }))
        });
        let jump_modes = [
            (
                "ssh-jump-inherit",
                language.pick("跟随 SSH config", "SSH config"),
                SshHostJumpMode::Inherit,
            ),
            ("ssh-jump-none", language.pick("不使用", "None"), SshHostJumpMode::None),
            ("ssh-jump-host", language.pick("指定主机", "Use host"), SshHostJumpMode::Host),
        ];
        let jump_buttons = jump_modes.into_iter().map(|(id, label, mode)| {
            Button::new(id)
                .label(label)
                .ghost()
                .small()
                .flex_1()
                .min_w_0()
                .h(px(30.0))
                .px_1()
                .toggled(editor.connection.jump_mode == mode)
                .when(editor.connection.jump_mode == mode, |button| {
                    button.bg(theme.popover).font_medium().shadow_sm()
                })
                .when(editor.connection.jump_mode != mode, |button| {
                    button.text_color(theme.muted_foreground)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.ssh_editor.as_mut() {
                        editor.connection.jump_mode = mode;
                        editor.jump_picker_open = false;
                    }
                    this.touch_ssh_editor(cx);
                }))
        });
        let destination = self.ssh_destination_from_draft(cx).unwrap_or_default();
        let route = self.ssh_connection_from_draft(&destination, cx).ok().map(|connection| {
            let mut steps = vec![language.pick("本机", "Local").to_owned()];
            match connection.proxy_mode {
                SshHostProxyMode::Inherit => {
                    steps.push(language.pick("全局网络规则", "Global network rules").to_owned())
                },
                SshHostProxyMode::Direct => {},
                SshHostProxyMode::Socks5 | SshHostProxyMode::Http => steps.push(format!(
                    "{} {}:{}",
                    if connection.proxy_mode == SshHostProxyMode::Socks5 {
                        "SOCKS5"
                    } else {
                        "HTTP"
                    },
                    connection.proxy_host,
                    connection.effective_proxy_port(),
                )),
            }
            match connection.jump_mode {
                SshHostJumpMode::Inherit => steps
                    .push(language.pick("SSH config 跳板规则", "SSH config jump rules").to_owned()),
                SshHostJumpMode::None => {},
                SshHostJumpMode::Host => steps.push(connection.jump_host),
            }
            steps.push(if destination.is_empty() {
                language.pick("目标主机", "Target host").to_owned()
            } else {
                destination.clone()
            });
            steps.join(" → ")
        });
        v_flex()
            .w_full()
            .child(div().text_xs().font_medium().child(language.pick("网络代理", "Network proxy")))
            .child(editor_hint(language.pick(
                "仅覆盖这台主机，不改变全局网络设置。",
                "Applies to this host without changing global network settings.",
            ), cx).mt_1())
            .child(h_flex().mt_3().p(px(3.0)).gap(px(3.0)).rounded_lg().bg(theme.input).children(proxy_buttons))
            .when(custom_proxy, |body| {
                body.child(
                    h_flex().mt_4().gap_3().items_start()
                        .child(div().flex_1().min_w_0().child(editor_field(
                            language.pick("代理地址", "Proxy host"),
                            editor_input(&self.ssh_proxy_host_input, language.pick("代理地址", "Proxy host"), cx),
                        )))
                        .child(div().w(px(84.0)).flex_shrink_0().child(editor_field(
                            language.pick("端口", "Port"),
                            editor_input(&self.ssh_proxy_port_input, language.pick("代理端口", "Proxy port"), cx),
                        ))),
                )
                .child(
                    h_flex().mt_4().gap_3().items_start()
                        .child(div().flex_1().min_w_0().child(editor_field(
                            language.pick("用户名（可选）", "Username (optional)"),
                            editor_input(&self.ssh_proxy_username_input, language.pick("代理用户名", "Proxy username"), cx),
                        )))
                        .child(div().flex_1().min_w_0().child(editor_field(
                            language.pick("密码（可选）", "Password (optional)"),
                            editor_input(&self.ssh_proxy_password_input, language.pick("代理密码", "Proxy password"), cx)
                                .mask_toggle().disabled(editor.clear_proxy_password),
                        ))),
                )
                .child(editor_hint(language.pick(
                    "代理密码仅保存在系统凭据库，不写入主机配置。",
                    "Proxy passwords stay in the system credential store, never in host configuration.",
                ), cx).mt_2())
                .when(editor.original_destination.is_some(), |body| {
                    body.child(
                        div().mt_2().child(
                            Checkbox::new("ssh-clear-proxy-password").small()
                                .checked(editor.clear_proxy_password)
                                .label(language.pick("清除已保存的代理密码", "Remove the saved proxy password"))
                                .on_click(cx.listener(|this, value: &bool, window, cx| {
                                    if let Some(editor) = this.ssh_editor.as_mut() {
                                        editor.clear_proxy_password = *value;
                                    }
                                    if *value {
                                        this.ssh_proxy_password_input.update(cx, |input, cx| input.set_value("", window, cx));
                                    }
                                    this.touch_ssh_editor(cx);
                                })),
                        ),
                    )
                })
                .when(editor.connection.proxy_mode == SshHostProxyMode::Http, |body| {
                    body.child(editor_hint(language.pick(
                        "HTTP CONNECT 代理；代理认证本身不使用 TLS，请仅在可信网络中使用。",
                        "HTTP CONNECT; proxy authentication is not protected by TLS. Use a trusted network.",
                    ), cx).mt_2())
                })
            })
            .child(
                v_flex().mt_5().pt_5().border_t_1().border_color(theme.border)
                    .child(div().text_xs().font_medium().child(language.pick("SSH 跳板机", "SSH jump host")))
                    .child(h_flex().mt_3().p(px(3.0)).gap(px(3.0)).rounded_lg().bg(theme.input).children(jump_buttons))
                    .when(editor.connection.jump_mode == SshHostJumpMode::Host, |section| {
                        section.child(
                            v_flex().mt_4().gap_2()
                                .child(
                                    h_flex().justify_between().items_center()
                                        .child(div().text_xs().font_medium().child(language.pick("跳板主机", "Jump host")))
                                        .child(
                                            Button::new("ssh-jump-choose-saved").ghost().xsmall()
                                                .label(language.pick("从已保存主机选择", "Choose saved host"))
                                                .icon(if editor.jump_picker_open { IconName::ChevronUp } else { IconName::ChevronDown })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if let Some(editor) = this.ssh_editor.as_mut() {
                                                        editor.jump_picker_open = !editor.jump_picker_open;
                                                    }
                                                    cx.notify();
                                                })),
                                        ),
                                )
                                .child(editor_input(&self.ssh_jump_host_input, language.pick("跳板主机", "Jump host"), cx))
                                .when(editor.jump_picker_open, |section| {
                                    let choices = editor.jump_choices.iter().filter(|(host, _)| host != &destination).collect::<Vec<_>>();
                                    if choices.is_empty() {
                                        return section.child(editor_hint(language.pick(
                                            "没有其他已保存主机；可直接填写地址或 SSH config 别名。",
                                            "No other saved hosts. Enter an address or SSH config alias.",
                                        ), cx));
                                    }
                                    section.child(
                                        v_flex().h(px((choices.len() as f32 * 42.0).min(168.0)))
                                            .overflow_y_scrollbar().rounded_md().border_1().border_color(theme.border)
                                            .child(v_flex().children(choices.into_iter().enumerate().map(|(index, (host, label))| {
                                                let host = host.clone();
                                                let selected = host == self.ssh_jump_host_input.read(cx).value().as_ref();
                                                v_flex().id(SharedString::from(format!("ssh-jump-choice-{index}")))
                                                    .w_full().h(px(42.0)).px_3().justify_center()
                                                    .cursor_pointer().hover(|row| row.bg(theme.list_hover))
                                                    .when(selected, |row| row.bg(theme.list_active))
                                                    .child(div().text_sm().truncate().child(label.clone()))
                                                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(host.clone()))
                                                    .on_click(cx.listener(move |this, _, window, cx| {
                                                        this.ssh_jump_host_input.update(cx, |input, cx| input.set_value(host.clone(), window, cx));
                                                        if let Some(editor) = this.ssh_editor.as_mut() {
                                                            editor.jump_picker_open = false;
                                                        }
                                                        this.touch_ssh_editor(cx);
                                                    }))
                                            }))),
                                    )
                                })
                                .child(editor_hint(language.pick(
                                    "沿用跳板主机自己的用户名、密钥和已存凭据；不会使用目标主机的密码。",
                                    "Uses the jump host's own username, keys and saved credentials, never the target password.",
                                ), cx)),
                        )
                    })
                    .child(editor_hint(language.pick(
                        "网络代理用于连接第一台 SSH 主机，再由跳板转发到目标。",
                        "The network proxy connects to the first SSH host; the jump host forwards to the target.",
                    ), cx).mt_3()),
            )
            .child(
                v_flex().mt_5().p_3().gap_2().rounded_md().bg(theme.group_box)
                    .child(div().text_xs().font_medium().child(language.pick("连接规则预览", "Connection rule preview")))
                    .child(editor_hint(route.unwrap_or_else(|| language.pick(
                        "填写有效的代理和跳板信息后显示连接规则。",
                        "Enter valid proxy and jump host details to preview the connection rules.",
                    ).to_owned()), cx)),
            )
    }
}
