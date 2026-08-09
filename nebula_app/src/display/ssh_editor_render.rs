use unicode_width::UnicodeWidthChar;

use super::ssh_connect::{cols_that_fit, rgb_of, truncate_cols};
use super::ssh_ui::{SshTestState, auth_sections};
use super::ui::theme;
use super::ui::tokens::{control, radius, space, type_scale};
use super::*;
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

/// 测试失败的原因最多铺几行。四行装得下绝大多数错误链；再长的尾巴收进
/// 悬浮层——但前四行必须直接可见，排障的人不该先去发现"这行字能悬浮"。
const STATUS_ROWS_MAX: usize = 4;

/// 身份条：头像方块的边长，以及右边名字输入框的高（设计单位，照原型）。
const AVATAR_H: f32 = 46.0;
const IDENT_NAME_H: f32 = 30.0;
/// 名字用比正文大一档的字（原型 15px / 基准 12.5px）。这一行是整张表单里
/// 唯一一处"标题级"的输入——它答的是"这台机器叫什么"，其余都是参数。
const IDENT_NAME_SCALE: f32 = 1.2;

/// 图标选择器弹出层的宽度与行高（设计单位）。
const ICON_POPUP_W: f32 = 232.0;
const ICON_ROW_H: f32 = 28.0;
/// 列表最多同时显示几个**可点的行**，再多就滚动。
///
/// 二十二个形状一次摊开有 600 多像素，会盖住半张表单——而这是这张表单里
/// 最不需要改的字段。给它一屏七行的窗口，其余交给滚轮和搜索。
const ICON_ROWS_MAX: usize = 7;

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
    /// 身份条：头像 + 名字 + 地址副行，以及它下面那条分隔线。
    ident_y: f32,
    ident_rule_y: f32,
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
    /// 代理覆盖三态分段器；自定义时下面跟一行 URL 输入框。
    proxy_y: f32,
    proxy_url_y: Option<f32>,
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

