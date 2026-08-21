//! 搜狗输入法空格失效取证探针（issue: 按 space 选词变成往 PTY 写空格）。
//!
//! 用途：把 GPUI 0.2.2 的 Windows 键盘/IME 处理**逐条复刻**到一个零依赖窗口
//! 里，再和标准 Win32 应用行为对照，看同一串按键在两种壳下分别走到哪儿。
//!
//! 复刻的是 `gpui/src/platform/windows/events.rs` 三处：
//!   * 消息循环**不调** `TranslateMessage`（gpui 只在 wndproc 内部用一条伪造
//!     的 `MSG` 调它，见 `translate_message()`）；
//!   * `WM_IME_STARTCOMPOSITION` → `handle_ime_position` 一律 `Some(0)`：吞掉，
//!     不给 `DefWindowProcW`（= 向 IME 宣称"组字串由我内嵌绘制"）；
//!   * `WM_KEYDOWN` → 是否交还 IME 只看应用侧 `marked_text_range()`，而
//!     `marked_text` 只在 `WM_IME_COMPOSITION` 带 `GCS_COMPSTR` 时才有值。
//!
//! 编译（不进 workspace target，避免和正在跑的 cargo 抢锁）：
//!   rustc --edition 2021 -O tools/probe_ime.rs -o .probe/probe_ime.exe
//!
//! 用法：运行后窗口里输入拼音再按空格。F9 在三种模式间切换：
//!   [0] gpui 复刻      —— 期望复现 bug
//!   [1] 候选补丁        —— wParam==VK_PROCESSKEY 一律交还 IME
//!   [2] 标准 Win32 应用 —— 循环里 TranslateMessage + 全部 DefWindowProcW
//! 日志同时打到控制台和 .probe/probe_ime.log。

use std::ffi::c_void;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

type Hwnd = *mut c_void;
type Himc = *mut c_void;
type Hinstance = *mut c_void;
type Wparam = usize;
type Lparam = isize;
type Lresult = isize;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
    time: u32,
    pt: Point,
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Hinstance,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: Hinstance,
        param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam) -> Lresult;
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> Lresult;
    fn PostQuitMessage(code: i32);
    fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
    fn LoadCursorW(instance: Hinstance, name: *const u16) -> *mut c_void;
    fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> i32;
    fn GetKeyboardLayout(thread_id: u32) -> *mut c_void;
    fn SetForegroundWindow(hwnd: Hwnd) -> i32;
    fn PostMessageW(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam) -> i32;
    fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
}

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: Hwnd) -> Himc;
    fn ImmReleaseContext(hwnd: Hwnd, himc: Himc) -> i32;
    fn ImmGetCompositionStringW(himc: Himc, index: u32, buf: *mut c_void, len: u32) -> i32;
    fn ImmGetVirtualKey(hwnd: Hwnd) -> u32;
    fn ImmGetOpenStatus(himc: Himc) -> i32;
    fn ImmSetOpenStatus(himc: Himc, open: i32) -> i32;
}

const WM_DESTROY: u32 = 0x0002;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
const WM_IME_COMPOSITION: u32 = 0x010F;
const WM_IME_SETCONTEXT: u32 = 0x0281;
const WM_IME_NOTIFY: u32 = 0x0282;
const WM_IME_CHAR: u32 = 0x0286;
const WM_IME_REQUEST: u32 = 0x0288;
const WM_IME_KEYDOWN: u32 = 0x0290;
const WM_IME_KEYUP: u32 = 0x0291;

const GCS_COMPSTR: u32 = 0x0008;
const GCS_CURSORPOS: u32 = 0x0080;
const GCS_RESULTSTR: u32 = 0x0800;

const VK_PROCESSKEY: usize = 0xE5;
const VK_F9: usize = 0x78;
const VK_SPACE: usize = 0x20;

const WM_CLOSE: u32 = 0x0010;
const KEYEVENTF_KEYUP: u32 = 0x0002;

