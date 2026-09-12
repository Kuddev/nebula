//! Keep the standard window controls in AppKit's native toolbar hierarchy.

use gpui::Window;
use objc2::{MainThreadOnly, rc::Retained};
use objc2_app_kit::{NSToolbar, NSView, NSWindow, NSWindowButton, NSWindowToolbarStyle};
use objc2_foundation::{NSRect, ns_string};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

fn native_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    // SAFETY: GPUI owns this live NSView and calls the adapter on the UI thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

pub(in super::super) fn configure(window: &Window) {
    let Some(native) = native_window(window) else { return };
    let toolbar = NSToolbar::initWithIdentifier(
        NSToolbar::alloc(native.mtm()),
        ns_string!("pebrel-window-controls"),
    );
    // An empty native toolbar supplies the system's current window-control
    // treatment. AppKit retains it; GPUI continues to render the app controls.
    native.setToolbarStyle(NSWindowToolbarStyle::Unified);
    native.setToolbar(Some(&toolbar));
}

pub(super) fn layout(window: &Window) -> (f32, f32) {
    if window.is_fullscreen() {
        return (52.0, 8.0);
    }
    let Some(native) = native_window(window) else { return (52.0, 96.0) };
    let Some(close) = native.standardWindowButton(NSWindowButton::CloseButton) else {
        return (52.0, 96.0);
    };
    let Some(zoom) = native.standardWindowButton(NSWindowButton::ZoomButton) else {
        return (52.0, 96.0);
    };
    let Some(content) = native.contentView() else { return (52.0, 96.0) };
    let close = close.convertRect_toView(close.bounds(), Some(&content));
    let zoom = zoom.convertRect_toView(zoom.bounds(), Some(&content));
    control_layout(close, zoom, content.bounds(), content.isFlipped())
}

fn control_layout(close: NSRect, zoom: NSRect, content: NSRect, flipped: bool) -> (f32, f32) {
    let center = close.origin.y + close.size.height / 2.0;
    let center_from_top = if flipped {
        center - content.origin.y
    } else {
        content.origin.y + content.size.height - center
    };
    (
        2.0 * center_from_top as f32,
        (zoom.origin.x + zoom.size.width - content.origin.x + 16.0) as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSSize};

    #[test]
    fn app_controls_follow_native_centers_and_leave_room_for_the_group() {
        for height in [600.0, 900.0] {
            for (inset, center) in [(19.0, 26.0), (12.0, 20.0)] {
                for flipped in [false, true] {
                    let content = NSRect::new(NSPoint::new(5.0, 3.0), NSSize::new(900.0, height));
                    let y = 3.0 + if flipped { center - 7.0 } else { height - center - 7.0 };
                    let close = NSRect::new(NSPoint::new(5.0 + inset, y), NSSize::new(14.0, 14.0));
                    let zoom = NSRect::new(NSPoint::new(5.0 + inset + 46.0, y), close.size);
                    let (bar_height, left_padding) = control_layout(close, zoom, content, flipped);
                    assert_eq!(bar_height / 2.0, center as f32);
                    assert_eq!(left_padding, (inset + 46.0 + 14.0 + 16.0) as f32);
                }
            }
        }
    }
}
