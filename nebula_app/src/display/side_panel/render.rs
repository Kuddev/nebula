//! 侧栏抽屉的像素：布局取自 [`super::panel_layout`]，本模块只落笔。
//!
//! 从 `side_panel.rs` 拆出（2026-08-31）。

use super::*;

// ---- rendering (mirrors the `settings.rs` split: the parent `display::mod`
// hands in a snapshot + renderer; this module owns the drawer's pixels) ----

use crate::display::color::Rgb;
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use crate::display::ui::widgets;
use crate::display::{NebulaTheme, SizeInfo, UI_CORNER_RADIUS_LOGICAL};

/// Git status colors (GitHub Primer hues), picked per theme brightness so
/// they hold contrast on both surface families.
pub(crate) fn status_color(status: char, is_light: bool) -> Option<Rgb> {
    Some(match (status, is_light) {
        ('M' | 'R' | 'C', false) => Rgb::new(210, 153, 34),
        ('M' | 'R' | 'C', true) => Rgb::new(154, 103, 0),
        ('A', false) => Rgb::new(63, 185, 80),
        ('A', true) => Rgb::new(26, 127, 55),
        ('D', false) => Rgb::new(248, 81, 73),
        ('D', true) => Rgb::new(207, 34, 46),
        _ => None?, // '?' and friends fall back to dim ink.
    })
}

/// The terminal palette colors the tree rows share with `ls` (Nebula-List
/// paints dirs with ANSI Blue and executables with ANSI Green — the drawer
/// must agree with what the user sees in the grid, including theme switches).
#[derive(Clone, Copy)]
pub struct LsColors {
    pub dir: Rgb,
    pub exec: Rgb,
}

/// Executable extensions, matching Nebula-List's green set.
pub(crate) fn is_executable(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["exe", "dll", "bat", "cmd", "ps1", "com", "msi", "sh"]
        .iter()
        .any(|ext| lower.rsplit('.').next() == Some(*ext) && lower.contains('.'))
}

/// Push the drawer's background quads: the flat panel surface (same 底色 as
/// the left tab sidebar), the active header view-tab pill, and the filter input
/// box — all curved with the shared chrome radius.
/// Display columns of a drag-chip label (CJK counts 2) — shared by the quad
/// pass (chip width; it has no cell metrics) and the text pass, so the label
/// and its chip agree on the same width.
pub(crate) fn drag_chip_cols(name: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    name.chars().map(|c| c.width().unwrap_or(0).max(1)).sum()
}

pub(crate) fn panel_action_tooltip(
    panel: &SidePanel,
    layout: &PanelLayout,
    scale: f32,
    cell_w: f32,
) -> Option<((f32, f32, f32, f32), &'static str)> {
    if panel.view != PanelView::Files {
        return None;
    }
    let (action, label) = match panel.hover {
        PanelHit::FollowCurrentDirectory => {
            (PanelHit::FollowCurrentDirectory, "跟随当前终端  Alt+R")
        },
        PanelHit::NewTerminalHere => (PanelHit::NewTerminalHere, "在此新建终端  Alt+T"),
        PanelHit::RevealDirectory => (PanelHit::RevealDirectory, "在资源管理器中打开  Alt+O"),
        _ => return None,
    };
    let action_rect =
        panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
            .find_map(|(hit, rect)| (hit == action).then_some(rect))?;
    let s = |value: f32| value * scale;
    let (px, _, pw, _) = layout.panel;
    let width = (drag_chip_cols(label) as f32 * cell_w + s(16.0)).min(pw - s(16.0));
    let x = (action_rect.0 + action_rect.2 * 0.5 - width * 0.5)
        .clamp(px + s(8.0), px + pw - s(8.0) - width);
    Some(((x, action_rect.1 + action_rect.3 + s(6.0), width, s(26.0)), label))
}

