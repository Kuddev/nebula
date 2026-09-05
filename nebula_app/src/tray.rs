//! 系统托盘常驻图标 + agent attention 状态（future_planning `T1-3`）。
//!
//! 终端最常见的姿态是「压在别的窗口底下跑着 agent」：任务栏闪烁会被忽略、
//! toast 会过期，而托盘图标一直在。任一 agent 停下来等输入时图标翻转成
//! attention 态（右下角橙点），右键菜单列出所有 agent pane 与状态，点击
//! 直达来源 pane——复用 toast 点击同一条 [`EventType::FocusWindow`] 路径，
//! 因此聚焦语义（还原最小化、选中 tab、聚焦 pane）与通知横幅完全一致。
//!
//! # 线程模型
//!
//! `Shell_NotifyIconW` 的回调消息需要一个窗口收；不能挂在 winit 的窗口上
//! （fork 治理红线：不往窗口层塞策略）。托盘因此拥有自己的线程：一扇永不
//! 显示的 Win32 窗口 + 独立消息泵。app 侧只做两件事——把 agent 清单写进
//! [`STATE`]、`PostMessageW` 摇醒托盘线程；托盘线程反向只通过
//! `EventLoopProxy` 发用户事件。两个方向都不共享锁跨越阻塞调用。
//!
//! 状态推送挂在既有的 1 Hz chrome 时钟（与会话自动保存同一节拍）：托盘是
//! 环境信息面，秒级延迟无感，换来的是零新增定时器与天然去抖。

#![cfg_attr(not(windows), allow(dead_code))]

use winit::window::WindowId;

/// GPUI 托盘动作：聚焦某个 pane，或真正退出驻留进程。
/// 旧壳 `tray::init(EventLoopProxy)` 路径不使用此枚举，菜单也没有「退出」。
#[derive(Debug, Clone, Copy)]
pub enum GpuiTrayCommand {
    Focus(Option<u64>),
    Quit,
}

/// 托盘菜单里的一行：一个正在跑 AI CLI 的 pane。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayAgent {
    /// 宿主窗口（点击后 `FocusWindow` 的路由目标）。
    pub window: WindowId,
    pub pane: u64,
    /// 菜单文案：「claude · nebula」（程序名 + cwd 尾段）。
    pub label: String,
    /// CLI 停在半路等用户（权限确认/提问），比「回合完成」更强。
    pub needs_attention: bool,
}

#[cfg(all(windows, feature = "legacy-shell"))]
pub use win::init;
#[cfg(all(windows, feature = "gpui-shell"))]
pub use win::init_gpui;
#[cfg(windows)]
pub use win::{refresh_app_icon, set_enabled, shutdown, update};

