//! 分屏 pane 的 24px 标题条，外加 tab 上的分屏数量胶囊。
//!
//! # 为什么只在分屏时出现
//!
//! 单 pane 的终端本来就是"这个 tab 就是它"，再加一条标题栏只是白扣 24px 网格
//! 高度。分屏之后情况反过来：四个 pane 长得一模一样，既分不清谁是谁，也没有
//! per-pane 的关闭入口（旧路径只有 30% 黑 veil 表达失焦）。所以标题条的存在
//! 判据就一条 [`header_visible`]：`pane_count > 1`。
//!
//! # 广播为什么不设"我是源"的标记位
//!
//! 直觉方案是给 `TerminalView` 挂一个 `broadcast_source: bool`，由宿主在切
//! 焦点/切 tab/翻转开关时同步。那是三条各自会漏的同步路径——只要有一条忘了
//! 更新，症状就是"广播莫名其妙失效"或者更糟的"关掉了还在广播"。这里改成
//! **视图无条件 emit、宿主决定要不要扇出**：`write_user_input` 每次用户输入都
//! 发一条事件，`fan_out_broadcast` 拿到后才去查发送方所在 tab 的开关。键盘
//! 焦点天然只有一个 pane 拿得到，所以"只有聚焦 pane 是源"这件事由焦点系统
//! 保证，不需要第二份记账。
//!
//! 扇出用 [`TerminalView::apply_broadcast_key`] /
//! [`TerminalView::apply_broadcast_text`]，它们**按接收方自己的 term mode 重新
//! 编码**，且不再 emit——因此天然无环。照搬源 pane 已编码好的字节是错的：
//! app-cursor / bracketed-paste / kitty 协议都是 per-pane 状态，一个 pane 开着
//! vim 的时候方向键序列跟旁边的 shell 根本不同。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ObjectFit, ParentElement as _, SharedString, Styled as _,
    StyledImage as _, Window, canvas, div, img, px, size,
};
use nebula_split::{SplitDirection, SplitTree};

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::terminal::view::{TerminalInput, TerminalView};

use super::{NebulaWorkspace, WorkspaceTab};

/// 标题条高度（逻辑 px）。终端网格自己会跟着变矮：`TerminalView` 用自身元素
/// 的实测 bounds 驱动 `set_layout`，父层少 24px 就是少 24px。
pub(super) const PANE_HEADER_H: f32 = 24.0;

/// 标题条的存在判据。见模块头。
pub(super) fn header_visible(pane_count: usize) -> bool {
    pane_count > 1
}

/// 这个 pane 是否还贴着终端卡片的上两角。
///
/// GPUI 的 `overflow_hidden` 只按**矩形**裁剪子元素，不跟圆角。标题条自带
/// 不透明底色，所以贴着卡角的那一枚必须自己收角，否则方角会盖在卡片圆角
/// 外面（用户 08-23 报的「上面一行没被圆角裁剪，越界了」）。
///
/// 收角资格沿分屏树往下传：左右分屏把左角给 first、右角给 second；上下分屏
/// 只有 first 还贴着上边，second 一个角都不占。
#[derive(Clone, Copy)]
pub(super) struct HeaderCorners {
    pub top_left: bool,
    pub top_right: bool,
}

impl HeaderCorners {
    pub(super) fn first_child(self, direction: SplitDirection) -> Self {
        match direction {
            // 左右分屏：first 在左，右角交给 second。
            SplitDirection::LeftRight => Self { top_left: self.top_left, top_right: false },
            // 上下分屏：first 独占整条上边。
            SplitDirection::TopBottom => self,
        }
    }

    pub(super) fn second_child(self, direction: SplitDirection) -> Self {
        match direction {
            SplitDirection::LeftRight => Self { top_left: false, top_right: self.top_right },
            SplitDirection::TopBottom => Self { top_left: false, top_right: false },
        }
    }
}

/// 分屏树的视觉次序（左上 → 右下）：`Split` 一律先 `first` 后 `second`，与
/// `render_split_node` 铺陈子节点的顺序同源。序号徽章和广播扇出都读这一份，
/// 所以标题条上的 "2" 永远指同一个 pane。
pub(super) fn pane_order(tree: &SplitTree<u64>) -> Vec<u64> {
    let mut out = Vec::new();
    collect_order(tree, &mut out);
    out
}

