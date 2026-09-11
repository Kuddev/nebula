//! Pebrel-owned reader geometry. GPUI supplies drawing, not document design.

pub(super) const PAGE_WIDTH: f32 = 720.0;
pub(super) const PAGE_MARGIN: f32 = 40.0;
pub(super) const TOP_MARGIN: f32 = 24.0;
pub(super) const BODY_SIZE: f32 = 14.0;
pub(super) const HEADING_BASE: f32 = 12.0;
pub(super) const LINE_HEIGHT: f32 = 1.7;
pub(super) const OUTLINE_WIDTH: f32 = 248.0;
pub(super) const OUTLINE_INDENT: f32 = 16.0;
/// The outline and file-info views share one panel width. Keep a readable
/// navigation column while leaving enough horizontal space for the document.
pub(super) const DETAILS_MIN_WIDTH: f32 = 216.0;
pub(super) const DETAILS_MAX_WIDTH: f32 = 360.0;
/// The visible divider is one pixel, but the pointer target is deliberately
/// wider so resizing does not depend on landing on an exact edge.
pub(super) const DETAILS_RESIZE_HIT_WIDTH: f32 = 8.0;
// Match the file-tree's 13px labels; secondary chrome is one step quieter.
pub(super) const CHROME_SIZE: f32 = 13.0;
pub(super) const SECONDARY_SIZE: f32 = 12.0;
pub(super) const ICON_SIZE: f32 = 16.0;
pub(super) const TOOLBAR_HEIGHT: f32 = 52.0;
pub(super) const PANEL_HEADER_HEIGHT: f32 = 44.0;
pub(super) const CONTROL_HEIGHT: f32 = 32.0;
pub(super) const ACTION_HEIGHT: f32 = 36.0;
pub(super) const PANEL_PADDING: f32 = 20.0;

pub(super) fn clamp_details_width(width: f32) -> f32 {
    width.clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH)
}
