//! 设置页的设计 token 与行原语。
//!
//! 一处定义、全页生效。上一版整页只有一种字重、行距全程 8px、132 个设置项零
//! 说明——那不是文案没写，是规格散落在四千行里、只能靠纪律维持一致性的直接
//! 结果。这个模块就是那个"一处"：字号档数、字重档数、留白梯度、轨道与脏值
//! 标记，改这里全页跟着变。
//!
//! 三条贯穿全页的规则：
//!
//! 1. **层次是正交维度的乘积，不是字号堆叠。** 字号只有两档（正文 / 说明），
//!    层级交给字重（600/500/400）和位置（标题出线）。把标题一放大就会出现三
//!    套字号，那是另一种病。
//! 2. **留白表达归属，不表达距离。** 2 / 16 / 24 三级：2px 说"这两行是同一
//!    件事"，16px 说"这是两件事"，24px 说"这是两组事"。均匀间距无论调多大都
//!    只会得到一条稀疏的平线。
//! 3. **说明写后果，不写定义。**「关掉后关窗即杀掉所有 shell」比「控制会话
//!    保留行为」有用——用户在这一行要做的判断是"关掉会怎样"。

use std::time::Duration;

use gpui::{Animation, AnimationExt as _, ElementId, FontWeight, ease_out_quint, relative};

use super::*;

/// 说明文字相对正文的字号比。全页只有两档字号：正文走用户基准字号，说明小
/// 一档。
pub(super) const DESC_SCALE: f32 = 0.82;
/// label ↔ 说明。这 2px 在说"这两行是同一件事"。
const LABEL_DESC_GAP: f32 = 2.0;
/// 行内上下留白，行与行之间因此是它的两倍。用 padding 而不是行间 gap，左侧
/// 轨道才连得上——断成一截一截的话，"哪几段亮着"根本读不出来。
const ROW_PAD_Y: f32 = 8.0;
/// 紧凑密度下的同一个值（「界面外观」里的密度开关对设置页真实生效）。
const ROW_PAD_Y_COMPACT: f32 = 5.0;
/// 组与组。
pub(super) const GROUP_GAP: f32 = 24.0;
/// 轨道宽度。
const RAIL_W: f32 = 2.0;
/// 内容相对轨道的缩进。标题左对齐轨道本身、行内容缩进这么多——标题是命名者
/// 而不是组员，这个位置差比任何字重都更能表达层级。
const RAIL_INDENT: f32 = 13.0;
/// 文字列的**最小**宽度。
///
/// 一开始写的是"文本区 flex_1 + 控件右对齐"，实机怎么改都对不齐：
/// `overflow_y_scroll` 那层给子项的横向可用空间是 max-content，于是文本列取
/// 自然宽、行宽跟着文字长度走，控件右缘成了"说明右缘 + 一个常数"——说明越长
/// 的行控件越靠右，差到 280px。靠 `w_full` / stretch 都救不回来，因为根子在
/// 可用空间本身。
///
/// 所以两列都给定宽：文字列固定，控件列固定并在内部右对齐。行的总宽从此与
/// 文字长度无关，控件左右缘各自成一条竖线。代价是说明在这个宽���处换行（约
/// 32 个汉字），比之前的 640 上限窄——换来的是全页对齐，值。
const TEXT_COL_MIN_W: f32 = 320.0;
/// 控件列宽。够放下最宽的下拉（220）加一点余量；开关这类窄控件在列内右对齐，
/// ���此右缘与下拉严丝合缝。
const CTRL_COL_W: f32 = 232.0;
/// 脏值段升起的时长。
const MARK_RISE: Duration = Duration::from_millis(260);

#[derive(Clone, Copy)]
enum RowLayout {
    Standard,
}

