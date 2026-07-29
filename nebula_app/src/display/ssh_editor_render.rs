use unicode_width::UnicodeWidthChar;

use super::ssh_ui::{SshTestState, auth_sections};
use super::*;
use super::ui::theme;
use crate::ssh_profiles::SshAuthMode;

type Rect = (f32, f32, f32, f32);

#[derive(Debug, Clone, Copy)]
struct SshEditorVerticalLayout {
    destination_y: f32,
    auth_y: f32,
    password_y: f32,
    save_toggle_y: f32,
    content_y: f32,
    key_header_y: f32,
    key_rows_y: f32,
    desired_h: f32,
}

/// 用同一组节奏令牌推导所有纵向位置。这样标签到自身控件保持紧密、不同
/// 认证组之间保持明确留白，避免此前密码标签贴住分段控件、私钥组却离得过远。
fn ssh_editor_vertical_layout(
    show_password: bool,
    show_keys: bool,
    key_row_count: usize,
    cell_h: f32,
) -> SshEditorVerticalLayout {
    const DESTINATION_Y: f32 = 84.0;
    const AUTH_Y: f32 = 176.0;
    const CONTROL_H: f32 = 32.0;
    const LABEL_GAP: f32 = 8.0;
    const GROUP_GAP: f32 = 18.0;
    const SECTION_GAP: f32 = 24.0;
    const SAVE_GAP: f32 = 12.0;
    const SAVE_H: f32 = 28.0;
    const KEY_LABEL_TO_ROWS: f32 = 30.0;
    const KEY_EMPTY_H: f32 = 42.0;
    const KEY_ROW_PITCH: f32 = 36.0;
    const CONTENT_TO_FOOTER: f32 = 24.0;
    const FOOTER_H: f32 = 64.0;

    let auth_bottom = AUTH_Y + CONTROL_H;
    let password_y = if show_password {
        // auth control -> inter-group gap -> password label -> label/control gap.
        auth_bottom + GROUP_GAP + cell_h + LABEL_GAP
    } else {
        0.0
    };
    let save_toggle_y = if show_password { password_y + CONTROL_H + SAVE_GAP } else { 0.0 };
    let password_bottom = save_toggle_y + SAVE_H;
    let content_y = auth_bottom + SECTION_GAP;
    let key_header_y = if show_password { password_bottom + SECTION_GAP } else { content_y };
    let key_rows_y = key_header_y + KEY_LABEL_TO_ROWS;
    let content_bottom = if show_keys {
        key_rows_y
            + if key_row_count == 0 {
                KEY_EMPTY_H
            } else {
                key_row_count as f32 * KEY_ROW_PITCH
            }
    } else if show_password {
        password_bottom
    } else {
        content_y + cell_h
    };

    SshEditorVerticalLayout {
        destination_y: DESTINATION_Y,
        auth_y: AUTH_Y,
        password_y,
        save_toggle_y,
        content_y,
        key_header_y,
        key_rows_y,
        desired_h: content_bottom + CONTENT_TO_FOOTER + FOOTER_H,
    }
}

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

        let size = self.ui_size_info();
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

        // 稿一的核心不是把旧弹窗换色，而是让高度由真实内容决定。私钥最
        // 多展示四行，更多条目保留尾部（最近添加项），避免小屏越过页脚。
        let key_row_count = if show_keys {
            editor.private_keys.len().max(1).min(4)
        } else {
            0
        };
        let vertical = ssh_editor_vertical_layout(
            show_password,
            show_keys,
            if editor.private_keys.is_empty() { 0 } else { key_row_count },
            cell_h / scale,
        );
        let box_w = s(460.0).min(size.width() - s(32.0));
        let box_h = s(vertical.desired_h).min(size.height() - s(32.0));
        let bx = (size.width() - box_w) * 0.5;
        let resting_y = (size.height() - box_h) * 0.5;
        let by = resting_y - (1.0 - progress) * s(14.0);
        let pad = s(24.0);
        let field_h = s(32.0);
        let field_w = box_w - pad * 2.0;
        let close = (bx + box_w - pad - s(28.0), by + s(14.0), s(28.0), s(28.0));
        let destination = (bx + pad, by + s(vertical.destination_y), field_w, field_h);
        let auth_y = by + s(vertical.auth_y);
        let auth_track = (destination.0, auth_y, field_w, s(32.0));
        let auth_pad = s(3.0);
        let auth_w = (field_w - auth_pad * 2.0) / 4.0;
        let auth_modes = [
            SshAuthMode::Auto,
            SshAuthMode::Password,
            SshAuthMode::PublicKey,
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
        let content_y = by + s(vertical.content_y);
        let zero = (0.0, 0.0, 0.0, 0.0);
        let password = if show_password {
            (destination.0, by + s(vertical.password_y), field_w, field_h)
        } else {
            zero
        };
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
                by + s(vertical.save_toggle_y),
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

        let key_header_y = by + s(vertical.key_header_y);
        let add_private_key = if show_keys {
            (destination.0 + field_w - s(112.0), key_header_y - s(6.0), s(112.0), s(28.0))
        } else {
            zero
        };
        let key_rows_y = by + s(vertical.key_rows_y);
        let footer_top = by + box_h - s(64.0);
        let footer_y = footer_top + s(16.0);
        let available_key_height = (footer_top - s(10.0) - key_rows_y).max(0.0);
        let available_rows = (available_key_height / s(36.0)).floor() as usize;
        let show_empty_key_state = available_key_height >= s(40.0);
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
        let cancel_action = language.pick("取消", "Cancel");
        let test_action = language.pick("测试连接", "Test connection");
        let primary_w = s(72.0).max(text_width(primary_action) + s(28.0));
        let cancel_w = s(72.0).max(text_width(cancel_action) + s(28.0));
        let primary = (bx + box_w - pad - primary_w, footer_y, primary_w, s(32.0));
        let cancel = (primary.0 - s(8.0) - cancel_w, primary.1, cancel_w, s(32.0));
        let test_w = s(96.0).max(text_width(test_action) + s(24.0));
        let test = (bx + pad, footer_y, test_w, s(32.0));
        let test_status_x = test.0 + test.2 + s(8.0);
        let test_status = (
            test_status_x,
            footer_y,
            (cancel.0 - s(8.0) - test_status_x).max(0.0),
            s(32.0),
        );

        self.nebula_ssh_editor_rects = Some(SshEditorRects {
            close,
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
            test,
            test_status,
            primary,
            cancel,
        });

        let status_tooltip = if self.nebula_ssh_editor_hover == SshEditorHit::TestStatus {
            match &editor.test {
                SshTestState::Failed { summary } => {
                    let max_cols = (((field_w - s(24.0)) / cell_w).floor() as usize).max(12);
                    let lines = wrap_status_tooltip(summary, max_cols);
                    let widest = lines
                        .iter()
                        .map(|line| line.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>())
                        .max()
                        .unwrap_or(0);
                    let line_h = cell_h + s(3.0);
                    let width = (widest as f32 * cell_w + s(24.0)).clamp(s(180.0), field_w);
                    let height = lines.len() as f32 * line_h + s(16.0);
                    let x = bx + box_w - pad - width;
                    let y = (footer_top - height - s(8.0)).max(by + s(10.0));
                    Some(((x, y, width, height), lines, line_h))
                },
                _ => None,
            }
        } else {
            None
        };

        // SSH 主机编辑器是 Modal：遮罩、外阴影、同心描边、圆角全部走共享
        // 配方。此前这里的遮罩是写死的 `Rgba::new(0, 0, 0, 170)`——不读
        // `skin.veil`，于是浅色主题下也罩一层 67% 的纯黑，比深色主题还重；
        // 圆角 10/11 也和别处的 8 对不上。
        let mut quads = Vec::new();
        super::ui::surface::push_surface(
            &mut quads,
            (bx, by, box_w, box_h),
            (size.width(), size.height()),
            scale,
            &skin,
            super::ui::surface::Elevation::Modal,
            progress,
        );
        quads.push(UiQuad::solid(
            bx,
            footer_top,
            box_w,
            box_h - (footer_top - by),
            0.0,
            super::ui::surface::fade(skin.surface, progress),
        ));
        quads.push(UiQuad::solid(
            bx,
            footer_top,
            box_w,
            s(1.0),
            0.0,
            super::ui::surface::fade(skin.hairline, progress),
        ));
        if self.nebula_ssh_editor_hover == SshEditorHit::Close {
            quads.push(UiQuad::solid(
                close.0,
                close.1,
                close.2,
                close.3,
                s(5.0),
                skin.hover,
            ));
        }
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
            s(7.0),
            skin.hairline,
        ));
        quads.push(UiQuad::solid(
            auth_track.0,
            auth_track.1,
            auth_track.2,
            auth_track.3,
            s(6.0),
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
            if self.nebula_ssh_editor_hover == SshEditorHit::AddPrivateKey {
                quads.push(UiQuad::solid(
                    add_private_key.0,
                    add_private_key.1,
                    add_private_key.2,
                    add_private_key.3,
                    s(5.0),
                    skin.hover,
                ));
            }
            if editor.private_keys.is_empty() && show_empty_key_state {
                let empty = (destination.0, key_rows_y, field_w, s(40.0));
                quads.push(UiQuad::solid(empty.0, empty.1, empty.2, empty.3, s(6.0), skin.input));
                dashed_border(&mut quads, empty, skin.hairline, scale);
            }
            for (index, row, remove) in &visible_keys {
                quads.push(UiQuad::solid(
                    row.0,
                    row.1,
                    row.2,
                    row.3,
                    s(6.0),
                    if self.nebula_ssh_editor_hover == SshEditorHit::RemovePrivateKey(*index) {
                        skin.hover
                    } else {
                        skin.input
                    },
                ));
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
            test,
            cancel,
            primary,
            self.nebula_ssh_editor_hover,
            &skin,
            scale,
            editor.focus.current(),
            show_password,
            accent,
        );
        let status_dot = match editor.test {
            SshTestState::Ok { .. } => Some(Rgba::new(63, 185, 80, 255)),
            SshTestState::Failed { .. } => Some(Rgba::new(248, 81, 73, 255)),
            SshTestState::Idle | SshTestState::Running { .. } => None,
        };
        if let Some(color) = status_dot {
            quads.push(UiQuad::solid(
                test.0 + test.2 + s(12.0),
                footer_y + (test.3 - s(6.0)) * 0.5,
                s(6.0),
                s(6.0),
                s(3.0),
                color,
            ));
        }

        // Footer focus belongs to buttons, so leave the text caret in the two
        // input fields only; otherwise it appears to edit a field while Enter
        // is actually activating Test/Cancel/Save.
        if editor.focus.current() <= usize::from(show_password) {
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
                SshEditorField::Password => (
                    password,
                    editor.password.chars().count(),
                    editor.password_selection.is_selected(),
                ),
            };
            // 选区需要持续可见；普通插入光标使用共享 500ms 相位，由
            // `chrome_editor_active` 的 8fps 时钟持续推进帧。
            if selected || super::caret_blink_on() {
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
            }
        }
        if let Some((tooltip, ..)) = &status_tooltip {
            quads.push(UiQuad::solid(
                tooltip.0 - s(1.0),
                tooltip.1 - s(1.0),
                tooltip.2 + s(2.0),
                tooltip.3 + s(2.0),
                s(7.0),
                skin.hairline,
            ));
            quads.push(UiQuad::solid(
                tooltip.0,
                tooltip.1,
                tooltip.2,
                tooltip.3,
                s(6.0),
                skin.panel,
            ));
        }
        self.renderer.draw_ui(&size, &quads);

        let glyph_cache = &mut self.glyph_cache;
        self.renderer.draw_ui_text(
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
            close.0 + (close.2 - text_width("×")) * 0.5,
            close.1 + (close.3 - cell_h) * 0.5,
            if self.nebula_ssh_editor_hover == SshEditorHit::Close {
                skin.icon_hover
            } else {
                skin.icon
            },
            "×",
            glyph_cache,
        );
        let destination_label = language.pick("连接地址", "Destination");
        let destination_label_y = destination.1 - cell_h - s(8.0);
        self.renderer.draw_chrome_text(
            &size,
            destination.0,
            destination_label_y,
            skin.ink_dim,
            destination_label,
            glyph_cache,
        );
        let destination_hint = editor.error.as_deref().unwrap_or(language.pick(
            "支持 user@host，非 22 端口写 ssh://host:2222",
            "Supports user@host; use ssh://host:2222 for a non-22 port",
        ));
        let destination_hint_scale = 0.72;
        self.renderer.draw_ui_text(
            &size,
            destination.0,
            destination.1 + destination.3 + s(6.0),
            destination_hint_scale,
            if editor.error.is_some() {
                if skin.is_light { Rgb::new(207, 34, 46) } else { Rgb::new(248, 81, 73) }
            } else {
                skin.ink_dim
            },
            Flags::empty(),
            destination_hint,
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
            auth_y - cell_h - s(8.0),
            skin.ink_dim,
            language.pick("认证方式", "Authentication"),
            glyph_cache,
        );
        let auth_labels = if language == super::UiLanguage::ZhCn {
            ["自动", "密码", "密钥", "交互式"]
        } else {
            ["Auto", "Password", "Key", "Interactive"]
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
            if editor.private_keys.is_empty() && show_empty_key_state {
                self.renderer.draw_chrome_text(
                    &size,
                    destination.0 + s(12.0),
                    key_rows_y + (s(40.0) - cell_h) * 0.5,
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
        for (rect, label, ink) in [
            (test, test_action, skin.ink),
            (cancel, cancel_action, skin.ink),
            (primary, primary_action, Rgb::new(8, 12, 20)),
        ] {
            self.renderer.draw_chrome_text(
                &size,
                rect.0 + (rect.2 - text_width(label)) * 0.5,
                rect.1 + (rect.3 - cell_h) * 0.5,
                ink,
                label,
                glyph_cache,
            );
        }

        let (status, status_ink, has_dot) = match &editor.test {
            SshTestState::Idle => (None, skin.ink_faint, false),
            SshTestState::Running { .. } => (
                Some(language.pick("正在连接…", "Connecting...").to_owned()),
                skin.ink_faint,
                false,
            ),
            SshTestState::Ok { elapsed_ms } => (
                Some(format!(
                    "{} · {elapsed_ms}ms",
                    language.pick("连接成功", "Connected")
                )),
                if skin.is_light { Rgb::new(26, 127, 55) } else { Rgb::new(63, 185, 80) },
                true,
            ),
            SshTestState::Failed { summary } => (
                Some(summary.replace(['\r', '\n'], " ")),
                if skin.is_light { Rgb::new(207, 34, 46) } else { Rgb::new(248, 81, 73) },
                true,
            ),
        };
        if let Some(status) = status {
            let status_x = test.0 + test.2 + s(if has_dot { 24.0 } else { 12.0 });
            let max_cols = (((cancel.0 - s(12.0) - status_x) / cell_w).floor() as isize).max(0);
            if max_cols > 0 {
                let shown = truncate_tab_label(&status, max_cols as usize);
                self.renderer.draw_chrome_text(
                    &size,
                    status_x,
                    footer_y + (test.3 - cell_h) * 0.5,
                    status_ink,
                    &shown,
                    glyph_cache,
                );
            }
        }
        if let Some((tooltip, lines, line_h)) = status_tooltip {
            for (line, text) in lines.iter().enumerate() {
                self.renderer.draw_chrome_text(
                    &size,
                    tooltip.0 + s(12.0),
                    tooltip.1 + s(8.0) + line as f32 * line_h,
                    skin.ink,
                    text,
                    glyph_cache,
                );
            }
        }

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

/// Wrap the complete SSH error by terminal display columns. Error chains often
/// contain long host/key paths, so wrapping by bytes or scalar count would
/// either split UTF-8 or let CJK text cross the tooltip edge.
fn wrap_status_tooltip(value: &str, budget: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source in value.replace('\r', "").split('\n') {
        if source.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut width = 0usize;
        for ch in source.chars() {
            let char_width = ch.width().unwrap_or(0).max(1);
            if !current.is_empty() && width + char_width > budget {
                lines.push(std::mem::take(&mut current));
                width = 0;
            }
            current.push(ch);
            width += char_width;
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() { vec![String::new()] } else { lines }
}

#[cfg(test)]
mod tests {
    use super::{ssh_editor_vertical_layout, wrap_status_tooltip};
    use unicode_width::UnicodeWidthChar;

    #[test]
    fn ssh_status_tooltip_keeps_the_complete_utf8_error_within_columns() {
        let source = "认证失败：私钥路径 C:/用户/密钥/id_ed25519 不可用";
        let lines = wrap_status_tooltip(source, 12);
        assert_eq!(lines.concat(), source);
        assert!(lines.iter().all(|line| {
            line.chars().map(|ch| ch.width().unwrap_or(0)).sum::<usize>() <= 12
        }));
    }

    #[test]
    fn ssh_editor_groups_keep_crap_proximity_rhythm() {
        let cell_h = 15.0;
        let layout = ssh_editor_vertical_layout(true, true, 0, cell_h);
        let auth_bottom = layout.auth_y + 32.0;
        let password_label_y = layout.password_y - cell_h - 8.0;
        let save_bottom = layout.save_toggle_y + 28.0;

        assert!((password_label_y - auth_bottom - 18.0).abs() < f32::EPSILON);
        assert!((layout.key_header_y - save_bottom - 24.0).abs() < f32::EPSILON);
        assert!(layout.key_rows_y > layout.key_header_y);
        assert!(layout.desired_h > layout.key_rows_y + 42.0);
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
    test: Rect,
    cancel: Rect,
    primary: Rect,
    hover: SshEditorHit,
    skin: &theme::Skin,
    scale: f32,
    focus: usize,
    shows_password: bool,
    accent: Rgba,
) {
    let s = |value: f32| value * scale;
    for rect in [test, cancel] {
        quads.push(UiQuad::solid(
            rect.0 - s(1.0),
            rect.1 - s(1.0),
            rect.2 + s(2.0),
            rect.3 + s(2.0),
            s(7.0),
            skin.hairline,
        ));
    }
    let test_focus = if shows_password { 2 } else { 1 };
    let cancel_focus = if shows_password { 3 } else { 2 };
    let primary_focus = if shows_password { 4 } else { 3 };
    for rect in [
        (focus == test_focus).then_some(test),
        (focus == cancel_focus).then_some(cancel),
        (focus == primary_focus).then_some(primary),
    ]
    .into_iter()
    .flatten()
    {
        quads.push(UiQuad::solid(
            rect.0 - s(2.0),
            rect.1 - s(2.0),
            rect.2 + s(4.0),
            rect.3 + s(4.0),
            s(8.0),
            skin.accent_soft,
        ));
    }
    quads.push(UiQuad::solid(test.0, test.1, test.2, test.3, s(6.0), skin.surface));
    quads.push(UiQuad::solid(cancel.0, cancel.1, cancel.2, cancel.3, s(6.0), skin.hover));
    quads.push(UiQuad::solid(primary.0, primary.1, primary.2, primary.3, s(6.0), accent));
    if hover == SshEditorHit::Test {
        quads.push(UiQuad::solid(test.0, test.1, test.2, test.3, s(6.0), skin.hover));
    }
    if hover == SshEditorHit::Cancel {
        quads.push(UiQuad::solid(cancel.0, cancel.1, cancel.2, cancel.3, s(6.0), skin.hover_strong));
    }
    if hover == SshEditorHit::Primary {
        quads.push(UiQuad::solid(
            primary.0,
            primary.1,
            primary.2,
            primary.3,
            s(6.0),
            Rgba::new(accent.r, accent.g, accent.b, 224),
        ));
    }
}

/// UiQuad 没有虚线描边 primitive；这里用短线段拼一圈，只用于私钥空态。
fn dashed_border(quads: &mut Vec<UiQuad>, rect: Rect, color: Rgba, scale: f32) {
    let s = |value: f32| value * scale;
    let dash = s(5.0).max(2.0);
    let gap = s(4.0).max(2.0);
    let stroke = s(1.0).max(1.0);
    let mut x = rect.0 + s(5.0);
    while x < rect.0 + rect.2 - s(5.0) {
        let width = dash.min(rect.0 + rect.2 - s(5.0) - x);
        quads.push(UiQuad::solid(x, rect.1, width, stroke, 0.0, color));
        quads.push(UiQuad::solid(x, rect.1 + rect.3 - stroke, width, stroke, 0.0, color));
        x += dash + gap;
    }
    let mut y = rect.1 + s(5.0);
    while y < rect.1 + rect.3 - s(5.0) {
        let height = dash.min(rect.1 + rect.3 - s(5.0) - y);
        quads.push(UiQuad::solid(rect.0, y, stroke, height, 0.0, color));
        quads.push(UiQuad::solid(rect.0 + rect.2 - stroke, y, stroke, height, 0.0, color));
        y += dash + gap;
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
        password.1 - cell_h - s(8.0),
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

fn path_tail(path: &std::path::Path, max_chars: usize) -> String {
    let value = path.to_string_lossy();
    let count = value.chars().count();
    if count <= max_chars {
        value.into_owned()
    } else {
        format!("…{}", value.chars().skip(count - max_chars + 1).collect::<String>())
    }
}
