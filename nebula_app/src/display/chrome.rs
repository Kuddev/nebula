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
    /// Top-bar toggles for the right-side drawer's two views (otty-style).
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
    let margin = s(8.0);
    let inner_pad = s(6.0);
    let center_y = top + bar_h / 2.0;
    let close_x = width - margin - inner_pad - s(18.0);
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
    let inner_pad = s(6.0);
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
    let inner_pad = s(6.0);
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
        };
    }

    // Expanded panel: the left leg of the connected chrome L-frame. It shares
    // the top bar's left edge (`margin`) and abuts its bottom edge with NO gap
    // (`panel_top = top + bar_h`) so the two read as one panel; the join corners
    // are squared off in `draw_chrome`. It runs down to the window's bottom
    // margin (symmetric with the top bar's top at `margin`), leaving only a
    // breathing gap before the terminal grid on its right.
    let sw = SIDEBAR_W_LOGICAL * scale_factor;
    let t = expand.clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
    let slide = (1.0 - eased) * sw;
    let panel_x = margin - slide;
    let panel_w = (sw - margin - s(12.0)).max(s(120.0));
    let panel_top = top + bar_h;
    let panel_bottom = h - margin;
    let panel_h = (panel_bottom - panel_top).max(0.0);
    let panel = (panel_x, panel_top, panel_w, panel_h);

    // Vertical tab rows: full panel width minus inner padding, stacked below
    // the "TABS" header. The new-tab affordance is a small square at the right
    // end of that header row (revealed on sidebar hover), not a trailing row.
    let tab_pad = s(14.0);
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

    // "+" square plus its dropdown chevron, vertically centred in the header
    // band, pinned to the right. The chevron is the rightmost element; the
    // "+" sits just left of it.
    let plus_sz = s(20.0);
    let menu_w = s(15.0);
    let plus_y = panel_top + (header - plus_sz) * 0.5;
    let menu = (panel_x + panel_w - tab_pad - menu_w, plus_y, menu_w, plus_sz);
    let plus = (menu.0 - s(2.0) - plus_sz, plus_y, plus_sz, plus_sz);

    let tabs_header = (panel_x, panel_top, panel_w, header);

    // ---- Elastic accordion split ----
    // Each open section wants `count` rows. If both fit, both get what they
    // want; if not, the panel's row budget is split so neither section can
    // starve the other below half of the budget, and every overflowing
    // section scrolls behind its own scrollbar.
    let avail_rows = (((panel_h - header - hosts_header_h - bottom_pad + gap) / pitch)
        .floor()
        .max(0.0)) as usize;
    let tabs_want = if model.tabs_open { count } else { 0 };
    let hosts_want = if model.hosts_open { host_count } else { 0 };
    let (tabs_show, hosts_show) = if tabs_want + hosts_want <= avail_rows {
        (tabs_want, hosts_want)
    } else {
        let half = avail_rows / 2;
        let tabs_show = tabs_want.min(avail_rows.saturating_sub(hosts_want).max(half));
        let hosts_show = hosts_want.min(avail_rows - tabs_show);
        // Hand any slack back to the tabs section (hosts already capped).
        (tabs_want.min(avail_rows - hosts_show), hosts_show)
    };
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

    // Hosts section stacks right below the tabs band.
    let hosts_header_y = tabs_band.1 + if tabs_show > 0 { gap } else { 0.0 };
    let hosts_header = (panel_x, hosts_header_y, panel_w, hosts_header_h);
    let hosts_top = hosts_header_y + hosts_header_h;
    let mut hosts = Vec::with_capacity(host_count);
    for i in 0..host_count {
        if !model.hosts_open || i < hosts_scroll || i >= hosts_scroll + hosts_show {
            hosts.push(zero);
            continue;
        }
        let y = hosts_top + (i - hosts_scroll) as f32 * pitch;
        hosts.push((tab_x, y, tab_w, row_h));
    }
    let hosts_band_h = if hosts_show > 0 { hosts_show as f32 * pitch - gap } else { 0.0 };
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
    }
}

