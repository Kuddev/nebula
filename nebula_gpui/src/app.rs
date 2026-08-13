use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;

use crate::views::component_gallery::ComponentGallery;

pub fn open_gallery_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.0), px(540.0))),
        app_id: Some("nebula-gpui".to_owned()),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let gallery = cx.new(|cx| ComponentGallery::new(window, cx));
        cx.new(|cx| Root::new(gallery, window, cx))
    })
    .expect("failed to open Nebula gallery window");
}