fn collect_order(tree: &SplitTree<u64>, out: &mut Vec<u64>) {
    match tree {
        SplitTree::Leaf(id) => out.push(*id),
        SplitTree::Split { first, second, .. } => {
            collect_order(first, out);
            collect_order(second, out);
        },
    }
}

/// 广播接收方：本 tab 除发送方以外的全部 pane。
pub(super) fn broadcast_targets(order: &[u64], source: u64) -> Vec<u64> {
    order.iter().copied().filter(|id| *id != source).collect()
}

/// 2×2 描边小方块：分屏 tab 的行首
/// 图标。四个描边格是内容本身，不是给图标套的方框——与图标语言的"禁方盒"
/// 不冲突。
pub(super) fn split_glyph(size_px: f32, color: Hsla) -> impl IntoElement {
    let cell = (size_px * 0.42).max(4.0);
    let gap = (size_px * 0.14).max(1.5);
    let radius = (cell * 0.28).max(1.0);
    let square =
        move || div().size(px(cell)).rounded(px(radius)).border(px(1.0)).border_color(color);
    let row = move || h_flex().gap(px(gap)).child(square()).child(square());
    v_flex().gap(px(gap)).child(row()).child(row())
}

/// 分屏数量胶囊，与同一标题栏的紧凑状态徽章保持一致。
/// （`top-tabs/TopTabItems.tsx:998-1006`：`text-[10px] px-1.5 rounded-full
/// min-w-[22px]`、前景 18% 描边、背景 60% 半透明底），但尺寸按 chrome 字号
/// 等比推导而不是钉死 10px——侧栏字号跟随配置字号，钉死会在大字号下缩成
/// 一粒米。
pub(super) fn split_badge(count: usize, label_px: f32, ink: Hsla, fill: Hsla) -> impl IntoElement {
    let height = (label_px * 1.18).max(15.0);
    h_flex()
        .flex_shrink_0()
        .h(px(height))
        .min_w(px(height * 1.25))
        .px(px(5.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border(px(1.0))
        .border_color(ink.opacity(0.18))
        .bg(fill.opacity(0.6))
        .text_size(px(label_px * 0.68))
        .font_weight(FontWeight::NORMAL)
        .text_color(ink.opacity(0.75))
        .child(SharedString::from(count.to_string()))
}

/// 胶囊在行内占的水平预算（含左侧 gap）：侧栏标题的列数换算要按同一份减法
/// 扣掉它，否则省略号会压到胶囊上。
pub(super) fn split_badge_slot_w(label_px: f32) -> f32 {
    (label_px * 1.18).max(15.0) * 1.25 + 8.0
}

/// 广播记号：中心点 + 左右各两段同心弧。fork 的 `IconName` 里没有
/// radio/broadcast（已核对 `crates/ui/src/icon.rs`），与其硬凑一个语义不对的
/// 现成图标，不如自绘——弧沿用 `sidebar.rs::spinner` 那套"沿弧铺圆点"的画法，
/// 笔画与 SVG 线性图标同粗，混在一排 ghost 按钮里不出戏。
fn broadcast_mark(side: f32, color: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            let ox = f32::from(bounds.origin.x);
            let oy = f32::from(bounds.origin.y);
            let box_side = f32::from(bounds.size.width);
            let (cx, cy) = (ox + box_side * 0.5, oy + box_side * 0.5);
            let stroke = (box_side * 0.13).max(1.2);
            // 中心点：实心，直径略大于笔画，才不会被两侧的弧压成一根线。
            let dot = stroke * 1.6;
            window.paint_quad(
                gpui::fill(
                    Bounds::new(
                        gpui::point(px(cx - dot * 0.5), px(cy - dot * 0.5)),
                        size(px(dot), px(dot)),
                    ),
                    color,
                )
                .corner_radii(px(dot * 0.5)),
            );
            // 两对同心弧，各覆盖 ±44°：右侧朝 0°、左侧朝 180°。相邻点重叠约
            // 一半，与 spinner 同一条不留缝也不过密的间距规则。
            const SPAN: f32 = 0.77; // 弧度，≈44°
            for ring in 0..2 {
                let radius = box_side * (0.26 + 0.16 * ring as f32);
                let steps = ((radius * SPAN * 2.0 / (stroke * 0.5)).ceil() as usize).clamp(6, 40);
                for centre in [0.0_f32, std::f32::consts::PI] {
                    for step in 0..=steps {
                        let t = step as f32 / steps as f32 * 2.0 - 1.0;
                        let angle = centre + t * SPAN;
                        let x0 = cx + radius * angle.cos() - stroke * 0.5;
                        let y0 = cy + radius * angle.sin() - stroke * 0.5;
                        window.paint_quad(
                            gpui::fill(
                                Bounds::new(
                                    gpui::point(px(x0), px(y0)),
                                    size(px(stroke), px(stroke)),
                                ),
                                color,
                            )
                            .corner_radii(px(stroke * 0.5)),
                        );
                    }
                }
            }
        },
    )
    .size(px(side))
}

