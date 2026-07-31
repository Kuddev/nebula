use unicode_width::UnicodeWidthChar;

use super::ssh_connect::{cols_that_fit, rgb_of, truncate_cols};
use super::ssh_ui::{SshTestState, auth_sections};
use super::*;
use super::ui::theme;
use super::ui::tokens::{control, radius, space, type_scale};
use crate::ssh_profiles::SshAuthMode;

type Rect = (f32, f32, f32, f32);

/// 卡片宽度，单位是**设计单位**（见 [`ui_scale`]）。
const EDITOR_W: f32 = 440.0;
/// 头部高度：标题 + 关闭按钮那一条。
const HEAD_H: f32 = 48.0;
/// 行式字段的 label 左列宽。定宽才能让所有控件左缘对齐成一条线——
/// 让 label 自己撑宽会把控件推成参差不齐的锯齿。
const LABEL_W: f32 = 84.0;
/// 控件高，同时也是最小点击区。
const CTL_H: f32 = control::MIN_HIT_TARGET;
/// 端口输入框宽度：够放 5 位端口号，再宽就是浪费横向空间。
const PORT_W: f32 = 76.0;
/// 私钥行高。
const KEY_ROW_H: f32 = 30.0;
/// 私钥最多显示几行，超出保留尾部（最近添加的）。
const KEY_ROWS_MAX: usize = 4;

/// 这套尺寸是按 12.5px 等宽字排的，那个字号下一格的推进宽度就是这个数。
const DESIGN_CELL_W: f32 = 6.9;

/// 面板尺寸相对设计稿的放大倍数。
///
/// 上面那些常量（440 宽、84 的 label 列、32 的控件高）都是按 12.5px 的字定
/// 的。但面板里的字跟着终端字号走：用户把终端调到 15px，字大了两成，盒子却
/// 还是原来那么大——留白被字吃干净，读起来就是"挤"。
///
/// 放大按**平方根**打折，不是线性跟随：字变大本身就增加了视觉重量，留白只需
/// 跟上一部分。线性跟随时字大两成、卡片也大两成，整张面板会压过它在界面里
/// 应有的分量——那是一个添加主机的表单，不是主界面。
///
/// 下限钉在 1.0：字比设计稿小时不跟着缩，否则 32px 的控件会掉到点击区以下。
/// 上限 1.3 是防止超大字号把卡片撑出窗口。
fn ui_scale(cell_w_logical: f32) -> f32 {
    (cell_w_logical / DESIGN_CELL_W).max(1.0).sqrt().min(1.3)
}

/// 纵向布局结果，单位是逻辑像素、原点在卡片左上角。
///
/// 从"绝对 y 常量"改成流式推进：字段的位置由它前面的内容决定，所以增删
/// 一行不需要手改后面每一个常量——之前 `DESTINATION_Y = 84` 那套写法，
/// 一旦在地址上面加个东西，下面全得重算。
#[derive(Debug, Clone, Default)]
struct EditorLayout {
    height: f32,
    /// 分组卡片外框 (y, h)：连接、认证。
    conn_group: (f32, f32),
    auth_group: (f32, f32),
    /// 分组标题行的 y。
    conn_head_y: f32,
    auth_head_y: f32,
    /// 「连接」组的字段行。
    dest_y: f32,
    helper_y: f32,
    port_y: f32,
    label_y: f32,
    /// 「认证」组：方式分段器，以及随方式切换的内容。
    auth_y: f32,
    note_y: f32,
    password_y: f32,
    save_y: f32,
    keys_y: f32,
    add_key_y: f32,
    /// 底部：测试状态条（无状态时为 None）与动作条。
    teststate_y: Option<f32>,
    footer_y: f32,
    footer_h: f32,
}

/// 推导整张卡片的纵向布局。`cell_h` 是逻辑像素的 UI 行高。
fn editor_layout(
    show_password: bool,
    show_keys: bool,
    note_lines: usize,
    key_rows: usize,
    has_teststate: bool,
    cell_h: f32,
) -> EditorLayout {
    let caption_h = cell_h * type_scale::SECTION_CAPTION;
    let support_h = cell_h * type_scale::SUPPORTING;
    // 组内字段之间的呼吸：比 XS 更紧，让同组字段读起来是一块。
    const FIELD_GAP: f32 = 6.0;

    let mut l = EditorLayout::default();
    let mut y = HEAD_H + space::S;

    // ── 连接组 ────────────────────────────────────────────────
    l.conn_group.0 = y;
    let mut gy = y + space::S;
    l.conn_head_y = gy;
    gy += caption_h + space::XS;
    l.dest_y = gy;
    gy += CTL_H + space::XXS;
    l.helper_y = gy;
    gy += support_h + FIELD_GAP;
    l.port_y = gy;
    gy += CTL_H + FIELD_GAP;
    l.label_y = gy;
    gy += CTL_H + space::S;
    l.conn_group.1 = gy - y;
    y = gy + space::S;

    // ── 认证组 ────────────────────────────────────────────────
    l.auth_group.0 = y;
    let mut gy = y + space::S;
    l.auth_head_y = gy;
    gy += caption_h + space::XS;
    l.auth_y = gy;
    gy += CTL_H;

    if show_password {
        gy += FIELD_GAP;
        l.password_y = gy;
        gy += CTL_H + space::XS;
        l.save_y = gy;
        gy += 26.0;
    }
    if show_keys {
        gy += FIELD_GAP;
        l.keys_y = gy;
        let rows = key_rows.max(1);
        gy += rows as f32 * (KEY_ROW_H + FIELD_GAP);
        l.add_key_y = gy;
        gy += CTL_H;
    }
    if !show_password && !show_keys {
        // 自动 / 交互式：只有一句说明，不放空字段。
        gy += space::XS;
        l.note_y = gy;
        gy += support_h * note_lines as f32;
    }
    gy += space::S;
    l.auth_group.1 = gy - y;
    y = gy + space::S;

    // ── 底部 ─────────────────────────────────────────────────
    if has_teststate {
        l.teststate_y = Some(y);
        y += space::XS * 2.0 + support_h;
    }
    l.footer_y = y;
    l.footer_h = space::S * 2.0 + CTL_H;
    l.height = y + l.footer_h;
    l
}