/// 推导整张卡片的纵向布局。`cell_h` 是逻辑像素的 UI 行高；`status_lines`
/// 是测试状态条要占的行数（0 = 没有状态条，失败原因折行后可能多行）。
fn editor_layout(
    show_password: bool,
    show_keys: bool,
    show_proxy_url: bool,
    note_lines: usize,
    key_rows: usize,
    status_lines: usize,
    cell_h: f32,
) -> EditorLayout {
    let caption_h = cell_h * type_scale::SECTION_CAPTION;
    let support_h = cell_h * type_scale::SUPPORTING;
    // 组内字段之间的呼吸：比 XS 更紧，让同组字段读起来是一块。
    const FIELD_GAP: f32 = 6.0;

    let mut l = EditorLayout::default();
    let mut y = HEAD_H + space::S;

    // ── 身份条 ────────────────────────────────────────────────
    // 图标 + 名字提到最顶上，合成一张「这台机器是谁」的卡片。它俩是同一件
    // 事的两半——一个给眼睛认，一个给嘴巴念——分开塞进下面的字段表里，就都
    // 退化成了可填可不填的杂项。它下面的「连接 / 认证」则纯粹是**怎么连上
    // 去**，与身份无关，所以中间用一条分隔线断开。
    l.ident_y = y;
    y += AVATAR_H;
    l.ident_rule_y = y + space::S;
    y = l.ident_rule_y + space::S;

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
    // 代理覆盖属于「怎么连上去」，跟地址端口同组；URL 行只在自定义时存在。
    l.proxy_y = gy;
    gy += CTL_H;
    if show_proxy_url {
        gy += FIELD_GAP;
        l.proxy_url_y = Some(gy);
        gy += CTL_H;
    }
    gy += space::S;
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
    if status_lines > 0 {
        l.teststate_y = Some(y);
        y += space::XS * 2.0 + support_h * status_lines as f32;
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
        let note_cols = ((ctl_w + s(LABEL_W) + s(space::S)) / (cell_w * type_scale::SUPPORTING))
            .floor()
            .max(8.0) as usize;
        let note_lines = wrap_status_tooltip(note_text, note_cols).len().max(1);

        // 私钥最多展示四行，更多条目保留尾部（最近添加项）。
        let key_rows = if show_keys { editor.private_keys.len().clamp(1, KEY_ROWS_MAX) } else { 0 };
        // 失败原因在这里先折好行：布局要按行数给状态条留高，绘制按行铺开，
        // 两边必须拿同一份折行结果，各折各的迟早对不上。
        let status_cols = (((content_w - s(space::S)) / (cell_w * type_scale::SUPPORTING)).floor())
            .max(12.0) as usize;
        let (status_wrapped, status_truncated) = match &editor.test {
            SshTestState::Failed { summary } => {
                let mut lines = wrap_status_tooltip(summary, status_cols);
                let truncated = lines.len() > STATUS_ROWS_MAX;
                if truncated {
                    lines.truncate(STATUS_ROWS_MAX);
                    if let Some(last) = lines.last_mut() {
                        // 末行钉上"未完"记号；超宽由 truncate 兜住。
                        *last = truncate_tab_label(&format!("{last} …"), status_cols);
                    }
                }
                (lines, truncated)
            },
            _ => (Vec::new(), false),
        };
        let status_lines = match &editor.test {
            SshTestState::Idle => 0,
            SshTestState::Failed { .. } => status_wrapped.len().max(1),
            _ => 1,
        };
        let show_proxy_url = editor.proxy_choice == ssh_ui::SshProxyChoice::Custom;
        let v = editor_layout(
            show_password,
            show_keys,
            show_proxy_url,
            note_lines,
            key_rows,
            status_lines,
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

        let close = (
            bx + box_w - s(space::S) - s(CTL_H),
            by + (s(HEAD_H) - s(CTL_H)) * 0.5,
            s(CTL_H),
            s(CTL_H),
        );
        let destination = (ctl_x, by + s(v.dest_y), ctl_w, field_h);
        let port = (ctl_x, by + s(v.port_y), s(PORT_W), field_h);
        // 身份条：[头像][名字 / 地址]。两者垂直居中对齐，头像跨着名字和它
        // 下面那行地址——地址是名字的注脚，不是另一个条目。
        let ident_x = bx + s(space::M);
        let avatar = (ident_x, by + s(v.ident_y), s(AVATAR_H), s(AVATAR_H));
        let ident_text_x = avatar.0 + avatar.2 + s(space::S);
        let ident_text_w = bx + box_w - s(space::M) - ident_text_x;
        let host_label = (ident_text_x, avatar.1, ident_text_w, s(IDENT_NAME_H));
        let ident_sub_y = host_label.1 + host_label.3 + s(2.0);

        let auth_track = (ctl_x, by + s(v.auth_y), ctl_w, field_h);
        let auth_pad = s(2.0);
        let auth_w = (auth_track.2 - auth_pad * 2.0) / 4.0;
        // 顺序按「人挑哪个」排，不按枚举定义排（2026-08-01 用户裁定）：密码
        // 和密钥是明确的意图，放前面；自动是"都试试"的兜底，交互式最少用。
        // 默认值仍是 Auto——顺序只关排版，不改语义。
        let auth_modes = [
            SshAuthMode::Password,
            SshAuthMode::PublicKey,
            SshAuthMode::Auto,
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
        // 代理覆盖分段器：三段（跟随全局 / 直连 / 自定义），轨道与认证方式
        // 的分段器同款；URL 输入行只在「自定义」时存在。
        let proxy_track = (ctl_x, by + s(v.proxy_y), ctl_w, field_h);
        let proxy_pad = s(2.0);
        let proxy_w = (proxy_track.2 - proxy_pad * 2.0) / 3.0;
        let proxy_choices = [
            ssh_ui::SshProxyChoice::Follow,
            ssh_ui::SshProxyChoice::Direct,
            ssh_ui::SshProxyChoice::Custom,
        ];
        let proxy = std::array::from_fn(|index| {
            (
                proxy_choices[index],
                (
                    proxy_track.0 + proxy_pad + index as f32 * proxy_w,
                    proxy_track.1 + proxy_pad,
                    proxy_w,
                    proxy_track.3 - proxy_pad * 2.0,
                ),
            )
        });
        let proxy_url = match v.proxy_url_y {
            Some(y) => (ctl_x, by + s(y), ctl_w, field_h),
            None => zero,
        };
        let password =
            if show_password { (ctl_x, by + s(v.password_y), ctl_w, field_h) } else { zero };
        let password_toggle = if show_password {
            (password.0 + password.2 - s(30.0), password.1 + s(2.0), s(28.0), password.3 - s(4.0))
        } else {
            zero
        };
        let save_label =
            language.pick("保存到 Windows 凭据管理器", "Save in Windows Credential Manager");
        let save_toggle = if show_password {
            (ctl_x, by + s(v.save_y), (s(24.0) + text_width(save_label)).min(ctl_w), s(26.0))
        } else {
            zero
        };
        let save_checkbox = if show_password {
            (save_toggle.0, save_toggle.1 + s(5.0), s(16.0), s(16.0))
        } else {
            zero
        };

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
        // 失败时有几行折行就有几行高，命中区（悬浮）也跟着长。
        let teststate = v.teststate_y.map(|y| {
            (
                bx + s(space::M),
                by + s(y) + s(space::XS),
                content_w,
                cell_h * type_scale::SUPPORTING * status_lines.max(1) as f32,
            )
        });
        let test_status = teststate.unwrap_or(zero);
        let footer_top = by + s(v.footer_y);

        // ── 图标选择器 ────────────────────────────────────────────
        // 默认收起。二十二个形状常驻摊开，会把"你正在编一台机器"变成"你正
        // 在挑一个贴纸"——图标是这张表单里最不需要改的字段，它本来就有值。
        let zh = language == super::UiLanguage::ZhCn;
        let picker_rows = if editor.icon_picker {
            super::ui::os_icons::picker_rows(&editor.icon_filter, zh)
        } else {
            Vec::new()
        };
        let icon_group_h = s(20.0);
        let icon_row_h = s(ICON_ROW_H);
        let list_budget = s(ICON_ROWS_MAX as f32 * ICON_ROW_H);
        let (icon_max_scroll, visible_icon_rows) = icon_list_window(
            &picker_rows,
            editor.icon_scroll,
            list_budget,
            icon_group_h,
            icon_row_h,
        );
        let mut list_h = visible_icon_rows.iter().map(|(_, _, h)| h).sum::<f32>();
        // 一条也不剩时列表不能是零高——那样浮层会塌成一条缝，看起来像坏了。
        // 留一行的位置写「没有匹配的图标」。
        let icon_empty = editor.icon_picker && picker_rows.is_empty();
        if icon_empty {
            list_h = icon_row_h;
        }
        let popup_pad = s(space::XXS);
        let search_h = s(26.0);
        let (icon_popup, icon_search, icon_rows) = if editor.icon_picker {
            let w = s(ICON_POPUP_W).min(box_w - s(space::M) * 2.0);
            let h = popup_pad * 2.0 + search_h + s(space::XXS) + list_h;
            let x = avatar.0.min(bx + box_w - s(space::M) - w);
            // 下面放不下就翻到头像**上方**：浮层宁可换边也不该滑出卡片，
            // 半截露在终端上的列表既点不全也读不出属于谁。
            let below = avatar.1 + avatar.3 + s(space::XXS);
            let y = if below + h > stage.1 + stage.3 - s(space::S) {
                (avatar.1 - s(space::XXS) - h).max(stage.1 + s(space::S))
            } else {
                below
            };
            let popup = (x, y, w, h);
            let search = (x + popup_pad, y + popup_pad, w - popup_pad * 2.0, search_h);
            let list_top = search.1 + search.3 + s(space::XXS);
            let rows = visible_icon_rows
                .iter()
                .filter_map(|(row, offset, h)| match row {
                    super::ui::os_icons::PickerRow::Option(pick) => {
                        Some((*pick, (search.0, list_top + offset, search.2, *h)))
                    },
                    super::ui::os_icons::PickerRow::Group(_) => None,
                })
                .collect::<Vec<_>>();
            (popup, search, rows)
        } else {
            (zero, zero, Vec::new())
        };

        // 文字起点：绘制和命中共用这一份。端口居中，其余靠左；端口空着时起点
        // 落在框正中，于是光标停在中央而不是贴着左边等着。
        let pad = s(space::XS);
        let port_shown = editor.display_text(SshEditorField::Port);
        let metrics = ssh_ui::SshFieldMetrics {
            destination_x: destination.0 + pad,
            port_x: port.0 + (port.2 - text_width(&port_shown)).max(0.0) * 0.5,
            label_x: host_label.0 + pad,
            password_x: password.0 + pad,
            proxy_url_x: proxy_url.0 + pad,
            cell_w,
            // 名字按 1.2× 真栅格化，绘制的步进是**未取整** UI advance 的
            // 1.2 倍；cell_w 是 floor 过的，拿它乘列宽每个字差零点几像素，
            // 点到第十个字光标就漂进邻格了。
            label_cell_w: self.glyph_cache.ui_font_metrics().average_advance as f32
                * IDENT_NAME_SCALE,
        };

        self.nebula_ssh_editor_rects = Some(SshEditorRects {
            close,
            destination,
            port,
            label: host_label,
            password,
            password_toggle,
            auth,
            proxy,
            proxy_url,
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
            avatar,
            icon_popup,
            icon_search,
            icon_search_text_x: icon_search.0 + pad,
            icon_search_cell_w: cell_w * type_scale::SUPPORTING,
            icon_rows: icon_rows.clone(),
            icon_max_scroll,
            metrics,
        });

        let status_tooltip =
            if status_truncated && self.nebula_ssh_editor_hover == SshEditorHit::TestStatus {
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
            self.nebula_density,
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
            super::ui::surface::push_group(
                &mut quads,
                group,
                scale,
                &skin,
                self.nebula_density,
                progress,
            );
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
        // 身份条的名字框：默认**没有框**，只有字。它是这张卡片的标题，一上
        // 来就画一圈边会让整块顶部变成"又一个表单行"；悬浮时给一条发丝提示
        // 可点，真编辑时才落下输入框的底和强调边。
        {
            let focused = editor.field == SshEditorField::Label;
            if focused {
                super::ui::surface::push_stroke(
                    &mut quads,
                    host_label,
                    s(radius::CONTROL),
                    scale,
                    Rgba::new(accent.r, accent.g, accent.b, if skin.is_light { 118 } else { 136 }),
                );
                quads.push(UiQuad::solid(
                    host_label.0,
                    host_label.1,
                    host_label.2,
                    host_label.3,
                    s(radius::CONTROL),
                    skin.input,
                ));
            } else if self.nebula_ssh_editor_hover == SshEditorHit::Label {
                super::ui::surface::push_stroke(
                    &mut quads,
                    host_label,
                    s(radius::CONTROL),
                    scale,
                    skin.hairline,
                );
                // 描边是**实心矩形**，得有东西盖住中心才成环。这里要的是
                // 透明底，所以拿它和面板色合成出的那个不透明色去盖。
                quads.push(UiQuad::solid(
                    host_label.0,
                    host_label.1,
                    host_label.2,
                    host_label.3,
                    s(radius::CONTROL),
                    Rgba::new(skin.panel.r, skin.panel.g, skin.panel.b, 255),
                ));
            }
        }
        // 头像沿用输入框的皮：它和右边的名字是同一条身份里的两半，长得像
        // 输入框才读得出"这也是你能改的"。开着列表时按聚焦态描边——弹层是
        // 从它身上长出来的，得看得出源头。
        //
        // 圆角走**浮层**那一档而不是控件档：46px 的方块用 6px 圆角会显得
        // 生硬，原型这里也是 r-overlay。
        {
            let open = editor.icon_picker;
            let hovered = self.nebula_ssh_editor_hover == SshEditorHit::Avatar;
            super::ui::surface::push_stroke(
                &mut quads,
                avatar,
                s(radius::OVERLAY),
                scale,
                if open || hovered {
                    Rgba::new(accent.r, accent.g, accent.b, if skin.is_light { 118 } else { 136 })
                } else {
                    skin.hairline
                },
            );
            quads.push(UiQuad::solid(
                avatar.0,
                avatar.1,
                avatar.2,
                avatar.3,
                s(radius::OVERLAY),
                skin.input,
            ));
            if hovered && !open {
                quads.push(UiQuad::solid(
                    avatar.0,
                    avatar.1,
                    avatar.2,
                    avatar.3,
                    s(radius::OVERLAY),
                    skin.hover,
                ));
            }
        }
        // 身份条与下面那两组之间的分隔线：上面答的是"这是谁"，下面答的是
        // "怎么连上去"，两件事。
        quads.push(UiQuad::solid(
            bx + s(space::M),
            by + s(v.ident_rule_y),
            content_w,
            s(control::HAIRLINE),
            0.0,
            hairline,
        ));
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
        // 代理覆盖：分段轨道与选中片和认证方式是同一套组件语言。
        super::ui::surface::push_stroke(
            &mut quads,
            proxy_track,
            s(radius::CONTROL),
            scale,
            skin.hairline,
        );
        quads.push(UiQuad::solid(
            proxy_track.0,
            proxy_track.1,
            proxy_track.2,
            proxy_track.3,
            s(radius::CONTROL),
            super::ui::surface::over(skin.surface, group_fill),
        ));
        for (choice, rect) in proxy {
            let active = editor.proxy_choice == choice;
            let hovered = self.nebula_ssh_editor_hover == SshEditorHit::ProxyChoice(choice);
            if active || hovered {
                quads.push(UiQuad::solid(
                    rect.0,
                    rect.1,
                    rect.2,
                    rect.3,
                    s(radius::CHIP),
                    if active { skin.panel } else { skin.hover },
                ));
            }
        }
        if show_proxy_url {
            input_quads(
                &mut quads,
                proxy_url,
                editor.field == SshEditorField::ProxyUrl,
                self.nebula_ssh_editor_hover == SshEditorHit::ProxyUrl,
                accent,
                &skin,
                scale,
            );
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
            editor.slots().get(editor.focus.current()).copied(),
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
                // 和侧栏标签行同一个组件（`ui::icons::push_spinner`）：这里原来
                // 有一份 8 点的私货，点少到能数出珠子。轨道用 hairline、亮弧用
                // accent——"正在连接"是这张表单里唯一在动的东西，值得用品牌色。
                super::ui::icons::push_spinner(
                    &mut quads,
                    bar.0 + s(5.5),
                    cy,
                    s(5.5),
                    spinner_phase(),
                    skin.hairline,
                    accent,
                    Rgba::new(skin.panel.r, skin.panel.g, skin.panel.b, 255),
                );
            }
        }

        // Footer focus belongs to buttons, so leave the text caret in the input
        // fields only; otherwise it appears to edit a field while Enter is
        // actually activating Test/Cancel/Save.
        if matches!(
            editor.slots().get(editor.focus.current()),
            Some(ssh_ui::SshEditorSlot::Field(_))
        ) {
            let caret_field = match editor.field {
                SshEditorField::Password if !show_password => SshEditorField::Destination,
                SshEditorField::ProxyUrl if !show_proxy_url => SshEditorField::Destination,
                field => field,
            };
            let caret_rect = match caret_field {
                SshEditorField::Destination => destination,
                SshEditorField::Port => port,
                SshEditorField::Label => host_label,
                SshEditorField::Password => password,
                SshEditorField::ProxyUrl => proxy_url,
            };
            // 光标、选区、命中三者共用 metrics 的起点与**同一份列宽**，所以
            // 点在哪一格，光标就落在哪一格——名字那一格比别处宽一档，这里跟
            // 着字段取，不能图省事全用 `cell_w`。
            super::ui::text_field::push_cursor(
                &mut quads,
                caret_rect.1,
                caret_rect.3,
                metrics.origin(caret_field),
                &editor.display_text(caret_field),
                editor.field_view(caret_field).1,
                metrics.cell_w_of(caret_field),
                scale,
                &skin,
            );
        }
        // tooltip 与图标选择器弹层不在这一批里：主批 quad 整体沉在表单
        // 文字之下，浮层的底混在这里就压不住字，表单文字会从弹层里透出
        // 来。它们在表单文字画完后作为 overlay 批另行提交（见下）。
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
        //
        // 两个都换成 codicon 的**空心**字形（原来是 Font Awesome 的实心机架
        // 和实心钥匙）。实心图标在这个尺寸上是一块墨，形状全靠外轮廓；空心
        // 的把内部结构留出来，读得出"这是机架的那几层""这是钥匙的那个齿"，
        // 而且墨量小得多——分组标题本来就该轻，一块实心墨会把它顶到比标题
        // 文字还重。整个 chrome 的图标语言也统一在 codicon 这一套上（齿轮、
        // 关闭、图钉早就是它）。
        for (head_y, icon, text) in [
            (v.conn_head_y, "\u{eb50}", language.pick("连接", "Connection")),
            (v.auth_head_y, "\u{eb11}", language.pick("认证", "Authentication")),
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

        // 端口只剩它自己一行了（名字已经提到身份条），但循环留着：把"画
        // label + 画内容/占位"这套规则写两遍，正是两个框慢慢长歪的开头。
        for (row_y, text, rect, value, placeholder, field) in [(
            v.port_y,
            language.pick("端口", "Port"),
            port,
            &editor.port,
            "22",
            SshEditorField::Port,
        )] {
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

        // ── 代理覆盖 ───────────────────────────────────────────────
        self.renderer.draw_ui_text(
            &size,
            field_x,
            label_y(by + s(v.proxy_y)),
            support,
            skin.ink_dim,
            Flags::empty(),
            language.pick("代理", "Proxy"),
            glyph_cache,
        );
        let proxy_labels = if language == super::UiLanguage::ZhCn {
            ["跟随全局", "直连", "自定义"]
        } else {
            ["Global", "Direct", "Custom"]
        };
        for ((choice, rect), label) in proxy.iter().zip(proxy_labels) {
            self.renderer.draw_chrome_text(
                &size,
                rect.0 + (rect.2 - text_width(label)) * 0.5,
                rect.1 + (rect.3 - cell_h) / 2.0,
                if editor.proxy_choice == *choice { skin.ink_strong } else { skin.ink_dim },
                label,
                glyph_cache,
            );
        }
        if show_proxy_url {
            let value = &editor.proxy_url;
            let (shown, x) = if value.is_empty() {
                ("socks5://127.0.0.1:7890", inner_x(proxy_url))
            } else {
                (value.as_str(), metrics.origin(SshEditorField::ProxyUrl))
            };
            self.renderer.draw_chrome_text(
                &size,
                x,
                inner_y(by + s(v.proxy_url_y.unwrap_or_default())),
                if value.is_empty() { skin.ink_faint } else { skin.ink },
                shown,
                glyph_cache,
            );
        }

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
            ["密码", "密钥", "自动", "交互式"]
        } else {
            ["Password", "Key", "Auto", "Interactive"]
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

        let (status_rows, status_ink, has_dot) = match &editor.test {
            SshTestState::Idle => (Vec::new(), skin.ink_faint, false),
            SshTestState::Running { .. } => (
                vec![language.pick("正在连接…", "Connecting...").to_owned()],
                skin.ink_faint,
                // 转圈指示器和成功/失败的圆点占同一格，文字的左缩进因此一致
                // ——不然状态在三态之间切换时，文字会横着跳一下。
                true,
            ),
            SshTestState::Ok { elapsed_ms } => (
                vec![format!("{} · {elapsed_ms}ms", language.pick("连接成功", "Connected"))],
                if skin.is_light { Rgb::new(26, 127, 55) } else { Rgb::new(63, 185, 80) },
                true,
            ),
            // 失败原因逐行铺开。此前这里把换行压成空格、截成一行，完整原因
            // 只活在悬浮层里——排障的人第一眼就该读到全文，而不是先发现
            // "这行字原来能悬浮"。
            SshTestState::Failed { .. } => (
                status_wrapped,
                if skin.is_light { Rgb::new(207, 34, 46) } else { Rgb::new(248, 81, 73) },
                true,
            ),
        };
        if let Some(bar) = teststate {
            // 状态条有几行，失败原因就铺几行；圆点缩进对齐每一行的左沿。
            let dot_w = if has_dot { s(space::S) } else { 0.0 };
            let max_cols = (((bar.2 - dot_w) / cell_w).floor() as isize).max(0);
            if max_cols > 0 {
                for (row, text) in status_rows.iter().enumerate() {
                    let shown = truncate_tab_label(text, max_cols as usize);
                    self.renderer.draw_ui_text(
                        &size,
                        bar.0 + dot_w,
                        bar.1 + row as f32 * cell_h * support,
                        support,
                        status_ink,
                        Flags::empty(),
                        &shown,
                        glyph_cache,
                    );
                }
            }
        }

        // ── 身份条 ───────────────────────────────────────────────
        // 名字：整张表单里唯一一处标题级的输入。空着时给一句招呼而不是
        // 「可选」——这一行的意思是"给它起个名字"，不是"这里还有个字段"。
        let name_empty = editor.label.is_empty();
        // 真栅格化（draw_ui_text 按 1.2× 字号重新出字形），不做位图拉伸——
        // `draw_chrome_text_scaled` 是把终端字号的图集位图硬放大，名字作为
        // 整张卡最大的一行字，糊得最扎眼。原型这行就是真 15px。
        self.renderer.draw_ui_text(
            &size,
            metrics.label_x,
            host_label.1 + (host_label.3 - cell_h * IDENT_NAME_SCALE) * 0.5,
            IDENT_NAME_SCALE,
            if name_empty { skin.ink_faint } else { skin.ink },
            Flags::empty(),
            if name_empty {
                language.pick("给这台机器起个名字", "Name this machine")
            } else {
                editor.label.as_str()
            },
            glyph_cache,
        );
        // 地址副行：名字的注脚，跟着上面的地址框实时变。它是等宽的——地址
        // 是机器读的东西，用等宽排能一眼看出点分十进制的对齐。
        let ident_sub = if editor.destination.is_empty() {
            language.pick("未填地址", "No address yet").to_owned()
        } else {
            ssh_ui::join_destination_port(&editor.destination, &editor.port)
        };
        let sub_cols = ((ident_text_w - s(space::XS)) / (cell_w * support)).floor().max(4.0);
        self.renderer.draw_ui_text(
            &size,
            metrics.label_x,
            ident_sub_y,
            support,
            skin.ink_faint,
            Flags::empty(),
            &truncate_tab_label(&ident_sub, sub_cols as usize),
            glyph_cache,
        );

        // ── 浮层批：tooltip 与图标选择器弹层 ─────────────────────
        // quad 与文字分两阶段提交，主批 quad 整体沉在表单文字之下。浮层
        // 的底必须反过来压住表单文字，所以在文字画完后另起一批提交；浮
        // 层自己的文字再叠在这批之上。
        let mut overlay_quads: Vec<UiQuad> = Vec::new();
        if let Some((tooltip, ..)) = &status_tooltip {
            super::ui::surface::push_stroke(
                &mut overlay_quads,
                *tooltip,
                s(radius::CONTROL),
                scale,
                skin.hairline,
            );
            overlay_quads.push(UiQuad::solid(
                tooltip.0,
                tooltip.1,
                tooltip.2,
                tooltip.3,
                s(radius::CONTROL),
                skin.panel,
            ));
        }
        if editor.icon_picker {
            // 和模态卡同一个 `panel` 底，靠 hairline + 外阴影分层——原型如此。
            // 换个更亮的底色也能分开，但那样界面里就多出一档只在这里出现的
            // 颜色；层级本来就该由阴影表达。
            super::ui::surface::push_surface_in(
                &mut overlay_quads,
                icon_popup,
                stage,
                stage_radius,
                scale,
                &skin,
                self.nebula_density,
                super::ui::surface::Elevation::Menu,
                1.0,
            );
            // 搜索框：正经输入框——弹层开着它就是聚焦的（键盘归它），按
            // 聚焦态描边；光标与选区走组件层，和表单字段同一套节律。
            input_quads(&mut overlay_quads, icon_search, true, false, accent, &skin, scale);
            super::ui::text_field::push_cursor(
                &mut overlay_quads,
                icon_search.1,
                icon_search.3,
                icon_search.0 + s(space::XS),
                &editor.icon_filter,
                &editor.icon_filter_cursor,
                cell_w * support,
                scale,
                &skin,
            );
            for (pick, rect) in &icon_rows {
                let picked = match pick {
                    Some(index) => super::ui::os_icons::CATALOG[*index].id == editor.icon,
                    // 「自动识别」= 没存过 id。空串和显式的 "auto" 都算它，
                    // 这样手改过配置文件的人也能看到自己那一项是选中的。
                    None => editor.icon.is_empty() || editor.icon == super::ui::os_icons::AUTO_ID,
                };
                let hovered = self.nebula_ssh_editor_hover == SshEditorHit::IconOption(*pick);
                if picked || hovered {
                    overlay_quads.push(UiQuad::solid(
                        rect.0,
                        rect.1,
                        rect.2,
                        rect.3,
                        s(radius::CHIP),
                        if picked { skin.accent_soft } else { skin.hover },
                    ));
                }
            }
        }
        if !overlay_quads.is_empty() {
            self.renderer.draw_ui(&size, &overlay_quads);
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

        // ── 头像里的形状 ─────────────────────────────────────────
        // 「自动识别」在真连上之前先用通用终端的形状顶着：头像不能是空的，
        // 一个空框会让人以为图标丢了。
        let picked_icon = super::ui::os_icons::resolve(Some(editor.icon.as_str()));
        let avatar_px = avatar.3 * 0.46;
        let avatar_mult = super::ui::os_icons::scale_for(picked_icon, cell_w, avatar_px);
        // 同名字：真栅格化。图标要放大到两倍上下，拉伸版在 46px 方块里
        // 糊成一团墨渍（原型 `.nf` 是真 24px）。
        self.renderer.draw_ui_text(
            &size,
            avatar.0 + (avatar.2 - avatar_px) * 0.5,
            avatar.1 + (avatar.3 - cell_h * avatar_mult) * 0.5,
            avatar_mult,
            if editor.icon_picker { skin.ink } else { skin.ink_dim },
            Flags::empty(),
            picked_icon.glyph.encode_utf8(&mut [0u8; 4]),
            glyph_cache,
        );

        // ── 选择器弹层的文字 ─────────────────────────────────────
        if editor.icon_picker {
            let filter_shown = !editor.icon_filter.is_empty();
            self.renderer.draw_ui_text(
                &size,
                icon_search.0 + s(space::XS),
                icon_search.1 + (icon_search.3 - cell_h * support) * 0.5,
                support,
                if filter_shown { skin.ink } else { skin.ink_faint },
                Flags::empty(),
                if filter_shown {
                    editor.icon_filter.as_str()
                } else {
                    language.pick("搜索图标…（直接打字）", "Search icons… (just type)")
                },
                glyph_cache,
            );
            let caption = type_scale::SECTION_CAPTION;
            let list_top = icon_search.1 + icon_search.3 + s(space::XXS);
            for (row, offset, h) in &visible_icon_rows {
                let y = list_top + offset;
                match row {
                    super::ui::os_icons::PickerRow::Group(title) => {
                        self.renderer.draw_ui_text_tracked(
                            &size,
                            icon_search.0 + s(space::XS),
                            y + (h - cell_h * caption) * 0.5,
                            caption,
                            s(0.65),
                            skin.ink_faint,
                            Flags::empty(),
                            title,
                            glyph_cache,
                        );
                    },
                    super::ui::os_icons::PickerRow::Option(pick) => {
                        let picked = match pick {
                            Some(index) => super::ui::os_icons::CATALOG[*index].id == editor.icon,
                            None => {
                                editor.icon.is_empty()
                                    || editor.icon == super::ui::os_icons::AUTO_ID
                            },
                        };
                        let ink = if picked { skin.ink_strong } else { skin.ink_dim };
                        // 图标列和名字列的节奏跟侧栏主机行一致：图标有自己的
                        // 槽，槽后固定气口再排字。两处对不齐的话，"我在列表里
                        // 挑的形状"和"侧栏里长出来的形状"就读不成同一个东西。
                        let slot = h * 0.62;
                        let glyph_x = icon_search.0 + s(space::XS);
                        let text_x = glyph_x + slot + s(space::XS);
                        match pick {
                            Some(index) => {
                                let icon = &super::ui::os_icons::CATALOG[*index];
                                let mult =
                                    super::ui::os_icons::scale_for(icon, cell_w, slot * 0.86);
                                // 列表里的图标同样走真栅格化，跟头像出自
                                // 同一张脸——挑的时候看的和挑完看到的必须
                                // 是同一个清晰度。
                                self.renderer.draw_ui_text(
                                    &size,
                                    glyph_x + slot * 0.07,
                                    y + (h - cell_h * mult) * 0.5,
                                    mult,
                                    ink,
                                    Flags::empty(),
                                    icon.glyph.encode_utf8(&mut [0u8; 4]),
                                    glyph_cache,
                                );
                                self.renderer.draw_ui_text(
                                    &size,
                                    text_x,
                                    y + (h - cell_h * support) * 0.5,
                                    support,
                                    ink,
                                    Flags::empty(),
                                    if zh { icon.zh } else { icon.en },
                                    glyph_cache,
                                );
                                // 码位贴右缘。这是给"照着配置文件找过来"的
                                // 人留的锚点，所以用最淡的一档——它是注解，
                                // 不参与挑选。
                                let cp = format!("U+{:X}", icon.glyph as u32);
                                let cp_w = cp.chars().count() as f32
                                    * self.renderer.ui_text_advance(glyph_cache, caption);
                                self.renderer.draw_ui_text(
                                    &size,
                                    icon_search.0 + icon_search.2 - s(space::XS) - cp_w,
                                    y + (h - cell_h * caption) * 0.5,
                                    caption,
                                    skin.ink_faint,
                                    Flags::empty(),
                                    &cp,
                                    glyph_cache,
                                );
                            },
                            None => {
                                self.renderer.draw_ui_text(
                                    &size,
                                    text_x,
                                    y + (h - cell_h * support) * 0.5,
                                    support,
                                    ink,
                                    Flags::empty(),
                                    language.pick(
                                        "自动识别（连上后按系统认）",
                                        "Automatic (detect on first connect)",
                                    ),
                                    glyph_cache,
                                );
                            },
                        }
                    },
                }
            }
            if icon_empty {
                self.renderer.draw_ui_text(
                    &size,
                    icon_search.0 + s(space::XS),
                    list_top + (icon_row_h - cell_h * support) * 0.5,
                    support,
                    skin.ink_faint,
                    Flags::empty(),
                    language.pick("没有匹配的图标", "No matching icon"),
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
    use super::{
        AVATAR_H, CTL_H, DESIGN_CELL_W, editor_layout, icon_list_window, type_scale, ui_scale,
        wrap_status_tooltip,
    };
    use crate::display::ui::os_icons::{PickerRow, picker_rows};
    use unicode_width::UnicodeWidthChar;

    /// 失败原因折成几行，状态条就长几行高——错误全文必须直接可见，压成
    /// 单行等于把原因藏回悬浮层。
    #[test]
    fn the_test_status_bar_grows_with_the_wrapped_error() {
        let single = editor_layout(true, false, false, 1, 0, 1, 15.0);
        let triple = editor_layout(true, false, false, 1, 0, 3, 15.0);
        let support_h = 15.0 * type_scale::SUPPORTING;
        assert!(
            (triple.height - single.height - support_h * 2.0).abs() < 0.01,
            "三行状态该比一行高出恰好两行，实际差 {}",
            triple.height - single.height
        );
        // 状态条从同一处起笔：长的是它自己，顺带把页脚推下去。
        assert_eq!(single.teststate_y, triple.teststate_y);
        assert!(triple.footer_y > single.footer_y);
        // 没有状态时整条不存在，不占一行空白。
        assert!(editor_layout(true, false, false, 1, 0, 0, 15.0).teststate_y.is_none());
    }

    /// 列表的窗口按**高度**开，不按行数——标题矮一档，按行数除会算错。
    #[test]
    fn the_icon_list_window_never_overruns_its_height_budget() {
        let rows = picker_rows("", true);
        let (_, visible) = icon_list_window(&rows, 0, 196.0, 20.0, 28.0);
        let used: f32 = visible.iter().map(|(_, _, h)| h).sum();
        assert!(used <= 196.0, "窗口用了 {used}px，预算只有 196px");
        assert!(visible.len() >= 7, "196px 至少能放下七行，实际只有 {}", visible.len());
        // 偏移必须首尾相接：中间空一档会被读成"这里有一行没画出来"。
        let mut expected = 0.0;
        for (_, offset, h) in &visible {
            assert!((offset - expected).abs() < 0.01);
            expected += h;
        }
    }

    /// 滚到底时最后一行正好贴着下沿。按 `len - 可见行数` 算会多滚出一段空
    /// 白，看起来像下面还有东西却怎么也滚不出来。
    #[test]
    fn scrolling_to_the_bottom_lands_flush_on_the_last_row() {
        let rows = picker_rows("", true);
        let (max_scroll, _) = icon_list_window(&rows, 0, 196.0, 20.0, 28.0);
        assert!(max_scroll > 0, "二十多个图标塞不进 196px，理应可滚");

        let (_, bottom) = icon_list_window(&rows, max_scroll, 196.0, 20.0, 28.0);
        assert_eq!(
            bottom.last().map(|(row, ..)| *row),
            rows.last().copied(),
            "滚到底该看见最后一行"
        );
        // 再往下滚也是同一屏——夹取在窗口里做掉，输入侧不必自己算上限。
        let (_, past_end) = icon_list_window(&rows, max_scroll + 5, 196.0, 20.0, 28.0);
        assert_eq!(past_end.len(), bottom.len());
    }

    /// 全放得下时不该报出可滚——一个滚不动的滚动条比没有更让人困惑。
    #[test]
    fn a_short_list_reports_no_scroll() {
        let rows = picker_rows("ubuntu", true);
        let (max_scroll, visible) = icon_list_window(&rows, 0, 196.0, 20.0, 28.0);
        assert_eq!(max_scroll, 0);
        assert_eq!(visible.len(), rows.len());
        assert!(matches!(visible[0].0, PickerRow::Group(_)));
    }

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
        assert!(
            lines.iter().all(|line| {
                line.chars().map(|ch| ch.width().unwrap_or(0)).sum::<usize>() <= 12
            })
        );
    }

    #[test]
    fn editor_layout_stacks_fields_without_overlap() {
        let l = editor_layout(true, true, false, 1, 2, 1, 15.0);
        // 身份条在最上面，它下面那条分隔线把"这是谁"和"怎么连"隔开，连接
        // 组从线以下开始。
        assert!(l.ident_y + AVATAR_H <= l.ident_rule_y);
        assert!(l.ident_rule_y < l.conn_group.0);
        // 连接组的三行按视觉顺序推进，helper 夹在地址和端口之间。
        assert!(l.dest_y + CTL_H <= l.helper_y);
        assert!(l.helper_y < l.port_y);
        assert!(l.port_y + CTL_H <= l.conn_group.0 + l.conn_group.1);
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
        let full = editor_layout(true, true, false, 1, 4, 0, 15.0);
        // keyboard-interactive：没有密码框也没有私钥列表，只剩一句说明。
        let bare = editor_layout(false, false, false, 1, 0, 0, 15.0);
        assert!(bare.height < full.height);
        // 说明文案折成两行时，认证组要跟着长高——否则文字会顶穿组框下缘。
        let two_lines = editor_layout(false, false, false, 2, 0, 0, 15.0);
        assert!(two_lines.auth_group.1 > bare.auth_group.1);
        assert!(
            two_lines.note_y + 15.0 * 2.0 * 0.8 <= two_lines.auth_group.0 + two_lines.auth_group.1
        );
    }

    #[test]
    fn editor_layout_grows_with_the_private_key_list() {
        let one = editor_layout(false, true, false, 1, 1, 0, 15.0);
        let four = editor_layout(false, true, false, 1, 4, 0, 15.0);
        assert!(four.height > one.height);
        assert!(four.add_key_y > one.add_key_y);
    }
}

/// 选择器列表的可视窗口：`(最大滚动量, [(行, 行内偏移, 行高)])`。
///
/// 分组标题比选项矮一档，所以窗口只能按**高度**算，不能按行数除。
///
/// 最大滚动量从**末尾**倒推：滚到底时最后一行正好贴着列表下沿。按
/// `len - 可见行数` 算是不对的——那是拿"从顶上看能显示几行"去推"从底下
/// 数该留几行"，两头行高不一样时会多滚出一段空白，看起来像列表下面还有
/// 东西却怎么也滚不出来。
fn icon_list_window(
    rows: &[super::ui::os_icons::PickerRow],
    scroll: usize,
    budget: f32,
    group_h: f32,
    option_h: f32,
) -> (usize, Vec<(super::ui::os_icons::PickerRow, f32, f32)>) {
    use super::ui::os_icons::PickerRow;
    let height = |row: &PickerRow| match row {
        // 分组标题比选项矮：它是路牌不是选项，占同样的高度会让人去点它。
        PickerRow::Group(_) => group_h,
        PickerRow::Option(_) => option_h,
    };

    let mut max_scroll = rows.len();
    let mut tail = 0.0;
    for (index, row) in rows.iter().enumerate().rev() {
        tail += height(row);
        if tail > budget {
            break;
        }
        max_scroll = index;
    }

    let mut visible = Vec::new();
    let mut used = 0.0;
    for row in rows.iter().skip(scroll.min(max_scroll)) {
        let h = height(row);
        if used + h > budget {
            break;
        }
        visible.push((*row, used, h));
        used += h;
    }
    (max_scroll, visible)
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
    focused: Option<ssh_ui::SshEditorSlot>,
    accent: Rgba,
) {
    let s = |value: f32| value * scale;
    for rect in [test, cancel] {
        super::ui::surface::push_stroke(quads, rect, s(radius::CONTROL), scale, skin.hairline);
    }
    // 焦点高亮按 Tab 环上的停靠点判定（[`SshHostEditor::slots`]），字段增删
    // 不再牵动这里的序号。
    for rect in [
        (focused == Some(ssh_ui::SshEditorSlot::Test)).then_some(test),
        (focused == Some(ssh_ui::SshEditorSlot::Cancel)).then_some(cancel),
        (focused == Some(ssh_ui::SshEditorSlot::Save)).then_some(primary),
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
