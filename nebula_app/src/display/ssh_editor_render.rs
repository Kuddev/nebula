use unicode_width::UnicodeWidthChar;

use super::ssh_ui::auth_sections;
use super::*;
use crate::ssh_profiles::SshAuthMode;

type Rect = (f32, f32, f32, f32);

impl Display {
    pub(super) fn draw_ssh_editor_modal(&mut self) {
        let progress = self.nebula_ui_anims.ssh_editor.value().clamp(0.0, 1.0);
        if !self.nebula_ssh_editor_open && progress <= 0.004 {
            self.nebula_ssh_editor = None;
            self.nebula_ssh_editor_rects = None;
            self.nebula_ssh_editor_hover = SshEditorHit::None;
            return;
        }
        let Some(editor) = self.nebula_ssh_editor.clone() else {
            self.nebula_ssh_editor_rects = None;
            return;
        };

        let size = self.size_info;
        let scale = self.window.scale_factor as f32;
        let s = |value: f32| value * scale;
        let skin = self.nebula_theme.skin();
        let language = self.ui_language();
        let accent = Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, 255);
        let cell_h = size.cell_height();
        let cell_w = size.cell_width();
        let text_width = |text: &str| -> f32 {
            text.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w
        };
        let (show_password, show_keys) = auth_sections(editor.auth);

        let box_w = s(520.0).min(size.width() - s(32.0));
        let desired_h = if show_password && show_keys {
            540.0
        } else if show_keys {
            460.0
        } else if show_password {
            400.0
        } else {
            330.0
        };
        let box_h = s(desired_h).min(size.height() - s(32.0));
        let bx = (size.width() - box_w) * 0.5;
        let resting_y = (size.height() - box_h) * 0.5;
        let by = resting_y - (1.0 - progress) * s(14.0);
        let pad = s(24.0);
        let field_h = s(40.0);
        let field_w = box_w - pad * 2.0;
        let destination = (bx + pad, by + s(76.0), field_w, field_h);
        let auth_y = destination.1 + destination.3 + s(40.0);
        let auth_track = (destination.0, auth_y, field_w, s(40.0));
        let auth_pad = s(3.0);
        let auth_w = (field_w - auth_pad * 2.0) / 5.0;
        let auth_modes = [
            SshAuthMode::Auto,
            SshAuthMode::Password,
            SshAuthMode::PublicKey,
            SshAuthMode::Agent,
            SshAuthMode::KeyboardInteractive,
        ];
        let auth = std::array::from_fn(|index| {
            (
                auth_modes[index],
                (
                    auth_track.0 + auth_pad + index as f32 * auth_w,
                    auth_track.1 + auth_pad,
                    auth_w,
                    auth_track.3 - auth_pad * 2.0,
                ),
            )
        });
        let content_y = auth_y + s(72.0);
        let zero = (0.0, 0.0, 0.0, 0.0);
        let password =
            if show_password { (destination.0, content_y, field_w, field_h) } else { zero };
        let password_toggle = if show_password {
            (password.0 + password.2 - s(38.0), password.1 + s(4.0), s(34.0), password.3 - s(8.0))
        } else {
            zero
        };
        let save_label = language
            .pick("保存密码到 Windows 凭据管理器", "Save password in Windows Credential Manager");
        let save_toggle = if show_password {
            (
                destination.0,
                password.1 + password.3 + s(12.0),
                (s(28.0) + text_width(save_label)).min(field_w),
                s(28.0),
            )
        } else {
            zero
        };
        let save_checkbox = if show_password {
            (save_toggle.0, save_toggle.1 + s(5.0), s(18.0), s(18.0))
        } else {
            zero
        };