/// 一个 pane 的标题信息：图标（AI 品牌图 / Nerd Font 字位）+ 一行标题。
struct PaneTitle {
    logo: Option<std::sync::Arc<gpui::RenderImage>>,
    glyph: Option<&'static str>,
    text: SharedString,
}

impl NebulaWorkspace {
    fn pane_title(&self, view: &Entity<TerminalView>, cx: &App, dark: bool) -> PaneTitle {
        let view = view.read(cx);
        // 与 tab 标题同一套推导，只是作用在**这个** pane 上：跑着的程序最有
        // 信息量，其次是 SSH 目标，最后回落到 cwd 末级目录。
        let program = view
            .running_program
            .clone()
            .or_else(|| view.ai_session.as_ref().map(|identity| identity.source.clone()));
        let logo = program
            .as_deref()
            .and_then(crate::display::ai_logo_for_program)
            .and_then(|logo| self.sidebar_logo_images.get(&(logo, dark)).cloned());
        let glyph = program
            .as_deref()
            .filter(|_| logo.is_none())
            .map(crate::display::program_icon)
            .or(view.ssh_destination.as_ref().map(|_| "\u{f0716}"));
        let text = match (&program, &view.ssh_destination) {
            (Some(program), _) => SharedString::from(program.clone()),
            (None, Some(destination)) => SharedString::from(destination.clone()),
            (None, None) => SharedString::from(view.tab_label()),
        };
        PaneTitle { logo, glyph, text }
    }

    /// 一个 pane 的标题条。左区整条是切焦点的命中区，右区三枚按钮各自
    /// `stop_propagation`，不会顺手把焦点也换掉。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_pane_header(
        &self,
        tab_ix: usize,
        pane_id: u64,
        ordinal: usize,
        view: &Entity<TerminalView>,
        focused: bool,
        zoomed: bool,
        broadcast: bool,
        corners: HeaderCorners,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let dark = theme.is_dark();
        let muted = theme.muted_foreground;
        let ink = if focused { theme.foreground } else { muted };
        let accent = theme.primary;
        // 聚焦 pane 的标题条比正文亮一档，失焦的贴回卡底——veil 只盖终端区，
        // 标题条自己用色阶表达焦点（把标题也压暗 30% 会让四个 pane 的标题
        // 全都糊成一片灰）。
        let bar_bg =
            if focused { theme.sidebar_accent.opacity(0.55) } else { theme.muted.opacity(0.28) };
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let chrome_family = theme.mono_font_family.clone();
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        let title_px = label_px * 0.78;
        let PaneTitle { logo, glyph, text } = self.pane_title(view, cx, dark);
        let group: SharedString = format!("pane-header-{pane_id}").into();
        let icon_ink = if focused { ink } else { muted };

