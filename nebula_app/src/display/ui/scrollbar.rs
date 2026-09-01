//! Backend-neutral overlay-scrollbar geometry and hit testing.

pub(crate) type Rect = (f32, f32, f32, f32);

/// Geometry for a trackless overlay scrollbar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OverlayScrollbar {
    pub(crate) thumb: Rect,
    pub(crate) hit: Rect,
    track_y: f32,
    track_h: f32,
    max_scroll: f32,
}

impl OverlayScrollbar {
    pub(crate) fn hit_test(self, x: f32, y: f32) -> bool {
        let (left, top, width, height) = self.hit;
        x >= left && x < left + width && y >= top && y < top + height
    }

    pub(crate) fn target_scroll(self, y: f32, grab: f32) -> f32 {
        let travel = (self.track_h - self.thumb.3).max(1.0);
        let thumb_y = (y - grab - self.track_y).clamp(0.0, travel);
        self.max_scroll * thumb_y / travel
    }

    pub(crate) fn target_offset(self, y: f32, grab: f32, max_offset: usize) -> usize {
        if self.max_scroll <= 0.0 {
            return 0;
        }
        (self.target_scroll(y, grab) / self.max_scroll * max_offset as f32)
            .round()
            .clamp(0.0, max_offset as f32) as usize
    }
}

pub(crate) fn overlay_scrollbar(
    area: Rect,
    viewport: f32,
    content: f32,
    scroll: f32,
    scale: f32,
) -> Option<OverlayScrollbar> {
    let max_scroll = content - viewport;
    if viewport <= 0.0 || max_scroll <= 0.0 {
        return None;
    }
    let scaled = |value: f32| value * scale;
    let track_y = area.1 + scaled(6.0);
    let track_h = (area.3 - scaled(12.0)).max(scaled(26.0));
    let thumb_h = (track_h * viewport / content).max(scaled(26.0)).min(track_h);
    let thumb_y = track_y + (track_h - thumb_h) * (scroll / max_scroll).clamp(0.0, 1.0);
    let visual_w = scaled(5.0);
    let hit_w = scaled(14.0);
    let right = area.0 + area.2 - scaled(4.0);
    Some(OverlayScrollbar {
        thumb: (right - visual_w, thumb_y, visual_w, thumb_h),
        hit: (right - hit_w, track_y, hit_w, track_h),
        track_y,
        track_h,
        max_scroll,
    })
}
