//! Nebula top-bar / tab-sidebar chrome: shared geometry, hit-testing and the
//! drag model. Split out of the giant `display::mod` — every rect here is
//! consumed twice (drawing AND hit-testing), so keeping the maths in one leaf
//! module guarantees the two can never drift. Rendering itself still lives in
//! `display::mod::draw_chrome` (it touches most of `Display`'s state).
//!
//! As a child module it reaches the parent's private items (`SizeInfo`
//! constants, `SplitNav`, …) through the glob import below — same pattern as
//! `settings.rs`.

#![allow(clippy::wildcard_imports)]

use super::ui::{icons, tokens};
use super::*;

/// Result of hit-testing a pixel against the Nebula top chrome bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeHit {
    None,
    TitleBar,
    NewTab,
    /// The chevron beside "+" that opens the new-tab shell dropdown (Windows
    /// Terminal's profile menu).
    NewTabMenu,
    Tab(usize),
    TabClose(usize),
    /// The sidebar icon in the top bar that folds the left tab sidebar away and
    /// back. Lives in the title bar so it stays reachable while collapsed.
    SidebarToggle,
    /// A row in the sidebar's "SSH HOSTS" section (index into the host list).
    Host(usize),
    AddSshHost,
    /// The "TABS" section header — click toggles the accordion fold.
    TabsSection,
    /// The "SSH HOSTS" section header — click toggles the accordion fold.
    HostsSection,
    /// Fixed entry at the bottom of the sidebar. It owns a separate band and
    /// never competes with Tabs / SSH rows for height.
    MessageQueue,
    /// Top-bar toggles for the right-side drawer's two views.
    PanelFiles,
    PanelGit,
    Minimize,
    Maximize,
    Close,
}

/// An in-progress tab-bar reorder drag.
///
/// Armed when the pointer presses a tab and promoted to `active` only once it
/// travels past a small threshold, so an ordinary click still selects the tab
/// without nudging the order. While active, the grabbed pill follows the
/// pointer and the drop slot is derived from where its centre lands.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabDrag {
    /// Displayed index of the grabbed tab.
    pub(super) source: usize,
    /// Pointer X (physical px) when armed — crossing the horizontal threshold
    /// also activates the drag, so pulling a tab straight toward the terminal
    /// area (little Y motion) still engages docking.
    pub(super) origin_x: f32,
    /// Pointer coordinate along the tab axis (physical px) when armed. The tabs
    /// stack vertically, so this is the pointer Y.
    pub(super) origin: f32,
    /// Latest pointer coordinate along the tab axis (physical px, i.e. Y).
    pub(super) current: f32,
    /// Whether the move threshold has been crossed.
    pub(super) active: bool,
    /// Dock target while the pointer hovers the terminal area: dropping here
    /// splits the ACTIVE tab's layout on that side and moves the dragged tab's
    /// whole pane tree into it (VS Code-style edge docking).
    pub(super) dock: Option<SplitNav>,
}

/// What releasing a tab drag should do. `Click` covers the no-drag case: tab
/// selection is deferred from press to release, so the terminal area keeps
/// showing the ACTIVE tab while another tab is being dragged over it to dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabDropAction {
    /// Plain click (never crossed the drag threshold): select the tab.
    Click(usize),
    /// Reorder within the sidebar: move displayed `from` to displayed `to`.
    Reorder { from: usize, to: usize },
    /// Dock the dragged tab's layout into the active tab on `nav`'s side.
    Dock { source: usize, nav: SplitNav },
}

#[derive(Debug, Clone)]
/// Geometry for the left tab sidebar. Tabs are stacked vertically inside the
/// `panel` rect; each `tabs[i]` row carries a `closes[i]` × button, and `plus`
/// is the "new tab" row beneath the last tab. `toggle` is the sidebar icon in the
/// top bar (always present, the only tab affordance left when collapsed). All
/// rects are physical pixels — the same geometry drives drawing and hit-test.
pub(super) struct ChromeTabLayout {
    pub(super) tabs: Vec<(f32, f32, f32, f32)>,
    pub(super) closes: Vec<(f32, f32, f32, f32)>,
    pub(super) plus: (f32, f32, f32, f32),
    /// Chevron beside "+" that opens the new-tab shell dropdown. Zero-width
    /// when there's no room (never, in practice — it shares the plus band).
    pub(super) menu: (f32, f32, f32, f32),
    pub(super) toggle: (f32, f32, f32, f32),
    /// Full sidebar panel background rect. Zero-width when collapsed.
    pub(super) panel: (f32, f32, f32, f32),
    /// SSH host rows, same indexing contract as `tabs`: entry `i` is host `i`,
    /// scrolled-out rows carry a zero rect (skipped by drawing and hit-tests).
    pub(super) hosts: Vec<(f32, f32, f32, f32)>,
    pub(super) hosts_add: (f32, f32, f32, f32),
    /// Accordion headers ("TABS" / "SSH HOSTS" caption bands). Zero when the
    /// panel is folded; `hosts_header` is zero when there are no hosts at all.
    pub(super) tabs_header: (f32, f32, f32, f32),
    pub(super) hosts_header: (f32, f32, f32, f32),
    /// Per-section overlay scrollbars (track omitted, thumb only), present
    /// only when that section overflows its elastic share of the panel.
    pub(super) tabs_scrollbar: Option<(f32, f32, f32, f32)>,
    pub(super) hosts_scrollbar: Option<(f32, f32, f32, f32)>,
    /// Vertical band `(y0, y1)` of each section's row area, for wheel routing.
    pub(super) tabs_band: (f32, f32),
    pub(super) hosts_band: (f32, f32),
    /// Scroll clamps: the largest valid row offset for each section.
    pub(super) tabs_max_scroll: usize,
    pub(super) hosts_max_scroll: usize,
    /// 停靠区可用内容的底缘 y（物理 px）——HOSTS 分界拖拽用它把指针 y 换算
    /// 成停靠区高度；与布局同源，两头不会漂。折叠时为 0。
    pub(super) dock_content_bottom: f32,
}

/// Sidebar content model handed to the layout: everything that changes the
/// section geometry, read off `Display` in one place (`sidebar_model`) so the
/// drawing, hit-testing and wheel paths can never disagree.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SidebarModel {
    pub(super) tab_count: usize,
    pub(super) host_count: usize,
    pub(super) tabs_open: bool,
    pub(super) hosts_open: bool,
    pub(super) tabs_scroll: usize,
    pub(super) hosts_scroll: usize,
    /// 侧栏逻辑宽（拖拽调节的实时值；默认 [`super::SIDEBAR_W_LOGICAL`]）。
    pub(super) sidebar_w: f32,
    /// SSH HOSTS 停靠区高度覆盖（逻辑 px），0 = 自动（46% 弹性规则）。
    pub(super) hosts_band: f32,
}

pub(super) fn contains_rect((rx, ry, rw, rh): (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
}

/// Truncate `label` so its terminal display width fits within `max_cols`
/// columns, appending an ellipsis when clipped. CJK glyphs count as two columns,
/// matching how `draw_chrome_text` lays them out — callers already compute
/// `max_cols` as the available pixel span divided by `cell_w`, i.e. columns.
pub(super) fn truncate_tab_label(label: &str, max_cols: usize) -> String {
    let total: usize = label.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max_cols {
        return label.to_owned();
    }
    if max_cols <= 1 {
        return "…".to_owned();
    }
    // Reserve one column for the trailing ellipsis.
    let budget = max_cols - 1;
    let mut used = 0usize;
    let mut text = String::new();
    for ch in label.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        text.push(ch);
    }
    text.push('…');
    text
}

/// 分组标题文本的 y——chip、数字、标题共用的**唯一事实源**。
///
/// TABS 和 SSH HOSTS 的标题各有自己的落点（TABS 为躲开与顶栏的接缝特意往下
/// 压了几像素，HOSTS 在自己的带里居中）。之前 chip/数字用"header 居中"另算
/// 一份，TABS 那组就差出 6px——第三次栽在"两边各算一次"上了。现在谁要跟标题
/// 对齐，谁就从这里拿 y。
pub(super) fn section_title_y(
    header: (f32, f32, f32, f32),
    is_tabs: bool,
    text_h: f32,
    scale: f32,
) -> f32 {
    if is_tabs {
        // 与 draw_chrome 里 TABS 标题的 `pnl_y + s(22)` 同源（tabs_header.1
        // 就是 panel_top）。
        header.1 + 22.0 * scale
    } else {
        header.1 + (header.3 - text_h) / 2.0
    }
}

/// 分组标题里那颗数量 chip 的矩形（背景 + 数字共用）。
///
/// 背景在 quads 那一趟画、数字在 text 那一趟画，中间隔着 `draw_ui`。两边各
/// 算一次必然漂移——这正是本模块存在的理由，所以位置只在这里算一次。
///
/// UI 字体不是等宽，标题宽度只能按平均步进估。chip 是块独立的色片、不需要
/// 贴着标题的最后一个字形，估算的几像素误差落在那 6px 间距里看不出来。
pub(super) fn section_count_chip(
    header: (f32, f32, f32, f32),
    is_tabs: bool,
    title_chars: usize,
    digits: usize,
    advance: f32,
    tracking: f32,
    text_h: f32,
    scale: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let (hx, _, _, _) = header;
    // 标题的实际推进 = 字形步进 + **字距**，再乘一点粗体的富余。第一版漏了
    // tracking（0.65px/字），"SSH HOSTS" 十二个字符就差出近 8px——chip 于是
    // 正好压在标题最后一个字母上。BOLD 比常规再宽一档，用 1.06 兜住。
    let advance_per_char = (advance + tracking) * 1.06;
    let x = hx + s(16.0) + advance_per_char * title_chars as f32 + s(7.0);
    // 高度跟着**文本**走而不是写死 16px：字号或 DPI 一变，固定高度的 chip 要么
    // 箍不住数字、要么在小字号下胖成一坨。
    //
    // 1.45 那版把 chip 撑成了一颗比标题还抢眼的圆球——数量是**附注**，它只在
    // 折叠时替你数一下里面有几条，不该盖过它所标注的那个标题。原型里这颗片是
    // `padding: 0 5px` 的窄胶囊（高度就是行盒），所以这里也只留一线余量；下限
    // 同时把它压成横向的胶囊而不是正圆——正圆在这个尺寸上读起来就是个徽章。
    let h = (text_h * 1.18).max(s(11.0));
    let w = (advance * digits as f32 + s(8.0)).max(h * 1.25);
    // y 锚在标题文本上（chip 比文本高出的部分上下平分），不再自己在 header
    // 里居中——标题不是居中画的，居中的 chip 必然漂。
    let title_y = section_title_y(header, is_tabs, text_h, scale);
    (x, title_y - (h - text_h) / 2.0, w, h)
}

/// Shared geometry for the custom Windows-style titlebar controls.
///
/// The hit targets are intentionally wider than the visible glyphs: the design
/// follows the sample mockup's sparse controls, but the clickable area remains
/// comfortable for daily use.
#[inline]
pub(super) fn chrome_control_centers(
    width: f32,
    top: f32,
    bar_h: f32,
    scale_factor: f32,
) -> [(ChromeHit, f32, f32); 5] {
    let s = |v: f32| v * scale_factor;
    let center_y = top + bar_h / 2.0;
    let close_x = width - s(23.0);
    let max_x = close_x - s(46.0);
    let min_x = max_x - s(46.0);
    // Drawer view toggles sit left of the window controls, separated by a gap
    // so the destructive (close) and modal (panel) clusters don't read as one.
    let git_x = min_x - s(58.0);
    let files_x = git_x - s(42.0);

    [
        (ChromeHit::PanelFiles, files_x, center_y),
        (ChromeHit::PanelGit, git_x, center_y),
        (ChromeHit::Minimize, min_x, center_y),
        (ChromeHit::Maximize, max_x, center_y),
        (ChromeHit::Close, close_x, center_y),
    ]
}

#[inline]
fn is_window_control(hit: ChromeHit) -> bool {
    matches!(hit, ChromeHit::Minimize | ChromeHit::Maximize | ChromeHit::Close)
}

/// Full caption-button hit target. It deliberately reaches both physical
/// edges so a maximized/fullscreen window can be closed from the corner.
#[inline]
fn window_control_hit_rect(
    center_x: f32,
    center_y: f32,
    scale_factor: f32,
) -> (f32, f32, f32, f32) {
    let s = |value: f32| value * scale_factor;
    // 图标保持标题栏视觉居中，命中区域单独扩展到物理顶边；这样既能与
    // 文件/分支按钮对齐，也保留最大化窗口右上角的一步关闭操作。
    (center_x - s(23.0), 0.0, s(46.0), center_y + s(20.0))
}

/// Visible caption-button band. It reaches the physical top edge while the
/// glyph remains centred on the inset title bar. This removes the detached
/// strip visible above Chrome-style caption buttons in a borderless window.
#[inline]
fn window_control_visual_rect(
    center_x: f32,
    top: f32,
    bar_h: f32,
    scale_factor: f32,
) -> (f32, f32, f32, f32) {
    let width = 46.0 * scale_factor;
    (center_x - width * 0.5, 0.0, width, top + bar_h)
}

/// Top-left settings trigger. It occupies the old product-mark slot beside the
/// sidebar toggle; keeping this geometry in one helper keeps drawing and
/// hit-testing aligned.
#[inline]
pub(crate) fn chrome_settings_button_rect(
    _size_info: &SizeInfo,
    scale_factor: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale_factor;
    let margin = s(8.0);
    let top = margin;
    let bar_h = s(40.0);
    // 顶栏按钮的命中/背景边长 = bar_h - 2*inner_pad。6 那版给出 28px，比
    // tokens 的 `hit`(32) 小一圈——按钮看着紧贴图标，按下去也不跟手。
    let inner_pad = s(4.0);
    let pill_h = bar_h - 2.0 * inner_pad;
    let toggle_x = margin + inner_pad;
    let x = toggle_x + pill_h + s(8.0);
    (x, top + inner_pad, pill_h, pill_h)
}

