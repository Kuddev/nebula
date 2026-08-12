use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};

use crate::views::component_gallery::ComponentGallery;
use crate::views::workspace::NebulaWorkspace;

pub fn open_main_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.0), px(540.0))),
        // 自绘 TitleBar 要求系统标题栏透明化，窗口控制按钮由组件库接管。
        titlebar: Some(TitleBar::title_bar_options()),
        app_id: Some("nebula-gpui".to_owned()),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let workspace = cx.new(|cx| NebulaWorkspace::new(window, cx));
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("failed to open Nebula GPUI window");
}

/// 组件验收页降级为辅助窗口，从工作区标题栏进入，仍用于回归组件接入面。
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
