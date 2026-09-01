//! Backend-neutral geometry shared by GPUI and the legacy renderer.

pub(crate) type Rect = (f32, f32, f32, f32);

/// Return the top coordinate for content vertically centered in a container.
#[inline]
pub(crate) fn centered_y(container_y: f32, container_h: f32, content_h: f32) -> f32 {
    container_y + (container_h - content_h) * 0.5
}

/// Outer and inset rectangles for a modal pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PaneGeometry {
    pub(crate) panel: Rect,
    pub(crate) content: Rect,
}

/// Compute a pane rect and its inset content rect in physical pixels.
pub(crate) fn pane_geometry(
    win_w: f32,
    win_h: f32,
    scale: f32,
    desired_w: f32,
    desired_h: f32,
    margin: f32,
    inset: f32,
    top: Option<f32>,
) -> PaneGeometry {
    let scaled_margin = margin * scale;
    pane_geometry_in_horizontal_bounds(
        win_w,
        win_h,
        scale,
        desired_w,
        desired_h,
        scaled_margin / scale.max(f32::EPSILON),
        inset,
        top,
        (scaled_margin, (win_w - scaled_margin).max(scaled_margin)),
    )
}

/// Compute a pane inside explicit physical-pixel horizontal bounds.
pub(crate) fn pane_geometry_in_horizontal_bounds(
    win_w: f32,
    win_h: f32,
    scale: f32,
    desired_w: f32,
    desired_h: f32,
    margin: f32,
    inset: f32,
    top: Option<f32>,
    horizontal_bounds: (f32, f32),
) -> PaneGeometry {
    let scaled = |value: f32| value * scale;
    let margin = scaled(margin);
    let inset = scaled(inset);
    let left = horizontal_bounds.0.clamp(0.0, win_w);
    let right = horizontal_bounds.1.clamp(left, win_w);
    let available_w = (right - left).max(0.0);
    let panel_w = desired_w.min(available_w);
    let panel_h = desired_h.min((win_h - 2.0 * margin).max(0.0));
    let panel_x = left + (available_w - panel_w) * 0.5;
    let centered_y = ((win_h - panel_h) * 0.5).max(margin);
    let panel_y = top
        .map_or(centered_y, |value| value.min((win_h - panel_h - margin).max(margin)).max(margin));
    PaneGeometry {
        panel: (panel_x, panel_y, panel_w, panel_h),
        content: (
            panel_x + inset,
            panel_y + inset,
            (panel_w - inset * 2.0).max(0.0),
            (panel_h - inset * 2.0).max(0.0),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{centered_y, pane_geometry};

    #[test]
    fn centered_content_uses_the_container_center_line() {
        assert_eq!(centered_y(10.0, 40.0, 16.0), 22.0);
        assert_eq!(centered_y(10.0, 40.0, 40.0), 10.0);
    }

    #[test]
    fn pane_geometry_keeps_the_panel_inside_window_margins() {
        let pane = pane_geometry(800.0, 600.0, 2.0, 900.0, 700.0, 8.0, 12.0, None);
        assert_eq!(pane.panel, (16.0, 16.0, 768.0, 568.0));
        assert_eq!(pane.content, (40.0, 40.0, 720.0, 520.0));
    }
}
