//! GPUI UI 层（产品代码，feature = "gpui-shell"）。
//!
//! 这里是 nebula.exe 体内的新 UI：终端视图/渲染元素/工作区与配置桥全部
//! 住在本模块，`nebula_gpui` crate 只是组件验收的实验场，不承载产品代码
//! （ROADMAP 终局形态第 1 条）。
//!
//! 两种拉起形态：
//! - `nebula --gpui`：GPUI 主窗形态——主线程直接进 GPUI 消息循环，winit
//!   旧壳完全不启动。P3 的产品验收形态；三闸门复测都在这里做。
//! - `NEBULA_GPUI_SHELL=1`（需 gpui-shell 构建）：spike 形态——GPUI 跑在
//!   专用线程，与 winit 旧壳同进程并存，仅用于双运行时验证，P3 完成后移除。

pub mod config;
pub mod prelude;
pub mod terminal;
pub mod theme;
pub mod workspace;

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use gpui_component_assets::Assets;

use workspace::NebulaWorkspace;

/// 在当前线程启动 GPUI 运行时并打开主窗口，阻塞直至 UI 退出。
///
/// GPUI 拥有自己的消息循环：主窗形态从主线程调用（winit 不启动）；
/// spike 形态从专用线程调用。一个进程内只允许调用一次。
pub fn run_shell() {
    gpui::Application::new().with_assets(Assets).run(|cx| {
        init(cx);
        cx.activate(true);
        open_main_window(cx);
    });
}

/// 组件库/主题/快捷键/用户配置的一次性初始化。
fn init(cx: &mut App) {
    // 组件库必须只初始化一次，否则全局 action、菜单和主题状态会重复注册。
    gpui_component::init(cx);
    theme::apply_nebula_theme(cx);
    workspace::init(cx);

    // 用户 nebula.toml + nebula_settings.txt，启动读一次；失败静默回退默认。
    // 诊断走 stderr：GPUI 主窗形态在旧壳 logger 建立之前拉起。
    let settings = config::Settings::load();
    match &settings.source_path {
        Some(path) => eprintln!("[nebula:gpui] config loaded: {}", path.display()),
        None => eprintln!("[nebula:gpui] no user config found, using defaults"),
    }
    cx.set_global(settings);
}

fn open_main_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.0), px(540.0))),
        // 自绘 TitleBar 要求系统标题栏透明化，窗口控制按钮由组件库接管。
        titlebar: Some(TitleBar::title_bar_options()),
        app_id: Some("nebula".to_owned()),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let workspace = cx.new(|cx| NebulaWorkspace::new(window, cx));
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("failed to open Nebula GPUI window");
}
