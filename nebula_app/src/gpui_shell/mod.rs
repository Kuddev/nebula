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

pub mod code_tab;
pub mod config;
pub mod doc_tabs;
pub mod http;
pub mod math_view;
pub mod prelude;
pub mod session_restore;
pub mod network_settings;
pub mod settings_pane;
pub mod ssh_hosts;
pub mod ssh_settings;
pub mod terminal;
pub mod theme;
pub mod toast;
pub mod wallpaper;
pub mod widgets;
pub mod workspace;

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use gpui_component_assets::Assets;

#[cfg(windows)]
use std::borrow::Cow;

use workspace::NebulaWorkspace;

/// GPUI 壳的跨线程唤醒：托盘、mux ATTACH、runtime 控制都汇到工作区 pump。
pub(crate) enum GpuiShellEvent {
    TrayFocus(Option<u64>),
    TrayQuit,
    MuxAttach,
    RuntimeControl(std::sync::Arc<crate::runtime_api::RuntimeDispatch>),
}

/// 在当前线程启动 GPUI 运行时并打开主窗口，阻塞直至 UI 退出。
///
/// GPUI 拥有自己的消息循环：主窗形态从主线程调用（winit 不启动）；
/// spike 形态从专用线程调用。一个进程内只允许调用一次。
pub fn run_shell() {
    let (shell_tx, shell_rx) = std::sync::mpsc::channel();
    crate::tray::init_gpui({
        let tx = shell_tx.clone();
        move |command| {
            let event = match command {
                crate::tray::GpuiTrayCommand::Focus(pane) => GpuiShellEvent::TrayFocus(pane),
                crate::tray::GpuiTrayCommand::Quit => GpuiShellEvent::TrayQuit,
            };
            let _ = tx.send(event);
        }
    });
    // 驻留进程写 runtime.port：第二份 `nebula --gpui` 经 ATTACH + tab.new
    // 并入当前窗口，而不是再拉一套进程。Drop 会删 port 文件。
    let _runtime_server = crate::runtime_api::RuntimeServer::spawn_callback(
        {
            let tx = shell_tx;
            move |callback| {
                let event = match callback {
                    crate::runtime_api::RuntimeCallback::Attach => GpuiShellEvent::MuxAttach,
                    crate::runtime_api::RuntimeCallback::Control(dispatch) => {
                        GpuiShellEvent::RuntimeControl(dispatch)
                    },
                };
                let _ = tx.send(event);
            }
        },
        crate::runtime_api::RuntimeHub::new(),
    );
    gpui::Application::new().with_assets(Assets).run(|cx| {
        // GPUI is the only event loop in `--gpui` mode, so it owns the same
        // per-process hook pipe before the first TerminalView spawns.
        let ai_events = crate::ai_hook::spawn_gpui_server();
        #[cfg(windows)]
        crate::ai_hook::spawn_config_guard();
        init(cx);
        cx.activate(true);
        open_main_window(cx, ai_events, shell_rx);
    });
    crate::tray::shutdown();
}

/// 组件库/主题/快捷键/用户配置的一次性初始化。
fn init(cx: &mut App) {
    #[cfg(windows)]
    register_bundled_fonts(cx);

    // 组件库必须只初始化一次，否则全局 action、菜单和主题状态会重复注册。
    gpui_component::init(cx);
    // TextView 的公式渲染钩子：旧壳数学管线（compile → 栅格化）接入组件库
    // 的 markdown 渲染；不注册时公式回退为源码文本。
    math_view::register(cx);
    // 网络图片加载：gpui 默认 NullHttpClient，markdown 文档里的 http(s)
    // 图源全部失败；换成 ureq 实现（跑在后台 executor）。
    http::register(cx);
    theme::apply_chrome_theme(cx);
    terminal::init(cx);
    workspace::init(cx);

    // 用户 nebula.toml + nebula_settings.txt，启动读一次；失败静默回退默认。
    // 诊断走 stderr：GPUI 主窗形态在旧壳 logger 建立之前拉起。
    let settings = config::Settings::load(theme::effective_theme_name(cx));
    match &settings.source_path {
        Some(path) => eprintln!("[nebula:gpui] config loaded: {}", path.display()),
        None if nebula_settings::settings_path().is_file() => eprintln!(
            "[nebula:gpui] runtime settings loaded: {} (no nebula.toml)",
            nebula_settings::settings_path().display()
        ),
        None => eprintln!("[nebula:gpui] no user config found, using defaults"),
    }
    cx.set_global(settings);
}

