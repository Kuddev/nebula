//! GPUI UI 层（C 路线）：既可作为独立 bin 运行（开发/验证形态），
//! 也可被 nebula.exe 作为库在专用线程上拉起（产品形态）。
//! 铁律：领域逻辑不进本 crate；这里只有视图与组件接线。

mod app;
mod config;
mod terminal;
mod ui;
mod views;

use gpui_component_assets::Assets;

/// 在当前线程启动 GPUI 运行时并打开主窗口，阻塞直至 UI 退出。
///
/// GPUI 拥有自己的消息循环，宿主必须从一个专用线程调用（不能是
/// winit 主循环所在线程）。一个进程内只允许调用一次。
pub fn run_shell() {
    gpui::Application::new().with_assets(Assets).run(|cx| {
        ui::bootstrap::init(cx);
        cx.activate(true);
        app::open_main_window(cx);
    });
}