pub(crate) fn push_quads(
    panel: &SidePanel,
    layout: &PanelLayout,
    theme: &NebulaTheme,
    quads: &mut Vec<UiQuad>,
    scale: f32,
    cell_w: f32,
) {
    let s = |v: f32| v * scale;
    let palette = theme.palette();
    let sk = theme.skin();
    let (px, py, pw, ph) = layout.panel;
    // Shared chrome radius + the tab sidebar's accent (edge_r) — so the drawer
    // curves and lights up exactly like the left vertical tabs.
    let radius = s(UI_CORNER_RADIUS_LOGICAL);
    let accent = palette.edge_r;

    // Panel surface: the SAME flat 底色 as the left tab sidebar (`palette.panel`,
    // not a gradient — the gradient budget belongs to the brand art, chrome
    // stays flat).
    quads.push(UiQuad::solid(px, py, pw, ph, radius, palette.panel));

    // One segmented shell owns both Files and Git. This is deliberately a
    // single dark control, not two unrelated pills floating in the header.
    let segment = (px + s(12.0), py + s(12.0), pw - s(24.0), s(32.0));
    let tab_w = (segment.2 - s(4.0)) * 0.5;
    let tab_h = segment.3 - s(4.0);
    let (fx, gx) = (segment.0 + s(2.0), segment.0 + s(2.0) + tab_w);
    let active_x = match panel.view {
        PanelView::Files => fx,
        PanelView::Git => gx,
    };
    let ty = segment.1 + s(2.0);
    quads.push(UiQuad::solid(segment.0, segment.1, segment.2, segment.3, radius, sk.input));
    quads.push(UiQuad::solid(active_x, ty, tab_w, tab_h, radius, sk.card));
    quads.push(UiQuad::solid(active_x, ty, tab_w, tab_h, radius, sk.surface));
    // Hover never changes segmented-control geometry or fill. The text pass
    // alone raises the inactive label's ink, so switching stays visually still.
    if let Some(git) = panel.git() {
        let count = git.unstaged.len() + git.staged.len();
        if count > 0 {
            let digits = count.to_string();
            let badge_w = digits.len() as f32 * cell_w + s(12.0);
            let content_w = cell_w + s(6.0) + cell_w * 3.0 + s(6.0) + badge_w;
            let start = gx + (tab_w - content_w) * 0.5;
            let badge_x = start + cell_w + s(6.0) + cell_w * 3.0 + s(6.0);
            quads.push(UiQuad::solid(
                badge_x,
                ty + (tab_h - s(16.0)) * 0.5,
                badge_w,
                s(16.0),
                s(16.0) * 0.5,
                sk.accent_soft,
            ));
        }
    }

    if panel.view == PanelView::Files {
        let tools = panel_tools_layout(layout);
        crate::display::ui::surface::push_stroke(
            quads,
            tools.directory,
            radius,
            scale,
            sk.hairline,
        );
        quads.push(UiQuad::solid(
            tools.directory.0,
            tools.directory.1,
            tools.directory.2,
            tools.directory.3,
            radius,
            sk.input,
        ));
        for (hit, (x, y, w, h)) in
            panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
        {
            let fill = if panel.hover == hit {
                Some(sk.hover_strong)
            } else if hit == PanelHit::FollowCurrentDirectory && !panel.custom_root_active() {
                Some(sk.accent_soft)
            } else {
                None
            };
            if let Some(fill) = fill {
                quads.push(UiQuad::solid(x, y, w, h, radius, fill));
            }
        }
        if !panel.custom_root_active() {
            let (_, y, w, _) = tools.follow;
            quads.push(UiQuad::solid(
                tools.follow.0 + w - s(7.0),
                y + s(4.0),
                s(5.0),
                s(5.0),
                s(5.0) * 0.5,
                sk.ok,
            ));
        }
    }

    // Hovered list row: a quiet wash under the pointer (never on top of the
    // selected pill — selection outranks hover).
    if let PanelHit::Row(i) = panel.hover {
        if i < layout.max_rows {
            let hover_ok = match panel.view {
                PanelView::Files => panel
                    .file_rows()
                    .get(panel.scroll + i)
                    .is_some_and(|row| panel.selected.as_ref() != Some(&row.path)),
                PanelView::Git => panel.git_row_is_file(i),
            };
            if hover_ok {
                let ry = layout.list_y + i as f32 * layout.row_h;
                quads.push(UiQuad::solid(
                    px + s(10.0),
                    ry - s(1.0),
                    pw - s(20.0),
                    layout.row_h - s(4.0),
                    radius,
                    sk.hover,
                ));
            }
        }
    }

    // Files-view filter box (input surface; accent ring while focused).
    if panel.view == PanelView::Files {
        let (sx, sy, sw, sh) = layout.search;
        if panel.search_focus {
            let a = sk.accent;
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw + s(2.0),
                sh + s(2.0),
                radius + s(1.0),
                Rgba::new(a.r, a.g, a.b, 200),
            ));
        }
        quads.push(UiQuad::solid(sx, sy, sw, sh, radius, sk.input));
        if panel.search_all_selected() && !panel.search.is_empty() {
            let columns: usize = panel.search.chars().map(|c| c.width().unwrap_or(0)).sum();
            let selection_x = sx + s(8.0) + cell_w * 1.8;
            let selection_w = (columns as f32 * cell_w).min(sw - (selection_x - sx) - s(8.0));
            quads.push(UiQuad::solid(
                selection_x - s(2.0),
                sy + s(6.0),
                selection_w + s(4.0),
                sh - s(12.0),
                s(4.0),
                sk.accent_soft,
            ));
        }

        // The selected file row wears the tab's floating-pill language: an
        // accent halo + the tab 底色 + a soft accent wash — the same treatment
        // the left sidebar's active tab and the header view-tab use, so a
        // picked row reads as "selected" identically across the whole chrome.
        // The dragged row shares it, so the drag has a visible subject from press.
        let marked = panel.drag_file.as_ref().map(|d| &d.path).or(panel.selected.as_ref());
        if let Some(mark) = marked {
            if let Some(i) = panel
                .file_rows()
                .iter()
                .skip(panel.scroll)
                .take(layout.max_rows)
                .position(|row| &row.path == mark)
            {
                let ry = layout.list_y + i as f32 * layout.row_h - s(1.0);
                let (px, _, pw, _) = layout.panel;
                let rx = px + s(10.0);
                let rw = pw - s(20.0);
                let rh = layout.row_h - s(2.0);
                quads.push(UiQuad::solid(
                    rx - s(1.0),
                    ry - s(1.0),
                    rw + s(2.0),
                    rh + s(2.0),
                    radius + s(1.0),
                    Rgba::new(accent.r, accent.g, accent.b, 40),
                ));
                quads.push(UiQuad::solid(rx, ry, rw, rh, radius, palette.tab_bg_l));
                quads.push(UiQuad::solid(
                    rx,
                    ry,
                    rw,
                    rh,
                    radius,
                    Rgba::new(accent.r, accent.g, accent.b, 26),
                ));
            }
        }

        // Drag ghost: a floating chip beside the pointer while a file is in
        // flight — the pointer alone was invisible feedback.
        if let Some(drag) = panel.drag_file.as_ref().filter(|d| d.active) {
            let (mx, my) = drag.pos;
            let chip_w = (drag_chip_cols(&drag.name) as f32 * s(8.0) + s(32.0)).min(s(220.0));
            quads.push(UiQuad::solid(
                mx + s(12.0),
                my + s(14.0),
                chip_w,
                s(26.0),
                s(8.0),
                sk.accent_soft,
            ));
            quads.push(UiQuad::solid(
                mx + s(12.0),
                my + s(14.0),
                s(2.0),
                s(26.0),
                s(1.0),
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 190),
            ));
        }
    } else if panel.git().is_some() {
        // Git view: the strip is either the commit-message input (accent
        // ring) or the three action buttons (暂存 / 提交 / 推送). Outside a
        // repository there is nothing to act on — no strip at all.
        let (sx, sy, sw, sh) = layout.search;
        if panel.commit_focus {
            let a = sk.accent;
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw + s(2.0),
                sh + s(2.0),
                radius + s(1.0),
                Rgba::new(a.r, a.g, a.b, 200),
            ));
            quads.push(UiQuad::solid(sx, sy, sw, sh, radius, sk.input));
            if panel.commit_all_selected() && !panel.commit_msg.is_empty() {
                let columns: usize = panel.commit_msg.chars().map(|c| c.width().unwrap_or(0)).sum();
                let selection_w = (columns as f32 * cell_w).min(sw - s(16.0));
                quads.push(UiQuad::solid(
                    sx + s(6.0),
                    sy + s(6.0),
                    selection_w + s(4.0),
                    sh - s(12.0),
                    s(4.0),
                    sk.accent_soft,
                ));
            }
        } else {
            for (bx, bw) in git_button_rects(sx, sw, s(6.0)) {
                quads.push(UiQuad::solid(bx, sy, bw, sh, radius, sk.input));
            }
            // Hovered action button brightens (hover wash over the pill).
            if panel.hover == PanelHit::Search {
                let (hx, _) = panel.hover_pos;
                for (bx, bw) in git_button_rects(sx, sw, s(6.0)) {
                    if hx >= bx && hx < bx + bw {
                        quads.push(UiQuad::solid(bx, sy, bw, sh, radius, sk.hover));
                    }
                }
            }
        }
    }

    // Tooltip is appended last so it floats above the search field below the
    // tools row. The text pass uses the same helper and therefore cannot drift.
    if let Some((tooltip, _)) = panel_action_tooltip(panel, layout, scale, cell_w) {
        let tooltip_radius = crate::display::ui::tokens::radius::CONTROL * scale;
        crate::display::ui::surface::push_stroke(
            quads,
            tooltip,
            tooltip_radius,
            scale,
            sk.hairline,
        );
        quads.push(UiQuad::solid(
            tooltip.0,
            tooltip.1,
            tooltip.2,
            tooltip.3,
            tooltip_radius,
            sk.card,
        ));
    }
}

