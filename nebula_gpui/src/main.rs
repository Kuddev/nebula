mod app;
mod terminal;
mod ui;
mod views;

use app::open_main_window;
use gpui_component_assets::Assets;

fn main() {
    gpui::Application::new().with_assets(Assets).run(|cx| {
        ui::bootstrap::init(cx);
        cx.activate(true);
        open_main_window(cx);
    });
}
