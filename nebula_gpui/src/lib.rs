//! GPUI 组件验收实验场（scratch）。
//!
//! 产品代码不在这里：GPUI UI 层住在 `nebula_app/src/gpui_shell/`
//! （feature = "gpui-shell"），本 crate 只保留组件验收页，用于快速验证
//! gpui-component 组件、fork 补丁与上游升级回归，验证通过的方案进产品。

mod app;
mod ui;
mod views;

use gpui_component_assets::Assets;

/// 在当前线程启动 GPUI 运行时并打开组件验收窗口，阻塞直至 UI 退出。
pub fn run_shell() {
    gpui::Application::new().with_assets(Assets).run(|cx| {
        ui::bootstrap::init(cx);
        cx.activate(true);
        app::open_gallery_window(cx);
    });
}