/// The four git action buttons' `(x, w)` spans inside `sx..sx+sw`.
pub fn git_button_rects(sx: f32, sw: f32, gap: f32) -> [(f32, f32); 4] {
    let bw = (sw - 3.0 * gap) / 4.0;
    [(sx, bw), (sx + bw + gap, bw), (sx + 2.0 * (bw + gap), bw), (sx + 3.0 * (bw + gap), bw)]
}

/// Draw the drawer's text: header tabs, the summary line (cwd tail or the
/// branch ± counts), the filter box content, then the visible rows.
pub(crate) fn draw_text(
    panel: &SidePanel,
    layout: &PanelLayout,
    theme: &NebulaTheme,
    _ls: LsColors,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let sk = theme.skin();
    let is_light = theme.palette().is_light;
    let (px, py, pw, _) = layout.panel;
    let text_pad = s(12.0);
    // Truncation budgets are in display COLUMNS (CJK counts 2), matching
    // draw_chrome_text's advance — a char-count budget lets a CJK name run
    // twice as wide as intended, straight across the hover wash.
    // Paths left-truncate (`…tail` — the discriminating end stays visible);
    // file names right-truncate (`name…`, see `truncate_tab_label`).
    let clip_tail = |t: &str, budget_cols: usize| -> String {
        use unicode_width::UnicodeWidthChar;
        let budget = budget_cols.max(4);
        let total: usize = t.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        if total <= budget {
            return t.to_string();
        }
        // Walk from the end, keeping the widest tail that fits after the `…`.
        let mut used = 1usize; // the ellipsis column
        let mut tail = std::collections::VecDeque::new();
        for ch in t.chars().rev() {
            let w = ch.width().unwrap_or(0).max(1);
            if used + w > budget {
                break;
            }
            used += w;
            tail.push_front(ch);
        }
        format!("…{}", tail.iter().collect::<String>())
    };
    // Right edge every row's text must stop before: the hover wash ends at
    // `px + pw - s(10)`, keep a small inset inside it.
    let row_text_right = px + pw - s(18.0);

    // Center each icon/label group inside its segmented-control slot.
    let segment_x = px + s(12.0);
    let segment_w = pw - s(24.0);
    let slot_w = (segment_w - s(4.0)) * 0.5;
    let header_ty = widgets::centered_y(py + s(12.0), s(32.0), cell_h);
    let files_hover = panel.hover == PanelHit::ViewFiles;
    let git_hover = panel.hover == PanelHit::ViewGit;
    let files_ink = if panel.view == PanelView::Files {
        sk.ink_strong
    } else if files_hover {
        sk.ink
    } else {
        sk.ink_dim
    };
    let git_ink = if panel.view == PanelView::Git {
        sk.ink_strong
    } else if git_hover {
        sk.ink
    } else {
        sk.ink_dim
    };
    let files_content_w = cell_w + s(6.0) + cell_w * 4.0;
    let fx = segment_x + s(2.0) + (slot_w - files_content_w) * 0.5;
    r.draw_chrome_text(size, fx, header_ty, files_ink, ICON_FOLDER, gc);
    r.draw_chrome_text(size, fx + cell_w + s(6.0), header_ty, files_ink, "文件", gc);
    let git_count = panel.git().map(|git| git.unstaged.len() + git.staged.len()).unwrap_or(0);
    let badge = (git_count > 0).then(|| git_count.to_string());
    let badge_w = badge.as_ref().map_or(0.0, |text| text.len() as f32 * cell_w + s(12.0));
    let git_content_w =
        cell_w + s(6.0) + cell_w * 3.0 + if badge.is_some() { s(6.0) + badge_w } else { 0.0 };
    let gx = segment_x + s(2.0) + slot_w + (slot_w - git_content_w) * 0.5;
    r.draw_chrome_text(size, gx, header_ty, git_ink, ICON_BRANCH, gc);
    let git_label_x = gx + cell_w + s(6.0);
    r.draw_chrome_text(size, git_label_x, header_ty, git_ink, "Git", gc);
    if let Some(badge) = badge {
        r.draw_chrome_text(
            size,
            git_label_x + cell_w * 3.0 + s(12.0),
            header_ty,
            sk.accent,
            &badge,
            gc,
        );
    }

    let directory = panel_tools_layout(layout).directory;
    let summary_y = widgets::centered_y(directory.1, directory.3, cell_h);
    let scroll = panel.scroll;
    let row_ty = |i: usize| {
        widgets::centered_y(layout.list_y + i as f32 * layout.row_h, layout.row_h, cell_h)
    };

    match panel.view {
        PanelView::Files => {
            let tools = panel_tools_layout(layout);
            let path_x = tools.directory.0 + s(9.0) + cell_w + s(10.0);
            let summary_cols =
                (((tools.directory.0 + tools.directory.2 - s(9.0) - path_x) / cell_w).floor()
                    as usize)
                    .max(4);
            let (summary, summary_ink) = if let Some(notice) = panel.root_notice() {
                (clip_tail(notice, summary_cols), Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b))
            } else {
                (
                    panel
                        .root()
                        .map(|root| clip_tail(&root.display().to_string(), summary_cols))
                        .unwrap_or_else(|| "（无目录）".into()),
                    sk.ink_dim,
                )
            };
            let path_ink =
                if panel.hover == PanelHit::OpenDirectory { sk.ink_strong } else { summary_ink };
            r.draw_chrome_text(
                size,
                tools.directory.0 + s(9.0),
                summary_y,
                sk.ink_faint,
                ICON_HOME,
                gc,
            );
            r.draw_chrome_text(size, path_x, summary_y, path_ink, &summary, gc);

            for (hit, (x, y, w, h)) in
                panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
            {
                let enabled = panel.root().is_some() || hit == PanelHit::FollowCurrentDirectory;
                let ink = if panel.hover == hit {
                    sk.ink_strong
                } else if !enabled {
                    sk.ink_faint
                } else if hit == PanelHit::FollowCurrentDirectory && !panel.custom_root_active() {
                    sk.accent
                } else {
                    sk.ink_dim
                };
                let label = match hit {
                    PanelHit::RevealDirectory => ICON_FOLDER_OPEN,
                    PanelHit::NewTerminalHere => ICON_TERMINAL,
                    PanelHit::FollowCurrentDirectory => ICON_FOLLOW,
                    _ => continue,
                };
                let tx = x + ((w - cell_w) / 2.0).max(0.0);
                let ty = widgets::centered_y(y, h, cell_h);
                r.draw_chrome_text(size, tx, ty, ink, label, gc);
            }

            // Filter box: magnifier + query (caret while focused) or hint.
            let (sx, sy, _, sh) = layout.search;
            let search_ty = widgets::centered_y(sy, sh, cell_h);
            r.draw_chrome_text(size, sx + s(8.0), search_ty, sk.ink_faint, ICON_SEARCH, gc);
            let qx = sx + s(8.0) + cell_w * 1.8;
            if panel.search.is_empty() && !panel.search_focus {
                r.draw_chrome_text(size, qx, search_ty, sk.ink_faint, "筛选文件…", gc);
            } else {
                let shown = if panel.search_focus
                    && !panel.search_all_selected()
                    && crate::display::caret_blink_on()
                {
                    format!("{}▏", panel.search)
                } else {
                    panel.search.clone()
                };
                r.draw_chrome_text(size, qx, search_ty, sk.ink_strong, &shown, gc);
            }

            // Tree rows: chevron (dirs, tree mode only) + folder/file icon + name.
            let filtering = !panel.search.trim().is_empty();
            for (i, row) in panel.file_rows().iter().skip(scroll).take(layout.max_rows).enumerate()
            {
                let ry = row_ty(i);
                let mut x = px + text_pad + row.depth as f32 * cell_w * 2.4;
                if !filtering {
                    if row.is_dir && !row.is_parent {
                        let chev =
                            if row.expanded { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_RIGHT };
                        r.draw_chrome_text(size, x, ry, sk.ink_faint, chev, gc);
                    }
                    x += cell_w * 1.9;
                }
                let (icon, icon_ink, name_ink) = if row.ignored {
                    (
                        if row.is_dir && row.expanded {
                            ICON_FOLDER_OPEN
                        } else if row.is_dir {
                            ICON_FOLDER
                        } else {
                            file_type_icon(&row.name)
                        },
                        sk.ink_ignored,
                        sk.ink_ignored,
                    )
                } else if row.is_dir {
                    (
                        if row.expanded { ICON_FOLDER_OPEN } else { ICON_FOLDER },
                        sk.icon,
                        sk.ink_strong,
                    )
                } else {
                    (file_type_icon(&row.name), sk.ink_dim, sk.ink)
                };
                r.draw_chrome_text(size, x, ry, icon_ink, icon, gc);
                // Name budget from its REAL pixel start (indent + chevron +
                // icon) to the hover wash's right edge — a long name ends in
                // `…` exactly inside the wash instead of bleeding past it.
                let name_x = x + cell_w * 2.2;
                let name_cols = (((row_text_right - name_x) / cell_w).floor() as usize).max(2);
                let name = crate::display::truncate_tab_label(&row.name, name_cols);
                r.draw_chrome_text(size, name_x, ry, name_ink, &name, gc);
            }
            let has_real_rows = panel.file_rows().iter().any(|row| !row.is_parent);
            if !has_real_rows {
                let empty = if filtering {
                    crate::ux::EmptyState::new(
                        "没有匹配文件",
                        "当前筛选词未匹配工作区内容。",
                        "修改筛选词，或按 Esc 清空筛选。",
                    )
                } else if panel.root.is_none() {
                    crate::ux::EmptyState::new(
                        "没有可浏览的目录",
                        "当前终端尚未报告工作目录。",
                        "在终端中进入一个目录后点击刷新。",
                    )
                } else {
                    crate::ux::EmptyState::new(
                        "此目录为空",
                        "当前工作目录中没有可显示的文件。",
                        "在终端创建文件，或选择其他目录。",
                    )
                };
                let parent_row_offset = panel
                    .file_rows()
                    .first()
                    .is_some_and(|row| row.is_parent)
                    .then_some(layout.row_h)
                    .unwrap_or(0.0);
                let y = layout.list_y + parent_row_offset + s(8.0);
                r.draw_chrome_text(size, px + text_pad, y, sk.ink_strong, &empty.title, gc);
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    y + s(20.0),
                    sk.ink_dim,
                    &crate::display::truncate_tab_label(&empty.reason, 32),
                    gc,
                );
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    y + s(40.0),
                    sk.accent,
                    &crate::display::truncate_tab_label(&empty.action, 32),
                    gc,
                );
            }

            // Drag ghost label, riding the chip pushed by `push_quads`.
            // Same chip-width formula as there (that pass has no cell_w), then
            // truncated against the REAL glyph advance so the label always
            // ends inside the chip.
            if let Some(drag) = panel.drag_file.as_ref().filter(|d| d.active) {
                let (mx, my) = drag.pos;
                let ty = widgets::centered_y(my + s(14.0), s(26.0), cell_h);
                let chip_w = (drag_chip_cols(&drag.name) as f32 * s(8.0) + s(32.0)).min(s(220.0));
                let max_cols = (((chip_w - s(26.0)) / cell_w).floor() as usize).max(2);
                r.draw_chrome_text(
                    size,
                    mx + s(10.0) + s(12.0),
                    ty,
                    sk.ink_strong,
                    &crate::display::truncate_tab_label(&drag.name, max_cols),
                    gc,
                );
            }
        },
        PanelView::Git => match panel.git() {
            Some(git) => {
                // Branch line: icon + name strong; ↑ahead + line counts on the
                // right (an op error takes the line over instead).
                let bx = px + text_pad;
                r.draw_chrome_text(size, bx, summary_y, sk.ink_dim, ICON_BRANCH, gc);
                let branch =
                    clip_tail(if git.branch.is_empty() { "(no branch)" } else { &git.branch }, 18);
                r.draw_chrome_text(size, bx + cell_w * 1.8, summary_y, sk.ink_strong, &branch, gc);
                if let Some(err) = panel.op_error() {
                    let msg = clip_tail(&err, branch.chars().count() + 4);
                    let ex = px + pw - text_pad - msg.chars().count() as f32 * cell_w;
                    let c_del = status_color('D', is_light).unwrap();
                    r.draw_chrome_text(size, ex, summary_y, c_del, &msg, gc);
                } else {
                    let c_add = status_color('A', is_light).unwrap();
                    let c_del = status_color('D', is_light).unwrap();
                    let minus = format!("\u{2212}{}", git.minus);
                    let plus = format!("+{}", git.plus);
                    let ahead =
                        if git.ahead > 0 { format!("↑{} ", git.ahead) } else { String::new() };
                    let minus_x = px + pw - text_pad - minus.chars().count() as f32 * cell_w;
                    let plus_x = minus_x - (plus.chars().count() + 1) as f32 * cell_w;
                    let ahead_x = plus_x - (ahead.chars().count() + 1) as f32 * cell_w;
                    if !ahead.is_empty() {
                        r.draw_chrome_text(size, ahead_x, summary_y, sk.accent, &ahead, gc);
                    }
                    r.draw_chrome_text(size, plus_x, summary_y, c_add, &plus, gc);
                    r.draw_chrome_text(size, minus_x, summary_y, c_del, &minus, gc);
                }

                // Action strip: commit-message input while composing, else the
                // 暂存 / 提交 / 推送 buttons (disabled = dim ink).
                let (sx, sy, sw, sh) = layout.search;
                let strip_ty = widgets::centered_y(sy, sh, cell_h);
                if panel.commit_focus {
                    let caret = if !panel.commit_all_selected() && crate::display::caret_blink_on()
                    {
                        "▏"
                    } else {
                        ""
                    };
                    let shown = format!("{}{caret}", panel.commit_msg);
                    let hint = if panel.commit_msg.is_empty() {
                        "提交信息…  Enter 提交 · Esc 取消"
                    } else {
                        ""
                    };
                    if hint.is_empty() {
                        r.draw_chrome_text(size, sx + s(8.0), strip_ty, sk.ink_strong, &shown, gc);
                    } else {
                        r.draw_chrome_text(size, sx + s(8.0), strip_ty, sk.ink_faint, hint, gc);
                    }
                } else {
                    let busy = panel.op_running();
                    let stage_on = !busy && !git.unstaged.is_empty();
                    let commit_on = !busy && !git.staged.is_empty();
                    let pull_on = !busy;
                    let push_on = !busy && git.ahead > 0;
                    let push_label = if git.ahead > 0 {
                        format!("推送 ↑{}", git.ahead)
                    } else {
                        "推送".to_string()
                    };
                    let labels: [(&str, bool); 4] = [
                        (if busy { "…" } else { "暂存" }, stage_on),
                        ("提交", commit_on),
                        ("拉取", pull_on),
                        (&push_label, push_on),
                    ];
                    for ((bx, bw), (label, enabled)) in
                        git_button_rects(sx, sw, s(6.0)).into_iter().zip(labels)
                    {
                        let hovered = panel.hover == PanelHit::Search
                            && panel.hover_pos.0 >= bx
                            && panel.hover_pos.0 < bx + bw;
                        let cols: usize =
                            label.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
                        let lx = bx + (bw - cols as f32 * cell_w).max(0.0) / 2.0;
                        let ink = if enabled { sk.ink_strong } else { sk.ink_faint };
                        r.draw_chrome_text(
                            size,
                            lx,
                            strip_ty + if hovered { -s(1.0) } else { 0.0 },
                            ink,
                            label,
                            gc,
                        );
                    }
                }

                // Sectioned rows: 未暂存 header, its files, 已暂存 header, its
                // files — one flat scroll space.
                enum GLine<'a> {
                    Header(String),
                    File(char, &'a String),
                }
                let mut lines: Vec<GLine<'_>> = Vec::new();
                if git.unstaged.is_empty() && git.staged.is_empty() {
                    lines.push(GLine::Header("工作区干净".into()));
                } else {
                    lines.push(GLine::Header(format!("未暂存 ({})", git.unstaged.len())));
                    for (c, p) in &git.unstaged {
                        lines.push(GLine::File(*c, p));
                    }
                    lines.push(GLine::Header(format!("已暂存 ({})", git.staged.len())));
                    for (c, p) in &git.staged {
                        lines.push(GLine::File(*c, p));
                    }
                }
                for (i, line) in lines.iter().skip(scroll).take(layout.max_rows).enumerate() {
                    let ry = row_ty(i);
                    match line {
                        GLine::Header(t) => {
                            r.draw_chrome_text(size, px + text_pad, ry, sk.ink_dim, t, gc)
                        },
                        GLine::File(status, path) => {
                            let sc = status_color(*status, is_light).unwrap_or(sk.ink_dim);
                            r.draw_chrome_text(
                                size,
                                px + text_pad,
                                ry,
                                sc,
                                &status.to_string(),
                                gc,
                            );
                            let path_x = px + text_pad + cell_w * 2.0;
                            let path_cols =
                                (((row_text_right - path_x) / cell_w).floor() as usize).max(4);
                            let text = clip_tail(path, path_cols);
                            r.draw_chrome_text(size, path_x, ry, sk.ink, &text, gc);
                        },
                    }
                }
            },
            None => {
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    summary_y,
                    sk.ink_dim,
                    "不在 git 仓库中",
                    gc,
                );
            },
        },
    }

    if let Some((tooltip, label)) = panel_action_tooltip(panel, layout, scale, cell_w) {
        r.draw_chrome_text(
            size,
            tooltip.0 + s(8.0),
            widgets::centered_y(tooltip.1, tooltip.3, cell_h),
            sk.ink_strong,
            label,
            gc,
        );
    }
}
