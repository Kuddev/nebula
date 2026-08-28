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

// 父模块宏会覆盖所有外置子模块里的同名预导入宏，让普通 `cargo check`
// 就能拦住会在 Windows GUI 无效标准句柄上 panic 的裸 print 调用。
#[allow(unused_macros)]
macro_rules! println {
    ($($args:tt)*) => {
        compile_error!("GPUI code must not use println!; use a fallible diagnostic sink")
    };
}

#[allow(unused_macros)]
macro_rules! eprintln {
    ($($args:tt)*) => {
        compile_error!("GPUI code must not use eprintln!; use try_write_stderr")
    };
}

mod assets;
pub mod code_tab;
pub mod config;
pub mod doc_tabs;
pub mod file_drop;
pub mod http;
pub mod math_view;
pub mod network_settings;
pub mod prelude;
pub mod session_restore;
pub mod settings_pane;
pub mod ssh_hosts;
pub mod ssh_settings;
pub mod terminal;
pub mod theme;
pub mod toast;
pub mod wallpaper;
pub mod widgets;
pub mod workspace;

use assets::NebulaAssets;
use gpui::{App, AppContext as _};

#[cfg(windows)]
use std::borrow::Cow;

/// GPUI 主窗没有可靠的标准错误句柄；保留父进程重定向诊断，但写失败不能
/// 再把非致命错误升级成应用 panic。
pub(crate) fn try_write_stderr(args: std::fmt::Arguments<'_>) {
    use std::io::Write as _;

    let _ = writeln!(std::io::stderr(), "{args}");
}

/// GPUI 壳的跨线程唤醒：托盘、mux ATTACH、runtime 控制都汇到工作区 pump。
pub(crate) enum GpuiShellEvent {
    TrayFocus(Option<u64>),
    TrayQuit,
    MuxAttach,
    RuntimeControl(std::sync::Arc<crate::runtime_api::RuntimeDispatch>),
    UpdateAvailable(crate::update_check::UpdateCheckResult),
}