        h_flex()
            .id(("pane-header", pane_id as usize))
            .group(group.clone())
            .h(px(PANE_HEADER_H))
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px(px(6.0))
            .bg(bar_bg)
            // 贴着卡角的那一枚自己收角，见 [`HeaderCorners`]。
            .when(corners.top_left, |bar| {
                bar.rounded_tl(crate::gpui_shell::theme::card_radius())
            })
            .when(corners.top_right, |bar| {
                bar.rounded_tr(crate::gpui_shell::theme::card_radius())
            })
            // 广播开启时每个 pane 都亮一条底轨：模式状态必须永远可见，不能
            // 只靠那枚小按钮的颜色——四分屏时它只有 14px。
            .when(broadcast, |bar| bar.border_b(px(1.5)).border_color(accent))
            // 标题条盖在终端元素上方：不占住命中区的话，在标题栏上按下会
            // 直接给终端起选区。
            .occlude()
            .child(
                // 左区 = 焦点命中 + 拖拽手柄。按压handler**只**挂在这里，不挂
                // 整条：挂整条的话按住广播/关闭键再手抖 4px 就会把 pane 拖出去。
                h_flex()
                    .id(("pane-header-grip", pane_id as usize))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .cursor_grab()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.focus_pane(tab_ix, pane_id, window, cx);
                            this.begin_pane_drag(tab_ix, pane_id, event.position, cx);
                        }),
                    )
                    .child(
                        // 序号：与 tab 上的数量胶囊同一份 pane_order 次序，
                        // "这是第 2 个"在标题条和标签栏上指的是同一个 pane。
                        div()
                            .flex_shrink_0()
                            .w(px(title_px * 1.1))
                            .font_family(chrome_family.clone())
                            .text_size(px(title_px * 0.92))
                            .text_color(if broadcast { accent } else { muted })
                            .child(SharedString::from(ordinal.to_string())),
                    )
                    .when_some(logo, |grip, image| {
                        grip.child(
                            img(image)
                                .size(px(title_px))
                                .flex_shrink_0()
                                .object_fit(ObjectFit::Contain),
                        )
                    })
                    .when_some(glyph, |grip, glyph| {
                        grip.child(
                            div()
                                .flex_shrink_0()
                                .font_family(symbol_family)
                                .text_size(px(title_px))
                                .text_color(icon_ink)
                                .child(glyph),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(chrome_family)
                            .text_size(px(title_px))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(ink)
                            .child(text),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(1.0))
                    .child(
                        Button::new(("pane-broadcast", pane_id as usize))
                            .ghost()
                            .xsmall()
                            .selected(broadcast)
                            .tooltip(if broadcast {
                                "关闭广播输入"
                            } else {
                                "广播输入到本标签全部分栏"
                            })
                            .child(broadcast_mark(
                                title_px,
                                if broadcast { accent } else { muted },
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.toggle_tab_broadcast(tab_ix, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("pane-zoom", pane_id as usize))
                            .icon(
                                Icon::new(if zoomed {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                })
                                .text_color(icon_ink),
                            )
                            .ghost()
                            .xsmall()
                            .selected(zoomed)
                            .tooltip(if zoomed {
                                "退出独占 (Ctrl+Shift+Enter)"
                            } else {
                                "独占放大 (Ctrl+Shift+Enter)"
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.focus_pane(tab_ix, pane_id, window, cx);
                                this.toggle_zoom(cx);
                            })),
                    )
                    .child(
                        Button::new(("pane-close", pane_id as usize))
                            .icon(Icon::new(IconName::Close).text_color(icon_ink))
                            .ghost()
                            .xsmall()
                            .tooltip("关闭此分栏")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.request_close_pane(tab_ix, pane_id, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// 翻转某个 tab 的广播开关。收敛到单 pane 的 tab 不给开——一个 pane 的
    /// 广播没有语义，留着只会让开关状态骗人。
    pub(super) fn toggle_tab_broadcast(
        &mut self,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(WorkspaceTab::Terminal { panes, broadcast, .. }) = self.tabs.get_mut(tab_ix)
        else {
            return;
        };
        if panes.len() < 2 {
            return;
        }
        *broadcast = !*broadcast;
        let on = *broadcast;
        let count = panes.len();
        // 驻留指示（底轨 + 图标常亮）才是这个模式的主信号，所以这里用 5s 的
        // toast 而不是消息栏：开关本身没有待办动作，只需要在开启的一刻说清
        // 影响面。关闭时不打扰。
        if on {
            crate::gpui_shell::toast::toast(
                window,
                cx,
                crate::display::ToastKind::Info,
                format!("广播输入已开启：键入将同步到本标签的 {count} 个分栏"),
            );
        }
        cx.notify();
    }

    /// 数量胶囊的点击动作：在本 tab 的 pane 之间按视觉次序循环切焦点。
    pub(super) fn cycle_pane_focus(
        &mut self,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(WorkspaceTab::Terminal { tree, focused, .. }) = self.tabs.get(tab_ix) else {
            return;
        };
        let order = pane_order(tree);
        if order.len() < 2 {
            return;
        }
        let at = order.iter().position(|id| *id == *focused).unwrap_or(0);
        let next = order[(at + 1) % order.len()];
        if tab_ix != self.active {
            self.activate_tab(tab_ix, window, cx);
        }
        self.focus_pane(tab_ix, next, window, cx);
    }

    /// 广播扇出。发送方所在 tab 没开广播、或只剩一个 pane 时直接返回——判断
    /// 放在这里而不是视图侧，理由见模块头。
    pub(super) fn fan_out_broadcast(
        &mut self,
        source: u64,
        input: &TerminalInput,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.tab_of_pane(source) else { return };
        let Some(WorkspaceTab::Terminal { panes, tree, broadcast, .. }) = self.tabs.get(tab_ix)
        else {
            return;
        };
        if !*broadcast || panes.len() < 2 {
            return;
        }
        let targets = broadcast_targets(&pane_order(tree), source);
        let views: Vec<Entity<TerminalView>> = targets
            .iter()
            .filter_map(|id| panes.iter().find(|pane| pane.id == *id))
            .map(|pane| pane.view.clone())
            .collect();
        for view in views {
            view.update(cx, |view, cx| match input {
                TerminalInput::Key(keystroke) => view.apply_broadcast_key(keystroke, cx),
                TerminalInput::Text { text, paste } => view.apply_broadcast_text(text, *paste, cx),
            });
        }
    }

    /// 标题条按下：先记待命态，不动结构。真正的拖拽要越过阈值才开始，否则
    /// 「点一下标题条切焦点」这个最常用的动作会变成拖拽。
    pub(super) fn begin_pane_drag(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        // 单 pane 没有「拉出去」的语义（它本来就是独立 tab）。
        let splittable = matches!(
            self.tabs.get(tab_ix),
            Some(WorkspaceTab::Terminal { panes, .. }) if panes.len() > 1
        );
        if !splittable {
            return;
        }
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        self.pane_drag = Some(PaneDrag {
            tab: tab_ix,
            pane: pane_id,
            press_x: x,
            press_y: y,
            x,
            y,
            active: false,
            detach: false,
        });
        cx.notify();
    }

    pub(super) fn update_pane_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if self.pane_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.pane_drag = None;
            cx.notify();
            return;
        }
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        // 落点判定要读 `active_terminal_area`（借 self）；先算完再拿可变引用。
        let outside = !self.active_terminal_area().is_some_and(|area| area.contains(x, y));
        let Some(drag) = self.pane_drag.as_mut() else { return };
        drag.x = x;
        drag.y = y;
        if !drag.active && drag_crossed_threshold(x - drag.press_x, y - drag.press_y) {
            drag.active = true;
        }
        if drag.active {
            drag.detach = outside;
        }
        cx.notify();
    }

    /// 待命态的 move 由 workspace 根节点转发（罩层还不存在）。已激活时罩层
    /// 独占指针，这里就不再重复喂。
    pub(super) fn continue_pending_pane_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if self.pane_drag.as_ref().is_some_and(|drag| !drag.active) {
            self.update_pane_drag(event, cx);
        }
    }

    /// 根节点 capture 阶段的释放兜底。返回是否**吃掉**了这次释放：只有真拖拽
    /// 需要吃（否则源标题条的 click 和终端选区都会再收到一次）；未过阈值的
    /// 按压是普通点击，清掉状态就放行。
    pub(super) fn release_pane_drag(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.pane_drag.as_ref().map(|drag| drag.active) {
            Some(true) => {
                self.finish_pane_drag(window, cx);
                true
            },
            Some(false) => {
                self.pane_drag = None;
                cx.notify();
                false
            },
            None => false,
        }
    }

    /// 松手：越过阈值且落在终端区之外才摘出，其余情况一律恢复原状。
    pub(super) fn finish_pane_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.pane_drag.take() else { return };
        if drag.active && drag.detach {
            self.detach_pane_to_new_tab(drag.tab, drag.pane, window, cx);
        }
        cx.notify();
    }

    /// 把一个 pane 从分屏树里摘出来、原封不动搬进紧随其后的新 tab。
    ///
    /// **绝不能走 `close_pane`**：那条路会 `shutdown()` 掉视图（连 PTY 一起
    /// 回收）。这里要的是活体搬迁——视图实体、PTY、滚动历史全部保留，只换
    /// 归属。同窗口内搬迁不需要动 `runtime_hub`：它按 `(window_id, pane_id)`
    /// 记账，两者都没变（跨窗口才需要 `move_panes_to_window`）。
    fn detach_pane_to_new_tab(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(WorkspaceTab::Terminal { panes, .. }) = self.tabs.get(tab_ix) else { return };
        if panes.len() < 2 {
            return;
        }
        let outcome = match self.tabs.get_mut(tab_ix) {
            Some(WorkspaceTab::Terminal { tree, .. }) => tree.remove_leaf(pane_id),
            _ => return,
        };
        let nebula_split::RemoveOutcome::Collapsed(next_focus) = outcome else {
            // panes.len() >= 2 时树上必然还有别的叶子，摘掉这个只会是 Collapsed。
            // 真出现 NotFound/WasRoot 说明树与 panes 已经不同步；此时啥都不做
            // 比继续搬迁安全（后者会把 pane 从两边同时摘掉，留下孤儿 PTY）。
            log::warn!("detach_pane_to_new_tab: unexpected remove outcome for pane {pane_id}");
            return;
        };
        let Some(WorkspaceTab::Terminal { panes, focused, zoomed, broadcast, .. }) =
            self.tabs.get_mut(tab_ix)
        else {
            return;
        };
        let Some(at) = panes.iter().position(|pane| pane.id == pane_id) else { return };
        let pane = panes.remove(at);
        if *focused == pane_id {
            *focused = next_focus;
        }
        *zoomed = false;
        // 剩一个 pane 就没有广播语义了（与 close_pane 同一条收敛）。
        if panes.len() < 2 {
            *broadcast = false;
        }
        // 标签元数据不继承：`custom_name` / `color` 描述的是**那个 tab**，
        // `shell_tag` 也可能记的是另一个 pane 的 shell。新 tab 从默认起。
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane_id),
            panes: vec![pane],
            focused: pane_id,
            zoomed: false,
            broadcast: false,
        };
        let at = tab_ix + 1;
        self.insert_tab_at(at, tab, super::TabMeta::default());
        self.active = at;
        self.focus_active(window, cx);
        self.sync_side_panel_to_active(true, cx);
        self.reveal_active_tab();
        cx.notify();
    }

    /// 拖拽激活期间的全窗罩层（同 tab 拖拽的指针捕获模式）+ 跟着指针走的
    /// 意图提示。提示必须有：这个手势在界面上没有静态痕迹，不告诉用户「现在
    /// 松手会怎样」，拖出去就是一次赌博。
    pub(super) fn pane_drag_overlay(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let drag = self.pane_drag.as_ref().filter(|drag| drag.active)?;
        let (x, y, detach) = (drag.x, drag.y, drag.detach);
        let theme = cx.theme();
        let hint_bg = if detach { theme.primary } else { theme.muted };
        let hint_fg = if detach { theme.primary_foreground } else { theme.muted_foreground };
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .cursor_grab()
                .on_mouse_move(cx.listener(|this, event, _, cx| {
                    this.update_pane_drag(event, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.finish_pane_drag(window, cx);
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(x + 12.0))
                        .top(px(y + 12.0))
                        .px_2()
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .bg(hint_bg)
                        .text_size(px(11.0))
                        .text_color(hint_fg)
                        .child(if detach {
                            "松手：拉出为独立标签"
                        } else {
                            "拖到终端区外可拉出"
                        }),
                )
                .into_any_element(),
        )
    }
}