/// Draw the new-tab mark from the same physical rectangle used for its hit and
/// hover area. Maple's Codicon `add` glyph has asymmetric outline bounds, so
/// centering it by terminal-cell metrics makes the ink look offset even though
/// Maple itself is a monospaced text font. A two-stroke primitive keeps this
/// one control optically and geometrically centered at every DPI.
fn push_centered_add_icon(
    quads: &mut Vec<UiQuad>,
    rect: (f32, f32, f32, f32),
    scale: f32,
    color: Rgb,
) {
    let (x, y, width, height) = rect;
    let side = width.min(height);
    if side <= 0.0 {
        return;
    }

    let center_x = x + width * 0.5;
    let center_y = y + height * 0.5;
    let stroke = (side * 0.105).clamp((scale * 1.25).max(1.0), scale * 2.0);
    let arm = side * 0.285;
    let ink = Rgba::new(color.r, color.g, color.b, 235);

    quads.push(UiQuad::solid(
        center_x - arm,
        center_y - stroke * 0.5,
        arm * 2.0,
        stroke,
        stroke * 0.5,
        ink,
    ));
    quads.push(UiQuad::solid(
        center_x - stroke * 0.5,
        center_y - arm,
        stroke,
        arm * 2.0,
        stroke * 0.5,
        ink,
    ));
}

/// Native three-dot menu mark. Its geometry comes from the same control rect
/// as hover/hit-testing, so Maple private-use glyph metrics cannot skew it.
fn push_centered_more_icon(
    quads: &mut Vec<UiQuad>,
    rect: (f32, f32, f32, f32),
    scale: f32,
    color: Rgb,
) {
    let (x, y, width, height) = rect;
    let diameter = (2.2 * scale).max(2.0);
    let gap = (3.2 * scale).max(diameter + scale);
    let center_x = x + width * 0.5;
    let center_y = y + height * 0.5;
    let ink = Rgba::new(color.r, color.g, color.b, 235);
    for offset in [-gap, 0.0, gap] {
        quads.push(UiQuad::solid(
            center_x - diameter * 0.5,
            center_y + offset - diameter * 0.5,
            diameter,
            diameter,
            diameter * 0.5,
            ink,
        ));
    }
}

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