        let key_header_y = if show_password { save_toggle.1 + s(54.0) } else { content_y };
        let add_private_key = if show_keys {
            (destination.0 + field_w - s(126.0), key_header_y - s(8.0), s(126.0), s(32.0))
        } else {
            zero
        };
        let key_rows_y = key_header_y + s(30.0);
        let footer_y = by + box_h - s(58.0);
        let footer_top = footer_y - s(16.0);
        let available_rows =
            (((footer_top - s(16.0) - key_rows_y) / s(36.0)).floor() as isize).max(1) as usize;
        let visible_start = editor.private_keys.len().saturating_sub(available_rows);
        let visible_keys = if show_keys {
            editor
                .private_keys
                .iter()
                .enumerate()
                .skip(visible_start)
                .take(available_rows)
                .map(|(index, _)| {
                    let row = (
                        destination.0,
                        key_rows_y + (index - visible_start) as f32 * s(36.0),
                        field_w,
                        s(32.0),
                    );
                    let remove = (row.0 + row.2 - s(34.0), row.1, s(34.0), row.3);
                    (index, row, remove)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let primary_action = language.pick("保存", "Save");
        let primary_key = "Enter";
        let cancel_action = language.pick("取消", "Cancel");
        let cancel_key = "Esc";
        let key_pad = s(6.0);
        let label_gap = s(8.0);
        let button_pad = s(14.0);
        let primary_key_w = text_width(primary_key) + key_pad * 2.0;
        let cancel_key_w = text_width(cancel_key) + key_pad * 2.0;
        let primary_w =
            s(108.0).max(text_width(primary_action) + label_gap + primary_key_w + button_pad * 2.0);
        let cancel_w =
            s(100.0).max(text_width(cancel_action) + label_gap + cancel_key_w + button_pad * 2.0);
        let primary = (bx + box_w - pad - primary_w, footer_y, primary_w, s(36.0));
        let cancel = (primary.0 - s(12.0) - cancel_w, primary.1, cancel_w, s(36.0));
        let primary_group_w = text_width(primary_action) + label_gap + primary_key_w;
        let primary_group_x = primary.0 + (primary.2 - primary_group_w) * 0.5;
        let primary_key_rect = (
            primary_group_x + text_width(primary_action) + label_gap,
            primary.1 + s(7.0),
            primary_key_w,
            primary.3 - s(14.0),
        );
        let cancel_group_w = text_width(cancel_action) + label_gap + cancel_key_w;
        let cancel_group_x = cancel.0 + (cancel.2 - cancel_group_w) * 0.5;
        let cancel_key_rect = (
            cancel_group_x + text_width(cancel_action) + label_gap,
            cancel.1 + s(7.0),
            cancel_key_w,
            cancel.3 - s(14.0),
        );

        self.nebula_ssh_editor_rects = Some(SshEditorRects {
            destination,
            password,
            password_toggle,
            auth,
            add_private_key,
            private_key_rows: visible_keys
                .iter()
                .map(|(index, _, remove)| (*index, *remove))
                .collect(),
            save_checkbox,
            save_toggle,
            primary,
            cancel,
        });

        let mut quads = vec![
            UiQuad::solid(
                0.0,
                0.0,
                size.width(),
                size.height(),
                0.0,
                Rgba::new(0, 0, 0, (170.0 * progress).round() as u8),
            ),
            UiQuad::solid(
                bx - s(1.0),
                by - s(1.0),
                box_w + s(2.0),
                box_h + s(2.0),
                s(13.0),
                skin.hairline,
            ),
            UiQuad::solid(bx, by, box_w, box_h, s(12.0), skin.panel),
            UiQuad::solid(bx, footer_top, box_w, box_h - (footer_top - by), 0.0, skin.surface),
            UiQuad::solid(bx, footer_top, box_w, s(1.0), 0.0, skin.hairline),
        ];
        input_quads(
            &mut quads,
            destination,
            editor.field == SshEditorField::Destination,
            self.nebula_ssh_editor_hover == SshEditorHit::Destination,
            accent,
            &skin,
            scale,
        );
        if show_password {
            input_quads(
                &mut quads,
                password,
                editor.field == SshEditorField::Password,
                self.nebula_ssh_editor_hover == SshEditorHit::Password,
                accent,
                &skin,
                scale,
            );
        }
        quads.push(UiQuad::solid(
            auth_track.0 - s(1.0),
            auth_track.1 - s(1.0),
            auth_track.2 + s(2.0),
            auth_track.3 + s(2.0),
            s(9.0),
            skin.hairline,
        ));
        quads.push(UiQuad::solid(
            auth_track.0,
            auth_track.1,
            auth_track.2,
            auth_track.3,
            s(8.0),
            skin.surface,
        ));
        for (mode, rect) in auth {
            let active = editor.auth == mode;
            let hovered = self.nebula_ssh_editor_hover == SshEditorHit::Auth(mode);
            if active || hovered {
                quads.push(UiQuad::solid(
                    rect.0,
                    rect.1,
                    rect.2,
                    rect.3,
                    s(6.0),
                    if active { skin.hover_strong } else { skin.hover },
                ));
            }
        }
        if show_password {
            if self.nebula_ssh_editor_hover == SshEditorHit::PasswordToggle {
                quads.push(UiQuad::solid(
                    password_toggle.0,
                    password_toggle.1,
                    password_toggle.2,
                    password_toggle.3,
                    s(6.0),
                    skin.hover,
                ));
            }
            quads.push(UiQuad::solid(
                save_checkbox.0 - s(1.0),
                save_checkbox.1 - s(1.0),
                save_checkbox.2 + s(2.0),
                save_checkbox.3 + s(2.0),
                s(5.0),
                skin.hairline,
            ));
            quads.push(UiQuad::solid(
                save_checkbox.0,
                save_checkbox.1,
                save_checkbox.2,
                save_checkbox.3,
                s(4.0),
                skin.input,
            ));
        }
        if show_keys {
            quads.push(UiQuad::solid(
                add_private_key.0 - s(1.0),
                add_private_key.1 - s(1.0),
                add_private_key.2 + s(2.0),
                add_private_key.3 + s(2.0),
                s(7.0),
                skin.hairline,
            ));
            quads.push(UiQuad::solid(
                add_private_key.0,
                add_private_key.1,
                add_private_key.2,
                add_private_key.3,
                s(7.0),
                if self.nebula_ssh_editor_hover == SshEditorHit::AddPrivateKey {
                    skin.hover
                } else {
                    skin.surface
                },
            ));
            for (index, row, remove) in &visible_keys {
                quads.push(UiQuad::solid(row.0, row.1, row.2, row.3, s(6.0), skin.input));
                if self.nebula_ssh_editor_hover == SshEditorHit::RemovePrivateKey(*index) {
                    quads.push(UiQuad::solid(
                        remove.0,
                        remove.1,
                        remove.2,
                        remove.3,
                        s(6.0),
                        skin.hover,
                    ));
                }
            }
        }
        button_quads(
            &mut quads,
            cancel,
            primary,
            self.nebula_ssh_editor_hover,
            accent,
            &skin,
            scale,
        );
        for rect in [cancel_key_rect, primary_key_rect] {
            quads.push(UiQuad::solid(
                rect.0 - s(1.0),
                rect.1 - s(1.0),
                rect.2 + s(2.0),
                rect.3 + s(2.0),
                s(5.0),
                skin.hairline,
            ));
            quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(4.0), skin.input));
        }

        let caret_field = if editor.field == SshEditorField::Password && show_password {
            SshEditorField::Password
        } else {
            SshEditorField::Destination
        };
        let (caret_rect, caret_columns, selected) = match caret_field {
            SshEditorField::Destination => (
                destination,
                editor.destination.chars().count(),
                editor.destination_selection.is_selected(),
            ),
            SshEditorField::Password => {
                (password, editor.password.chars().count(), editor.password_selection.is_selected())
            },
        };
        draw_caret_quad(
            &mut quads,
            caret_rect,
            caret_columns,
            selected,
            caret_field == SshEditorField::Password,
            cell_w,
            scale,
            &skin,
        );
        self.renderer.draw_ui(&size, &quads);

        let glyph_cache = &mut self.glyph_cache;
        self.renderer.draw_doc_text(
            &size,
            bx + pad,
            by + s(20.0),
            1.15,
            skin.ink_strong,
            Flags::empty(),
            if editor.original_destination.is_some() {
                language.pick("编辑 SSH 主机", "Edit SSH host")
            } else {
                language.pick("添加 SSH 主机", "Add SSH host")
            },
            glyph_cache,
        );
        self.renderer.draw_chrome_text(
            &size,
            destination.0,
            destination.1 - cell_h - s(5.0),
            skin.ink,
            language.pick("连接地址", "Destination"),
            glyph_cache,
        );
        self.renderer.draw_doc_text(
            &size,
            destination.0 + s(82.0),
            destination.1 - cell_h - s(3.0),
            0.72,
            if editor.error.is_some() {
                if skin.is_light { Rgb::new(207, 34, 46) } else { Rgb::new(248, 81, 73) }
            } else {
                skin.ink_dim
            },
            Flags::empty(),
            editor.error.as_deref().unwrap_or(language.pick(
                "user@host · 非 22 端口用 ssh://",
                "user@host · use ssh:// for non-22 ports",
            )),
            glyph_cache,
        );
        self.renderer.draw_chrome_text(
            &size,
            destination.0 + s(12.0),
            destination.1 + (field_h - cell_h) / 2.0,
            if editor.destination.is_empty() { skin.ink_faint } else { skin.ink },
            if editor.destination.is_empty() { "user@example.com" } else { &editor.destination },
            glyph_cache,
        );
        self.renderer.draw_chrome_text(
            &size,
            destination.0,
            auth_y - cell_h - s(5.0),
            skin.ink_dim,
            language.pick("认证方式", "Authentication"),
            glyph_cache,
        );
        let auth_labels = if language == super::UiLanguage::ZhCn {
            ["自动", "密码", "密钥", "Agent", "交互式"]
        } else {
            ["Auto", "Password", "Key", "Agent", "Interactive"]
        };
        for ((mode, rect), label) in auth.iter().zip(auth_labels) {
            self.renderer.draw_chrome_text(
                &size,
                rect.0 + (rect.2 - text_width(label)) * 0.5,
                rect.1 + (rect.3 - cell_h) / 2.0,
                if editor.auth == *mode { skin.ink_strong } else { skin.ink_dim },
                label,
                glyph_cache,
            );
        }

        if show_password {
            draw_password_text(
                &mut self.renderer,
                glyph_cache,
                &size,
                &editor,
                password,
                password_toggle,
                save_toggle,
                save_checkbox,
                save_label,
                language,
                field_h,
                cell_h,
                cell_w,
                scale,
                &skin,
                self.nebula_ssh_editor_hover,
            );
        }
        if show_keys {
            self.renderer.draw_chrome_text(
                &size,
                destination.0,
                key_header_y,
                skin.ink_dim,
                language.pick("私钥", "Private keys"),
                glyph_cache,
            );
            self.renderer.draw_chrome_text(
                &size,
                add_private_key.0
                    + (add_private_key.2 - text_width(language.pick("+ 添加私钥", "+ Add key")))
                        * 0.5,
                add_private_key.1 + (add_private_key.3 - cell_h) / 2.0,
                skin.ink,
                language.pick("+ 添加私钥", "+ Add key"),
                glyph_cache,
            );
            if editor.private_keys.is_empty() {
                self.renderer.draw_chrome_text(
                    &size,
                    destination.0,
                    key_rows_y + s(7.0),
                    skin.ink_faint,
                    language.pick(
                        "未指定；将使用 IdentityFile 和默认 id_* 私钥",
                        "None specified; IdentityFile and default id_* keys will be used",
                    ),
                    glyph_cache,
                );
            }
            for (index, row, remove) in &visible_keys {
                let label = path_tail(&editor.private_keys[*index], 64);
                self.renderer.draw_chrome_text(
                    &size,
                    row.0 + s(10.0),
                    row.1 + (row.3 - cell_h) / 2.0,
                    skin.ink,
                    &label,
                    glyph_cache,
                );
                self.renderer.draw_chrome_text(
                    &size,
                    remove.0 + (remove.2 - text_width("×")) * 0.5,
                    remove.1 + (remove.3 - cell_h) / 2.0,
                    skin.icon,
                    "×",
                    glyph_cache,
                );
            }
        } else if editor.auth == SshAuthMode::Agent {
            self.renderer.draw_chrome_text(
                &size,
                destination.0,
                content_y,
                skin.ink_dim,
                language.pick(
                    "仅使用 Windows OpenSSH Agent 与 Pageant，不回退密码。",
                    "Use Windows OpenSSH Agent and Pageant only; do not fall back to passwords.",
                ),
                glyph_cache,
            );
        } else if editor.auth == SshAuthMode::KeyboardInteractive {
            self.renderer.draw_chrome_text(
                &size,
                destination.0,
                content_y,
                skin.ink_dim,
                language.pick(
                    "仅响应服务器的 keyboard-interactive / MFA 提示。",
                    "Respond only to server keyboard-interactive / MFA prompts.",
                ),
                glyph_cache,
            );
        }
        draw_button_text(
            &mut self.renderer,
            glyph_cache,
            &size,
            cancel,
            primary,
            cancel_group_x,
            primary_group_x,
            cancel_key_rect,
            primary_key_rect,
            cancel_action,
            primary_action,
            cancel_key,
            primary_key,
            cell_h,
            &skin,
        );

        if self.nebula_ui_anims.ssh_editor.animating_to(if self.nebula_ssh_editor_open {
            1.0
        } else {
            0.0
        }) {
            self.pending_update.dirty = true;
            self.window.request_redraw();
        }
    }
}