/// 进行中的 pane 拖拽：按住标题条左区把这个 pane 从分屏树里拉出来。
///
/// 这是 dock（拖 tab 进终端区合成分屏）的逆操作，两者共用同一套「按压待命 →
/// 越阈值激活 → 根罩层独占指针 → 松手提交」的形状（见 `tab_drag`）。落点判据
/// 只有一条：松手时指针**离开了本 tab 的终端区**就摘出来，否则原样不动。用
/// 「离开终端区」而不是「落在标签栏上」，是因为标签栏在两种布局下位置完全
/// 不同（左侧 / 顶部），而「我把它拖到外面去了」这个意图两种布局共通。
pub(super) struct PaneDrag {
    tab: usize,
    pane: u64,
    press_x: f32,
    press_y: f32,
    /// 当前指针位置：激活后的提示气泡跟着它走。
    x: f32,
    y: f32,
    active: bool,
    /// 此刻松手会不会摘出（提示气泡与提交共用同一份判定）。
    detach: bool,
}

/// 越过这个位移才算拖拽，之前都当点击（与 tab 拖拽同一个阈值）。
fn drag_crossed_threshold(dx: f32, dy: f32) -> bool {
    dx.abs() >= super::TAB_DRAG_THRESHOLD || dy.abs() >= super::TAB_DRAG_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::{
        HeaderCorners, broadcast_targets, drag_crossed_threshold, header_visible, pane_order,
        split_badge_slot_w,
    };
    use nebula_split::{SplitDirection, SplitTree};

    fn split(first: SplitTree<u64>, second: SplitTree<u64>) -> SplitTree<u64> {
        SplitTree::Split {
            direction: SplitDirection::LeftRight,
            ratio: 0.5,
            preview_ratio: None,
            dragging: false,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    #[test]
    fn header_only_exists_once_the_tab_is_actually_split() {
        assert!(!header_visible(0));
        assert!(!header_visible(1));
        assert!(header_visible(2));
    }

    #[test]
    fn pane_order_walks_first_then_second() {
        assert_eq!(pane_order(&SplitTree::leaf(7)), vec![7]);
        let tree = split(SplitTree::leaf(1), split(SplitTree::leaf(2), SplitTree::leaf(3)));
        assert_eq!(pane_order(&tree), vec![1, 2, 3]);
        let tree = split(split(SplitTree::leaf(9), SplitTree::leaf(4)), SplitTree::leaf(5));
        assert_eq!(pane_order(&tree), vec![9, 4, 5]);
    }

    #[test]
    fn broadcast_never_echoes_back_to_the_source() {
        assert_eq!(broadcast_targets(&[1, 2, 3], 2), vec![1, 3]);
        assert_eq!(broadcast_targets(&[1], 1), Vec::<u64>::new());
        // 源不在本 tab（理论上进不来）时不该静默把所有人都打一遍之外的事：
        // 仍然只是全量转发，调用方已按 tab_of_pane 定位过。
        assert_eq!(broadcast_targets(&[1, 2], 9), vec![1, 2]);
    }

    #[test]
    fn badge_slot_tracks_the_chrome_font_size() {
        assert!(split_badge_slot_w(15.0) > split_badge_slot_w(11.0));
        // 小字号下仍保留 15px 的胶囊最小高度，槽宽因此有下限。
        assert_eq!(split_badge_slot_w(8.0), 15.0 * 1.25 + 8.0);
    }

    /// 收角资格必须**只**落在真正贴着卡角的那一枚上：多收一个角会在分屏缝
    /// 里露出圆角缺口，少收一个角就是用户报的方角越界。
    #[test]
    fn only_the_panes_touching_the_card_corners_round_themselves() {
        let root = HeaderCorners { top_left: true, top_right: true };

        // 左右分屏：左角归 first，右角归 second。
        let left = root.first_child(SplitDirection::LeftRight);
        let right = root.second_child(SplitDirection::LeftRight);
        assert!(left.top_left && !left.top_right);
        assert!(!right.top_left && right.top_right);

        // 上下分屏：上边整条都是 first 的，second 一个角都不占。
        let top = root.first_child(SplitDirection::TopBottom);
        let bottom = root.second_child(SplitDirection::TopBottom);
        assert!(top.top_left && top.top_right);
        assert!(!bottom.top_left && !bottom.top_right);

        // 三分屏（左 | 右上/右下）：只有左、右上各收一个角。
        let right_top = right.first_child(SplitDirection::TopBottom);
        let right_bottom = right.second_child(SplitDirection::TopBottom);
        assert!(!right_top.top_left && right_top.top_right);
        assert!(!right_bottom.top_left && !right_bottom.top_right);
    }

    /// 点标题条切焦点是最高频的动作；阈值以内的抖动绝不能变成「把 pane 拉出
    /// 去」这种破坏性结果。
    #[test]
    fn a_click_on_the_header_never_becomes_a_drag() {
        assert!(!drag_crossed_threshold(0.0, 0.0));
        assert!(!drag_crossed_threshold(3.9, -3.9));
        assert!(drag_crossed_threshold(4.0, 0.0));
        assert!(drag_crossed_threshold(0.0, -4.0));
        // 单轴够了就算，不要求两轴同时越界。
        assert!(drag_crossed_threshold(-12.0, 1.0));
    }
}