/// 在当前线程启动 GPUI 运行时并打开主窗口，阻塞直至 UI 退出。
///
/// GPUI 拥有自己的消息循环：主窗形态从主线程调用（winit 不启动）；
/// spike 形态从专用线程调用。一个进程内只允许调用一次。
pub fn run_shell(initial_cwd: Option<std::path::PathBuf>) {
    let (shell_tx, shell_rx) = std::sync::mpsc::channel();
    crate::update_check::spawn_gpui_once(shell_tx.clone());
    let runtime_hub = crate::runtime_api::RuntimeHub::new();
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
    let runtime_server = crate::runtime_api::RuntimeServer::spawn_callback(
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
        runtime_hub.clone(),
    );
    if runtime_server.is_none() {
        // 与另一份进程同时启动时，对方可能已经拿到 owner lock、但尚未来得及
        // 发布 endpoint。短暂等待并交接，避免继续打开一个无控制面的空窗口。
        for _ in 0..40 {
            let behavior = nebula_settings::RuntimeSettings::load().windowing_behavior;
            let handed_over = match behavior {
                nebula_settings::WindowingBehaviorName::UseNew => {
                    crate::runtime_api::try_open_window_existing(initial_cwd.as_deref())
                },
                _ => initial_cwd.as_deref().map_or_else(
                    crate::runtime_api::try_open_default_tab_existing,
                    crate::runtime_api::try_open_directory_existing,
                ),
            };
            if handed_over {
                crate::tray::shutdown();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    let _runtime_server = runtime_server;
    gpui_platform::application().with_assets(NebulaAssets).run(move |cx| {
        // GPUI is the only event loop in `--gpui` mode, so it owns the same
        // per-process hook pipe before the first TerminalView spawns.
        let ai_events = crate::ai_hook::spawn_gpui_server();
        #[cfg(windows)]
        crate::ai_hook::spawn_config_guard();
        init(cx);
        cx.activate(true);
        open_main_window(cx, ai_events, shell_rx, runtime_hub, initial_cwd);
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

    // 用户 nebula.toml + nebula_settings.txt，启动读一次；失败回退默认，
    // 具体错误由 config 的 notice 上浮，并在可用时写入 stderr。
    let settings = config::Settings::load(theme::effective_theme_name(cx));
    gpui_component::set_locale(settings.ui_language.gpui_component_locale());
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
        try_write_stderr(format_args!(
            "[nebula:gpui] failed to register bundled Maple font: {error}"
        ));
    }
    // 用户导入的私有字体（设置页字体选择器写入的目录）：与旧壳
    // `refresh_private_fonts` 同源同径——启动一次性注册，选择器和终端
    // 都能立刻解析这些族。
    let imported = crate::font_install::imported_font_files();
    for path in &imported {
        match std::fs::read(path) {
            Ok(bytes) => {
                if let Err(error) = cx.text_system().add_fonts(vec![Cow::Owned(bytes)]) {
                    try_write_stderr(format_args!(
                        "[nebula:gpui] failed to register imported font {}: {error}",
                        path.display()
                    ));
                }
            },
            Err(error) => {
                try_write_stderr(format_args!(
                    "[nebula:gpui] failed to read imported font {}: {error}",
                    path.display()
                ));
            },
        }
    }
}

fn open_main_window(
    cx: &mut App,
    ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
    shell_events: std::sync::mpsc::Receiver<GpuiShellEvent>,
    runtime_hub: crate::runtime_api::RuntimeHub,
    initial_cwd: Option<std::path::PathBuf>,
) {
    workspace::windowing::initialize(cx, runtime_hub);
    workspace::windowing::open_initial_window(cx, ai_events, shell_events, initial_cwd);
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

/// 只恢复被 `SW_HIDE` 的 HWND，不改变当前前台窗口。
///
/// Windows 的 `SW_SHOW` 自带激活语义，不能用来处理后台 runtime 请求；
/// 显式 focus 由调用方在恢复可见性后另行调用 `activate_window`。
pub(crate) fn reveal_native_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        native_show(window, true);
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

/// 把藏起来的窗口全部显示出来。
///
/// 必须 `defer`：本函数会从 `apply_runtime_settings` →
/// `reveal_if_tray_disabled` 被调到，而那条链路正处在某个窗口自己的 update
/// 回调里（设置页事件）。此刻该窗口已被 `App::update_window` 从 slot 里 take
/// 走，对同一 handle 再 update 只会拿到 `Err("window not found")`。
///
/// 2026-08-21：这里原先是 `let _ = handle.update(..)`，吞掉那个错误的后果很重
/// ——「窗口已最小化到托盘 + 在设置页关掉托盘」时，托盘图标按设置消失了，窗口
/// 却没被显示出来，而 `reveal_session` 已经把 `window_hidden` 置回 false，
/// 守卫 `if self.window_hidden && ..` 从此永假、不再重试：应用变成既无窗口也无
/// 托盘图标、只能从任务管理器结束的僵尸。与 `wallpaper::apply_window_effects`
/// 同一个根因、同一个修法。
pub(crate) fn reveal_all_windows(cx: &mut App) {
    cx.defer(|cx| {
        for handle in cx.windows() {
            if let Err(err) = handle.update(cx, |_, window, _| {
                reveal_native_window(window);
            }) {
                log::warn!("failed to reveal window: {err}");
            }
        }
    });
}

/// 给 GPUI 主窗补上任务栏悬停预览 / Alt+Tab 用的窗口图标。
///
/// exe 资源里有图标（`windows/nebula.rc` 的 `IDI_ICON`），任务栏按钮因此
/// 是对的；但 GPUI 注册窗口类时不带 `hIcon`，也从不发 `WM_SETICON`，于是
/// 悬停预览角标和 Alt+Tab 走系统默认白框占位图（用户 #51）。开窗后把资源
/// 图标按系统尺寸各挂一份即可，`LR_SHARED` 句柄由系统缓存、无需释放。
#[cfg(windows)]
pub(crate) fn set_native_window_icon(window: &gpui::Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_SHARED, LoadImageW, SM_CXICON,
        SM_CXSMICON, SendMessageW, WM_SETICON,
    };

    /// `windows/nebula.rc` 里的 `IDI_ICON`。
    const IDI_ICON: u16 = 0x101;

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;

    // SAFETY: 模块句柄取自身；LoadImageW 按 MAKEINTRESOURCE 约定接受资源序号，
    // 失败返回 null；SendMessageW 对 null 图标只是清除该槽位，均安全失败。
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let resource = IDI_ICON as usize as *const u16;
        for (slot, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = GetSystemMetrics(metric);
            let icon = LoadImageW(module, resource, IMAGE_ICON, size, size, LR_SHARED);
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, slot as usize, icon as isize);
            }
        }
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
        windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE
    } else {
        windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE
    };
    // SAFETY: hwnd 来自当前 GPUI 窗口；ShowWindow 对无效句柄安全失败。
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, cmd);
    }
    true
}