/// Draw the Nebula window chrome: top title bar and left tab sidebar.
///
/// This is the first chrome milestone: it paints the rounded, gradient
/// panels and pills with the dedicated UI renderer to validate the native
/// (egui-free) chrome pipeline. Text labels and interactivity follow.
pub(super) fn draw_chrome(d: &mut Display) {
    // Tick command palette cursor animation (blink cycle).
    if d.nebula_palette.is_open() {
        d.nebula_palette.tick_cursor();
    }

    // Chrome colors come from the theme skin (hover washes flip to dark
    // smoke on the light themes); close-hover red stays semantic.
    let palette = d.nebula_theme.palette();
    let sk = d.nebula_theme.skin();
    let language = d.ui_language();
    #[allow(non_snake_case)]
    let (HOVER_FILL, HOVER_FILL_STRONG) = (sk.hover, sk.hover_strong);
    const CLOSE_HOVER_FILL: Rgba = Rgba::new(240, 80, 104, 96);

    let size = d.size_info;
    let scale = d.window.scale_factor as f32;
    let w = size.width();
    let h = size.height();

    // Logical-pixel helper.
    let s = |v: f32| v * scale;

    let margin = s(8.0);
    let bar_h = s(40.0);
    let inner_pad = s(6.0);
    let radius = s(UI_CORNER_RADIUS_LOGICAL);
    let pill_h = bar_h - 2.0 * inner_pad;
    let pill_r = s(UI_CORNER_RADIUS_LOGICAL);
    let hairline_w = s(UI_HAIRLINE_LOGICAL).max(1.0);

    let mut quads: Vec<UiQuad> = Vec::new();

    // ---- Background ambient light (very subtle, drawn first) ----
    // Purple bloom in the lower-left, cool blue in the upper-right, giving
    // the flat backdrop a sense of depth without competing with content.
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

    // ---- Top title / tab bar ----
    // The top bar and the left sidebar form one connected chrome shell (an
    // L-frame): the top bar's bottom-left corner is squared so the sidebar
    // abuts it seamlessly, while the three outer corners keep the larger shell
    // radius. Collapsed (no sidebar) the bar is a standalone card, all four
    // corners shell-rounded.
    let shell_r = s(UI_SHELL_RADIUS_LOGICAL);
    let sidebar_expand = d.left_sidebar_progress();
    let top_y = margin;
    // The bottom-left corner closes toward square as the sidebar slides in, so
    // the join seals smoothly instead of the corner popping from round to square
    // the instant the panel becomes visible. `expand` is 0 collapsed → 1 open.
    let join_r = shell_r * (1.0 - sidebar_expand.clamp(0.0, 1.0));
    // [top-left, top-right, bottom-right, bottom-left]
    let top_bar_corners = [shell_r, shell_r, shell_r, join_r];
    quads.push(
        UiQuad::solid(margin, top_y, w - 2.0 * margin, bar_h, radius, palette.panel)
            .with_corners(top_bar_corners),
    );

    let tab_layout = chrome_tab_layout(&size, scale, d.sidebar_model(), sidebar_expand);

    // Sidebar toggle at the far left of the top bar folds the tab sidebar
    // away and back; it's the one tab affordance that survives collapse.
    let (tog_x, tog_y, tog_w, tog_h) = tab_layout.toggle;
    let toggle_hovered = d.nebula_chrome_hover == ChromeHit::SidebarToggle;
    if toggle_hovered {
        quads.push(UiQuad::solid(tog_x, tog_y, tog_w, tog_h, pill_r, HOVER_FILL_STRONG));
    }
    let icon_c = if toggle_hovered { sk.icon_hover } else { sk.icon };
    let line = Rgba::new(icon_c.r, icon_c.g, icon_c.b, 185);
    let cutout = Rgba::new(palette.panel.r, palette.panel.g, palette.panel.b, 255);
    let ix = tog_x + (tog_w - s(15.0)) * 0.5;
    let iy = tog_y + (tog_h - s(15.0)) * 0.5;
    let iw = s(15.0);
    let ih = s(15.0);
    let stroke = s(1.35).max(1.0);
    quads.push(UiQuad::solid(ix, iy, iw, ih, s(3.2), line));
    quads.push(UiQuad::solid(
        ix + stroke,
        iy + stroke,
        iw - 2.0 * stroke,
        ih - 2.0 * stroke,
        s(2.2),
        cutout,
    ));
    quads.push(UiQuad::solid(
        ix + s(5.3),
        iy + stroke,
        stroke,
        ih - 2.0 * stroke,
        stroke * 0.5,
        line,
    ));

    // Settings moved into the old product-mark slot.
    let (set_x, set_y, set_w, set_h) = chrome_settings_button_rect(&size, scale);
    let settings_hovered = d.nebula_settings_hover == SettingsHit::Toggle;
    if settings_hovered {
        quads.push(UiQuad::solid(set_x, set_y, set_w, set_h, pill_r, HOVER_FILL_STRONG));
    }

    // Sidebar panel background — the left leg of the chrome shell. Its top
    // corners are squared so it abuts the top bar's squared bottom-left into
    // one continuous L; only the two outer bottom corners keep the shell
    // radius. Draws through slide-out so folding stays visible.
    //
    // Seam seal: the sidebar is painted AFTER the top bar and in the same
    // panel color, so we extend its top edge UP by a couple of physical pixels
    // to tuck under the top bar. Without this, the top bar's bottom edge and
    // the sidebar's top edge each carry a 1px antialiasing falloff; where two
    // translucent edges merely abut, both are under-covered and the dark
    // background bleeds through as a hairline "black edge". Overlapping turns
    // that transition into panel-over-panel (≈1.5% delta, invisible) and the
    // gap is gone.
    if d.left_sidebar_visible() {
        let (pnl_x, pnl_y, pnl_w, pnl_h) = tab_layout.panel;
        let seam = s(2.0).max(1.0);
        quads.push(
            UiQuad::solid(pnl_x, pnl_y - seam, pnl_w, pnl_h + seam, radius, palette.panel)
                // [top-left, top-right, bottom-right, bottom-left]
                .with_corners([0.0, 0.0, shell_r, shell_r]),
        );
    }

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
        d.nebula_tab_anim = tab_layout.tabs.iter().map(|t| t.1).collect();
    }
    let mut tab_anim_active = false;

    for (index, (tab_x, row_y, tab_w, tab_h)) in tab_layout.tabs.iter().copied().enumerate() {
        // Scrolled-out / folded rows carry a zero rect: skip them, and snap
        // their eased position so they don't fly in when they reappear.
        if tab_w <= 0.0 {
            d.nebula_tab_anim[index] = row_y;
            continue;
        }
        let target_y = d.tab_drag_draw_y(index, row_y, &tab_layout);
        let cur = d.nebula_tab_anim[index];
        // The grabbed pill tracks the pointer 1:1 (easing it would feel
        // laggy); only the rows making way ease toward their slots.
        let dragging_this =
            d.nebula_tab_drag.as_ref().is_some_and(|d| d.active && d.source == index);
        let tab_y = if dragging_this || (target_y - cur).abs() < 0.5 {
            target_y
        } else {
            tab_anim_active = true;
            cur + (target_y - cur) * 0.28
        };
        d.nebula_tab_anim[index] = tab_y;
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
            // Floating-pill active tab: a soft accent wash over the pill plus
            // a hairline border. The narrow identity light below carries the
            // optional user-picked color.
            let accent = palette.edge_r;
            quads.push(UiQuad::solid(
                tab_draw_x - s(1.0),
                tab_draw_y - s(1.0),
                tab_w + s(2.0),
                tab_h + s(2.0),
                pill_r + s(1.0),
                Rgba::new(accent.r, accent.g, accent.b, 40),
            ));
            quads.push(UiQuad::solid(
                tab_draw_x,
                tab_draw_y,
                tab_w,
                tab_h,
                pill_r,
                palette.tab_bg_l,
            ));
            // The accent wash on top is a DARK-theme depth cue: the white pill
            // (`tab_bg_l`) is the state on the light themes, so tinting it just
            // grays the white the user asked to keep pure. Only the dark themes
            // layer the wash over their (dark) active pill.
            if !palette.is_light {
                quads.push(UiQuad::solid(
                    tab_draw_x,
                    tab_draw_y,
                    tab_w,
                    tab_h,
                    pill_r,
                    Rgba::new(accent.r, accent.g, accent.b, 26),
                ));
            }
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
            if has_dot {
                // The one state that earns a dot: an unseen result (bell
                // in a background tab / long command finished unseen).
                // Design-spec blue with a soft glow halo.
                let dot_d = s(6.0);
                let dot_x = close_x + (close_w - dot_d) / 2.0;
                let dot_y = close_y + (close_h - dot_d) / 2.0;
                let halo = dot_d * 3.0;
                quads.push(UiQuad::glow(
                    dot_x + dot_d / 2.0 - halo / 2.0,
                    dot_y + dot_d / 2.0 - halo / 2.0,
                    halo,
                    halo,
                    Rgba::new(82, 168, 255, 80),
                ));
                quads.push(UiQuad::solid(
                    dot_x,
                    dot_y,
                    dot_d,
                    dot_d,
                    dot_d / 2.0,
                    Rgba::new(82, 168, 255, 230),
                ));
            } else if running {
                // Spinner: three orbiting dots, head bright / tail dim —
                // state expressed through brightness, per the design
                // language. Phase derives from wall-clock millis, so no
                // frame counter has to live anywhere.
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_millis())
                    .unwrap_or(0);
                let phase = (millis / 100) % 8; // one revolution ≈ 800ms
                let cx = close_x + close_w / 2.0;
                let cy = close_y + close_h / 2.0;
                let radius = s(4.5);
                for k in 0..3u32 {
                    let step = (phase + 8 - k) % 8;
                    let angle = step as f32 * std::f32::consts::FRAC_PI_4;
                    let alpha = [225u8, 140, 70][k as usize];
                    let d = s(2.4);
                    quads.push(UiQuad::solid(
                        cx + radius * angle.cos() - d / 2.0,
                        cy + radius * angle.sin() - d / 2.0,
                        d,
                        d,
                        d / 2.0,
                        Rgba::new(palette.edge_r.r, palette.edge_r.g, palette.edge_r.b, alpha),
                    ));
                }
            }
            // Idle tab → nothing: the row stays clean by default.
        }
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
        push_centered_add_icon(&mut quads, tab_layout.plus, scale, plus_ink);
    }
    // The dropdown chevron beside "+": same hover-only lift.
    let (menu_x, menu_y, menu_w, menu_h) = tab_layout.menu;
    if d.nebula_chrome_hover == ChromeHit::NewTabMenu {
        quads.push(UiQuad::solid(menu_x, menu_y, menu_w, menu_h, pill_r, HOVER_FILL_STRONG));
    }
    push_centered_more_icon(
        &mut quads,
        tab_layout.menu,
        scale,
        if d.nebula_chrome_hover == ChromeHit::NewTabMenu { sk.icon_hover } else { sk.icon },
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
            push_centered_add_icon(
                &mut quads,
                tab_layout.hosts_add,
                scale,
                if d.nebula_chrome_hover == ChromeHit::AddSshHost {
                    sk.icon_hover
                } else {
                    sk.icon
                },
            );
        }
        let thumb_c = sk.scrollbar_thumb;
        for bar in [tab_layout.tabs_scrollbar, tab_layout.hosts_scrollbar].into_iter().flatten() {
            let (bx, by, bw, bh) = bar;
            quads.push(UiQuad::solid(bx, by, bw, bh, bw * 0.5, thumb_c));
        }
    }

    // Window controls keep native-size hit targets; icons are rendered in
    // the chrome text layer with Maple Nerd / Codicons.
    for (hit, cx, _cy) in chrome_control_centers(w, top_y, bar_h, scale) {
        let hovered = d.nebula_chrome_hover == hit;
        if hovered {
            let hit_w = s(42.0);
            quads.push(UiQuad::solid(
                cx - hit_w / 2.0,
                top_y + inner_pad,
                hit_w,
                pill_h,
                pill_r,
                if hit == ChromeHit::Close { CLOSE_HOVER_FILL } else { HOVER_FILL_STRONG },
            ));
        }
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
    d.step_chrome_anims();
    if d.side_panel_visible() {
        if let Some(panel) = d.nebula_sftp_panel.as_ref() {
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

    // Nebula command palette floats above the chrome; add its quads last.
    command_palette::push_quads(
        &d.nebula_palette,
        &d.nebula_theme,
        &mut quads,
        &d.size_info,
        d.window.scale_factor as f32,
    );

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
    const ICON_CHROME_MINIMIZE: &str = "\u{eaba}";
    const ICON_CHROME_MAXIMIZE: &str = "\u{eab9}";
    const ICON_CHROME_CLOSE: &str = "\u{eab8}";

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
        let hovered = d.nebula_chrome_hover == hit;
        let icon = match hit {
            ChromeHit::Minimize => ICON_CHROME_MINIMIZE,
            ChromeHit::Maximize => ICON_CHROME_MAXIMIZE,
            ChromeHit::Close => ICON_CHROME_CLOSE,
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
        let (pnl_x, pnl_y, _, _) = tab_layout.panel;
        let tabs_chevron = if d.nebula_tabs_section_open { "\u{eab4}" } else { "\u{eab6}" };
        const SECTION_TITLE_SCALE: f32 = 0.82;
        let section_title_tracking = s(0.65);
        let section_title_flags = nebula_terminal::term::cell::Flags::BOLD;
        d.renderer.draw_doc_text_tracked(
            &size,
            pnl_x + s(16.0),
            pnl_y + s(22.0),
            SECTION_TITLE_SCALE,
            section_title_tracking,
            TXT_DIM,
            section_title_flags,
            &format!("{}  {tabs_chevron}", language.pick("标签页", "TABS")),
            &mut d.glyph_cache,
        );
        for (index, (tab_x, row_y, tab_w, tab_h)) in tab_layout.tabs.iter().copied().enumerate() {
            // Scrolled-out / folded rows: zero rect, nothing to draw.
            if tab_w <= 0.0 {
                continue;
            }
            let row_y = d.nebula_tab_anim.get(index).copied().unwrap_or(row_y);
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
            // Real AI brand logo (claude/codex): a textured quad in the
            // icon slot, sized to the glyph ink height so it reads like
            // an icon, not a sticker. Staged here, drawn after ALL chrome
            // text (see nebula_chrome_logo_draws). Other programs keep
            // their Nerd Font glyph inside the label text.
            let mut text_x = tab_x + s(14.0) + hover_lift_x;
            let mut reserved = s(60.0);
            if let Some(logo) = d.nebula_tab_logos.get(index).copied().flatten() {
                let icon_s = (cell_h * 0.72).round();
                if let Some((id, rgba, px)) = d.ai_logo_pixels(logo, color) {
                    let icon_y = (draw_row_y + (tab_h - icon_s) / 2.0).round();
                    d.nebula_chrome_logo_draws.push((
                        id,
                        rgba,
                        px,
                        (text_x, icon_y, icon_s, icon_s),
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

            d.renderer.draw_doc_text_tracked(
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
            d.renderer.draw_doc_text_tracked(
                &size,
                hh_x + s(16.0),
                hh_y + (hh_h - cell_h * SECTION_TITLE_SCALE) / 2.0,
                SECTION_TITLE_SCALE,
                section_title_tracking,
                TXT_DIM,
                section_title_flags,
                &format!("{}  {hosts_chevron}", language.pick("SSH 主机", "SSH HOSTS")),
                &mut d.glyph_cache,
            );
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
                const HINT_SCALE: f32 = design_tokens::type_scale::SUPPORTING;
                let hint_flags = nebula_terminal::term::cell::Flags::empty();
                let (pnl_x, _, pnl_w, _) = tab_layout.panel;
                let text_x = hh_x + s(28.0);
                // Wrap budget in the TRUE scaled advance (`draw_doc_text`
                // steps by unfloored `average_advance × scale`, not by the
                // floored grid cell) or long lines overrun the panel edge.
                let hint_cell_w = d.glyph_cache.font_metrics().average_advance as f32 * HINT_SCALE;
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
                        d.renderer.draw_doc_text(
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
                    d.renderer.draw_doc_text(
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
                // Label budget from its real start to the row's right edge
                // (minus the pin marker's slot when pinned), in columns. The
                // leading remote icon + space cost 2 columns of the drawn
                // string, so the alias budget subtracts them — a fixed pixel
                // reserve under-counted at larger font sizes and let long
                // aliases run past the hover pill.
                let text_x = hx + s(14.0);
                let right = hx + hw - s(42.0);
                let row_tracking = s(0.35);
                let max_cols = (((right - text_x) / (cell_w + row_tracking)).floor() as usize)
                    .saturating_sub(2);
                let label = truncate_tab_label(name, max_cols.max(1));
                d.renderer.draw_chrome_text(
                    &size,
                    text_x,
                    cy,
                    color,
                    "\u{f489}",
                    &mut d.glyph_cache,
                );
                d.renderer.draw_doc_text_tracked(
                    &size,
                    text_x + cell_w * 2.0,
                    cy,
                    1.0,
                    row_tracking,
                    color,
                    nebula_terminal::term::cell::Flags::empty(),
                    &label,
                    &mut d.glyph_cache,
                );
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
        let settings_shell_icons =
            settings::draw_text(&view, &mut d.renderer, &mut d.glyph_cache, &size, scale);
        for (shell_id, rect) in settings_shell_icons {
            if let Some((id, rgba, px)) = d.shell_icon_pixels(&shell_id) {
                d.nebula_chrome_logo_draws.push((id, rgba, px, rect));
            }
        }
    }

    // Palette text (query + result rows) sits on top of every chrome label.
    // Drawer text remains in its own shell region beside the Settings page.
    if d.side_panel_visible() {
        if let Some(panel) = d.nebula_sftp_panel.as_ref() {
            let scale = d.window.scale_factor as f32;
            let layout = sftp_panel::layout(&d.side_panel_layout(), scale);
            sftp_panel::draw_text(
                panel,
                &layout,
                &d.nebula_theme,
                &mut d.renderer,
                &mut d.glyph_cache,
                &d.size_info,
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
                &d.size_info,
                d.window.scale_factor as f32,
            );
        }
    }
    let shell_icon_draws = command_palette::draw_text(
        &d.nebula_palette,
        &d.nebula_theme,
        &mut d.renderer,
        &mut d.glyph_cache,
        &d.size_info,
        d.window.scale_factor as f32,
    );

    // Palette's full-color shell icons (textured quads) staged after all chrome
    // text, like AI brand logos. Decode + cache each PNG once per id.
    for (shell_id, rect) in shell_icon_draws {
        if let Some((id, rgba, px)) = d.shell_icon_pixels(&shell_id) {
            d.nebula_chrome_logo_draws.push((id, rgba, px, rect));
        }
    }
}

#[cfg(test)]
mod resize_edge_tests {
    use super::resize_edge;
    use crate::display::SizeInfo;

    #[test]
    fn maximized_or_fullscreen_windows_do_not_expose_drag_resize_edges() {
        let size = SizeInfo::new(1000.0, 800.0, 10.0, 20.0, 0.0, 0.0, false);
        assert!(resize_edge(&size, 1.0, 999.0, 400.0, true).is_some());
        assert!(resize_edge(&size, 1.0, 999.0, 400.0, false).is_none());
    }
}