#[cfg(windows)]
fn register_bundled_fonts(cx: &App) {
    // GPUI resolves a family through the system collection first and silently
    // falls back when it is absent. Add Maple before any component/window can
    // resolve a font so the default remains the same private face as winit.
    if let Err(error) =
        cx.text_system().add_fonts(vec![Cow::Borrowed(crate::font_install::REQUIRED_FONT_BYTES)])
    {
        eprintln!("[nebula:gpui] failed to register bundled Maple font: {error}");
    }
    // 用户导入的私有字体（设置页字体选择器写入的目录）：与旧壳
    // `refresh_private_fonts` 同源同径——启动一次性注册，选择器和终端
    // 都能立刻解析这些族。
    let imported = crate::font_install::imported_font_files();
    for path in &imported {
        match std::fs::read(path) {
            Ok(bytes) => {
                if let Err(error) = cx.text_system().add_fonts(vec![Cow::Owned(bytes)]) {
                    eprintln!(
                        "[nebula:gpui] failed to register imported font {}: {error}",
                        path.display()
                    );
                }
            },
            Err(error) => {
                eprintln!("[nebula:gpui] failed to read imported font {}: {error}", path.display())
            },
        }
    }
}

fn open_main_window(
    cx: &mut App,
    ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
    shell_events: std::sync::mpsc::Receiver<GpuiShellEvent>,
) {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.0), px(540.0))),
        // 自绘 TitleBar 要求系统标题栏透明化，窗口控制按钮由组件库接管。
        titlebar: Some(TitleBar::title_bar_options()),
        app_id: Some("nebula".to_owned()),
        // 透明/模糊窗口必须在创建时声明，否则合成器不给 alpha 通道。
        window_background: wallpaper::initial_background_appearance(),
        ..Default::default()
    };

    cx.open_window(options, move |window, cx| {
        let workspace = cx.new(|cx| NebulaWorkspace::new(window, ai_events, shell_events, cx));
        // 调试/验收后门：启动即打开指定文档（见 open_document_at_startup）。
        if let Ok(path) = std::env::var("NEBULA_GPUI_OPEN_DOC")
            && !path.is_empty()
        {
            workspace.update(cx, |workspace, cx| {
                workspace.open_document_at_startup(path.into(), window, cx);
            });
        }
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("failed to open Nebula GPUI window");

    // 窗口存在后补一次视效应用（DWM backdrop 等窗口级效果此前无处落）。
    wallpaper::refresh(cx);
}

/// 按 `tray` 设置挂上或摘掉系统托盘图标（旧壳 `tray::set_enabled`）。
/// 启动、设置热应用、系统外观变化都经 [`theme::apply_chrome_theme`] 落到这里。
pub(crate) fn apply_tray_setting() {
    crate::tray::set_enabled(nebula_settings::RuntimeSettings::load().tray);
}

/// 关窗保留会话：藏起 HWND，PTY 与工作区继续活着。
///
/// 旧壳 detach 是销毁窗口、进程无窗驻留。GPUI 用 `SW_HIDE` 达到同一
/// 用户可见效果。禁止 `minimize_window`：托盘关着时任务栏不能留一个
/// 点不掉的最小化窗口。
pub(crate) fn hide_native_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        native_show(window, false);
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

/// 托盘点击 / 关掉托盘后：把藏着的窗口捞回来。
pub(crate) fn show_native_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        native_show(window, true);
    }
    window.activate_window();
}

pub(crate) fn reveal_all_windows(cx: &mut App) {
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| {
            show_native_window(window);
        });
    }
}

#[cfg(windows)]
fn native_show(window: &gpui::Window, show: bool) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;
    let cmd = if show {
        windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW
    } else {
        windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE
    };
    // SAFETY: hwnd 来自当前 GPUI 窗口；ShowWindow 对无效句柄安全失败。
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, cmd);
    }
    true
}