/// 忙碌指示器的相位，一圈 `0..1`。
///
/// 挂在挂钟上而不是累加帧增量：任何帧率下角度都是对的，掉帧只让它顿，不会
/// 让它变慢——"转得比别处慢"恰恰最容易被读成已经卡死。
fn spinner_phase() -> f32 {
    const PERIOD_MS: u128 = 900;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| (since.as_millis() % PERIOD_MS) as f32 / PERIOD_MS as f32)
}

/// 转圈的忙碌指示器。
///
/// 原型那个 `border-top-color` 圆环靠旋转得到，而 quad 是轴对齐的、转不了。
/// 所以把环离散成一圈点，让亮的那一段随相位绕行——读起来仍是"一段亮弧在
/// 转"，尾迹衰减还比纯色圆环更能表达方向。
fn push_spinner(quads: &mut Vec<UiQuad>, cx: f32, cy: f32, radius: f32, phase: f32, accent: Rgba) {
    const DOTS: usize = 8;
    let dot = (radius * 0.44).max(1.0);
    for index in 0..DOTS {
        let at = index as f32 / DOTS as f32;
        let angle = std::f32::consts::TAU * at;
        // 离头部越远越淡。平方衰减让头部足够突出，尾巴不至于糊成一个静止的环。
        let behind = (at - phase).rem_euclid(1.0);
        let strength = (1.0 - behind).powi(2);
        quads.push(UiQuad::solid(
            cx + radius * angle.cos() - dot * 0.5,
            cy + radius * angle.sin() - dot * 0.5,
            dot,
            dot,
            dot * 0.5,
            Rgba::new(accent.r, accent.g, accent.b, (40.0 + strength * 200.0) as u8),
        ));
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
        let cell_h = size.cell_height();
        let cell_w = size.cell_width();
        // `s()` 把**设计单位**换成物理像素：先按字号放大盒子，再乘 DPI。字本身
        // 不走这条路（它跟 cell 尺寸走），所以放大的只有留白和框。
        let ui = ui_scale(cell_w / scale);
        let s = |value: f32| value * scale * ui;
        let skin = self.nebula_theme.skin();
        let language = self.ui_language();
        let accent = Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, 255);
        let text_width = |text: &str| -> f32 {
            text.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w
        };
        let (show_password, show_keys) = auth_sections(editor.auth);

        // 说明文案先按可用宽度折行，行数回填给布局——布局不能假设它只有
        // 一行，否则中文说明会顶穿分组卡片的下边缘。
        let note_text = match editor.auth {
            SshAuthMode::Auto => language.pick(
                "依次尝试可用私钥，失败再询问密码。多数情况选这个就行。",
                "Tries each available key, then asks for a password. Pick this unless you need otherwise.",
            ),
            _ => language.pick(
                "不预存任何凭据，连接时在终端里按提示输入（支持两步验证）。",
                "Stores nothing; you answer the server's prompts in the terminal (2FA works).",
            ),
        };
        // 模态住在终端卡里，不是整个窗口里。用卡片矩形而不是网格 padding：
        // 卡片才是用户眼里"终端"的边界，它还带着侧栏折叠动画的位移。
        let stage = self.terminal_card_rect();
        let stage_radius = (UI_SHELL_RADIUS_LOGICAL * scale).round();
        let box_w = s(EDITOR_W).min(stage.2 - s(space::XL));
        let content_w = box_w - s(space::M) * 2.0;
        // 组内字段的可用宽度：卡片内边距 → 分组卡片内边距 → label 左列。
        let field_w = content_w - s(space::S) * 2.0;
        let ctl_w = field_w - s(LABEL_W) - s(space::S);
        let note_cols = ((ctl_w + s(LABEL_W) + s(space::S))
            / (cell_w * type_scale::SUPPORTING))
            .floor()
            .max(8.0) as usize;
        let note_lines = wrap_status_tooltip(note_text, note_cols).len().max(1);

        // 私钥最多展示四行，更多条目保留尾部（最近添加项）。
        let key_rows = if show_keys { editor.private_keys.len().clamp(1, KEY_ROWS_MAX) } else { 0 };
        let has_teststate = editor.test != SshTestState::Idle;
        let v = editor_layout(
            show_password,
            show_keys,
            note_lines,
            key_rows,
            has_teststate,
            // layout 全程用设计单位，所以行高也要换算过去。
            cell_h / scale / ui,
        );

        let box_h = s(v.height).min(stage.3 - s(space::XL));
        let bx = stage.0 + (stage.2 - box_w) * 0.5;
        let resting_y = stage.1 + (stage.3 - box_h) * 0.5;
        let by = resting_y - (1.0 - progress) * s(14.0);
        let field_h = s(CTL_H);
        // 分组卡片外框与组内字段原点。
        let group_x = bx + s(space::M);
        let field_x = group_x + s(space::S);
        let ctl_x = field_x + s(LABEL_W) + s(space::S);
        let conn_group = (group_x, by + s(v.conn_group.0), content_w, s(v.conn_group.1));
        let auth_group = (group_x, by + s(v.auth_group.0), content_w, s(v.auth_group.1));

        let close =
            (bx + box_w - s(space::S) - s(CTL_H), by + (s(HEAD_H) - s(CTL_H)) * 0.5, s(CTL_H), s(CTL_H));
        let destination = (ctl_x, by + s(v.dest_y), ctl_w, field_h);
        let port = (ctl_x, by + s(v.port_y), s(PORT_W), field_h);
        let host_label = (ctl_x, by + s(v.label_y), ctl_w, field_h);

        let auth_track = (ctl_x, by + s(v.auth_y), ctl_w, field_h);
        let auth_pad = s(2.0);
        let auth_w = (auth_track.2 - auth_pad * 2.0) / 4.0;
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
        let zero = (0.0, 0.0, 0.0, 0.0);
        let password = if show_password { (ctl_x, by + s(v.password_y), ctl_w, field_h) } else { zero };
        let password_toggle = if show_password {
            (password.0 + password.2 - s(30.0), password.1 + s(2.0), s(28.0), password.3 - s(4.0))
        } else {
            zero
        };
        let save_label = language
            .pick("保存到 Windows 凭据管理器", "Save in Windows Credential Manager");
        let save_toggle = if show_password {
            (ctl_x, by + s(v.save_y), (s(24.0) + text_width(save_label)).min(ctl_w), s(26.0))
        } else {
            zero
        };
        let save_checkbox =
            if show_password { (save_toggle.0, save_toggle.1 + s(5.0), s(16.0), s(16.0)) } else { zero };

        // 私钥行占满 label 右侧的整条控件列。
        let key_rows_y = by + s(v.keys_y);
        let visible_start = editor.private_keys.len().saturating_sub(KEY_ROWS_MAX);
        let visible_keys = if show_keys {
            editor
                .private_keys
                .iter()
                .enumerate()
                .skip(visible_start)
                .take(KEY_ROWS_MAX)
                .map(|(index, _)| {
                    let row = (
                        ctl_x,
                        key_rows_y + (index - visible_start) as f32 * s(KEY_ROW_H + 6.0),
                        ctl_w,
                        s(KEY_ROW_H),
                    );
                    let remove = (row.0 + row.2 - s(24.0), row.1 + s(5.0), s(20.0), s(20.0));
                    (index, row, remove)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let add_key_label = language.pick("+ 添加私钥", "+ Add private key");
        let add_private_key = if show_keys {
            // 链接式按钮的点击区贴着文字本身，所以宽度也要按它的真实字号算——
            // 用 1.0 的字宽会把命中区撑大两成，鼠标停在空白处却亮起来。
            let w = text_width(add_key_label) * type_scale::SUPPORTING + s(space::XS) * 2.0;
            (ctl_x, by + s(v.add_key_y), w, s(CTL_H))
        } else {
            zero
        };

        let primary_action = language.pick("保存", "Save");
        let cancel_action = language.pick("取消", "Cancel");
        let test_action = language.pick("测试连接", "Test connection");
        let footer_y = by + s(v.footer_y) + s(space::S);
        let primary_w = s(72.0).max(text_width(primary_action) + s(space::L));
        let cancel_w = s(72.0).max(text_width(cancel_action) + s(space::L));
        let primary = (bx + box_w - s(space::M) - primary_w, footer_y, primary_w, s(CTL_H));
        let cancel = (primary.0 - s(space::XS) - cancel_w, primary.1, cancel_w, s(CTL_H));
        let test_w = s(96.0).max(text_width(test_action) + s(space::L));
        let test = (bx + s(space::M), footer_y, test_w, s(CTL_H));
        // 测试状态自己占一条，不再挤在按钮之间——那里放不下真实的错误原因。
        let teststate = v.teststate_y.map(|y| {
            (bx + s(space::M), by + s(y) + s(space::XS), content_w, cell_h * type_scale::SUPPORTING)
        });
        let test_status = teststate.unwrap_or(zero);
        let footer_top = by + s(v.footer_y);

        // 文字起点：绘制和命中共用这一份。端口居中，其余靠左；端口空着时起点
        // 落在框正中，于是光标停在中央而不是贴着左边等着。
        let pad = s(space::XS);
        let port_shown = editor.display_text(SshEditorField::Port);
        let metrics = ssh_ui::SshFieldMetrics {
            destination_x: destination.0 + pad,
            port_x: port.0 + (port.2 - text_width(&port_shown)).max(0.0) * 0.5,
            label_x: host_label.0 + pad,
            password_x: password.0 + pad,
            cell_w,
        };

        self.nebula_ssh_editor_rects = Some(SshEditorRects {
            close,
            destination,
            port,
            label: host_label,
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
            metrics,
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
                    let x = bx + box_w - s(space::M) - width;
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
        super::ui::surface::push_surface_in(
            &mut quads,
            (bx, by, box_w, box_h),
            stage,
            stage_radius,
            scale,
            &skin,
            super::ui::surface::Elevation::Modal,
            progress,
        );
        let hairline = super::ui::surface::fade(skin.hairline, progress);
        // 头部与页脚各一条 hairline，把"标题 / 内容 / 动作"分成三段。页脚
        // 不再整块填 surface——原型只有一条上边线，填色会让动作区看起来
        // 比内容区更重，而它其实是从属的。
        quads.push(UiQuad::solid(bx, by + s(HEAD_H), box_w, s(control::HAIRLINE), 0.0, hairline));
        quads.push(UiQuad::solid(bx, footer_top, box_w, s(control::HAIRLINE), 0.0, hairline));
        if teststate.is_some() {
            quads.push(UiQuad::solid(
                bx,
                by + s(v.teststate_y.unwrap_or_default()),
                box_w,
                s(control::HAIRLINE),
                0.0,
                hairline,
            ));
        }

        // 分组卡片：hairline 环 + card 内芯，两者由 `push_group` 合成成一个
        // 不透明填充。分开画会让描边色渗满整张卡（见该函数的注释）。
        for group in [conn_group, auth_group] {
            super::ui::surface::push_group(&mut quads, group, scale, &skin, progress);
        }
        // 卡片内芯的实色。卡片里再嵌的东西（分段轨道）要拿它当底做合成。
        let group_fill = super::ui::surface::over(skin.card, skin.panel);
        if self.nebula_ssh_editor_hover == SshEditorHit::Close {
            quads.push(UiQuad::solid(
                close.0,
                close.1,
                close.2,
                close.3,
                s(radius::CHIP),
                skin.hover,
            ));
        }
        input_quads(
            &mut quads,
            destination,
            editor.field == SshEditorField::Destination,
            self.nebula_ssh_editor_hover == SshEditorHit::Destination,
            // 地址校验失败时用 danger 描边：错误既在下方的 helper 里说清
            // 楚，也在出错的那个框上标出来，不让用户自己找是哪一行错了。
            if editor.error.is_some() { skin.danger } else { accent },
            &skin,
            scale,
        );
        input_quads(
            &mut quads,
            port,
            editor.field == SshEditorField::Port,
            self.nebula_ssh_editor_hover == SshEditorHit::Port,
            accent,
            &skin,
            scale,
        );
        input_quads(
            &mut quads,
            host_label,
            editor.field == SshEditorField::Label,
            self.nebula_ssh_editor_hover == SshEditorHit::Label,
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
        super::ui::surface::push_stroke(
            &mut quads,
            auth_track,
            s(radius::CONTROL),
            scale,
            skin.hairline,
        );
        quads.push(UiQuad::solid(
            auth_track.0,
            auth_track.1,
            auth_track.2,
            auth_track.3,
            s(radius::CONTROL),
            // 轨道底同样要先跟卡片色合成成不透明的，否则上面那圈描边会渗满
            // 整条轨道（见 `surface::push_group`）。
            super::ui::surface::over(skin.surface, group_fill),
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
                    s(radius::CHIP),
                    // 选中片就是 `panel`，两个主题共用一个 token——原型如此。
                    //
                    // 前两轮我按"选中片必须比轨道亮"去分主题，那条规则本身是
                    // 错的：深色下原型的选中片比轨道**暗** 18 级（panel 48,53,65
                    // vs 轨道 66,71,83），读起来是按键被按进去；浅色下 panel 是
                    // 纯白，比灰轨道亮，读起来是浮起来。方向相反但对比都成立，
                    // 而"同一个语义用同一个 token"才是可维护的那条线。
                    if active { skin.panel } else { skin.hover },
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
                    s(radius::CONTROL),
                    skin.hover,
                ));
            }
            // 勾选后整块填 accent：白框白勾在深色底上和未勾选几乎读不出差别，
            // 而这个开关决定密码存不存进凭据管理器，必须一眼看出状态。
            let checked = editor.save_password;
            super::ui::surface::push_stroke(
                &mut quads,
                save_checkbox,
                s(radius::CHIP),
                scale,
                if checked { accent } else { skin.hairline },
            );
            quads.push(UiQuad::solid(
                save_checkbox.0,
                save_checkbox.1,
                save_checkbox.2,
                save_checkbox.3,
                s(radius::CHIP),
                if checked { accent } else { skin.input },
            ));
        }
        if show_keys {
            if self.nebula_ssh_editor_hover == SshEditorHit::AddPrivateKey {
                quads.push(UiQuad::solid(
                    add_private_key.0,
                    add_private_key.1,
                    add_private_key.2,
                    add_private_key.3,
                    s(radius::CHIP),
                    skin.hover,
                ));
            }
            if editor.private_keys.is_empty() {
                // 空态用虚线框：它是"这里可以放东西"的占位，不是一个已存在
                // 的条目，实线框会读成一行空数据。
                let empty = (ctl_x, key_rows_y, ctl_w, s(KEY_ROW_H));
                dashed_border(&mut quads, empty, skin.hairline, scale);
            }
            for (index, row, remove) in &visible_keys {
                super::ui::surface::push_stroke(
                    &mut quads,
                    *row,
                    s(radius::CONTROL),
                    scale,
                    skin.hairline,
                );
                quads.push(UiQuad::solid(
                    row.0,
                    row.1,
                    row.2,
                    row.3,
                    s(radius::CONTROL),
                    skin.input,
                ));
                if self.nebula_ssh_editor_hover == SshEditorHit::RemovePrivateKey(*index) {
                    quads.push(UiQuad::solid(
                        remove.0,
                        remove.1,
                        remove.2,
                        remove.3,
                        s(radius::CHIP),
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
        // 状态点挪进独立的状态条：它跟状态文字是一件事，之前贴在「测试连接」
        // 按钮右边，读起来像那个按钮自己的角标。
        if let Some(bar) = teststate {
            let cy = bar.1 + cell_h * type_scale::SUPPORTING * 0.5;
            if let Some(color) = status_dot {
                quads.push(UiQuad::solid(bar.0, cy - s(3.0), s(6.0), s(6.0), s(3.0), color));
            } else if matches!(editor.test, SshTestState::Running { .. }) {
                push_spinner(&mut quads, bar.0 + s(5.5), cy, s(5.5), spinner_phase(), accent);
            }
        }

        // Footer focus belongs to buttons, so leave the text caret in the input
        // fields only; otherwise it appears to edit a field while Enter is
        // actually activating Test/Cancel/Save.
        let text_slots = if show_password { 4 } else { 3 };
        if editor.focus.current() < text_slots {
            let caret_field = match editor.field {
                SshEditorField::Password if !show_password => SshEditorField::Destination,
                field => field,
            };
            let caret_rect = match caret_field {
                SshEditorField::Destination => destination,
                SshEditorField::Port => port,
                SshEditorField::Label => host_label,
                SshEditorField::Password => password,
            };
            // 光标、选区、命中三者共用 metrics 的起点与组件层的列换算，所以
            // 点在哪一格，光标就落在哪一格。
            super::ui::text_field::push_cursor(
                &mut quads,
                caret_rect.1,
                caret_rect.3,
                metrics.origin(caret_field),
                &editor.display_text(caret_field),
                editor.field_view(caret_field).1,
                cell_w,
                scale,
                &skin,
            );
        }
        if let Some((tooltip, ..)) = &status_tooltip {
            super::ui::surface::push_stroke(
                &mut quads,
                *tooltip,
                s(radius::CONTROL),
                scale,
                skin.hairline,
            );
            quads.push(UiQuad::solid(
                tooltip.0,
                tooltip.1,
                tooltip.2,
                tooltip.3,
                s(radius::CONTROL),
                skin.panel,
            ));
        }
        self.renderer.draw_ui(&size, &quads);

        let glyph_cache = &mut self.glyph_cache;
        let support = type_scale::SUPPORTING;
        let support_h = cell_h * support;
        // 字段 label 在自己那一行里垂直居中。
        let label_y = |row_y: f32| row_y + (field_h - support_h) * 0.5;
        // 输入框内的文字基线，左内边距与原型一致。
        let inner_y = |row_y: f32| row_y + (field_h - cell_h) * 0.5;
        let inner_x = |rect: Rect| rect.0 + s(space::XS);

        self.renderer.draw_ui_text(
            &size,
            bx + s(space::M),
            by + (s(HEAD_H) - cell_h * type_scale::DIALOG_TITLE) * 0.5,
            type_scale::DIALOG_TITLE,
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

        // ── 分组标题 ───────────────────────────────────────────────
        for (head_y, icon, text) in [
            (v.conn_head_y, "\u{f233}", language.pick("连接", "Connection")),
            (v.auth_head_y, "\u{f084}", language.pick("认证", "Authentication")),
        ] {
            // 图标和标题同色同层：它是标题的一部分，抢眼了反而把"这一组叫
            // 什么"推到第二位。
            self.renderer.draw_chrome_text(
                &size,
                field_x,
                by + s(head_y) - (cell_h - cell_h * type_scale::SECTION_CAPTION) * 0.5,
                skin.ink_dim,
                icon,
                glyph_cache,
            );
            self.renderer.draw_ui_text(
                &size,
                field_x + cell_w + s(space::XS),
                by + s(head_y),
                type_scale::SECTION_CAPTION,
                skin.ink_dim,
                Flags::empty(),
                text,
                glyph_cache,
            );
        }

        // ── 连接组的三个字段 ───────────────────────────────────────
        let dest_label = language.pick("地址", "Address");
        self.renderer.draw_ui_text(
            &size,
            field_x,
            label_y(by + s(v.dest_y)),
            support,
            skin.ink_dim,
            Flags::empty(),
            dest_label,
            glyph_cache,
        );
        // 必填星紧跟标签，而不是塞在标签文字里——它是状态标记，不是名字的
        // 一部分，颜色也必须是 danger 才读得出"缺了会拦你"。
        self.renderer.draw_ui_text(
            &size,
            field_x + text_width(dest_label) * support + s(2.0),
            label_y(by + s(v.dest_y)),
            support,
            rgb_of(skin.danger),
            Flags::empty(),
            "*",
            glyph_cache,
        );
        self.renderer.draw_chrome_text(
            &size,
            inner_x(destination),
            inner_y(by + s(v.dest_y)),
            if editor.destination.is_empty() { skin.ink_faint } else { skin.ink },
            if editor.destination.is_empty() { "user@example.com" } else { &editor.destination },
            glyph_cache,
        );
        // helper 缩进到控件列，读起来是挂在地址框下面的注解，而不是新的一行。
        let hint = editor.error.as_deref().unwrap_or(language.pick(
            "支持 user@host，也可粘贴 ssh://host:2222",
            "Supports user@host, or paste ssh://host:2222",
        ));
        self.renderer.draw_ui_text(
            &size,
            ctl_x,
            by + s(v.helper_y),
            support,
            if editor.error.is_some() { rgb_of(skin.danger) } else { skin.ink_faint },
            Flags::empty(),
            hint,
            glyph_cache,
        );

        for (row_y, text, rect, value, placeholder, field) in [
            (
                v.port_y,
                language.pick("端口", "Port"),
                port,
                &editor.port,
                "22",
                SshEditorField::Port,
            ),
            (
                v.label_y,
                language.pick("标签", "Label"),
                host_label,
                &editor.label,
                language.pick("可选，列表里显示这个名字", "Optional, shown in the list"),
                SshEditorField::Label,
            ),
        ] {
            self.renderer.draw_ui_text(
                &size,
                field_x,
                label_y(by + s(row_y)),
                support,
                skin.ink_dim,
                Flags::empty(),
                text,
                glyph_cache,
            );
            // 有内容时起点取 metrics——光标和命中用的是同一个数，三者才不会
            // 在全角字符或居中字段上互相漂移。占位文案不参与定位，它自己按
            // 同样的规则摆一次就行（端口居中，其余靠左）。
            let (shown, x) = if value.is_empty() {
                let x = if field == SshEditorField::Port {
                    rect.0 + (rect.2 - text_width(placeholder)).max(0.0) * 0.5
                } else {
                    inner_x(rect)
                };
                (placeholder, x)
            } else {
                (value.as_str(), metrics.origin(field))
            };
            self.renderer.draw_chrome_text(
                &size,
                x,
                inner_y(by + s(row_y)),
                if value.is_empty() { skin.ink_faint } else { skin.ink },
                shown,
                glyph_cache,
            );
        }
        // 端口的默认值写在框外：默认 22 是事实，不该占着输入框冒充用户填过。
        self.renderer.draw_ui_text(
            &size,
            port.0 + port.2 + s(space::XS),
            label_y(by + s(v.port_y)),
            support,
            skin.ink_faint,
            Flags::empty(),
            language.pick("默认 22", "default 22"),
            glyph_cache,
        );

        self.renderer.draw_ui_text(
            &size,
            field_x,
            label_y(by + s(v.auth_y)),
            support,
            skin.ink_dim,
            Flags::empty(),
            language.pick("方式", "Method"),
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
            // 密码的 label 和其它字段走同一条左列，helper 只负责框内内容。
            self.renderer.draw_ui_text(
                &size,
                field_x,
                label_y(by + s(v.password_y)),
                support,
                skin.ink_dim,
                Flags::empty(),
                language.pick("密码", "Password"),
                glyph_cache,
            );
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
            // 私钥 label 顶对齐第一行，不是整列居中——列会随条目变高，居中
            // 会让标签越飘越低，读不出它管的是哪一段。
            self.renderer.draw_ui_text(
                &size,
                field_x,
                label_y(by + s(v.keys_y)),
                support,
                skin.ink_dim,
                Flags::empty(),
                language.pick("私钥", "Private keys"),
                glyph_cache,
            );
            // 「添加私钥」是链接式按钮，不是实心键：它开的是系统文件对话框，
            // 分量应当低于底部的保存/取消。
            self.renderer.draw_ui_text(
                &size,
                add_private_key.0 + s(space::XXS),
                add_private_key.1 + (add_private_key.3 - support_h) * 0.5,
                support,
                if self.nebula_ssh_editor_hover == SshEditorHit::AddPrivateKey {
                    skin.ink_strong
                } else {
                    skin.accent
                },
                Flags::empty(),
                add_key_label,
                glyph_cache,
            );
            if editor.private_keys.is_empty() {
                // 空态文案必须按框宽截断：中文说明比看上去长，不截会一路画到
                // 虚线框外面，读起来像"这行字漏出来了"。
                let text = language.pick(
                    "未指定，将用 IdentityFile 与默认 id_* 私钥",
                    "None; IdentityFile and default id_* keys are used",
                );
                let cols = cols_that_fit(ctl_w - s(space::XS) * 2.0, cell_w, support);
                self.renderer.draw_ui_text(
                    &size,
                    ctl_x + s(space::XS),
                    key_rows_y + (s(KEY_ROW_H) - support_h) * 0.5,
                    support,
                    skin.ink_faint,
                    Flags::empty(),
                    &truncate_cols(text, cols),
                    glyph_cache,
                );
            }
            for (index, row, remove) in &visible_keys {
                let max_chars = (((row.2 - s(34.0)) / cell_w).floor() as usize).max(8);
                let shown = path_tail(&editor.private_keys[*index], max_chars);
                self.renderer.draw_chrome_text(
                    &size,
                    row.0 + s(space::XS),
                    row.1 + (row.3 - cell_h) * 0.5,
                    skin.ink,
                    &shown,
                    glyph_cache,
                );
                self.renderer.draw_chrome_text(
                    &size,
                    remove.0 + (remove.2 - text_width("×")) * 0.5,
                    remove.1 + (remove.3 - cell_h) * 0.5,
                    if self.nebula_ssh_editor_hover == SshEditorHit::RemovePrivateKey(*index) {
                        rgb_of(skin.danger)
                    } else {
                        skin.icon
                    },
                    "×",
                    glyph_cache,
                );
            }
        } else if !show_password {
            // 自动 / 交互式：这两种模式没有要填的东西，那就把"它会做什么"
            // 讲清楚，而不是留一片空白让人怀疑没加载完。
            for (i, line) in wrap_status_tooltip(note_text, note_cols).into_iter().enumerate() {
                self.renderer.draw_ui_text(
                    &size,
                    field_x,
                    by + s(v.note_y) + i as f32 * support_h,
                    support,
                    skin.ink_dim,
                    Flags::empty(),
                    &line,
                    glyph_cache,
                );
            }
        }
        for (rect, label, ink) in [
            (test, test_action, skin.ink),
            (cancel, cancel_action, skin.ink),
            // 主按钮的字必须跟着 accent 走。写死的近黑在深色主题上碰巧成立
            // （accent 是亮蓝），到浅色主题就是黑字压深灰 accent——对比度掉到
            // 读不出来。ink_on_accent 就是为这件事存在的。
            (primary, primary_action, skin.ink_on_accent),
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
                // 转圈指示器和成功/失败的圆点占同一格，文字的左缩进因此一致
                // ——不然状态在三态之间切换时，文字会横着跳一下。
                true,
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
        if let (Some(status), Some(bar)) = (status, teststate) {
            // 状态条独占一行，失败原因终于有地方完整显示，不再被挤在两个
            // 按钮之间截断成一句读不懂的半句话。
            let dot_w = if has_dot { s(space::S) } else { 0.0 };
            let max_cols = (((bar.2 - dot_w) / cell_w).floor() as isize).max(0);
            if max_cols > 0 {
                let shown = truncate_tab_label(&status, max_cols as usize);
                self.renderer.draw_ui_text(
                    &size,
                    bar.0 + dot_w,
                    bar.1,
                    support,
                    status_ink,
                    Flags::empty(),
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
    use super::{CTL_H, DESIGN_CELL_W, editor_layout, ui_scale, wrap_status_tooltip};
    use unicode_width::UnicodeWidthChar;

    #[test]
    fn panel_grows_with_the_font_so_padding_survives() {
        // 字号正好是设计稿的基准：一比一，不放大。
        assert!((ui_scale(DESIGN_CELL_W) - 1.0).abs() < f32::EPSILON);
        // 放大打折：字大 44%，盒子只大 20%（平方根）。线性跟随会让这张
        // 添加主机的表单压过它该有的分量。
        assert!((ui_scale(DESIGN_CELL_W * 1.44) - 1.2).abs() < 1e-5);
        assert!(ui_scale(DESIGN_CELL_W * 1.25) < 1.25);
        // 字更小时不跟着缩——32px 的控件缩下去就低于点击区了。
        assert!((ui_scale(DESIGN_CELL_W * 0.5) - 1.0).abs() < f32::EPSILON);
        // 超大字号封顶，免得卡片撑出窗口。
        assert!((ui_scale(DESIGN_CELL_W * 5.0) - 1.3).abs() < f32::EPSILON);
    }

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
    fn editor_layout_stacks_fields_without_overlap() {
        let l = editor_layout(true, true, 1, 2, true, 15.0);
        // 连接组的三行按视觉顺序推进，helper 夹在地址和端口之间。
        assert!(l.dest_y + CTL_H <= l.helper_y);
        assert!(l.helper_y < l.port_y);
        assert!(l.port_y + CTL_H <= l.label_y);
        // 两个分组不重叠，且认证组在连接组下面。
        assert!(l.conn_group.0 + l.conn_group.1 <= l.auth_group.0);
        // 认证组内部的字段都落在组框里。
        let auth_bottom = l.auth_group.0 + l.auth_group.1;
        for y in [l.auth_y, l.password_y, l.save_y, l.keys_y, l.add_key_y] {
            assert!(y >= l.auth_group.0 && y <= auth_bottom, "字段 {y} 越出认证组");
        }
        // 底部在所有内容之下，卡片高度包住页脚。
        assert!(l.teststate_y.unwrap() >= auth_bottom);
        assert!(l.footer_y >= l.teststate_y.unwrap());
        assert_eq!(l.height, l.footer_y + l.footer_h);
    }

    #[test]
    fn editor_layout_shrinks_when_the_auth_mode_needs_no_fields() {
        let full = editor_layout(true, true, 1, 4, false, 15.0);
        // keyboard-interactive：没有密码框也没有私钥列表，只剩一句说明。
        let bare = editor_layout(false, false, 1, 0, false, 15.0);
        assert!(bare.height < full.height);
        // 说明文案折成两行时，认证组要跟着长高——否则文字会顶穿组框下缘。
        let two_lines = editor_layout(false, false, 2, 0, false, 15.0);
        assert!(two_lines.auth_group.1 > bare.auth_group.1);
        assert!(two_lines.note_y + 15.0 * 2.0 * 0.8 <= two_lines.auth_group.0 + two_lines.auth_group.1);
    }

    #[test]
    fn editor_layout_grows_with_the_private_key_list() {
        let one = editor_layout(false, true, 1, 1, false, 15.0);
        let four = editor_layout(false, true, 1, 4, false, 15.0);
        assert!(four.height > one.height);
        assert!(four.add_key_y > one.add_key_y);
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
    super::ui::surface::push_stroke(
        quads,
        rect,
        s(radius::CONTROL),
        scale,
        if active {
            Rgba::new(accent.r, accent.g, accent.b, if skin.is_light { 118 } else { 136 })
        } else {
            skin.hairline
        },
    );
    quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(radius::CONTROL), skin.input));
    if hovered && !active {
        quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(radius::CONTROL), skin.hover));
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
        super::ui::surface::push_stroke(quads, rect, s(radius::CONTROL), scale, skin.hairline);
    }
    // 焦点序号跟 `ssh_editor_next_field` 的 Tab 顺序走：地址、端口、标签、
    // [密码] 之后才轮到这三个按钮。
    let test_focus = if shows_password { 4 } else { 3 };
    let cancel_focus = test_focus + 1;
    let primary_focus = test_focus + 2;
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
            s(radius::OVERLAY),
            skin.accent_soft,
        ));
    }
    let r = s(radius::CONTROL);
    // 「测试连接」和「取消」是同一档次的次要动作，同底同描边。此前取消用了更
    // 深的 hover，读起来像它比测试更重要——而真正的主动作是保存。
    //
    // 底色跟 panel 合成成不透明的：surface 只有 6–8% 不透明度，盖不住
    // `push_stroke` 那个实心环，描边色会渗满整个按钮（见 `surface::push_group`）。
    // hover 是**换**底色不是叠一层——叠加会得到 surface+hover 的和，比原型深。
    let secondary = |hovered: bool| {
        super::ui::surface::over(if hovered { skin.hover } else { skin.surface }, skin.panel)
    };
    quads.push(UiQuad::solid(
        test.0,
        test.1,
        test.2,
        test.3,
        r,
        secondary(hover == SshEditorHit::Test),
    ));
    quads.push(UiQuad::solid(
        cancel.0,
        cancel.1,
        cancel.2,
        cancel.3,
        r,
        secondary(hover == SshEditorHit::Cancel),
    ));
    // 主按钮悬浮是**提亮** 8%，跟原型的 `filter: brightness(1.08)` 一致。
    // 此前叠一层 88% 的同色，等于把它往底色方向拉暗——悬浮反而显得更沉。
    let primary_fill = if hover == SshEditorHit::Primary {
        let lift = |c: u8| (c as f32 * 1.08).min(255.0) as u8;
        Rgba::new(lift(accent.r), lift(accent.g), lift(accent.b), accent.a)
    } else {
        accent
    };
    quads.push(UiQuad::solid(primary.0, primary.1, primary.2, primary.3, r, primary_fill));
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
    let masked = if editor.password.is_empty() {
        language.pick("留空则连接时询问", "Leave blank to be asked").to_owned()
    } else if editor.show_password {
        editor.password.clone()
    } else {
        "•".repeat(editor.password.chars().count())
    };
    renderer.draw_chrome_text(
        size,
        password.0 + s(space::XS),
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
        save_toggle.0 + s(24.0),
        save_toggle.1 + (save_toggle.3 - cell_h) / 2.0,
        skin.ink_dim,
        save_label,
        glyph_cache,
    );
    if editor.save_password {
        renderer.draw_chrome_text(
            size,
            save_checkbox.0 + (save_checkbox.2 - cell_w) * 0.5,
            save_checkbox.1 + (save_checkbox.3 - cell_h) / 2.0,
            // 勾画在 accent 上，所以用 ink_on_accent——深色主题的强调色偏亮，
            // 白勾会糊在里面。
            skin.ink_on_accent,
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