impl SettingsPane {
    /// 一组设置的开头：标题出线。轨道不在这里画——它由组内每一行自己接续，
    /// 这样才能做到"同一条线，某几段是亮的"。
    pub(crate) fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        let base_px = self.font_size_px(cx);
        // 组间距归 section 容器的 `gap`（HTML 原型里就是 `.a-main` 自己
        // `gap:24`），组不自带 `pt`：自带的话，首个元素不是分组的页会拿不到
        // 那段留白而直接贴住页头线，而首组又会拿到"正文上留白 + 组上留白"的
        // 双份。间距是**容器**的事，不是组的事。
        // 这里**不能**写 `w_full()`。`width:100%` 依赖父的已解析宽度，链条上
        // 任何一层是 auto / max-content，100% 就解析成内容宽——于是每个分组、
        // 每一行各按自己的文字长度取宽，控件右缘参差不齐（宽窗口下差到 280px，
        // 窄窗口反而齐，因为那时被可用宽度压住了）。
        //
        // 不设宽度则走 flex 交叉轴 stretch：布局算法直接拉伸，不依赖父宽解析。
        v_flex().w_full().child(
            div()
                .pb(px(5.0))
                .text_size(px(base_px * DESC_SCALE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
    }

    /// 组与组之间的间隔。
    ///
    /// 原来这里是 32px 容器夹一条 1px 细线。线现在多余了：分组的范围由左侧
    /// 轨道表达，横线再圈一道等于同一件事说两遍，而且横线会把"一组"读成
    /// "一段到此为止"——前者是归属，后者是切断。
    /// 保留给"两块内容之间需要一口气"的非分组场景（关于页把外链沉到下面就
    /// 用它）。分组之间**不要**用它——组自己带上留白，再插一段就是两倍。
    pub(crate) fn group_divider(_cx: &Context<Self>) -> gpui::Div {
        div().w_full().h(px(GROUP_GAP - 8.0)).flex_shrink_0()
    }

    /// 设置行。`desc` 写后果；确实无后果可说的项才传空串（尽量不要有）。
    pub(crate) fn row(
        &self,
        label: &'static str,
        desc: &'static str,
        control: impl IntoElement,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        self.row_shell(label, desc, None, false, RowLayout::Standard, control, cx)
    }

    /// 带撤销的设置行：该项被覆盖过时，左侧轨道这一段亮起来，行内出现 ↶。
    ///
    /// 两个信号的时态不同，所以显示时机也不同：
    /// - 轨道亮色是**状态**（"这台机器上我动过它"），必须常显才能一眼扫完；
    /// - ↶ 是**动作**，只有真想撤销时才有用，常显就是噪音——所以 hover 才现。
    pub(crate) fn row_with_reset(
        &self,
        label: &'static str,
        desc: &'static str,
        dirty: bool,
        on_reset: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        control: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover_group = Self::row_hover_group(label);
        let reset = dirty.then(|| {
            div()
                .id(SharedString::from(format!("setting-reset-{label}")))
                .size(px(20.0))
                .rounded_md()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .invisible()
                .group_hover(hover_group, |el| el.visible())
                .hover(|el| el.bg(cx.theme().list_hover))
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new(
                        crate::gpui_shell::config::ui_language(cx)
                            .pick("还原为默认值", "Restore default"),
                    )
                    .build(window, cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| on_reset(this, window, cx)))
                .child(Icon::new(IconName::Undo2).xsmall())
                .into_any_element()
        });
        self.row_shell(label, desc, reset, dirty, RowLayout::Standard, control, cx)
    }

    /// 行 hover 组名。↶ 要跟着**整行**的 hover 显形，而不是自己被指到才现
    /// ——后者等于让用户先找到一个看不见的东西。
    fn row_hover_group(label: &'static str) -> SharedString {
        SharedString::from(format!("settings-row-{label}"))
    }

    /// 说明文字。反引号包起来的片段走等宽——这是全页 mono 唯一的用途。
    ///
    /// 我们是终端，所以等宽在界面里不能当默认字体用，否则它什么也没说。
    /// 让它只出现在路径、键帽、配置键名这类**机器读的字面量**上，它才变成
    /// 一个信号：这几个字符是可以照抄的，一个也不能改。
    ///
    /// 直接拼 `div` 做不到这件事——那样每段会各占一个 flex 盒子，一句话被
    /// 切成几块、还断不了行。所以走 `StyledText` 的 run：同一次排版里换字体，
    /// 换行照旧。
    fn desc_text(desc: &'static str, cx: &Context<Self>) -> gpui::AnyElement {
        if !desc.contains('`') {
            return desc.into_any_element();
        }
        let theme = cx.theme();
        let sans = gpui::font(theme.font_family.clone());
        let mono = gpui::font(theme.mono_font_family.clone());
        let (dim, ink) = (theme.muted_foreground, theme.foreground);

        let mut text = String::with_capacity(desc.len());
        let mut runs: Vec<gpui::TextRun> = Vec::new();
        // 奇数段在反引号内。反引号本身不进最终文本，它只是标记。
        for (ix, piece) in desc.split('`').enumerate() {
            if piece.is_empty() {
                continue;
            }
            let literal = ix % 2 == 1;
            text.push_str(piece);
            runs.push(gpui::TextRun {
                len: piece.len(),
                font: if literal { mono.clone() } else { sans.clone() },
                // 字面量比周围说明亮一档：字体已经把它分出来了，颜色再补一点
                // 权重，扫视时才不至于滑过去。
                color: if literal { ink } else { dim },
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        gpui::StyledText::new(text).with_runs(runs).into_any_element()
    }

    fn row_shell(
        &self,
        label: &'static str,
        desc: &'static str,
        reset: Option<gpui::AnyElement>,
        dirty: bool,
        layout: RowLayout,
        control: impl IntoElement,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let base_px = self.font_size_px(cx);
        let pad_y = if self.runtime.density == nebula_settings::DensityName::Compact {
            ROW_PAD_Y_COMPACT
        } else {
            ROW_PAD_Y
        };
        let text = v_flex()
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(base_px))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(label),
                    )
                    .children(reset),
            )
            .when(!desc.is_empty(), |text| {
                text.child(
                    div()
                        .mt(px(LABEL_DESC_GAP))
                        .text_size(px(base_px * DESC_SCALE))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.muted_foreground)
                        .child(Self::desc_text(desc, cx)),
                )
            });
        let control = control.into_any_element();
        let columns = match layout {
            RowLayout::Standard => h_flex()
                .w_full()
                .items_start()
                .gap_4()
                .child(text.flex_1().min_w(px(TEXT_COL_MIN_W)))
                .child(
                    h_flex()
                        .w(px(CTRL_COL_W))
                        .flex_shrink_0()
                        .justify_end()
                        .items_center()
                        .child(control),
                ),
        };
        div()
            .id(label)
            .group(Self::row_hover_group(label))
            .relative()
            .w_full()
            .flex_shrink_0()
            .pl(px(RAIL_INDENT))
            .pr_4()
            .py(px(pad_y))
            // 四角都收。这里原来只圆右侧，是为了让 hover 底看起来"从灰轨道
            // 上长出来"；轨道已经删掉，再留着左边两个直角就只是缺角。
            .rounded(px(7.0))
            .hover(|row| row.bg(theme.list_hover.opacity(0.55)))
            // 竖线整条让给状态，不再画常驻的灰轨道。
            //
            // 灰线原本表达"这几行是一组"，但那件事组标题说了一遍、24px 组间
            // 距又说了一遍，第三遍是冗余；而"这行被改过"没有别的元素在说。
            // 一个视觉通道只能有一个主人，所以给不可替代的那个。
            //
            // 何况暗色下灰线必须淡到几乎看不见才不抢戏，一旦提亮到能看清，就
            // 会和左侧导航的分割线形成两条平行的近距离竖线——页面开始变格子。
            //
            // 组的层级仍然成立：标题的字重与颜色、组间距、以及行内容相对标题
            // 的 13px 缩进。缩进不需要一条线来证明自己存在。
            .when(dirty, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .w(px(RAIL_W))
                        .bg(crate::gpui_shell::theme::settings_mark(cx))
                        .with_animation(
                            ElementId::Name(format!("settings-mark-{label}").into()),
                            Animation::new(MARK_RISE).with_easing(ease_out_quint()),
                            |mark, t| mark.h(relative(t)),
                        ),
                )
            })
            .child(columns)
    }
}