/// `--auto` 自测：程序化打开 IME 中文模式，再注入 `n` `i` `空格`，把三种模式
/// 各跑一遍。用来在没有搜狗的机器上验证两件事：gpui 复刻是否忠实（微软拼音
/// 应当一路正常），以及组字中按空格时 `wParam` 是否真的被系统换成
/// `VK_PROCESSKEY`——后者是 `handle_keydown_msg` 那个补丁成立的前提。
fn auto_test(hwnd_bits: usize) {
    let hwnd = hwnd_bits as Hwnd;
    let tap = |vk: u8| unsafe {
        keybd_event(vk, 0, 0, 0);
        std::thread::sleep(std::time::Duration::from_millis(60));
        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
        std::thread::sleep(std::time::Duration::from_millis(340));
    };

    std::thread::sleep(std::time::Duration::from_millis(700));
    for round in 0..5 {
        unsafe {
            SetForegroundWindow(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(300));
            // 每轮都重新打开中文模式：上一轮的按键可能把 IME 切回英文。
            let himc = ImmGetContext(hwnd);
            let opened = ImmSetOpenStatus(himc, 1);
            let now = ImmGetOpenStatus(himc);
            ImmReleaseContext(hwnd, himc);
            log(&format!(
                "\n---- 自测第 {round} 轮：模式 [{}] {} ；ImmSetOpenStatus={opened} 当前 open={now} ----",
                MODE.load(Ordering::Relaxed),
                mode_name(MODE.load(Ordering::Relaxed))
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        log("[注入] n");
        tap(b'N');
        log("[注入] i");
        tap(b'I');
        log("[注入] 空格（这一下如果被 IME 吃掉就是对的；出现 WM_CHAR 0x20 就是 bug）");
        tap(VK_SPACE as u8);
        // 收尾：Esc 清掉可能残留的组字，再 F9 切下一模式。
        tap(0x1B);
        if round < 4 {
            log("[注入] F9 切模式");
            tap(VK_F9 as u8);
        }
    }
    log("\n---- 自测结束 ----");
    unsafe {
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
}

/// 0 = gpui 复刻，1 = 候选补丁，2 = 标准 Win32。
static MODE: AtomicUsize = AtomicUsize::new(0);
static MARKED: Mutex<String> = Mutex::new(String::new());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn log(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(".probe/probe_ime.log")
    {
        let _ = writeln!(f, "{line}");
    }
}

fn mode_name(mode: usize) -> &'static str {
    match mode {
        0 => "gpui 复刻",
        1 => "候选补丁(VK_PROCESSKEY 直通)",
        2 => "标准 Win32",
        3 => "gpui 复刻 + 搜狗模拟(不回填 compstr)",
        _ => "候选补丁 + 搜狗模拟",
    }
}

fn vk_name(vk: usize) -> String {
    match vk {
        VK_SPACE => "VK_SPACE".to_owned(),
        VK_PROCESSKEY => "VK_PROCESSKEY".to_owned(),
        0x0D => "VK_RETURN".to_owned(),
        0x08 => "VK_BACK".to_owned(),
        0x1B => "VK_ESCAPE".to_owned(),
        0x30..=0x39 => format!("'{}'", (vk as u8) as char),
        0x41..=0x5A => format!("'{}'", (vk as u8) as char),
        _ => format!("0x{vk:02X}"),
    }
}

/// gpui 的 `translate_message`：伪造一条 `MSG` 在 wndproc 内部调
/// `TranslateMessage`，而不是在消息循环里按常规顺序调用。
unsafe fn translate_fake(hwnd: Hwnd, w: Wparam, l: Lparam) {
    let msg = Msg {
        hwnd,
        message: WM_KEYDOWN,
        w_param: w,
        l_param: l,
        time: 0,
        pt: Point::default(),
    };
    let r = TranslateMessage(&msg);
    log(&format!("        ↳ TranslateMessage(伪造 MSG) → {r}"));
}

/// 读 IMM 组字串（GCS_COMPSTR / GCS_RESULTSTR）。
unsafe fn comp_string(himc: Himc, index: u32) -> String {
    let bytes = ImmGetCompositionStringW(himc, index, std::ptr::null_mut(), 0);
    if bytes <= 0 {
        return String::new();
    }
    let units = bytes as usize / 2;
    let mut buf = vec![0u16; units];
    ImmGetCompositionStringW(himc, index, buf.as_mut_ptr() as *mut c_void, bytes as u32);
    String::from_utf16_lossy(&buf)
}

unsafe extern "system" fn wnd_proc(hwnd: Hwnd, msg: u32, w: Wparam, l: Lparam) -> Lresult {
    let mode = MODE.load(Ordering::Relaxed);

    match msg {
        WM_KEYDOWN | WM_KEYUP => {
            let tag = if msg == WM_KEYDOWN { "WM_KEYDOWN" } else { "WM_KEYUP  " };
            // gpui `handle_key_event`：wParam 是 VK_PROCESSKEY 时用
            // ImmGetVirtualKey 把真实键挖出来，然后当普通键继续走。
            let unpacked = if w == VK_PROCESSKEY {
                let real = ImmGetVirtualKey(hwnd) as usize;
                format!("  → ImmGetVirtualKey={}", vk_name(real))
            } else {
                String::new()
            };
            let marked = MARKED.lock().unwrap().clone();
            log(&format!(
                "{tag} wParam={}{unpacked}   marked_text={:?}",
                vk_name(w),
                marked
            ));

            if msg == WM_KEYDOWN && w == VK_F9 {
                let next = (mode + 1) % 5;
                MODE.store(next, Ordering::Relaxed);
                MARKED.lock().unwrap().clear();
                log(&format!("\n======== 模式切换 → [{next}] {} ========\n", mode_name(next)));
                let title =
                    wide(&format!("IME 探针 —— 模式 [{next}] {}（F9 切换）", mode_name(next)));
                SetWindowTextW(hwnd, title.as_ptr());
                return 0;
            }

            if mode == 2 {
                // 标准应用：键盘消息一律交给 DefWindowProcW，IME 由消息循环里
                // 的 TranslateMessage 驱动。
                return DefWindowProcW(hwnd, msg, w, l);
            }

            if msg == WM_KEYUP {
                return 1;
            }

            // 候选补丁：IMM 已经宣布这个键归 IME，应用不看也不派发。
            if (mode == 1 || mode == 4) && w == VK_PROCESSKEY {
                log("        ↳ [补丁] wParam==VK_PROCESSKEY：不派发给应用，交还 IME");
                translate_fake(hwnd, w, l);
                return 0;
            }

            // gpui：是否交还 IME 只看应用侧 marked_text。
            if !marked.is_empty() {
                log("        ↳ is_composing=true（marked_text 非空）：不派发，交还 IME");
                translate_fake(hwnd, w, l);
                return 0;
            }
            log("        ↳ is_composing=false：派发给应用 → 应用不消费空格 → 再 TranslateMessage");
            translate_fake(hwnd, w, l);
            1
        },
        WM_CHAR => {
            let ch = char::from_u32(w as u32).unwrap_or('?');
            log(&format!(
                "WM_CHAR   ch=U+{:04X} {:?}   *** 这一步就是「空格被写进 PTY」***",
                w as u32, ch
            ));
            if mode == 2 { DefWindowProcW(hwnd, msg, w, l) } else { 0 }
        },
        WM_IME_STARTCOMPOSITION => {
            log("WM_IME_STARTCOMPOSITION");
            if mode == 2 {
                return DefWindowProcW(hwnd, msg, w, l);
            }
            // gpui `handle_ime_position` 无条件 Some(0)：吞掉 = 宣称内嵌绘制。
            log("        ↳ [gpui] 返回 0 吞掉（不给 DefWindowProcW）= 宣称组字串我自己画");
            0
        },
        WM_IME_COMPOSITION => {
            let himc = ImmGetContext(hwnd);
            let flags = l as u32;
            let comp = if flags & GCS_COMPSTR > 0 { comp_string(himc, GCS_COMPSTR) } else { String::new() };
            let result =
                if flags & GCS_RESULTSTR > 0 { comp_string(himc, GCS_RESULTSTR) } else { String::new() };
            let open = ImmGetOpenStatus(himc);
            ImmReleaseContext(hwnd, himc);
            let mut names = Vec::new();
            if flags & GCS_COMPSTR > 0 {
                names.push("GCS_COMPSTR");
            }
            if flags & GCS_RESULTSTR > 0 {
                names.push("GCS_RESULTSTR");
            }
            if flags & GCS_CURSORPOS > 0 {
                names.push("GCS_CURSORPOS");
            }
            log(&format!(
                "WM_IME_COMPOSITION lParam=0x{flags:04X} [{}] compstr={comp:?} resultstr={result:?} open={open}",
                names.join("|")
            ));
            if mode == 2 {
                return DefWindowProcW(hwnd, msg, w, l);
            }
            if flags & GCS_COMPSTR > 0 {
                if mode >= 3 {
                    log("        ↳ [搜狗模拟] 丢弃 compstr：marked_text 保持 None（搜狗从不回填）");
                } else {
                    *MARKED.lock().unwrap() = comp;
                    log("        ↳ [gpui] replace_and_mark_text_in_range → marked_text 有值了");
                }
            }
            if flags & GCS_RESULTSTR > 0 {
                MARKED.lock().unwrap().clear();
                log("        ↳ [gpui] replace_text_in_range → 提交进 PTY，marked_text 清空");
                return 0;
            }
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_ENDCOMPOSITION => {
            log("WM_IME_ENDCOMPOSITION");
            MARKED.lock().unwrap().clear();
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_CHAR => {
            log(&format!("WM_IME_CHAR wParam=U+{:04X}", w as u32));
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_KEYDOWN | WM_IME_KEYUP => {
            log(&format!("WM_IME_KEY{} wParam={}", if msg == WM_IME_KEYDOWN { "DOWN" } else { "UP" }, vk_name(w)));
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_SETCONTEXT => {
            log(&format!("WM_IME_SETCONTEXT active={w} lParam=0x{:X}", l));
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_NOTIFY => {
            log(&format!("WM_IME_NOTIFY wParam=0x{w:X} lParam=0x{:X}", l));
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_IME_REQUEST => {
            log(&format!("WM_IME_REQUEST wParam=0x{w:X}"));
            DefWindowProcW(hwnd, msg, w, l)
        },
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        },
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn main() {
    let _ = std::fs::create_dir_all(".probe");
    let _ = std::fs::remove_file(".probe/probe_ime.log");

    unsafe {
        let class_name = wide("NebulaImeProbe");
        let class = WndClassW {
            style: 0,
            wnd_proc: Some(wnd_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance: std::ptr::null_mut(),
            icon: std::ptr::null_mut(),
            cursor: LoadCursorW(std::ptr::null_mut(), 32512 as *const u16),
            // COLOR_WINDOW+1
            background: 6 as *mut c_void,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        if RegisterClassW(&class) == 0 {
            log("RegisterClassW 失败");
            return;
        }
        let title = wide("IME 探针 —— 模式 [0] gpui 复刻（F9 切换）");
        // WS_OVERLAPPEDWINDOW | WS_VISIBLE
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0x00CF_0000 | 0x1000_0000,
            200,
            200,
            760,
            300,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            log("CreateWindowExW 失败");
            return;
        }
        ShowWindow(hwnd, 5);

        log("=========================================================");
        log(" 搜狗输入法空格取证探针");
        log(" 窗口已弹出（不画字，只记消息）。请在窗口里操作：");
        log("   1) 切到搜狗输入法");
        log("   2) 打拼音 ni，再按一次空格");
        log("   3) 按 F9 切到 [1] 候选补丁，重复第 2 步");
        log("   4) 再按 F9 切到 [2] 标准 Win32，重复第 2 步");
        log(&format!(" 当前键盘布局 HKL=0x{:X}", GetKeyboardLayout(0) as usize));
        log(&format!(" 模式 [0] {}", mode_name(0)));
        log("=========================================================");

        if std::env::args().any(|a| a == "--auto") {
            let bits = hwnd as usize;
            std::thread::spawn(move || auto_test(bits));
        }

        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            // 标准应用在这里 TranslateMessage；gpui 的循环没有这一步，
            // 它只在 wndproc 内部用伪造 MSG 调用（见 translate_fake）。
            if MODE.load(Ordering::Relaxed) == 2 {
                TranslateMessage(&msg);
            }
            DispatchMessageW(&msg);
        }
    }
}