/// Lay out the vertical tab sidebar plus its top-bar affordances. When
/// `collapsed`, the panel folds to zero width and the "new tab" pill moves up
/// beside the sidebar icon in the top bar, so both stay reachable. Geometry is in
/// physical pixels and shared verbatim by drawing and hit-testing, so the two
/// can never drift across DPI scales.
pub(super) fn chrome_tab_layout(
    size_info: &SizeInfo,
    scale_factor: f32,
    model: SidebarModel,
    expand: f32,
) -> ChromeTabLayout {
    let s = |v: f32| v * scale_factor;
    let h = size_info.height();
    let margin = s(8.0);
    let bar_h = s(40.0);
    // 顶栏按钮的命中/背景边长 = bar_h - 2*inner_pad。6 那版给出 28px，比
    // tokens 的 `hit`(32) 小一圈——按钮看着紧贴图标，按下去也不跟手。
    let inner_pad = s(4.0);
    let pill_h = bar_h - 2.0 * inner_pad;
    let top = margin;
    let count = model.tab_count.max(1);
    let host_count = model.host_count;

    // Sidebar toggle: leftmost square in the top bar, always present.
    let toggle = (margin + inner_pad, top + inner_pad, pill_h, pill_h);

    // `expand` is the fold animation progress: 1 = resting expanded, 0 =
    // fully collapsed. Between the two the whole panel (rows, ×, "+") slides
    // off to the LEFT with a swift-out ease, so folding reads as motion
    // instead of a pop.
    if expand <= 0.004 {
        // Folded: no panel, no per-tab rows. The new-tab pill parks just right
        // of the top-left settings button so opening tabs still works with the
        // sidebar hidden.
        let gear = chrome_settings_button_rect(size_info, scale_factor);
        let plus = (gear.0 + gear.2 + s(8.0), top + inner_pad, pill_h, pill_h);
        // Chevron dropdown just right of "+", narrower (it's a caret, not a
        // full pill).
        let menu_w = s(16.0);
        let menu = (plus.0 + plus.2 + s(2.0), top + inner_pad, menu_w, pill_h);
        return ChromeTabLayout {
            tabs: Vec::new(),
            closes: Vec::new(),
            plus,
            menu,
            toggle,
            panel: (0.0, 0.0, 0.0, 0.0),
            hosts: Vec::new(),
            hosts_add: (0.0, 0.0, 0.0, 0.0),
            tabs_header: (0.0, 0.0, 0.0, 0.0),
            hosts_header: (0.0, 0.0, 0.0, 0.0),
            tabs_scrollbar: None,
            hosts_scrollbar: None,
            tabs_band: (0.0, 0.0),
            hosts_band: (0.0, 0.0),
            tabs_max_scroll: 0,
            hosts_max_scroll: 0,
            dock_content_bottom: 0.0,
        };
    }

    // Expanded panel: the left leg of the connected chrome L-frame. It shares
    // the top bar's left edge (`margin`) and abuts its bottom edge with NO gap
    // (`panel_top = top + bar_h`) so the two read as one panel; the join corners
    // are squared off in `draw_chrome`. It runs down to the window's bottom
    // margin (symmetric with the top bar's top at `margin`), leaving only a
    // breathing gap before the terminal grid on its right.
    let sw = model.sidebar_w * scale_factor;
    let slide = (1.0 - expand.clamp(0.0, 1.0)) * sw;
    let panel_x = margin - slide;
    let panel_w = (sw - margin - s(12.0)).max(s(120.0));
    let panel_top = top + bar_h;
    let panel_bottom = h - margin;
    let panel_h = (panel_bottom - panel_top).max(0.0);
    let panel = (panel_x, panel_top, panel_w, panel_h);

    // Vertical tab rows: full panel width minus inner padding, stacked below
    // the "TABS" header. The new-tab affordance is a small square at the right
    // end of that header row (revealed on sidebar hover), not a trailing row.
    // 行的左右内缩。窄内缩 = 更宽的行 = 名称与徽章之间拉得开——侧栏总宽
    // 一点没变，变的只是行在里面占多少。14 → 8 → 4 一路收：14 那版把 28px
    // 让给了两侧空白；参照原型的行几乎贴满侧栏，呼吸感来自行**内部**的留白，
    // 不是行外的边框。
    let tab_pad = s(4.0);
    let tab_x = panel_x + tab_pad;
    let tab_w = panel_w - 2.0 * tab_pad;
    let row_h = s(34.0);
    let gap = s(8.0);
    let pitch = row_h + gap;
    // Header band below the top-bar join: the panel now abuts the top bar with
    // no gap, so this carries the seam clearance (the old +12 panel gap) plus
    // room for the "TABS" caption and the "+" square.
    let header = s(54.0);
    // "SSH HOSTS" caption band — always present, so the feature is
    // discoverable even before the user has any `~/.ssh/config` entries
    // (an empty section shows a hint instead of vanishing).
    let hosts_header_h = s(38.0);
    let bottom_pad = s(10.0);
    let queue_reserved = message_queue_entry::reserved_height(scale_factor);
    // The empty SSH section still draws a two-line onboarding hint below its
    // header. Reserve that real content height so it cannot paint through the
    // bottom-docked message queue when many tabs consume the elastic budget.
    let empty_hosts_hint_h =
        if queue_reserved > 0.0 && host_count == 0 && model.hosts_open { s(38.0) } else { 0.0 };

    // "+" square plus its dropdown chevron, vertically centred in the header
    // band, pinned to the right. The chevron is the rightmost element; the
    // "+" sits just left of it.
    let plus_sz = s(20.0);
    let menu_w = s(15.0);
    let plus_y = panel_top + (header - plus_sz) * 0.5;
    let menu = (panel_x + panel_w - tab_pad - menu_w, plus_y, menu_w, plus_sz);
    let plus = (menu.0 - s(2.0) - plus_sz, plus_y, plus_sz, plus_sz);

    let tabs_header = (panel_x, panel_top, panel_w, header);

    // ---- 两个分区：标签吃满弹性高度，主机沉到底 ----
    //
    // 原来两段排在同一条流里、互相削减。问题是它们不是同一类东西：标签是
    // **活的状态**（哪个在跑、谁在等你批准），那套徽章的全部价值就在于它一
    // 直在余光里；主机是**目录**，只在开新会话那一刻用一次。排在一条流里，
    // 主机一多就会把标签顶出视口——用得少的那类挤掉了必须常驻的那类。
    //
    // 所以主机整块沉到面板底部：自己的地盘、自己的滚动、封顶。标签拿走上面
    // 剩下的全部，永远不会被挤没。
    //
    // 主机行是**双行**的（名字在上、地址在下），比标签行高一档；两段行高不
    // 同，所以到处都按**高度**算，不能再除同一个 pitch。
    let host_row_h = row_h + s(13.0);
    let host_pitch = host_row_h + gap;
    // n 行占的高度：行间距只在行**之间**，末行后面不留。
    let rows_h = |n: usize, pitch: f32| if n == 0 { 0.0 } else { n as f32 * pitch - gap };

    let content_top = panel_top + header;
    let content_bottom = (panel_bottom - queue_reserved - bottom_pad).max(content_top);
    let content_h = content_bottom - content_top;

    let tabs_want = if model.tabs_open { count } else { 0 };
    let hosts_want = if model.hosts_open { host_count } else { 0 };

    // 停靠区的地盘：**至少** 46%，但标签用不完的话它可以更多。只写死 46%
    // 的话，两个标签配四十台主机会在中间空出一大片——封顶是为了保护标签，
    // 标签不需要保护的时候就不该继续封着。
    const DOCK_MIN_SHARE: f32 = 0.46;
    // 标题（和空态提示）是停靠区的固定开销，先扣掉再让行去分剩下的。
    let dock_fixed = hosts_header_h + empty_hosts_hint_h;
    // 用户拖过 HOSTS 分界（设置开启拖拽调节）后，停靠区高度以覆盖值为准，
    // 钳在「只剩标题」与「至少给标签区留一行」之间；没拖过走弹性规则。
    let dock_cap = if model.hosts_band > 0.0 {
        (model.hosts_band * scale_factor).clamp(dock_fixed, (content_h - pitch - gap).max(dock_fixed))
    } else {
        (content_h - rows_h(tabs_want, pitch) - gap).max(content_h * DOCK_MIN_SHARE).min(content_h)
    };
    let mut hosts_show = hosts_want;
    while hosts_show > 0 && dock_fixed + gap + rows_h(hosts_show, host_pitch) > dock_cap {
        hosts_show -= 1;
    }
    // 内容需要的高度。没拖过分界时它**就是**停靠区的高度——贴着内容走。
    let content_dock_h =
        dock_fixed + if hosts_show > 0 { gap + rows_h(hosts_show, host_pitch) } else { 0.0 };
    // 拖过分界之后改以拖出来的高度为准：手动分界的语义是「手动覆盖弹性」。
    // 只拿它当上限是不够的——主机数撑不满上限时（一台主机配一屏高度就是
    // 这样），上限抬得再高内容也长不出来，分界线纹丝不动，手感上就是
    // 「往上拖不动、被限制死了」（用户 08-02 报的正是这个）。多出来的空白
    // 是用户自己要的：他把地盘划给了 HOSTS。
    let dock_h = if model.hosts_band > 0.0 { dock_cap } else { content_dock_h };
    // 贴底：停靠区的**下**沿固定，上沿随内容升降。折起来（hosts_open=false）
    // 就只剩一条标题贴在最下面。
    let dock_top = (content_bottom - dock_h).max(content_top);

    // 标签区 = 停靠区上方剩下的全部。
    let tabs_avail = (dock_top - gap - content_top).max(0.0);
    let mut tabs_show = tabs_want;
    while tabs_show > 0 && rows_h(tabs_show, pitch) > tabs_avail {
        tabs_show -= 1;
    }
    let tabs_max_scroll = tabs_want.saturating_sub(tabs_show);
    let hosts_max_scroll = hosts_want.saturating_sub(hosts_show);
    let tabs_scroll = model.tabs_scroll.min(tabs_max_scroll);
    let hosts_scroll = model.hosts_scroll.min(hosts_max_scroll);

    // Tab rows: real rects only for the visible scroll window; scrolled-out
    // rows keep their index but carry a zero rect so hit-testing and drawing
    // skip them without disturbing the index contract.
    let zero = (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
    let tabs_top = panel_top + header;
    let mut tabs = Vec::with_capacity(count);
    let mut closes = Vec::with_capacity(count);
    for i in 0..count {
        if !model.tabs_open || i < tabs_scroll || i >= tabs_scroll + tabs_show {
            tabs.push(zero);
            closes.push(zero);
            continue;
        }
        let y = tabs_top + (i - tabs_scroll) as f32 * pitch;
        tabs.push((tab_x, y, tab_w, row_h));
        let close_size = (row_h * 0.58).max(s(16.0));
        closes.push((
            tab_x + tab_w - close_size - s(10.0),
            y + (row_h - close_size) * 0.5,
            close_size,
            close_size,
        ));
    }
    let tabs_band_h = if tabs_show > 0 { tabs_show as f32 * pitch - gap } else { 0.0 };
    let tabs_band = (tabs_top, tabs_top + tabs_band_h);

    // 停靠区：位置由 `dock_top` 定死（贴着面板底），不再跟着标签区的末尾走。
    let hosts_header_y = dock_top;
    let hosts_header = (panel_x, hosts_header_y, panel_w, hosts_header_h);
    let hosts_top = hosts_header_y + hosts_header_h;
    let mut hosts = Vec::with_capacity(host_count);
    for i in 0..host_count {
        if !model.hosts_open || i < hosts_scroll || i >= hosts_scroll + hosts_show {
            hosts.push(zero);
            continue;
        }
        let y = hosts_top + (i - hosts_scroll) as f32 * host_pitch;
        hosts.push((tab_x, y, tab_w, host_row_h));
    }
    let hosts_band_h = if hosts_show > 0 { hosts_show as f32 * host_pitch - gap } else { 0.0 };
    let hosts_band = (hosts_top, hosts_top + hosts_band_h);

    // Overlay scrollbar thumbs, one per overflowing section: pinned to the
    // panel's right inner edge, sized to the visible fraction of the list.
    let thumb = |band: (f32, f32), show: usize, want: usize, scroll: usize| {
        if show == 0 || want <= show {
            return None;
        }
        let track_h = band.1 - band.0;
        let th = (track_h * show as f32 / want as f32).max(s(24.0));
        let ty = band.0 + (track_h - th) * scroll as f32 / (want - show) as f32;
        Some((panel_x + panel_w - s(6.0), ty, s(3.0), th))
    };
    let tabs_scrollbar = thumb(tabs_band, tabs_show, tabs_want, tabs_scroll);
    let hosts_scrollbar = thumb(hosts_band, hosts_show, hosts_want, hosts_scroll);

    ChromeTabLayout {
        tabs,
        closes,
        plus,
        menu,
        toggle,
        panel,
        hosts,
        hosts_add: (
            hosts_header.0 + hosts_header.2 - s(42.0),
            hosts_header.1 + (hosts_header.3 - s(20.0)) * 0.5,
            s(20.0),
            s(20.0),
        ),
        tabs_header,
        hosts_header,
        tabs_scrollbar,
        hosts_scrollbar,
        tabs_band,
        hosts_band,
        tabs_max_scroll,
        hosts_max_scroll,
        dock_content_bottom: content_bottom,
    }
}

// Icon geometry (add / more / caption marks / sidebar toggle / chevrons)
// lives in `display::icons` — a pure rectangle-in, quads-out layer with no
// Display state, so drawing can never drift from hit-testing.

pub(super) fn chrome_hit_with_tabs(
    size_info: &SizeInfo,
    scale_factor: f32,
    model: SidebarModel,
    collapsed: bool,
    x: f32,
    y: f32,
) -> ChromeHit {
    let s = |v: f32| v * scale_factor;
    let w = size_info.width();
    let margin = s(8.0);
    let bar_h = s(40.0);
    let top = margin;

    let expand = if collapsed { 0.0 } else { 1.0 };
    let layout = chrome_tab_layout(size_info, scale_factor, model, expand);

    // Toggle + new-tab + vertical tab rows are checked before the bar regions,
    // since the sidebar lives outside the top bar's vertical band.
    if contains_rect(layout.toggle, x, y) {
        return ChromeHit::SidebarToggle;
    }
    if contains_rect(layout.menu, x, y) {
        return ChromeHit::NewTabMenu;
    }
    if contains_rect(layout.plus, x, y) {
        return ChromeHit::NewTab;
    }
    if message_queue_entry::contains(message_queue_entry::layout(layout.panel, scale_factor), x, y)
    {
        return ChromeHit::MessageQueue;
    }
    if contains_rect(layout.hosts_add, x, y) {
        return ChromeHit::AddSshHost;
    }
    for (index, rect) in layout.closes.iter().copied().enumerate() {
        if contains_rect(rect, x, y) {
            return ChromeHit::TabClose(index);
        }
    }
    for (index, rect) in layout.tabs.iter().copied().enumerate() {
        if contains_rect(rect, x, y) {
            return ChromeHit::Tab(index);
        }
    }
    for (index, rect) in layout.hosts.iter().copied().enumerate() {
        if contains_rect(rect, x, y) {
            return ChromeHit::Host(index);
        }
    }
    // Section captions toggle the accordion. Checked after the "+" (which
    // lives inside the TABS header band) and the rows.
    if layout.tabs_header.2 > 0.0 && contains_rect(layout.tabs_header, x, y) {
        return ChromeHit::TabsSection;
    }
    if layout.hosts_header.2 > 0.0 && contains_rect(layout.hosts_header, x, y) {
        return ChromeHit::HostsSection;
    }

    // Caption controls are allowed to escape the inset top-bar band. This is
    // what makes the rightmost close target reachable at (width - 1, 0) when
    // the borderless window owns the whole monitor.
    for (hit, cx, cy) in chrome_control_centers(w, top, bar_h, scale_factor) {
        if is_window_control(hit)
            && contains_rect(window_control_hit_rect(cx, cy, scale_factor), x, y)
        {
            return hit;
        }
    }

    // Top bar: window controls first, then the rest drags the window.
    if y >= top && y <= top + bar_h && x >= margin && x <= w - margin {
        let hit_half = s(18.0);
        for (hit, cx, cy) in chrome_control_centers(w, top, bar_h, scale_factor) {
            if x >= cx - hit_half && x <= cx + hit_half && y >= cy - hit_half && y <= cy + hit_half
            {
                return hit;
            }
        }
        return ChromeHit::TitleBar;
    }

    ChromeHit::None
}

/// Whether a window-space pixel falls within either Nebula chrome bar (top
/// title bar or left sidebar), used to pick the right mouse cursor.
pub fn in_chrome_bar(size_info: &SizeInfo, scale_factor: f32, x: f32, y: f32) -> bool {
    let s = |v: f32| v * scale_factor;
    let w = size_info.width();
    let h = size_info.height();
    let margin = s(8.0);
    let bar_h = s(40.0);

    // Left tab sidebar: everything from the window edge up to the grid's left
    // origin (padding_x) is chrome, so vertical tabs get hover feedback and the
    // arrow cursor rather than the text I-beam. When collapsed the gutter
    // shrinks to the ordinary padding and this band is effectively just margin.
    if x >= margin && x < size_info.padding_x() && y > margin + bar_h && y < h - margin {
        return true;
    }

    for (hit, cx, cy) in chrome_control_centers(w, margin, bar_h, scale_factor) {
        if is_window_control(hit)
            && contains_rect(window_control_hit_rect(cx, cy, scale_factor), x, y)
        {
            return true;
        }
    }

    if x < margin || x > w - margin {
        return false;
    }
    let in_top = y >= margin && y <= margin + bar_h;
    in_top
}

/// Resize direction when the pixel is within the window's resize border, used
/// to drive interactive edge/corner resizing on the borderless window.
pub fn resize_edge(
    size_info: &SizeInfo,
    scale_factor: f32,
    x: f32,
    y: f32,
    enabled: bool,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection::*;

    if !enabled {
        return None;
    }

    let b = 6.0 * scale_factor;
    let w = size_info.width();
    let h = size_info.height();
    let l = x <= b;
    let r = x >= w - b;
    let t = y <= b;
    let bo = y >= h - b;

    let dir = match (t, bo, l, r) {
        (true, _, true, _) => NorthWest,
        (true, _, _, true) => NorthEast,
        (_, true, true, _) => SouthWest,
        (_, true, _, true) => SouthEast,
        (true, _, _, _) => North,
        (_, true, _, _) => South,
        (_, _, true, _) => West,
        (_, _, _, true) => East,
        _ => return None,
    };
    Some(dir)
}

// ---- chrome rendering (moved verbatim from `display::mod`; `d` = Display) ----

const SPINNER_PERIOD: std::time::Duration = std::time::Duration::from_millis(800);

#[inline]
fn advance_spinner_phase(phase: f32, delta: std::time::Duration) -> f32 {
    (phase + delta.as_secs_f32() / SPINNER_PERIOD.as_secs_f32()).rem_euclid(1.0)
}

#[inline]
fn spinner_dot_center(
    phase: f32,
    tail_index: u32,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> (f32, f32) {
    let angle = phase.rem_euclid(1.0) * std::f32::consts::TAU
        - tail_index as f32 * std::f32::consts::FRAC_PI_4;
    (center_x + radius * angle.cos(), center_y + radius * angle.sin())
}

/// Draw the Nebula window chrome: top title bar and left tab sidebar.
///
/// This is the first chrome milestone: it paints the rounded, gradient
/// panels and pills with the dedicated UI renderer to validate the native
/// (egui-free) chrome pipeline. Text labels and interactivity follow.
pub(super) fn draw_chrome(d: &mut Display) {
    d.step_chrome_anims();
    let motion_frame = d.nebula_ui_anims.frame();
    let any_tab_running = d.any_tab_running();
    if any_tab_running {
        d.nebula_ui_anims.spinner_phase =
            advance_spinner_phase(d.nebula_ui_anims.spinner_phase, motion_frame.delta);
    }
    let spinner_phase = d.nebula_ui_anims.spinner_phase;

    // Chrome colors come from the theme skin (hover washes flip to dark
    // smoke on the light themes); close-hover red stays semantic.
    let palette = d.nebula_theme.palette();
    let sk = d.nebula_theme.skin();
    let restore_window = d.window.is_maximized() || d.window.is_fullscreen();
    let language = d.ui_language();
    #[allow(non_snake_case)]
    let (HOVER_FILL, HOVER_FILL_STRONG) = (sk.hover, sk.hover_strong);
    // Match the solid Windows caption-close treatment from the reference;
    // translucent red reads like a generic pill instead of a window control.
    const CLOSE_HOVER_FILL: Rgba = Rgba::new(232, 17, 35, 242);

    // UI 布局用 ui 版 SizeInfo：cell 尺寸按 UI 锚定比率补偿，与按同一比
    // 率栅格化的 chrome 文本同源；终端缩放因此不影响侧栏/顶栏/设置布局。
    let size = d.ui_size_info();
    let scale = d.window.scale_factor as f32;
    let w = size.width();
    let h = size.height();

    // Logical-pixel helper.
    let s = |v: f32| v * scale;

    let margin = s(8.0);
    let bar_h = s(40.0);
    // 顶栏按钮的命中/背景边长 = bar_h - 2*inner_pad。6 那版给出 28px，比
    // tokens 的 `hit`(32) 小一圈——按钮看着紧贴图标，按下去也不跟手。
    let inner_pad = s(4.0);
    let radius = s(UI_CORNER_RADIUS_LOGICAL);
    let pill_h = bar_h - 2.0 * inner_pad;
    let pill_r = s(UI_CORNER_RADIUS_LOGICAL);
    let hairline_w = s(UI_HAIRLINE_LOGICAL).max(1.0);

    let mut quads: Vec<UiQuad> = Vec::new();

    // ---- Unified shell frame (一体化外壳，2026-07-23 用户裁定) ----
    // 顶栏、侧栏与终端卡四周的边框是同一块玻璃：卡以外的整个窗口区域由
    // 4 条硬边条带 + 4 个凹角块铺出恰好一层壳色，每个像素只涂一次。旧
    // 分层感的根源（不透明清屏底上又叠半透明面板，叠加区更实、gap 区
    // 更透）从结构上消除——清屏现在全透明，见 `draw_window_backdrop`。
    //
    // 壳色 = panel 在 shell_bg 上的预合成（保住面板 token 的调子），
    // alpha 直接取用户透明度：滑块驱动整个外壳作为一体变化；cover 壁纸
    // 时壳浮在壁纸上，半透明壳整体透出壁纸。文字与图标保持不透明。
    let shell_r = s(UI_SHELL_RADIUS_LOGICAL);
    let sidebar_expand = d.left_sidebar_progress();
    let top_y = margin;
    let shell_alpha = surface_opacity::SurfaceOpacityPolicy::new(d.nebula_window_opacity).chrome;
    let shell_bg = palette.shell_bg;
    let pa = palette.panel.a as f32 / 255.0;
    let comp = |p: u8, b: u8| (p as f32 * pa + b as f32 * (1.0 - pa)).round() as u8;
    let shell_color = Rgba::new(
        comp(palette.panel.r, shell_bg.r),
        comp(palette.panel.g, shell_bg.g),
        comp(palette.panel.b, shell_bg.b),
        (shell_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    );
    let (card_x, card_y, card_w, card_h) = d.terminal_card_rect();
    let card_r = shell_r.round().min(card_w * 0.5).min(card_h * 0.5);
    // 条带贴卡的直边；凹角块叠在卡矩形四角的 r×r 区域上，只涂圆弧以外
    // 的部分。凹角的圆与卡片自身圆角同心同径，弧线两侧 coverage 互补相
    // 加为 1，半透明壳下也不会出现更实或漏底的接缝。
    quads.push(UiQuad::solid(0.0, 0.0, w, card_y, 0.0, shell_color));
    quads.push(UiQuad::solid(0.0, card_y, card_x, card_h, 0.0, shell_color));
    quads.push(UiQuad::solid(
        card_x + card_w,
        card_y,
        (w - card_x - card_w).max(0.0),
        card_h,
        0.0,
        shell_color,
    ));
    quads.push(UiQuad::solid(
        0.0,
        card_y + card_h,
        w,
        (h - card_y - card_h).max(0.0),
        0.0,
        shell_color,
    ));
    if card_r > 0.0 && card_w > 0.0 && card_h > 0.0 {
        quads.push(UiQuad::concave_corner(card_x, card_y, card_r, 0, shell_color));
        quads.push(UiQuad::concave_corner(
            card_x + card_w - card_r,
            card_y,
            card_r,
            1,
            shell_color,
        ));
        quads.push(UiQuad::concave_corner(
            card_x + card_w - card_r,
            card_y + card_h - card_r,
            card_r,
            2,
            shell_color,
        ));
        quads.push(UiQuad::concave_corner(
            card_x,
            card_y + card_h - card_r,
            card_r,
            3,
            shell_color,
        ));
    }

    // ---- Background ambient light ----
    // Purple bloom in the lower-left, cool blue in the upper-right, giving
    // the flat backdrop a sense of depth without competing with content.
    // Drawn ON TOP of the shell frame: the shell is exactly one layer now,
    // so ambience is added over it instead of shining through from a layer
    // below (which would vanish at 100% opacity anyway).
    // Light themes ship zero-alpha glows (8-bit banding on pale ground) —
    // skip the fill-rate cost entirely.
    let glow_r = w * 0.62;
    if palette.glow_l.a > 0 {
        quads.push(UiQuad::glow(
            -glow_r * 0.45,
            h - glow_r * 0.55,
            glow_r * 2.0,
            glow_r * 2.0,
            palette.glow_l,
        ));
    }
    if palette.glow_r.a > 0 {
        quads.push(UiQuad::glow(
            w - glow_r * 1.55,
            -glow_r * 0.45,
            glow_r * 2.0,
            glow_r * 2.0,
            palette.glow_r,
        ));
    }

    let tab_layout = chrome_tab_layout(&size, scale, d.sidebar_model(), sidebar_expand);

    // Sidebar toggle at the far left of the top bar folds the tab sidebar
    // away and back; it's the one tab affordance that survives collapse.
    let (tog_x, tog_y, tog_w, tog_h) = tab_layout.toggle;
    let toggle_hovered = d.nebula_chrome_hover == ChromeHit::SidebarToggle;
    // 展开时常驻一层底：这个按钮是**开关**不是动作，而开关必须能看出自己
    // 处在哪一档。此前只有 hover 才有背景，展开和收起长得一模一样，点下去
    // 除了侧栏动一下没有任何确认。
    if toggle_hovered || d.left_sidebar_visible() {
        let fill = if toggle_hovered { HOVER_FILL_STRONG } else { sk.surface };
        quads.push(UiQuad::solid(tog_x, tog_y, tog_w, tog_h, pill_r, fill));
    }
    let icon_c = if toggle_hovered { sk.icon_hover } else { sk.icon };
    icons::push_sidebar_toggle(
        &mut quads,
        tab_layout.toggle,
        scale,
        Rgba::new(icon_c.r, icon_c.g, icon_c.b, 185),
        Rgba::new(palette.panel.r, palette.panel.g, palette.panel.b, 255),
    );

    // Settings moved into the old product-mark slot.
    let (set_x, set_y, set_w, set_h) = chrome_settings_button_rect(&size, scale);
    let settings_hovered = d.nebula_settings_hover == SettingsHit::Toggle;
    if settings_hovered {
        quads.push(UiQuad::solid(set_x, set_y, set_w, set_h, pill_r, HOVER_FILL_STRONG));
    }

    // Sidebar/top-bar panel fills are gone: the unified shell frame above
    // already covers everything outside the terminal card in exactly one
    // layer. A per-panel fill here would stack a second alpha pass and
    // re-create the "solid panel on translucent border" split it replaced.

    // Dock preview: while a dragged tab hovers the terminal area, glow the
    // half where dropping would split the active tab (VS Code edge dock).
    if let Some(nav) = d.nebula_tab_drag.as_ref().filter(|d| d.active).and_then(|d| d.dock) {
        let gx = size.padding_x();
        let gy = size.padding_y();
        let gw = size.width() - gx - size.padding_right();
        let gh = size.height() - gy - size.padding_bottom();
        let (px2, py2, pw2, ph2) = match nav {
            SplitNav::Left => (gx, gy, gw / 2.0, gh),
            SplitNav::Right => (gx + gw / 2.0, gy, gw / 2.0, gh),
            SplitNav::Up => (gx, gy, gw, gh / 2.0),
            SplitNav::Down => (gx, gy + gh / 2.0, gw, gh / 2.0),
        };
        // Brand-cyan wash + hairline, same tokens as the settings shell.
        quads.push(UiQuad::solid(px2, py2, pw2, ph2, radius, Rgba::new(120, 200, 230, 32)));
        quads.push(UiQuad::solid(
            px2 + hairline_w,
            py2 + hairline_w,
            pw2 - 2.0 * hairline_w,
            ph2 - 2.0 * hairline_w,
            (radius - hairline_w).max(0.0),
            Rgba::new(120, 200, 230, 18),
        ));
    }

    // Ease each row toward its target draw-y (reorder "make way") instead of
    // snapping; a tab-count change resets to the freshly laid-out positions.
    if d.nebula_tab_anim.len() != tab_layout.tabs.len() {
        d.nebula_tab_anim = tab_layout
            .tabs
            .iter()
            .map(|tab| crate::motion::Spring::new(tab.1).with_response(0.14))
            .collect();
        d.nebula_tab_was_visible = tab_layout.tabs.iter().map(|tab| tab.2 > 0.0).collect();
    }
    let mut tab_anim_active = false;

    for (index, (tab_x, row_y, tab_w, tab_h)) in tab_layout.tabs.iter().copied().enumerate() {
        // Scrolled-out / folded rows carry a zero rect: skip them, and snap
        // their eased position so they don't fly in when they reappear.
        if tab_w <= 0.0 {
            d.nebula_tab_anim[index].snap_to(row_y);
            d.nebula_tab_was_visible[index] = false;
            continue;
        }
        let target_y = d.tab_drag_draw_y(index, row_y, &tab_layout);
        // The grabbed pill tracks the pointer 1:1 (easing it would feel
        // laggy); only the rows making way ease toward their slots.
        let dragging_this =
            d.nebula_tab_drag.as_ref().is_some_and(|d| d.active && d.source == index);
        let action = tab_reveal_motion_action(
            d.nebula_tab_reveal_motion,
            d.nebula_tab_was_visible[index],
            true,
            dragging_this,
        );
        let tab_y = match action {
            TabRevealMotionAction::Snap => {
                d.nebula_tab_anim[index].snap_to(target_y);
                target_y
            },
            TabRevealMotionAction::Spring => {
                let spring = &mut d.nebula_tab_anim[index];
                spring.set_target(target_y, crate::motion::MotionPolicy::Full);
                spring.step(motion_frame);
                tab_anim_active |= spring.is_active();
                spring.value()
            },
        };
        d.nebula_tab_was_visible[index] = true;
        let tab_hovered = matches!(
            d.nebula_chrome_hover,
            ChromeHit::Tab(i) | ChromeHit::TabClose(i) if i == index
        );
        let close_hovered = matches!(d.nebula_chrome_hover, ChromeHit::TabClose(i) if i == index);
        // No hover lift: the hover fill must stay pixel-aligned with the row
        // grid (the "+" square and the × buttons share its right edge; a 1px
        // offset reads as misalignment, not depth).
        let tab_draw_x = tab_x;
        let tab_draw_y = tab_y;
        if index == d.nebula_active_tab {
            // 选中态是**中性**的一档提亮，不带品牌色。
            //
            // 此前这里叠了三层：accent 外环 + 主题 tab_bg_l + 深色再补一层
            // accent。而标签左边本来就有程序 logo，claude / codex / grok 全是
            // 彩标——背景再染色就是两个彩色在同一行里抢。中性底反而让那颗
            // logo 的颜色立得住，这也是原型看起来"融合"的原因。
            //
            // 浅色用纯白 panel（从灰底上浮起来），深色用 hover_strong（白叠加
            // 提亮一档）。方向相反，都是"比周围亮"。
            let fill = if palette.is_light { sk.panel } else { sk.hover_strong };
            quads.push(UiQuad::solid(tab_draw_x, tab_draw_y, tab_w, tab_h, pill_r, fill));
        }
        // Inactive tabs carry no standalone fill — they sit flush on the
        // sidebar surface and only light up on hover (below). State is the
        // white active pill, not a per-row background.

        if tab_hovered {
            quads.push(UiQuad::solid(tab_draw_x, tab_draw_y, tab_w, tab_h, pill_r, HOVER_FILL));
        }
        // 侧边光条仅表示用户明确设置的标签色；默认 Tab 不额外占用视觉层级。
        if let Some(strip_color) = d.nebula_tab_colors.get(index).copied().flatten() {
            let strip_x = tab_draw_x + s(4.0);
            let strip_y = tab_draw_y + s(7.0);
            let strip_w = s(2.5).max(2.0);
            let strip_h = (tab_h - s(14.0)).max(s(10.0));
            let glow = s(12.0);
            quads.push(UiQuad::glow(
                strip_x + strip_w * 0.5 - glow * 0.5,
                strip_y + strip_h * 0.5 - glow * 0.5,
                glow,
                glow,
                Rgba::new(
                    strip_color.r,
                    strip_color.g,
                    strip_color.b,
                    if index == d.nebula_active_tab { 72 } else { 38 },
                ),
            ));
            quads.push(UiQuad::solid(
                strip_x,
                strip_y,
                strip_w,
                strip_h,
                strip_w * 0.5,
                Rgba::new(
                    strip_color.r,
                    strip_color.g,
                    strip_color.b,
                    if index == d.nebula_active_tab { 245 } else { 176 },
                ),
            ));
        }
        let (close_x, _, close_w, close_h) = tab_layout.closes[index];
        let close_y = tab_draw_y + (tab_h - close_h) / 2.0;
        if close_hovered {
            quads.push(UiQuad::solid(
                close_x,
                close_y,
                close_w,
                close_h,
                pill_r,
                HOVER_FILL_STRONG,
            ));
        }
        if !tab_hovered {
            let has_dot = d.nebula_tab_bells.get(index).copied().unwrap_or(false);
            let running = d.nebula_tab_running.get(index).copied().unwrap_or(false);
            let attention = d.nebula_tab_attention.get(index).copied().unwrap_or(false);
            let failed = d.nebula_tab_failed.get(index).copied().unwrap_or(false);
            let flashing = d.nebula_tab_flashing.get(index).copied().unwrap_or(false);
            let cx = close_x + close_w / 2.0;
            let cy = close_y + close_h / 2.0;
            // 挖空/合成色取**这一行当时的真实底**：非活动行是侧栏底，活动行
            // 是那块提亮的药丸——拿侧栏深底去合成画在浅药丸上的环，出来就是
            // 一个黑圈（活动 tab 的 spinner 曾因此显黑）。三角与对勾的挖空
            // 同理：填错底，挖空处就是一块颜色不对的补丁。
            let panel_under = {
                let shell =
                    Rgba::new(palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b, 255);
                if index == d.nebula_active_tab {
                    let fill = if palette.is_light { sk.panel } else { sk.hover_strong };
                    super::ui::surface::over(fill, shell)
                } else {
                    shell
                }
            };
            // 徽章按「不处理就没有进展」排序：等你批准 > 跑挂了 > 刚完成 >
            // 有未读结果 > 还在跑。一个标签同一时刻只说一件事，而且说最要紧
            // 的那件。
            //
            // 全部走 skin 的语义色：warn / danger / ok / accent。此前这里写死
            // 了 rgb(82,168,255)——那是 Nebula 一个主题的 accent，换到 Moss 或
            // Coal 上就是一颗与整屏无关的蓝点。
            if attention {
                // 手掌图形暂时下线（用户 08-02：现在这版不满意——五指靠挖空
                // 撑形，小尺寸下指缝糊成一团，读不出"手"）。在重画之前，
                // 「等你批准」退回一颗 warn 色圆点：与 accent 的"有未读结果"
                // 靠颜色区分，不靠形状。恢复时把下面这行换回 push_hand。
                // super::ui::icons::push_hand(&mut quads, cx, cy, scale, sk.warn);
                let dot_d = s(7.0);
                quads.push(UiQuad::solid(
                    cx - dot_d / 2.0,
                    cy - dot_d / 2.0,
                    dot_d,
                    dot_d,
                    dot_d / 2.0,
                    sk.warn,
                ));
            } else if failed {
                super::ui::icons::push_alert(&mut quads, cx, cy, scale, sk.danger, panel_under);
            } else if flashing {
                super::ui::icons::push_check_badge(&mut quads, cx, cy, scale, sk.ok, panel_under);
            } else if has_dot {
                // The one state that earns a dot: an unseen result (bell
                // in a background tab / long command finished unseen).
                let dot_d = s(6.0);
                let dot_x = close_x + (close_w - dot_d) / 2.0;
                let dot_y = close_y + (close_h - dot_d) / 2.0;
                let halo = dot_d * 3.0;
                let accent = sk.accent;
                quads.push(UiQuad::glow(
                    dot_x + dot_d / 2.0 - halo / 2.0,
                    dot_y + dot_d / 2.0 - halo / 2.0,
                    halo,
                    halo,
                    Rgba::new(accent.r, accent.g, accent.b, 80),
                ));
                quads.push(UiQuad::solid(
                    dot_x,
                    dot_y,
                    dot_d,
                    dot_d,
                    dot_d / 2.0,
                    Rgba::new(accent.r, accent.g, accent.b, 230),
                ));
            } else if running {
                // 一圈暗轨道 + 一段绕行的亮弧（组件层配方）。亮弧用中性灰而
                // 不是品牌色：spinner 表达的是"还在跑"，是这些状态里最不需
                // 要人动手的一个，不该最艳；活动/非活动行如今合成底不同、观
                // 感一致（见 panel_under）。
                icons::push_spinner(
                    &mut quads,
                    cx,
                    cy,
                    s(5.5),
                    spinner_phase,
                    sk.hairline,
                    Rgba::new(sk.ink_dim.r, sk.ink_dim.g, sk.ink_dim.b, 255),
                    panel_under,
                );
            }
            // Idle tab → nothing: the row stays clean by default.
        }
    }

    let queue_entry_layout = message_queue_entry::layout(tab_layout.panel, scale);
    if d.left_sidebar_visible() {
        message_queue_entry::push_quads(
            &mut quads,
            queue_entry_layout,
            d.nebula_message_queue_entry,
            sk,
            scale,
            d.nebula_chrome_hover == ChromeHit::MessageQueue,
        );
    }
    if tab_anim_active {
        d.window.request_redraw();
    }

    // "New tab" pill (a wide row when expanded, a square beside the toggle
    // when collapsed — both come straight from the layout). No resting fill:
    // the "+" is just an icon until the pointer arrives, then a hover pill
    // lifts under it (matches the top-bar window controls).
    let (plus_x, plus_y, plus_w, plus_h) = tab_layout.plus;
    if d.nebula_chrome_hover == ChromeHit::NewTab {
        quads.push(UiQuad::solid(plus_x, plus_y, plus_w, plus_h, pill_r, HOVER_FILL_STRONG));
    }
    let expanded_sidebar = d.left_sidebar_visible() && tab_layout.panel.2 > 0.0;
    let tabs_plus_visible = !expanded_sidebar
        || matches!(
            d.nebula_chrome_hover,
            ChromeHit::TabsSection | ChromeHit::NewTab | ChromeHit::NewTabMenu
        );
    if tabs_plus_visible {
        let plus_ink =
            if d.nebula_chrome_hover == ChromeHit::NewTab { sk.icon_hover } else { sk.icon };
        icons::push_add(
            &mut quads,
            tab_layout.plus,
            scale,
            Rgba::new(plus_ink.r, plus_ink.g, plus_ink.b, 235),
        );
    }
    // The dropdown chevron beside "+": same hover-only lift.
    let (menu_x, menu_y, menu_w, menu_h) = tab_layout.menu;
    if d.nebula_chrome_hover == ChromeHit::NewTabMenu {
        quads.push(UiQuad::solid(menu_x, menu_y, menu_w, menu_h, pill_r, HOVER_FILL_STRONG));
    }
    let more_ink =
        if d.nebula_chrome_hover == ChromeHit::NewTabMenu { sk.icon_hover } else { sk.icon };
    icons::push_more(
        &mut quads,
        tab_layout.menu,
        scale,
        Rgba::new(more_ink.r, more_ink.g, more_ink.b, 235),
    );

    // SSH HOSTS section (quad layer): hover fill per row + the per-section
    // overlay scrollbar thumbs. Row icons/labels render in the text pass.
    if d.left_sidebar_visible() {
        for (index, (hx, hy, hw, hh)) in tab_layout.hosts.iter().copied().enumerate() {
            if hw <= 0.0 {
                continue;
            }
            if matches!(d.nebula_chrome_hover, ChromeHit::Host(i) if i == index) {
                quads.push(UiQuad::solid(hx, hy, hw, hh, pill_r, HOVER_FILL));
            }
        }
        if d.nebula_chrome_hover == ChromeHit::AddSshHost {
            let (ax, ay, aw, ah) = tab_layout.hosts_add;
            quads.push(UiQuad::solid(ax, ay, aw, ah, s(6.0), HOVER_FILL_STRONG));
        }
        if matches!(d.nebula_chrome_hover, ChromeHit::HostsSection | ChromeHit::AddSshHost) {
            let add_ink = if d.nebula_chrome_hover == ChromeHit::AddSshHost {
                sk.icon_hover
            } else {
                sk.icon
            };
            icons::push_add(
                &mut quads,
                tab_layout.hosts_add,
                scale,
                Rgba::new(add_ink.r, add_ink.g, add_ink.b, 235),
            );
        }
        let thumb_c = sk.scrollbar_thumb;
        for bar in [tab_layout.tabs_scrollbar, tab_layout.hosts_scrollbar].into_iter().flatten() {
            let (bx, by, bw, bh) = bar;
            quads.push(UiQuad::solid(bx, by, bw, bh, bw * 0.5, thumb_c));
        }
    }

    // Directory/Git drawer controls remain compact pills and keep their hover
    // feedback separate from the contiguous Windows caption-button band.
    for (hit, cx, cy) in chrome_control_centers(w, top_y, bar_h, scale) {
        if !matches!(hit, ChromeHit::PanelFiles | ChromeHit::PanelGit) {
            continue;
        }
        let active = match hit {
            ChromeHit::PanelFiles => {
                d.nebula_side_panel.open && d.nebula_side_panel.view == side_panel::PanelView::Files
            },
            ChromeHit::PanelGit => {
                d.nebula_side_panel.open && d.nebula_side_panel.view == side_panel::PanelView::Git
            },
            _ => false,
        };
        if d.nebula_chrome_hover == hit || active {
            let button_w = s(34.0);
            let button_h = pill_h;
            quads.push(UiQuad::solid(
                cx - button_w / 2.0,
                cy - button_h / 2.0,
                button_w,
                button_h,
                pill_r,
                if active { sk.accent_soft } else { HOVER_FILL_STRONG },
            ));
        }
    }

    // The three caption buttons form one continuous Windows-style band: no
    // inset pill and no dead strip at the right edge. Hit-testing separately
    // extends the same horizontal rects to the physical top edge.
    for (hit, cx, cy) in chrome_control_centers(w, top_y, bar_h, scale) {
        if !is_window_control(hit) {
            continue;
        }
        let _ = cy;
        let hovered = d.nebula_chrome_hover == hit;
        let visual = window_control_visual_rect(cx, top_y, bar_h, scale);
        let hover_fill = if hit == ChromeHit::Close { CLOSE_HOVER_FILL } else { HOVER_FILL_STRONG };
        if hovered {
            quads.push(UiQuad::solid(visual.0, visual.1, visual.2, visual.3, 0.0, hover_fill));
        }
        // The glyph centres on the VISIBLE band (physical top edge → bar
        // bottom), not on the inset title bar — the old inset centre left the
        // marks riding ~4px low inside the full-height buttons.
        let icon_cy = visual.1 + visual.3 * 0.5;
        let ink_rgb = if hit == ChromeHit::Close && hovered {
            Rgb::new(255, 255, 255)
        } else if hovered {
            sk.icon_hover
        } else {
            sk.icon
        };
        // The rounded caption squares hollow their interior with the button's
        // effective surface color. Composited opaquely: on a translucent shell
        // the tiny interior reads as the shell tint, which is invisible at
        // everyday opacities and far cheaper than a stroked-quad shader.
        let base = Rgba::new(palette.panel.r, palette.panel.g, palette.panel.b, 255);
        let cutout = if hovered { icons::blend_over(base, hover_fill) } else { base };
        let kind = match hit {
            ChromeHit::Minimize => icons::WindowControlIcon::Minimize,
            ChromeHit::Maximize => icons::WindowControlIcon::Maximize { restore: restore_window },
            _ => icons::WindowControlIcon::Close,
        };
        icons::push_window_control(
            &mut quads,
            kind,
            cx,
            icon_cy,
            scale,
            Rgba::new(ink_rgb.r, ink_rgb.g, ink_rgb.b, 245),
            cutout,
        );
    }

    // Soft accent glow beneath the title bar: 1px, fading in from the left,
    // blue-purple through the middle to cyan on the right (like edge light).
    let underline_y = top_y + bar_h - s(1.0);
    let half = (w - 2.0 * margin) / 2.0;
    let glow_l = Rgba::new(palette.edge_l.r, palette.edge_l.g, palette.edge_l.b, 0);
    let glow_m = Rgba::new(
        ((palette.edge_l.r as u16 + palette.edge_r.r as u16) / 2) as u8,
        ((palette.edge_l.g as u16 + palette.edge_r.g as u16) / 2) as u8,
        ((palette.edge_l.b as u16 + palette.edge_r.b as u16) / 2) as u8,
        48,
    );
    let glow_r = Rgba::new(palette.edge_r.r, palette.edge_r.g, palette.edge_r.b, 18);
    quads.push(UiQuad::gradient(
        margin,
        underline_y,
        half,
        s(1.0),
        0.0,
        glow_l,
        glow_m,
        Gradient::Horizontal,
    ));
    quads.push(UiQuad::gradient(
        margin + half,
        underline_y,
        half,
        s(1.0),
        0.0,
        glow_m,
        glow_r,
        Gradient::Horizontal,
    ));

    // Right-side drawer (directory tree / git) remains part of the app shell. Keeps
    // drawing through slide-out; animation stepping is centralized in Display.
    if d.side_panel_visible() {
        if let Some(panel) = d.nebula_sftp_panel.as_ref().filter(|_| d.sftp_view_active()) {
            let layout = sftp_panel::layout(&d.side_panel_layout(), scale);
            sftp_panel::push_quads(
                panel,
                &layout,
                &d.nebula_theme,
                &mut quads,
                scale,
                size.cell_width(),
            );
        } else {
            side_panel::push_quads(
                &d.nebula_side_panel,
                &d.side_panel_layout(),
                &d.nebula_theme,
                &mut quads,
                scale,
                size.cell_width(),
            );
        }
    }

    // No base pill behind the gear — just the icon (hover still fills).
    // The Settings special tab paints its page inside the normal content card.
    if d.nebula_settings_open {
        settings::push_quads(&d.settings_view(), &mut quads, &size, scale);
    }

    // 分组标题的数量 chip（背景片）。文字在下面那趟画，位置同源于
    // `section_count_chip`。
    {
        const TITLE_SCALE: f32 = 0.82;
        let adv = d.renderer.ui_text_advance(&d.glyph_cache, TITLE_SCALE);
        let trk = s(0.65);
        let title_h = size.cell_height() * TITLE_SCALE;
        // 底色回到 `surface`（原型里的 `var(--surface)`）。上一版提到 `hover` 是
        // 为了"看得见"，但那是行悬浮的亮度——一个静态附注顶着交互态的底色，会
        // 让人以为鼠标正停在标题上。附注就该只比背景高一线。
        let chip_fill = sk.surface;
        if tab_layout.tabs_header.2 > 0.0 {
            let n = d.nebula_tab_labels.len();
            // 标题串是「箭头 + 两个空格 + TABS」，chip 跟在它后面。
            let chip = section_count_chip(
                tab_layout.tabs_header,
                true,
                7,
                n.to_string().len(),
                adv,
                trk,
                title_h,
                scale,
            );
            quads.push(UiQuad::solid(chip.0, chip.1, chip.2, chip.3, chip.3 * 0.5, chip_fill));
        }
        if tab_layout.hosts_header.2 > 0.0 {
            let n = d.nebula_ssh_hosts.len();
            let chip = section_count_chip(
                tab_layout.hosts_header,
                false,
                12,
                n.to_string().len(),
                adv,
                trk,
                title_h,
                scale,
            );
            quads.push(UiQuad::solid(chip.0, chip.1, chip.2, chip.3, chip.3 * 0.5, chip_fill));
        }
    }

    // 停靠区的分界线。
    //
    // 只有一条 hairline，没有另一层底色：独立滚动、贴底、封顶这三条行为已经
    // 让主机区是另一块地盘了。再刷一层底，侧栏就从"一块面板"变成"两块拼起来
    // 的面板"——可它们本来属于同一个外壳，一体感就是这么丢的。
    //
    // 上面那列真滚起来（也就是边界这时才开始有意义），才在分界线上方叠一道
    // 极浅的落影，让被滚走的行读作"从这条线下面穿过去了"。没滚动时它完全不
    // 存在——静止的界面不该带着一条解释不了的阴影。
    if tab_layout.hosts_header.2 > 0.0 && tab_layout.tabs_band.1 > tab_layout.tabs_band.0 {
        let (hx, hy, hw, _) = tab_layout.hosts_header;
        let line_y = (hy - s(5.0)).round();
        if d.nebula_tabs_scroll > 0 {
            // 三层递减的薄片就是一道渐变落影：越靠近线越实。
            for step in 1..=3 {
                let alpha = (26 / step) as u8;
                quads.push(UiQuad::solid(
                    hx,
                    line_y - s(3.0) * step as f32,
                    hw,
                    s(3.0),
                    0.0,
                    Rgba::new(0, 0, 0, alpha),
                ));
            }
        }
        quads.push(UiQuad::solid(hx + s(8.0), line_y, hw - s(16.0), s(1.0), 0.0, sk.hairline));
    }

    // Paint the panels and pills first.
    d.renderer.draw_ui(&size, &quads);

    // ---- Chrome text labels, drawn on top of the pills ----
    // The ink set comes from the skin and flips with the theme: light
    // chrome needs dark text.
    #[allow(non_snake_case)]
    let (TXT, TXT_ON_ACCENT, TXT_DIM, ICON, ICON_HOVER) =
        (sk.ink, sk.ink_strong, sk.ink_dim, sk.icon, sk.icon_hover);
    const ICON_SETTINGS: &str = "\u{eb51}";
    const ICON_CLOSE: &str = "\u{ea76}";

    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    // Top-bar text baseline used by the collapsed title.
    let cy_top = top_y + (bar_h - cell_h) / 2.0;
    let center_x = |px: f32, pw: f32, n: usize| px + (pw - n as f32 * cell_w) / 2.0;
    fn draw_centered_icon(
        renderer: &mut Renderer,
        glyph_cache: &mut GlyphCache,
        size: &SizeInfo,
        cell_w: f32,
        cell_h: f32,
        rect: (f32, f32, f32, f32),
        fg: Rgb,
        icon: &str,
    ) {
        let mut chars = icon.chars();
        let (first, second) = (chars.next(), chars.next());
        // Single-glyph marks center on the REAL rasterized ink: Nerd icons
        // ink well outside their 1-column advance, which is why grid-based
        // centering left the gear/folder/git marks visibly off inside their
        // hover pills.
        if let (Some(character), None) = (first, second) {
            if let Some((left, top, width, height)) =
                renderer.chrome_glyph_ink(glyph_cache, size, character)
            {
                let x = rect.0 + (rect.2 - width) / 2.0 - left;
                let y = rect.1 + (rect.3 - height) / 2.0 - top;
                renderer.draw_chrome_text(size, x, y, fg, icon, glyph_cache);
                return;
            }
        }
        let cols = icon.chars().map(|ch| ch.width().unwrap_or(1)).sum::<usize>().max(1);
        let x = rect.0 + (rect.2 - cols as f32 * cell_w) / 2.0;
        let y = rect.1 + (rect.3 - cell_h) / 2.0;
        renderer.draw_chrome_text(size, x, y, fg, icon, glyph_cache);
    }

    draw_centered_icon(
        &mut d.renderer,
        &mut d.glyph_cache,
        &size,
        cell_w,
        cell_h,
        (set_x, set_y, set_w, set_h),
        if settings_hovered { ICON_HOVER } else { ICON },
        ICON_SETTINGS,
    );
    for (hit, cx, cy) in chrome_control_centers(w, top_y, bar_h, scale) {
        if d.nebula_special_tab_active && matches!(hit, ChromeHit::PanelFiles | ChromeHit::PanelGit)
        {
            continue;
        }
        let hovered = d.nebula_chrome_hover == hit;
        let icon = match hit {
            // Caption icons are vector quads in the UI pass above. Keeping
            // them out of the font layer makes size/baseline DPI-independent.
            ChromeHit::Minimize | ChromeHit::Maximize | ChromeHit::Close => continue,
            ChromeHit::PanelFiles => "\u{ea83}",
            ChromeHit::PanelGit => "\u{ea68}",
            _ => continue,
        };
        // Drawer toggles light up in the accent while their view is open.
        let active = match hit {
            ChromeHit::PanelFiles => {
                d.nebula_side_panel.open && d.nebula_side_panel.view == side_panel::PanelView::Files
            },
            ChromeHit::PanelGit => {
                d.nebula_side_panel.open && d.nebula_side_panel.view == side_panel::PanelView::Git
            },
            _ => false,
        };
        let ink = if active {
            sk.accent
        } else if hovered {
            ICON_HOVER
        } else {
            ICON
        };
        draw_centered_icon(
            &mut d.renderer,
            &mut d.glyph_cache,
            &size,
            cell_w,
            cell_h,
            (cx - s(21.0), cy - pill_h / 2.0, s(42.0), pill_h),
            ink,
            icon,
        );
    }

    // Vertical tab labels. Each row's Y comes from the eased anim slot; the
    // label is left-aligned after the accent gutter, the × pinned right.
    let row_text_cy = |ry: f32, rh: f32| ry + (rh - cell_h) / 2.0;
    // Sidebar text remains visible because Settings is a normal tab.
    if d.left_sidebar_visible() && tab_layout.panel.2 > 0.0 {
        // "TABS" caption at the panel head, with an accordion chevron. The
        // panel abuts the top bar with no gap, so the caption is pushed down
        // inside the header band to keep clearance from the join.
        let (pnl_x, _, _, _) = tab_layout.panel;
        let tabs_chevron = if d.nebula_tabs_section_open { "\u{eab4}" } else { "\u{eab6}" };
        const SECTION_TITLE_SCALE: f32 = 0.82;
        let section_title_tracking = s(0.65);
        let section_title_flags = nebula_terminal::term::cell::Flags::BOLD;
        d.renderer.draw_ui_text_tracked(
            &size,
            pnl_x + s(16.0),
            section_title_y(tab_layout.tabs_header, true, cell_h * SECTION_TITLE_SCALE, scale),
            SECTION_TITLE_SCALE,
            section_title_tracking,
            TXT_DIM,
            section_title_flags,
            // 箭头前置、数量后置。
            //
            // 前置不是排版偏好，是**符号语义**：后置的 chevron 在 UI 语言里
            // 属于下拉菜单（combobox），前置的三角才是折叠（Finder / VS Code /
            // 所有 IDE 的 disclosure triangle）。用后置箭头表达折叠等于借错了
            // 符号。前置还让多个分组的箭头对齐成一条竖线，层级一眼可扫；后置
            // 的位置得跟着标题长度飘。
            //
            // 数量常驻：折叠之后它是这一段仅剩的信息量，而 SSH 那边"42 台"
            // 与"3 台"直接决定人是滚动找还是直接搜。
            &format!("{tabs_chevron}  TABS"),
            &mut d.glyph_cache,
        );
        {
            let n = d.nebula_tab_labels.len().to_string();
            let adv = d.renderer.ui_text_advance(&d.glyph_cache, SECTION_TITLE_SCALE);
            let chip = section_count_chip(
                tab_layout.tabs_header,
                true,
                7,
                n.len(),
                adv,
                section_title_tracking,
                cell_h * SECTION_TITLE_SCALE,
                scale,
            );
            d.renderer.draw_ui_text_tracked(
                &size,
                chip.0 + (chip.2 - adv * n.len() as f32) * 0.5,
                section_title_y(tab_layout.tabs_header, true, cell_h * SECTION_TITLE_SCALE, scale),
                SECTION_TITLE_SCALE,
                0.0,
                sk.ink_faint,
                nebula_terminal::term::cell::Flags::empty(),
                &n,
                &mut d.glyph_cache,
            );
        }
        for (index, (tab_x, row_y, tab_w, tab_h)) in tab_layout.tabs.iter().copied().enumerate() {
            // Scrolled-out / folded rows: zero rect, nothing to draw.
            if tab_w <= 0.0 {
                continue;
            }
            let row_y = d.nebula_tab_anim.get(index).map(|motion| motion.value()).unwrap_or(row_y);
            let tab_hovered = matches!(
                d.nebula_chrome_hover,
                ChromeHit::Tab(i) | ChromeHit::TabClose(i) if i == index
            );
            let close_hovered =
                matches!(d.nebula_chrome_hover, ChromeHit::TabClose(i) if i == index);
            // Text follows the quad layer: no hover lift (see draw_chrome).
            let hover_lift_x = 0.0;
            let draw_row_y = row_y;
            let color =
                if index == d.nebula_active_tab || tab_hovered { TXT_ON_ACCENT } else { TXT };
            let cy = row_text_cy(draw_row_y, tab_h);
            // Real AI brand logo: a textured quad in the icon slot, sized
            // to the glyph ink height so it reads like
            // an icon, not a sticker. Staged here, drawn after ALL chrome
            // text (see nebula_chrome_logo_draws). Other programs keep
            // their Nerd Font glyph inside the label text.
            let mut text_x = tab_x + s(14.0) + hover_lift_x;
            let mut reserved = s(60.0);
            if let Some(logo) = d.nebula_tab_logos.get(index).copied().flatten() {
                let standard_icon_s = (cell_h * 0.72).round();
                let icon_s = if logo == super::AiLogo::Grok {
                    (cell_h * 0.92).round()
                } else {
                    standard_icon_s
                };
                if let Some((id, rgba, px)) = d.ai_logo_pixels(logo, color, icon_s as u32) {
                    // Grok is intentionally larger for legibility. Keep the
                    // standard icon slot's centre instead of growing only to
                    // the right from its left edge.
                    let icon_x = text_x - (icon_s - standard_icon_s) / 2.0;
                    let icon_y = (draw_row_y + (tab_h - icon_s) / 2.0).round();
                    d.nebula_chrome_logo_draws.push((
                        id,
                        rgba,
                        px,
                        (icon_x, icon_y, icon_s, icon_s),
                    ));
                }
                text_x += icon_s + s(6.0);
                reserved += icon_s + s(6.0);
            }
            // When renaming this tab, show the edit buffer instead of the label
            let label = if d.nebula_tab_rename.as_ref().is_some_and(|(i, _)| *i == index) {
                d.nebula_tab_rename.as_ref().map(|(_, text)| text.as_str()).unwrap_or(".")
            } else {
                d.nebula_tab_labels.get(index).map(String::as_str).unwrap_or(".")
            };
            let row_tracking = s(0.35);
            let max_chars =
                ((tab_w - reserved).max(cell_w) / (cell_w + row_tracking)).floor() as usize;
            let label = truncate_tab_label(label, max_chars.max(1));

            // Input box + selection/caret when renaming this tab. These
            // MUST be flushed here, immediately, not pushed onto the shared
            // `quads` batch: that batch was already painted at the top of
            // draw_chrome (the draw_ui call above), so any quad appended in
            // this text phase would silently never render — which is exactly
            // why the rename box was invisible. Draw them now, before the
            // label glyphs, so box/selection sit under the text.
            let renaming_this = d.nebula_tab_rename.as_ref().is_some_and(|(i, _)| *i == index);
            let label_tracking = if renaming_this { 0.0 } else { row_tracking };
            let select_all = renaming_this && d.nebula_tab_rename_select_all;
            if renaming_this {
                let input_pad = s(4.0);
                let input_x = text_x - input_pad;
                let input_y = row_y + s(4.0);
                let input_w = tab_w - (text_x - tab_x) - s(8.0) + input_pad;
                let input_h = tab_h - s(8.0);
                let accent = palette.edge_r;
                let mut box_quads = vec![
                    // White base fill.
                    UiQuad::solid(
                        input_x,
                        input_y,
                        input_w,
                        input_h,
                        s(4.0),
                        Rgba::new(255, 255, 255, 250),
                    ),
                    // Accent wash over the whole box; the inner white below
                    // then leaves it showing only as a border ring.
                    UiQuad::solid(
                        input_x,
                        input_y,
                        input_w,
                        input_h,
                        s(4.0),
                        Rgba::new(accent.r, accent.g, accent.b, 120),
                    ),
                    // Inner white, inset by a hairline → accent ring border.
                    UiQuad::solid(
                        input_x + hairline_w,
                        input_y + hairline_w,
                        input_w - 2.0 * hairline_w,
                        input_h - 2.0 * hairline_w,
                        s(3.0),
                        Rgba::new(255, 255, 255, 250),
                    ),
                ];
                // Text metrics (column-based: CJK counts 2, matching
                // draw_chrome_text's advance).
                let text_cols: usize = label.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
                let text_w = text_cols as f32 * cell_w;
                let text_top = row_y + (tab_h - cell_h) / 2.0;
                // Publish the buffer's first-glyph X for click-to-place-caret
                // (the input path maps pointer X through this every frame).
                d.nebula_tab_rename_text_x = text_x;
                if select_all {
                    // nushell-style "everything selected" — a blue fill
                    // behind the whole name; the first keystroke replaces it.
                    let sel_w = (text_w + s(2.0)).min(input_w - 2.0 * hairline_w - s(2.0));
                    box_quads.push(UiQuad::solid(
                        text_x - s(1.0),
                        text_top - s(1.0),
                        sel_w,
                        cell_h + s(2.0),
                        s(2.0),
                        Rgba::new(38, 120, 220, 235),
                    ));
                } else {
                    // Insertion caret: a thin beam at the caret's column
                    // (click / arrows position it; edits happen there).
                    // Blinks on the shared 500ms phase — a frozen beam reads
                    // as a hang.
                    if caret_blink_on() {
                        let caret_cols: usize = label
                            .chars()
                            .take(d.nebula_tab_rename_caret)
                            .map(|c| c.width().unwrap_or(0).max(1))
                            .sum();
                        box_quads.push(UiQuad::solid(
                            (text_x + caret_cols as f32 * cell_w).min(input_x + input_w - s(4.0)),
                            text_top,
                            (2.0 * scale).max(1.0),
                            cell_h,
                            0.0,
                            Rgba::new(accent.r, accent.g, accent.b, 255),
                        ));
                    }
                }
                d.renderer.draw_ui(&size, &box_quads);
                // Anchor the IME candidate window to the caret inside the
                // box, not the terminal grid cursor (which the grid pass set
                // earlier this frame). draw_chrome runs last, so this wins.
                let caret_px = if select_all {
                    text_x
                } else {
                    let caret_cols: usize = label
                        .chars()
                        .take(d.nebula_tab_rename_caret)
                        .map(|c| c.width().unwrap_or(0).max(1))
                        .sum();
                    text_x + caret_cols as f32 * cell_w
                };
                d.window.set_ime_cursor_area_px(caret_px, row_y, cell_w, tab_h);
            }

            d.renderer.draw_ui_text_tracked(
                &size,
                text_x,
                cy,
                1.0,
                label_tracking,
                if renaming_this {
                    if select_all {
                        Rgb::new(255, 255, 255) // White on the blue selection
                    } else {
                        Rgb::new(0, 0, 0) // Black on white input
                    }
                } else {
                    color
                },
                nebula_terminal::term::cell::Flags::empty(),
                &label,
                &mut d.glyph_cache,
            );
            // 静默行的右侧亮出 shell 短标（原型里的 "zsh" 位）：徽章位空着时
            // 它回答"这个 tab 是什么环境"。任何徽章都比它要紧，来了就让；
            // 文字压 SUPPORTING 档、用最淡的墨——它是环境注脚，不是第二个
            // 标签名，和名字抢眼就输了。
            let badge_busy = d.nebula_tab_bells.get(index).copied().unwrap_or(false)
                || d.nebula_tab_running.get(index).copied().unwrap_or(false)
                || d.nebula_tab_attention.get(index).copied().unwrap_or(false)
                || d.nebula_tab_failed.get(index).copied().unwrap_or(false)
                || d.nebula_tab_flashing.get(index).copied().unwrap_or(false);
            if !tab_hovered && !renaming_this && !badge_busy {
                if let Some(tag) = d.nebula_tab_shells.get(index).filter(|tag| !tag.is_empty()) {
                    const TAG_SCALE: f32 = tokens::type_scale::SUPPORTING;
                    let tag_cols: usize = tag.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
                    let tag_w = tag_cols as f32 * cell_w * TAG_SCALE;
                    let tag_x = tab_x + tab_w - s(10.0) - tag_w;
                    let label_cols: usize =
                        label.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
                    let label_end = text_x + label_cols as f32 * (cell_w + row_tracking);
                    // 名字长到贴着右缘时不画：一半压在字上的注脚比没有更糟。
                    if tag_x > label_end + s(12.0) {
                        d.renderer.draw_ui_text(
                            &size,
                            tag_x,
                            draw_row_y + (tab_h - cell_h * TAG_SCALE) / 2.0,
                            TAG_SCALE,
                            sk.ink_faint,
                            nebula_terminal::term::cell::Flags::empty(),
                            tag,
                            &mut d.glyph_cache,
                        );
                    }
                }
            }
            if tab_hovered {
                let (close_x, _, close_w, close_h) = tab_layout.closes[index];
                let close_y = draw_row_y + (tab_h - close_h) / 2.0;
                draw_centered_icon(
                    &mut d.renderer,
                    &mut d.glyph_cache,
                    &size,
                    cell_w,
                    cell_h,
                    (close_x, close_y, close_w, close_h),
                    if close_hovered { ICON_HOVER } else { ICON },
                    ICON_CLOSE,
                );
            }
            #[cfg(any())]
            d.renderer.draw_chrome_text(
                &size,
                tab_x + tab_w - s(20.0),
                cy,
                TXT_DIM,
                "×",
                &mut d.glyph_cache,
            );
        }

        // ---- SSH HOSTS section (text pass) ----
        // Caption with accordion chevron, then one row per visible host:
        // remote icon + alias, plus a small pin glyph on pinned entries.
        if tab_layout.hosts_header.2 > 0.0 {
            let (hh_x, hh_y, _, hh_h) = tab_layout.hosts_header;
            let hosts_chevron = if d.nebula_hosts_section_open { "\u{eab4}" } else { "\u{eab6}" };
            d.renderer.draw_ui_text_tracked(
                &size,
                hh_x + s(16.0),
                section_title_y(
                    tab_layout.hosts_header,
                    false,
                    cell_h * SECTION_TITLE_SCALE,
                    scale,
                ),
                SECTION_TITLE_SCALE,
                section_title_tracking,
                TXT_DIM,
                section_title_flags,
                &format!("{hosts_chevron}  SSH HOSTS"),
                &mut d.glyph_cache,
            );
            {
                let n = d.nebula_ssh_hosts.len().to_string();
                let adv = d.renderer.ui_text_advance(&d.glyph_cache, SECTION_TITLE_SCALE);
                let chip = section_count_chip(
                    tab_layout.hosts_header,
                    false,
                    12,
                    n.len(),
                    adv,
                    section_title_tracking,
                    cell_h * SECTION_TITLE_SCALE,
                    scale,
                );
                d.renderer.draw_ui_text_tracked(
                    &size,
                    chip.0 + (chip.2 - adv * n.len() as f32) * 0.5,
                    section_title_y(
                        tab_layout.hosts_header,
                        false,
                        cell_h * SECTION_TITLE_SCALE,
                        scale,
                    ),
                    SECTION_TITLE_SCALE,
                    0.0,
                    sk.ink_faint,
                    nebula_terminal::term::cell::Flags::empty(),
                    &n,
                    &mut d.glyph_cache,
                );
            }
            // Empty state: the section stays visible with a hint teaching the
            // zero-config path — typing `ssh host` in any pane auto-saves the
            // destination here once the connection confirms (`~/.ssh/config`
            // aliases still appear automatically too). Styled as helper text,
            // NOT as content: smaller and fainter than the caption above it,
            // and indented to the row-label depth (`tab_pad + s(14)`, where
            // tab/host row text starts) so it reads as a child of the section.
            // The hint wraps to the sidebar width; drawing it as one line
            // would bleed across the seam onto the terminal grid.
            if d.nebula_ssh_hosts.is_empty() && d.nebula_hosts_section_open {
                use unicode_width::UnicodeWidthChar;
                const HINT_SCALE: f32 = tokens::type_scale::SUPPORTING;
                let hint_flags = nebula_terminal::term::cell::Flags::empty();
                let (pnl_x, _, pnl_w, _) = tab_layout.panel;
                let text_x = hh_x + s(28.0);
                // Wrap budget in the TRUE scaled advance (`draw_ui_text`
                // steps by unfloored `average_advance × scale`, not by the
                // floored grid cell) or long lines overrun the panel edge.
                let hint_cell_w = d.renderer.ui_text_advance(&d.glyph_cache, HINT_SCALE);
                let max_cols =
                    ((((pnl_x + pnl_w - s(12.0)) - text_x) / hint_cell_w).floor() as usize).max(4);
                let mut line = String::new();
                let mut cols = 0;
                let mut line_y = hh_y + hh_h + s(2.0);
                for ch in language
                    .pick(
                        "输入 ssh 命令，连接后自动保存",
                        "Run an ssh command to save hosts after connecting",
                    )
                    .chars()
                {
                    let ch_cols = ch.width().unwrap_or(0);
                    if cols + ch_cols > max_cols && !line.is_empty() {
                        d.renderer.draw_ui_text(
                            &size,
                            text_x,
                            line_y,
                            HINT_SCALE,
                            sk.ink_dim,
                            hint_flags,
                            line.trim_start(),
                            &mut d.glyph_cache,
                        );
                        line.clear();
                        cols = 0;
                        line_y += cell_h * HINT_SCALE + s(3.0);
                    }
                    line.push(ch);
                    cols += ch_cols;
                }
                if !line.trim_start().is_empty() {
                    d.renderer.draw_ui_text(
                        &size,
                        text_x,
                        line_y,
                        HINT_SCALE,
                        sk.ink_dim,
                        hint_flags,
                        line.trim_start(),
                        &mut d.glyph_cache,
                    );
                }
            }
            for (index, (hx, hy, hw, hh)) in tab_layout.hosts.iter().copied().enumerate() {
                if hw <= 0.0 {
                    continue;
                }
                let hovered = matches!(d.nebula_chrome_hover, ChromeHit::Host(i) if i == index);
                let color = if hovered { TXT_ON_ACCENT } else { TXT };
                let cy = row_text_cy(hy, hh);
                let name = d.nebula_ssh_hosts.get(index).map(String::as_str).unwrap_or("?");
                let pinned = d.nebula_pinned_hosts.iter().any(|p| p == name);
                // 起过名字就显示名字。地址仍然是这一行的身份（置顶、删除、连接
                // 都按地址走），只有给人看的那串文字换掉。
                let shown = d.nebula_ssh_labels.get(name).map(String::as_str).unwrap_or(name);
                // Label budget from its real start to the row's right edge
                // (minus the pin marker's slot when pinned), in columns. The
                // leading remote icon + space cost 2 columns of the drawn
                // string, so the alias budget subtracts them — a fixed pixel
                // reserve under-counted at larger font sizes and let long
                // aliases run past the hover pill.
                let text_x = hx + s(14.0);
                // 右缘预算：置顶行给 pin 记号留槽，普通行只留呼吸气口。此前
                // 无条件扣 42px——未置顶的行右边一片空白，地址却被截成
                // 「192.168.…」。
                let right = hx + hw - if pinned { s(42.0) } else { s(10.0) };
                let row_tracking = s(0.35);
                let max_cols = (((right - text_x) / (cell_w + row_tracking)).floor() as usize)
                    .saturating_sub(2);
                let label = truncate_tab_label(shown, max_cols.max(1));
                // 双行：名字在上、地址在下。地址是这一行的**身份**（置顶、删除、
                // 连接都按它走），但人认的是名字——所以名字占主行，地址退成一行
                // 更小更淡的副文本。两者都完整可见，不必再把名字截成「生…」。
                //
                // 没起过名字的旧条目退回单行：那时 `shown` 就是地址本身，画成
                // 两行等于把同一串字重复一遍，比留白更糟。
                const SUB_SCALE: f32 = tokens::type_scale::SUPPORTING;
                let two_line = shown != name;
                let sub_h = cell_h * SUB_SCALE;
                let title_y = if two_line {
                    hy + (hh - (cell_h + sub_h)) * 0.5
                } else {
                    hy + (hh - cell_h) * 0.5
                };
                // 图标跨着两行居中，而不是贴着名字那一行：它标注的是**整条**
                // 主机（名字和地址是同一台机器的两种写法），跟第一行对齐会让
                // 副行看起来像悬在图标外面的另一条目。
                //
                // 图标有**自己的槽**，宽度就是标签行品牌 logo 的那一格
                // （`cell_h * 0.72`），后面留 6px 气口再排文字——和标签行完全
                // 同一套节奏，两种行的图标因此同宽同列、文字也在同一竖线上。
                //
                // 之前是借用等宽网格（图标第 0 列、文字第 2 列，"\u{eb51} 设置"
                // 那类标签就是这么拼的）。但 Nerd Font 的图标墨迹比步进宽得多
                // 且各不相同（1.27～2.00 格，见 ui::os_icons），宽的那些右缘直
                // 接顶到文字上——这就是"贴得太紧"的来源。槽里再按墨迹缩到 0.82
                // 档，比品牌 logo 略收一点：logo 是彩色实心的，单色线条图标画到
                // 同样大就显得重。
                let icon =
                    super::ui::os_icons::resolve(d.nebula_ssh_icons.get(name).map(String::as_str));
                let icon_slot = (cell_h * 0.72).round();
                let icon_px = icon_slot * 0.82;
                let icon_mult = super::ui::os_icons::scale_for(icon, cell_w, icon_px);
                d.renderer.draw_chrome_text_scaled(
                    &size,
                    text_x + (icon_slot - icon_px) * 0.5,
                    hy + hh / 2.0 - cell_h * icon_mult / 2.0,
                    icon_mult,
                    color,
                    icon.glyph.encode_utf8(&mut [0u8; 4]),
                    &mut d.glyph_cache,
                );
                let label_x = text_x + icon_slot + s(6.0);
                d.renderer.draw_ui_text_tracked(
                    &size,
                    label_x,
                    title_y,
                    1.0,
                    row_tracking,
                    color,
                    nebula_terminal::term::cell::Flags::empty(),
                    &label,
                    &mut d.glyph_cache,
                );
                if two_line {
                    // 地址保持等宽：它是技术值，要被逐字符比对（哪台机器、哪个
                    // 端口），等宽下数字对齐读起来才快。名字是自然语言，走上面
                    // 那条路径。
                    let sub_cols =
                        (((hx + hw - s(42.0)) - (label_x)) / (cell_w * SUB_SCALE)).floor() as usize;
                    let sub = truncate_tab_label(name, sub_cols.max(1));
                    d.renderer.draw_ui_text_tracked(
                        &size,
                        label_x,
                        title_y + cell_h,
                        SUB_SCALE,
                        row_tracking,
                        sk.ink_faint,
                        nebula_terminal::term::cell::Flags::empty(),
                        &sub,
                        &mut d.glyph_cache,
                    );
                }
                if pinned {
                    // Pin marker pinned to the row's right edge (mirrors the
                    // × slot on tab rows).
                    d.renderer.draw_chrome_text(
                        &size,
                        hx + hw - s(10.0) - cell_w,
                        cy,
                        TXT_DIM,
                        "\u{eba0}",
                        &mut d.glyph_cache,
                    );
                }
            }
        }
    } else {
        // Collapsed: show the active tab's name centred in the top bar so the
        // user still knows where they are without the sidebar open.
        let title = d.nebula_tab_labels.get(d.nebula_active_tab).map(String::as_str).unwrap_or(".");
        let avail = ((w - 2.0 * margin - s(320.0)).max(cell_w) / cell_w).floor() as usize;
        let title = truncate_tab_label(title, avail.max(1));
        d.renderer.draw_chrome_text(
            &size,
            center_x(margin, w - 2.0 * margin, title.chars().count()),
            cy_top,
            TXT_ON_ACCENT,
            &title,
            &mut d.glyph_cache,
        );
    }
    // Settings tab text labels, above its quads.
    if d.nebula_settings_open {
        let view = d.settings_view();
        // Appearance 预览卡回显真实壁纸（同一 fit / 对齐 / 不透明度）：预
        // 览底色 quad 已在主 pass 画过，这里在示例文字之前叠图——文字仍
        // 浮在壁纸上，所见即所得。
        if view.section == settings::NebulaSettingsSection::Appearance {
            if let Some(wallpaper) = d.nebula_background_image.clone() {
                let trimmed = wallpaper.trim().trim_matches('"');
                if !trimmed.is_empty() {
                    if let Some((target, clip)) = settings::appearance_preview_wallpaper_rects(
                        &size,
                        scale,
                        view.area,
                        view.scroll,
                        view.hidden_hosts.len(),
                    ) {
                        d.renderer.draw_background_image(
                            &size,
                            std::path::Path::new(trimmed),
                            d.nebula_background_image_opacity,
                            d.nebula_background_image_fit,
                            d.nebula_background_image_alignment,
                            target,
                            clip,
                            s(10.0),
                        );
                    }
                }
            }
        }
        let settings_shell_icons =
            settings::draw_text(&view, &mut d.renderer, &mut d.glyph_cache, &size, scale);
        for (shell_id, rect) in settings_shell_icons {
            if let Some((id, rgba, px)) = d.shell_icon_pixels(&shell_id) {
                d.nebula_chrome_logo_draws.push((id, rgba, px, rect));
            }
        }
        // The expanded combobox floats ABOVE the page text: popup quads
        // first, then its option labels — the same base → base-text →
        // overlay → overlay-text layering the command palette needs.
        let mut popup_quads = Vec::new();
        settings::push_popup_quads(&view, &mut popup_quads, &size, scale);
        if !popup_quads.is_empty() {
            d.renderer.draw_ui(&size, &popup_quads);
            let popup_icons =
                settings::draw_popup_text(&view, &mut d.renderer, &mut d.glyph_cache, &size, scale);
            for (shell_id, rect) in popup_icons {
                if let Some((id, rgba, px)) = d.shell_icon_pixels(&shell_id) {
                    d.nebula_chrome_logo_draws.push((id, rgba, px, rect));
                }
            }
        }
    }

    if d.left_sidebar_visible() {
        message_queue_entry::draw_text(
            &mut d.renderer,
            &mut d.glyph_cache,
            &size,
            queue_entry_layout,
            d.nebula_message_queue_entry,
            sk,
            language,
            scale,
        );
    }

    // Drawer text remains in its own shell region beside the Settings page.
    if d.side_panel_visible() {
        if let Some(panel) = d.nebula_sftp_panel.as_ref().filter(|_| d.sftp_view_active()) {
            let scale = d.window.scale_factor as f32;
            let layout = sftp_panel::layout(&d.side_panel_layout(), scale);
            let ls_colors = side_panel::LsColors {
                dir: d.colors[nebula_terminal::vte::ansi::NamedColor::Blue],
                exec: d.colors[nebula_terminal::vte::ansi::NamedColor::Green],
            };
            sftp_panel::draw_text(
                panel,
                &layout,
                &d.nebula_theme,
                ls_colors,
                &mut d.renderer,
                &mut d.glyph_cache,
                &size,
                scale,
            );
        } else {
            // File-tree rows use the terminal's live ANSI palette so the drawer
            // matches `ls` colors exactly (dirs blue, executables green).
            let ls_colors = side_panel::LsColors {
                dir: d.colors[nebula_terminal::vte::ansi::NamedColor::Blue],
                exec: d.colors[nebula_terminal::vte::ansi::NamedColor::Green],
            };
            side_panel::draw_text(
                &d.nebula_side_panel,
                &d.side_panel_layout(),
                &d.nebula_theme,
                ls_colors,
                &mut d.renderer,
                &mut d.glyph_cache,
                &size,
                d.window.scale_factor as f32,
            );
        }
    }

    // A modal must be composited as a complete layer after every base label:
    // base quads → base text → palette quads → palette text. Painting the
    // palette panel in the first quad batch allowed Settings text to overwrite
    // it and produced the reported two-layer ghosting.
    let mut palette_quads = Vec::new();
    command_palette::push_quads(
        &d.nebula_palette,
        &d.nebula_theme,
        &mut palette_quads,
        &size,
        d.window.scale_factor as f32,
    );
    d.renderer.draw_ui(&size, &palette_quads);

    let shell_icon_draws = command_palette::draw_text(
        &d.nebula_palette,
        &d.nebula_theme,
        &mut d.renderer,
        &mut d.glyph_cache,
        &size,
        d.window.scale_factor as f32,
    );

    // Palette's full-color shell icons (textured quads) staged after all chrome
    // text. AI-session rows no longer come through here — they draw the
    // shared double-star glyph in skin ink, which follows the theme for free.
    for (icon_id, rect) in shell_icon_draws {
        if let Some((id, rgba, px)) = d.shell_icon_pixels(&icon_id) {
            d.nebula_chrome_logo_draws.push((id, rgba, px, rect));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabRevealMotionAction {
    Snap,
    Spring,
}

fn tab_reveal_motion_action(
    preference: super::settings::TabRevealMotion,
    was_visible: bool,
    is_visible: bool,
    dragging: bool,
) -> TabRevealMotionAction {
    if dragging
        || (is_visible && !was_visible && preference == super::settings::TabRevealMotion::Instant)
    {
        TabRevealMotionAction::Snap
    } else {
        TabRevealMotionAction::Spring
    }
}

#[cfg(test)]
mod sidebar_dock_tests {
    use super::{SidebarModel, chrome_tab_layout};
    use crate::display::SizeInfo;

    fn layout(tabs: usize, hosts: usize, hosts_open: bool) -> super::ChromeTabLayout {
        let size = SizeInfo::new(1400.0, 900.0, 10.0, 20.0, 0.0, 0.0, false);
        chrome_tab_layout(
            &size,
            1.0,
            SidebarModel {
                tab_count: tabs,
                host_count: hosts,
                tabs_open: true,
                hosts_open,
                tabs_scroll: 0,
                hosts_scroll: 0,
                sidebar_w: crate::display::SIDEBAR_W_LOGICAL,
                hosts_band: 0.0,
            },
            1.0,
        )
    }

    /// 停靠区的**下**沿贴着面板底，上沿随内容升降——这就是"沉底"。折起来时
    /// 只剩那条标题，它照样贴在最下面而不是跟着标签跑。
    #[test]
    fn hosts_dock_to_the_panel_bottom_whatever_they_contain() {
        for (hosts, open) in [(0, true), (4, true), (40, true), (12, false)] {
            let l = layout(3, hosts, open);
            let panel_bottom = l.panel.1 + l.panel.3;
            let dock_bottom = if open && hosts > 0 {
                l.hosts_band.1
            } else {
                l.hosts_header.1 + l.hosts_header.3
            };
            assert!(
                dock_bottom <= panel_bottom && panel_bottom - dock_bottom < 40.0,
                "hosts={hosts} open={open}：停靠区底 {dock_bottom} 离面板底 {panel_bottom} 太远",
            );
        }
    }

    /// 沉底之所以值得做：主机再多也吃不掉标签的地盘。这是旧的同流布局做不到
    /// 的——那时四十台主机会把标签一路顶出视口。
    #[test]
    fn a_long_host_list_never_starves_the_tab_list() {
        let few = layout(6, 2, true);
        let many = layout(6, 60, true);
        let visible = |l: &super::ChromeTabLayout| l.tabs.iter().filter(|r| r.2 > 0.0).count();

        assert_eq!(visible(&few), 6, "六个标签本来就该全放得下");
        assert_eq!(visible(&many), 6, "主机涨到 60 台，标签一行都不该少");
        assert!(many.hosts_max_scroll > 0, "放不下的主机应该退给滚动，而不是去挤标签");
    }

    /// 拖拽调节：HOSTS 分界的覆盖值取代弹性规则，且两端有钳制——往下拖到
    /// 只剩标题、往上拖也抢不走标签的最后一行。
    #[test]
    fn a_dragged_hosts_band_overrides_the_elastic_cap_within_clamps() {
        let with_band = |band: f32| {
            let size = SizeInfo::new(1400.0, 900.0, 10.0, 20.0, 0.0, 0.0, false);
            chrome_tab_layout(
                &size,
                1.0,
                SidebarModel {
                    tab_count: 3,
                    host_count: 20,
                    tabs_open: true,
                    hosts_open: true,
                    tabs_scroll: 0,
                    hosts_scroll: 0,
                    sidebar_w: crate::display::SIDEBAR_W_LOGICAL,
                    hosts_band: band,
                },
                1.0,
            )
        };
        let rows = |l: &super::ChromeTabLayout| l.hosts.iter().filter(|r| r.2 > 0.0).count();
        let auto = with_band(0.0);
        let small = with_band(140.0);
        assert!(rows(&small) < rows(&auto), "把停靠区拖矮后可见主机行必须变少");
        let huge = with_band(5000.0);
        assert!(huge.tabs[0].3 > 0.0, "覆盖值再大，标签区也要保住至少一行");
        assert!(rows(&huge) >= rows(&auto), "拖高后主机行不应少于弹性规则");
    }

    /// 停靠区高度必须真的跟着分界走，哪怕主机根本撑不满：只有一台主机时，
    /// 把分界往上拖，标题条要真的上移。此前 `hosts_band` 只当上限用，内容
    /// 长不到那么高，分界线就纹丝不动——手感上就是「拖不动」（用户 08-02）。
    #[test]
    fn dragging_the_divider_up_moves_it_even_with_a_single_host() {
        let layout = |band: f32| {
            let size = SizeInfo::new(1400.0, 900.0, 10.0, 20.0, 0.0, 0.0, false);
            chrome_tab_layout(
                &size,
                1.0,
                SidebarModel {
                    tab_count: 3,
                    host_count: 1,
                    tabs_open: true,
                    hosts_open: true,
                    tabs_scroll: 0,
                    hosts_scroll: 0,
                    sidebar_w: crate::display::SIDEBAR_W_LOGICAL,
                    hosts_band: band,
                },
                1.0,
            )
        };
        let hugging = layout(0.0);
        let dragged = layout(360.0);
        assert!(
            dragged.hosts_header.1 < hugging.hosts_header.1 - 100.0,
            "拖高 360px 后 HOSTS 标题条必须明显上移，而不是贴着那一台主机不动"
        );
        // 那一台主机仍然画在标题条下面（多出来的地盘是空白，不是把行推走）。
        assert!(dragged.hosts[0].3 > 0.0);
        assert!(dragged.hosts[0].1 > dragged.hosts_header.1);
    }


    #[test]
    fn a_dragged_sidebar_width_widens_the_panel_one_to_one() {
        let with_width = |w: f32| {
            let size = SizeInfo::new(1400.0, 900.0, 10.0, 20.0, 0.0, 0.0, false);
            chrome_tab_layout(
                &size,
                1.0,
                SidebarModel {
                    tab_count: 3,
                    host_count: 2,
                    tabs_open: true,
                    hosts_open: true,
                    tabs_scroll: 0,
                    hosts_scroll: 0,
                    sidebar_w: w,
                    hosts_band: 0.0,
                },
                1.0,
            )
        };
        let narrow = with_width(230.0);
        let wide = with_width(360.0);
        assert_eq!(wide.panel.2 - narrow.panel.2, 130.0);
        assert!(wide.tabs[0].2 > narrow.tabs[0].2, "行宽要跟着面板一起变宽");
    }

    /// 封顶是为了保护标签；标签不需要保护的时候就不该继续封着，否则两个标签
    /// 配四十台主机会在中间空出一大片。
    #[test]
    fn the_dock_grows_past_its_cap_when_tabs_leave_room() {
        let crowded = layout(20, 40, true);
        let roomy = layout(2, 40, true);
        let rows = |l: &super::ChromeTabLayout| l.hosts.iter().filter(|r| r.2 > 0.0).count();

        assert!(
            rows(&roomy) > rows(&crowded),
            "标签只有两个时主机该多显示几行（{} vs {}）",
            rows(&roomy),
            rows(&crowded),
        );
    }

    /// 两个区不能重叠：标签的最后一行必须停在停靠区标题上面。
    #[test]
    fn the_two_zones_never_overlap() {
        for tabs in [1, 5, 12, 30] {
            let l = layout(tabs, 8, true);
            assert!(
                l.tabs_band.1 <= l.hosts_header.1,
                "tabs={tabs}：标签带底 {} 压到了停靠区标题 {}",
                l.tabs_band.1,
                l.hosts_header.1,
            );
        }
    }
}

#[cfg(test)]
mod resize_edge_tests {
    use super::{
        ChromeHit, advance_spinner_phase, chrome_control_centers, contains_rect, in_chrome_bar,
        resize_edge, spinner_dot_center, tab_reveal_motion_action, window_control_hit_rect,
        window_control_visual_rect,
    };
    use crate::display::SizeInfo;
    use crate::display::settings::TabRevealMotion;

    #[test]
    fn maximized_or_fullscreen_windows_do_not_expose_drag_resize_edges() {
        let size = SizeInfo::new(1000.0, 800.0, 10.0, 20.0, 0.0, 0.0, false);
        assert!(resize_edge(&size, 1.0, 999.0, 400.0, true).is_some());
        assert!(resize_edge(&size, 1.0, 999.0, 400.0, false).is_none());
    }

    #[test]
    fn close_control_reaches_the_fullscreen_top_right_corner() {
        let size = SizeInfo::new(1000.0, 800.0, 10.0, 20.0, 0.0, 0.0, false);
        let controls = chrome_control_centers(size.width(), 8.0, 40.0, 1.0);
        let (_, close_x, close_y) = controls
            .into_iter()
            .find(|(hit, _, _)| *hit == ChromeHit::Close)
            .expect("close control");

        assert_eq!((close_x, close_y), (977.0, 28.0));
        assert!(controls.iter().all(|(_, _, center_y)| *center_y == close_y));
        assert!(contains_rect(window_control_hit_rect(close_x, close_y, 1.0), 999.0, 0.0));
        assert!(in_chrome_bar(&size, 1.0, 999.0, 0.0));
    }

    #[test]
    fn top_right_control_icons_share_one_visual_baseline_at_scaled_dpi() {
        let controls = chrome_control_centers(1920.0, 12.0, 60.0, 1.5);
        let baseline = controls[0].2;

        assert_eq!(baseline, 42.0);
        assert!(controls.iter().all(|(_, _, center_y)| *center_y == baseline));
    }

    #[test]
    fn caption_button_rects_are_contiguous_and_flush_right() {
        let width = 1000.0;
        let controls = chrome_control_centers(width, 8.0, 40.0, 1.0);
        let rects: Vec<_> = controls[2..]
            .iter()
            .map(|(_, center_x, _)| window_control_visual_rect(*center_x, 8.0, 40.0, 1.0))
            .collect();

        assert_eq!(rects[0].0 + rects[0].2, rects[1].0);
        assert_eq!(rects[1].0 + rects[1].2, rects[2].0);
        assert_eq!(rects[2].0 + rects[2].2, width);
        assert!(rects.iter().all(|rect| rect.1 == 0.0 && rect.3 == 48.0));
    }

    #[test]
    fn spinner_position_is_continuous_across_the_cycle_boundary() {
        let before = spinner_dot_center(0.999, 0, 0.0, 0.0, 4.5);
        let after = spinner_dot_center(0.001, 0, 0.0, 0.0, 4.5);
        let distance = (after.0 - before.0).hypot(after.1 - before.1);

        assert!(distance < 0.1, "cycle boundary jumped by {distance}px");
    }

    #[test]
    fn spinner_phase_advances_fractionally_and_preserves_wrap_remainder() {
        let half_turn = advance_spinner_phase(0.0, std::time::Duration::from_millis(400));
        let wrapped = advance_spinner_phase(0.99, std::time::Duration::from_millis(16));

        assert!((half_turn - 0.5).abs() < f32::EPSILON);
        assert!((wrapped - 0.01).abs() < 0.000_001);
    }

    #[test]
    fn instant_reveal_snaps_only_when_a_hidden_tab_becomes_visible() {
        assert_eq!(
            tab_reveal_motion_action(TabRevealMotion::Instant, false, true, false),
            super::TabRevealMotionAction::Snap
        );
        assert_eq!(
            tab_reveal_motion_action(TabRevealMotion::Slide, false, true, false),
            super::TabRevealMotionAction::Spring
        );
        assert_eq!(
            tab_reveal_motion_action(TabRevealMotion::Instant, true, true, false),
            super::TabRevealMotionAction::Spring
        );
        assert_eq!(
            tab_reveal_motion_action(TabRevealMotion::Instant, true, true, true),
            super::TabRevealMotionAction::Snap
        );
    }
}