fn input_quads(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    active: bool,
    hovered: bool,
    accent: Rgba,
    skin: &theme::Skin,
    scale: f32,
) {
    let s = |value: f32| value * scale;
    quads.push(UiQuad::solid(
        rect.0 - s(1.0),
        rect.1 - s(1.0),
        rect.2 + s(2.0),
        rect.3 + s(2.0),
        s(7.0),
        if active {
            Rgba::new(accent.r, accent.g, accent.b, if skin.is_light { 118 } else { 136 })
        } else {
            skin.hairline
        },
    ));
    quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(6.0), skin.input));
    if hovered && !active {
        quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(6.0), skin.hover));
    }
}

fn button_quads(
    quads: &mut Vec<UiQuad>,
    cancel: Rect,
    primary: Rect,
    hover: SshEditorHit,
    accent: Rgba,
    skin: &theme::Skin,
    scale: f32,
) {
    let s = |value: f32| value * scale;
    let primary_edge = Rgba::new(accent.r, accent.g, accent.b, if skin.is_light { 72 } else { 92 });
    for (rect, edge) in [(cancel, skin.hairline), (primary, primary_edge)] {
        quads.push(UiQuad::solid(
            rect.0 - s(1.0),
            rect.1 - s(1.0),
            rect.2 + s(2.0),
            rect.3 + s(2.0),
            s(9.0),
            edge,
        ));
    }
    quads.push(UiQuad::solid(cancel.0, cancel.1, cancel.2, cancel.3, s(8.0), skin.surface));
    quads.push(UiQuad::solid(primary.0, primary.1, primary.2, primary.3, s(8.0), skin.accent_soft));
    if hover == SshEditorHit::Cancel {
        quads.push(UiQuad::solid(cancel.0, cancel.1, cancel.2, cancel.3, s(8.0), skin.hover));
    }
    if hover == SshEditorHit::Primary {
        quads.push(UiQuad::solid(primary.0, primary.1, primary.2, primary.3, s(8.0), skin.hover));
    }
}

