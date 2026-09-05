//! 平台闸门层。
//!
//! 目的是让业务代码不再自己写 `#[cfg(windows)]`。三平台的差异全部收在
//! 这里的子模块中，对外只暴露平台无关的签名。
//!
//! 设计裁定：**不用 trait 对象**。把平台差异塞进一个二十来个方法的
//! `dyn` trait 适合运行期替换实现，但在这里使用只会换来强制动态分发
//! 和造假实现的测试负担——平台实现本就是编译期确定的。这里改用「按能力
//! 切模块 + 模块内 `#[cfg]` 分派」，只保留真正值钱的两点：单一入口，以及
//! 能力探测（让 UI 隐藏入口，而不是让功能在别的平台报错）。

pub mod capabilities;
pub mod credentials;
pub mod dirs;
#[cfg(feature = "gpui-shell")]
pub mod fonts;
pub mod notifications;
pub mod shell;
pub mod shell_integration;
pub mod startup;

pub use capabilities::CAPABILITIES;

/// Play the system's default notification sound.
///
/// Used for the audible terminal bell (BEL / `\a`), which is the primary
/// "an AI CLI turn finished / needs you" cue when Nebula is focused on a
/// different tab. Throttled internally so a bell-happy program (a build
/// looping BEL, PSReadLine ringing on every ambiguous completion) cannot
/// machine-gun the sound; distinct events a fraction of a second apart still
/// each get one beep.
///
/// The actual playback is handed to a throwaway thread so a wedged audio
/// service can never stall the winit event loop — the same discipline the
/// toast path in [`crate::notify`] follows. No-op off Windows for now.
pub fn beep() {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Minimum spacing between beeps. Long enough to coalesce a tight BEL
    /// loop into a steady tick rather than a screech, short enough that two
    /// separate turns finishing back to back are still both heard.
    const COOLDOWN: Duration = Duration::from_millis(200);
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);

    {
        // A poisoned lock only means a prior beep thread panicked mid-check;
        // the state is a plain Option, safe to keep using.
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(at) if at.elapsed() < COOLDOWN => return,
            _ => *last = Some(Instant::now()),
        }
    }

    #[cfg(windows)]
    {
        // MessageBeep(MB_OK) plays the user's configured "Default Beep"
        // sound and returns before it finishes, but we still isolate it on a
        // named worker thread: best-effort by contract, a failure or stall
        // costs the sound, never the terminal.
        let _ = std::thread::Builder::new().name("nebula-beep".into()).spawn(|| {
            // SAFETY: MessageBeep takes a plain sound-type flag and has no
            // pointer arguments or shared state.
            unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::MessageBeep(
                    windows_sys::Win32::UI::WindowsAndMessaging::MB_OK,
                );
            }
        });
    }
}

/// 运行平台。与「配置平台」分离：前者是我真正跑在哪，后者是
/// 该套用哪份键位/修饰键默认值——Mac 用户在 Windows 上可以选 ⌘ 语义。
/// 配置平台待 L5 键位分文件时落地。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOS,
    Linux,
}

impl Platform {
    pub const fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOS
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Linux
        }
    }
}