#[cfg(all(not(windows), feature = "legacy-shell"))]
pub fn init(_proxy: winit::event_loop::EventLoopProxy<crate::event::Event>) {}
#[cfg(not(windows))]
pub fn init_gpui(_on_command: impl Fn(GpuiTrayCommand) + Send + Sync + 'static) {}
#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) {}
#[cfg(not(windows))]
pub fn update(_agents: Vec<TrayAgent>) {}
#[cfg(not(windows))]
pub fn shutdown() {}
#[cfg(not(windows))]
pub fn refresh_app_icon() {}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    #[cfg(feature = "legacy-shell")]
    use winit::event_loop::EventLoopProxy;

    use super::TrayAgent;
    #[cfg(feature = "legacy-shell")]
    use crate::event::{Event, EventType};

    /// 托盘回调（lParam 携带鼠标消息；未 SETVERSION 的经典 v0 语义，
    /// Win7 至今行为一致，不依赖 v4 的坐标打包）。
    const WM_APP_TRAY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;
    /// STATE.agents 变了：重读并 NIM_MODIFY 图标/气泡提示。
    const WM_APP_REFRESH: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
    /// STATE.enabled 变了：NIM_ADD / NIM_DELETE。
    const WM_APP_SET: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;
    /// 进程收尾：删图标 + 退出消息泵（不删的话图标要等用户悬停才消失）。
    const WM_APP_SHUTDOWN: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 4;
    const WM_APP_ICON: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 5;

    /// 菜单命令 id 的起点；`MENU_AGENT_BASE + i` = 聚焦第 i 个 agent。
    const MENU_AGENT_BASE: usize = 1000;
    /// 「显示 Nebula」——没有 agent 时托盘仍要能把窗口捞回来。
    const MENU_SHOW: usize = 1;
    /// GPUI 驻留：hide 之后可能只剩托盘，需要一条真退出。旧壳没有此项。
    const MENU_QUIT: usize = 2;

    struct Shared {
        agents: Vec<TrayAgent>,
        enabled: bool,
    }

    #[cfg(feature = "legacy-shell")]
    static PROXY: OnceLock<EventLoopProxy<Event>> = OnceLock::new();
    /// GPUI 主窗没有 winit `EventLoopProxy`：点击托盘时走命令回调。
    static GPUI_COMMAND: OnceLock<Box<dyn Fn(super::GpuiTrayCommand) + Send + Sync>> =
        OnceLock::new();
    static STATE: Mutex<Shared> = Mutex::new(Shared { agents: Vec::new(), enabled: false });
    /// 托盘窗口句柄（isize 形态跨线程存取；0 = 线程未起或窗口未建）。
    static HWND: AtomicIsize = AtomicIsize::new(0);
    static THREAD_STARTED: AtomicBool = AtomicBool::new(false);

    fn state() -> std::sync::MutexGuard<'static, Shared> {
        // 中毒只表示某线程带锁 panic；清单本身是普通数据，继续用。
        STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 装好事件代理。真正的线程按需（首次启用）才起。
    #[cfg(feature = "legacy-shell")]
    pub fn init(proxy: EventLoopProxy<Event>) {
        let _ = PROXY.set(proxy);
    }

    /// GPUI 主窗：点击托盘时发 [`super::GpuiTrayCommand`]。
    #[cfg_attr(not(feature = "gpui-shell"), allow(dead_code))]
    pub fn init_gpui(on_command: impl Fn(super::GpuiTrayCommand) + Send + Sync + 'static) {
        let _ = GPUI_COMMAND.set(Box::new(on_command));
    }

    /// 开/关托盘图标（设置·高级）。开 = 保证线程在跑并 NIM_ADD；
    /// 关 = 只删图标，线程留着（重开是高频往复，不值得反复建窗）。
    pub fn set_enabled(enabled: bool) {
        state().enabled = enabled;
        if enabled {
            ensure_thread();
        }
        post(WM_APP_SET);
    }

    /// 发布最新 agent 清单。内容没变就不打扰托盘线程（1 Hz 调用方无需去抖）。
    pub fn update(agents: Vec<TrayAgent>) {
        {
            let mut shared = state();
            if !shared.enabled || shared.agents == agents {
                return;
            }
            shared.agents = agents;
        }
        post(WM_APP_REFRESH);
    }

    /// 进程退出前删掉图标。best-effort：托盘线程没起过就什么都不用做。
    pub fn shutdown() {
        post(WM_APP_SHUTDOWN);
    }

    pub fn refresh_app_icon() {
        post(WM_APP_ICON);
    }

    fn post(message: u32) {
        let hwnd = HWND.load(Ordering::Acquire);
        if hwnd != 0 {
            // SAFETY: hwnd 由托盘线程发布且只在 WM_APP_SHUTDOWN 后失效；
            // PostMessageW 对已销毁窗口安全失败。
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd as _, message, 0, 0);
            }
        }
    }

    fn ensure_thread() {
        if THREAD_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(err) = std::thread::Builder::new().name("nebula-tray".into()).spawn(tray_thread)
        {
            THREAD_STARTED.store(false, Ordering::SeqCst);
            log::warn!("tray: failed to spawn tray thread: {err}");
        }
    }

    // ─── 托盘线程 ────────────────────────────────────────────────────────

    /// 托盘线程本地资源：两个状态图标 + 图标是否已挂在通知区。
    struct Icons {
        normal: isize,
        attention: isize,
        added: bool,
    }

    impl Drop for Icons {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(self.normal as _);
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(self.attention as _);
            }
        }
    }

    thread_local! {
        static ICONS: RefCell<Option<Icons>> = const { RefCell::new(None) };
    }

    fn tray_thread() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
            TranslateMessage, WNDCLASSW, WS_OVERLAPPED,
        };

        let class_name = wide("NebulaTrayWindow");
        // SAFETY: 类名/实例句柄在调用期间有效；重复注册返回 0 也无妨——
        // 单进程只会走到这里一次（THREAD_STARTED 闸）。
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tray_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wc);
            // 隐藏顶层窗口（不是 message-only：通知区回调对 HWND_MESSAGE
            // 窗口在部分 Windows 版本上不投递）。永不 ShowWindow。
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("Nebula Tray").as_ptr(),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                log::warn!("tray: CreateWindowExW failed; tray icon disabled");
                return;
            }
            HWND.store(hwnd as isize, Ordering::Release);

            let icons = build_icons();
            ICONS.with(|slot| {
                *slot.borrow_mut() = Some(Icons {
                    normal: icons.0 as isize,
                    attention: icons.1 as isize,
                    added: false,
                })
            });
            // init 与首次 set_enabled 之间可能已经积累了状态：起跑先对齐。
            apply_enabled(hwnd);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: windows_sys::Win32::Foundation::HWND,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, PostQuitMessage, WM_LBUTTONUP, WM_RBUTTONUP,
        };
        match msg {
            WM_APP_TRAY => {
                match lparam as u32 {
                    // 左键 = 最短路径：直达最需要人的 pane。
                    WM_LBUTTONUP => focus_best(),
                    WM_RBUTTONUP => show_menu(hwnd),
                    _ => {},
                }
                0
            },
            WM_APP_REFRESH => {
                refresh_icon(hwnd);
                0
            },
            WM_APP_SET => {
                apply_enabled(hwnd);
                0
            },
            WM_APP_ICON => {
                replace_app_icon(hwnd);
                0
            },
            WM_APP_SHUTDOWN => {
                remove_icon(hwnd);
                // SAFETY: 本线程的消息泵，退出后线程结束。
                unsafe { PostQuitMessage(0) };
                0
            },
            // SAFETY: 转发给默认过程，参数原样透传。
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// 左键/菜单兜底的聚焦策略：先挑等输入的，再挑任意 agent，最后裸窗口。
    fn focus_best() {
        let target = {
            let shared = state();
            shared
                .agents
                .iter()
                .find(|agent| agent.needs_attention)
                .or_else(|| shared.agents.first())
                .cloned()
        };
        send_focus(target);
    }

    fn send_focus(agent: Option<TrayAgent>) {
        if let Some(command) = GPUI_COMMAND.get() {
            command(super::GpuiTrayCommand::Focus(agent.as_ref().map(|agent| agent.pane)));
            return;
        }
        #[cfg(feature = "legacy-shell")]
        {
            let Some(proxy) = PROXY.get() else { return };
            let event = match agent {
                Some(agent) => {
                    Event::new(EventType::FocusWindow { pane: Some(agent.pane) }, agent.window)
                },
                // 没有 agent：把第一扇窗捞到前台（processor 对 None 窗口 id 的
                // FocusWindow 就是这个语义）。
                None => Event::new(EventType::FocusWindow { pane: None }, None),
            };
            let _ = proxy.send_event(event);
        }
    }

    fn show_menu(hwnd: windows_sys::Win32::Foundation::HWND) {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_GRAYED, MF_SEPARATOR,
            MF_STRING, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
            TrackPopupMenu, WM_NULL,
        };

        // 菜单期间用快照：TrackPopupMenu 是模态泵，STATE 可能被 1 Hz 更新，
        // 命令 id 必须映射回打开菜单那一刻的清单。
        let agents = state().agents.clone();

        // SAFETY: 菜单句柄本函数创建/销毁；文案缓冲活过 AppendMenuW（菜单
        // 复制内容）。SetForegroundWindow + WM_NULL 是 TrackPopupMenu 在
        // 托盘场景的官方仪式（否则菜单点外部不收合）。
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            if agents.is_empty() {
                let text = wide("没有正在运行的 agent");
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr());
            }
            for (index, agent) in agents.iter().enumerate() {
                // 状态点进文案：菜单不画自定义图形，实心/空心圈已经把
                // 「等你」和「在跑」分开。
                let mark = if agent.needs_attention { "● " } else { "○ " };
                let suffix = if agent.needs_attention { " — 等待输入" } else { "" };
                let text = wide(&format!("{mark}{}{suffix}", agent.label));
                AppendMenuW(menu, MF_STRING, MENU_AGENT_BASE + index, text.as_ptr());
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            let show = wide(&format!("显示 {}", crate::brand::NAME));
            AppendMenuW(menu, MF_STRING, MENU_SHOW, show.as_ptr());
            // 旧壳没有托盘「退出」：真退出是 window+detached 都空。GPUI hide
            // 之后可能只剩托盘，所以只在 GPUI 回调路径上加这一项。
            if GPUI_COMMAND.get().is_some() {
                let quit = wide(&format!("退出 {}", crate::brand::NAME));
                AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());
            }

            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            ) as usize;
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);

            if cmd == MENU_SHOW {
                send_focus(None);
            } else if cmd == MENU_QUIT {
                if let Some(command) = GPUI_COMMAND.get() {
                    command(super::GpuiTrayCommand::Quit);
                }
            } else if cmd >= MENU_AGENT_BASE {
                send_focus(agents.get(cmd - MENU_AGENT_BASE).cloned());
            }
        }
    }

    // ─── 通知区图标 ──────────────────────────────────────────────────────

    fn notify_data(
        hwnd: windows_sys::Win32::Foundation::HWND,
    ) -> windows_sys::Win32::UI::Shell::NOTIFYICONDATAW {
        // SAFETY: NOTIFYICONDATAW 是纯 POD，零值是合法起点。
        let mut data: windows_sys::Win32::UI::Shell::NOTIFYICONDATAW =
            unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<windows_sys::Win32::UI::Shell::NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data
    }

    /// 依据 STATE 计算图标形态并 NIM_MODIFY；未挂图标时什么都不做。
    fn refresh_icon(hwnd: windows_sys::Win32::Foundation::HWND) {
        use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_TIP, NIM_MODIFY, Shell_NotifyIconW};

        let (agent_count, attention_count) = {
            let shared = state();
            let attention = shared.agents.iter().filter(|agent| agent.needs_attention).count();
            (shared.agents.len(), attention)
        };
        ICONS.with(|slot| {
            let slot = slot.borrow();
            let Some(icons) = slot.as_ref().filter(|icons| icons.added) else { return };
            let mut data = notify_data(hwnd);
            data.uFlags = NIF_ICON | NIF_TIP;
            data.hIcon = (if attention_count > 0 { icons.attention } else { icons.normal }) as _;
            let tip = if agent_count == 0 {
                crate::brand::NAME.to_owned()
            } else if attention_count > 0 {
                format!("{} — {attention_count} 个 agent 等待输入", crate::brand::NAME)
            } else {
                format!("{} — {agent_count} 个 agent 运行中", crate::brand::NAME)
            };
            copy_tip(&mut data.szTip, &tip);
            // SAFETY: data 完整初始化且 hwnd 属于本线程。
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        });
    }

    fn apply_enabled(hwnd: windows_sys::Win32::Foundation::HWND) {
        use windows_sys::Win32::UI::Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, Shell_NotifyIconW,
        };

        let enabled = state().enabled;
        ICONS.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(icons) = slot.as_mut() else { return };
            if enabled && !icons.added {
                let mut data = notify_data(hwnd);
                data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
                data.uCallbackMessage = WM_APP_TRAY;
                data.hIcon = icons.normal as _;
                copy_tip(&mut data.szTip, crate::brand::NAME);
                // SAFETY: 同上；失败（如 explorer 未起）留 added=false，
                // 下一次 WM_APP_SET 重试。
                if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0 {
                    icons.added = true;
                }
            } else if !enabled && icons.added {
                let data = notify_data(hwnd);
                // SAFETY: 删除只需要 hWnd/uID。
                unsafe {
                    windows_sys::Win32::UI::Shell::Shell_NotifyIconW(
                        windows_sys::Win32::UI::Shell::NIM_DELETE,
                        &data,
                    )
                };
                icons.added = false;
            }
        });
        if enabled {
            refresh_icon(hwnd);
        }
    }

    fn remove_icon(hwnd: windows_sys::Win32::Foundation::HWND) {
        ICONS.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(icons) = slot.as_mut().filter(|icons| icons.added) {
                let data = notify_data(hwnd);
                // SAFETY: 同 apply_enabled 的删除分支。
                unsafe {
                    windows_sys::Win32::UI::Shell::Shell_NotifyIconW(
                        windows_sys::Win32::UI::Shell::NIM_DELETE,
                        &data,
                    )
                };
                icons.added = false;
            }
        });
    }

    // ─── 图标像素 ────────────────────────────────────────────────────────

    fn replace_app_icon(hwnd: windows_sys::Win32::Foundation::HWND) {
        let (normal, attention) = build_icons();
        let replacement =
            Icons { normal: normal as isize, attention: attention as isize, added: false };
        if normal.is_null() || attention.is_null() {
            return;
        }
        remove_icon(hwnd);
        ICONS.with(|slot| *slot.borrow_mut() = Some(replacement));
        apply_enabled(hwnd);
    }

    /// 从内嵌 logo 造两个 HICON：常态 + attention（右下角橙点）。
    /// 失败退化为空句柄——Shell_NotifyIconW 会用系统默认图标，托盘仍可用。
    fn build_icons() -> (
        windows_sys::Win32::UI::WindowsAndMessaging::HICON,
        windows_sys::Win32::UI::WindowsAndMessaging::HICON,
    ) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

        // SAFETY: GetSystemMetrics 无前置条件。
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        let Some(base) = crate::app_icon::rgba(crate::app_icon::selected(), size) else {
            log::warn!("tray: embedded logo failed to decode");
            return (std::ptr::null_mut(), std::ptr::null_mut());
        };
        let normal = create_icon(size, base.as_raw());

        let mut attention_pixels = base.clone();
        draw_attention_dot(&mut attention_pixels, size);
        let attention = create_icon(size, attention_pixels.as_raw());
        (normal, attention)
    }

    /// 右下角实心橙点（约 5/8 直径比）。挑警示橙而不是主题 accent：托盘
    /// 图标不知道主题，且橙点在亮暗任务栏上对比都够。
    fn draw_attention_dot(pixels: &mut image::RgbaImage, size: u32) {
        let radius = (size as f32) * 0.30;
        let center = (size as f32) - radius - 0.5;
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 - center;
                let dy = y as f32 - center;
                let distance = (dx * dx + dy * dy).sqrt();
                // 1px 羽化边，免得小尺寸下是一颗锯齿方块。
                let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let pixel = pixels.get_pixel_mut(x, y);
                    let blend = |src: u8, dst: u8| -> u8 {
                        (src as f32 * coverage + dst as f32 * (1.0 - coverage)) as u8
                    };
                    *pixel = image::Rgba([
                        blend(255, pixel[0]),
                        blend(140, pixel[1]),
                        blend(0, pixel[2]),
                        blend(255, pixel[3]),
                    ]);
                }
            }
        }
    }

    /// RGBA（直 alpha）→ HICON：32bpp 预乘 BGRA DIB + 单色掩码。
    fn create_icon(size: u32, rgba: &[u8]) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
        use windows_sys::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
            DeleteObject,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};

        // SAFETY: header 描述的缓冲由 CreateDIBSection 分配并在拷贝期间有效；
        // 两个位图在 CreateIconIndirect 后立即释放（图标持有自己的副本）。
        unsafe {
            let mut header: BITMAPINFOHEADER = std::mem::zeroed();
            header.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            header.biWidth = size as i32;
            // 负高 = 自顶向下，行序与解码像素一致，免得手工翻转。
            header.biHeight = -(size as i32);
            header.biPlanes = 1;
            header.biBitCount = 32;
            header.biCompression = BI_RGB;
            let info = BITMAPINFO { bmiHeader: header, bmiColors: std::mem::zeroed() };

            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let color = CreateDIBSection(
                std::ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if color.is_null() || bits.is_null() {
                return std::ptr::null_mut();
            }
            let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len());
            for (dst, src) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
                let alpha = src[3] as u32;
                // 预乘：DWM 按 premultiplied 合成托盘图标，直 alpha 会在
                // 深色任务栏上泛白边。
                dst[0] = (src[2] as u32 * alpha / 255) as u8;
                dst[1] = (src[1] as u32 * alpha / 255) as u8;
                dst[2] = (src[0] as u32 * alpha / 255) as u8;
                dst[3] = src[3];
            }

            let mask = CreateBitmap(size as i32, size as i32, 1, 1, std::ptr::null());
            let icon_info =
                ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
            let icon = CreateIconIndirect(&icon_info);
            DeleteObject(color);
            DeleteObject(mask);
            icon
        }
    }

    // ─── 小工具 ─────────────────────────────────────────────────────────

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// UTF-16 截断拷贝进 szTip（128 wchar，含 NUL）。
    fn copy_tip(tip: &mut [u16; 128], text: &str) {
        let mut cursor = 0;
        for unit in text.encode_utf16() {
            if cursor >= tip.len() - 1 {
                break;
            }
            tip[cursor] = unit;
            cursor += 1;
        }
        tip[cursor] = 0;
    }
}