fn draw_caret_quad(
    quads: &mut Vec<UiQuad>,
    rect: Rect,
    columns: usize,
    selected: bool,
    password: bool,
    cell_w: f32,
    scale: f32,
    skin: &theme::Skin,
) {
    let s = |value: f32| value * scale;
    let right_pad = if password { s(48.0) } else { s(10.0) };
    if selected && columns > 0 {
        let width = (columns as f32 * cell_w).min(rect.2 - s(12.0) - right_pad);
        quads.push(UiQuad::solid(
            rect.0 + s(10.0),
            rect.1 + s(7.0),
            width + s(4.0),
            rect.3 - s(14.0),
            s(4.0),
            skin.accent_soft,
        ));
    } else {
        let x = (rect.0 + s(12.0) + columns as f32 * cell_w).min(rect.0 + rect.2 - right_pad);
        quads.push(UiQuad::solid(
            x,
            rect.1 + s(10.0),
            s(1.5).max(1.0),
            rect.3 - s(20.0),
            0.0,
            Rgba::new(skin.ink_strong.r, skin.ink_strong.g, skin.ink_strong.b, 235),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_password_text(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    size: &SizeInfo,
    editor: &SshHostEditor,
    password: Rect,
    password_toggle: Rect,
    save_toggle: Rect,
    save_checkbox: Rect,
    save_label: &str,
    language: super::UiLanguage,
    field_h: f32,
    cell_h: f32,
    cell_w: f32,
    scale: f32,
    skin: &theme::Skin,
    hover: SshEditorHit,
) {
    let s = |value: f32| value * scale;
    renderer.draw_chrome_text(
        size,
        password.0,
        password.1 - cell_h - s(5.0),
        skin.ink_dim,
        language.pick("密码", "Password"),
        glyph_cache,
    );
    let masked = if editor.password.is_empty() {
        language.pick("连接时询问", "Ask when connecting").to_owned()
    } else if editor.show_password {
        editor.password.clone()
    } else {
        "•".repeat(editor.password.chars().count())
    };
    renderer.draw_chrome_text(
        size,
        password.0 + s(12.0),
        password.1 + (field_h - cell_h) / 2.0,
        if editor.password.is_empty() { skin.ink_faint } else { skin.ink },
        &masked,
        glyph_cache,
    );
    let eye = if editor.show_password { "" } else { "" };
    renderer.draw_chrome_text(
        size,
        password_toggle.0 + (password_toggle.2 - cell_w) * 0.5,
        password_toggle.1 + (password_toggle.3 - cell_h) / 2.0,
        if hover == SshEditorHit::PasswordToggle { skin.icon_hover } else { skin.icon },
        eye,
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        save_toggle.0 + s(28.0),
        save_toggle.1 + (save_toggle.3 - cell_h) / 2.0,
        skin.ink,
        save_label,
        glyph_cache,
    );
    if editor.save_password {
        renderer.draw_chrome_text(
            size,
            save_checkbox.0 + (save_checkbox.2 - cell_w) * 0.5,
            save_checkbox.1 + (save_checkbox.3 - cell_h) / 2.0,
            skin.icon_hover,
            "",
            glyph_cache,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_button_text(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    size: &SizeInfo,
    cancel: Rect,
    primary: Rect,
    cancel_group_x: f32,
    primary_group_x: f32,
    cancel_key_rect: Rect,
    primary_key_rect: Rect,
    cancel_action: &str,
    primary_action: &str,
    cancel_key: &str,
    primary_key: &str,
    cell_h: f32,
    skin: &theme::Skin,
) {
    renderer.draw_chrome_text(
        size,
        cancel_group_x,
        cancel.1 + (cancel.3 - cell_h) / 2.0,
        skin.ink,
        cancel_action,
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        primary_group_x,
        primary.1 + (primary.3 - cell_h) / 2.0,
        skin.ink_strong,
        primary_action,
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        cancel_key_rect.0 + (cancel_key_rect.2 - size.cell_width() * cancel_key.len() as f32) * 0.5,
        cancel_key_rect.1 + (cancel_key_rect.3 - cell_h) / 2.0,
        skin.ink_dim,
        cancel_key,
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        primary_key_rect.0
            + (primary_key_rect.2 - size.cell_width() * primary_key.len() as f32) * 0.5,
        primary_key_rect.1 + (primary_key_rect.3 - cell_h) / 2.0,
        skin.ink_dim,
        primary_key,
        glyph_cache,
    );
}

fn path_tail(path: &std::path::Path, max_chars: usize) -> String {
    let value = path.to_string_lossy();
    let count = value.chars().count();
    if count <= max_chars {
        value.into_owned()
    } else {
        format!("…{}", value.chars().skip(count - max_chars + 1).collect::<String>())
    }
}
