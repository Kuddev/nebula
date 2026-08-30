//! 程序上报的任务进度（OSC 9;4）→ Windows 任务栏。
//!
//! OSC 9;4 是 ConEmu 定的序列，Windows 任务栏原生就有这套语义
//! （`ITaskbarList3::SetProgressState` / `SetProgressValue`），所以这条链路在
//! Windows 上几乎是零成本：程序在任务开始、进度变化和结束时各发一次，任务栏
//! 就有了真实进度——支持该协议的构建工具跑着，切到别的窗口也看得见。
//!
//! 状态映射刻意宽容：解析层不收窄状态码（见 `osc_cwd::OscEvent::Progress`），
//! 这里把规范外的码一律当成「清除」。部分 shell 集成实测会发 `9;4;5;0` 表示
//! 成功完成，而 ConEmu 只定义了 0..=4——把未知码当非法丢掉，等于让进度条永远
//! 停在最后一个状态上。

/// 任务栏能画出来的进度形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskProgress {
    /// 没有进度，任务栏恢复常态。
    #[default]
    None,
    /// 不确定进度（来回扫的条）：知道在忙，不知道还剩多少。
    Indeterminate,
    /// 正常进度，0..=100。
    Value(u8),
    /// 出错（红条）。命令失败时 shell 集成会发这个。
    Error(Option<u8>),
    /// 暂停或需要注意（黄条）。
    Paused(Option<u8>),
}

impl TaskProgress {
    /// 按 ConEmu 的状态码映射。0 清除 / 1 正常 / 2 错误 / 3 不确定 / 4 暂停，
    /// 其余（包括部分 shell 集成用来表示成功的 5）都归到清除。
    pub fn from_osc(state: u8, value: Option<u8>) -> Self {
        match state {
            1 => Self::Value(value.unwrap_or(0).min(100)),
            2 => Self::Error(value.map(|percent| percent.min(100))),
            3 => Self::Indeterminate,
            4 => Self::Paused(value.map(|percent| percent.min(100))),
            _ => Self::None,
        }
    }

    /// 任务栏是否需要画东西。
    pub fn is_active(self) -> bool {
        self != Self::None
    }

    /// `TBPFLAG` 值。
    #[cfg(windows)]
    fn taskbar_flag(self) -> i32 {
        match self {
            Self::None => 0,          // TBPF_NOPROGRESS
            Self::Indeterminate => 1, // TBPF_INDETERMINATE
            Self::Value(_) => 2,      // TBPF_NORMAL
            Self::Error(_) => 4,      // TBPF_ERROR
            Self::Paused(_) => 8,     // TBPF_PAUSED
        }
    }

    /// 有确定百分比时返回它。错误和暂停态可以带值也可以不带——不带值时只染色，
    /// 不动条的长度（这正是 `SetProgressState` 单独存在的原因）。
    #[cfg(windows)]
    fn percent(self) -> Option<u8> {
        match self {
            Self::Value(percent) => Some(percent),
            Self::Error(percent) | Self::Paused(percent) => percent,
            Self::None | Self::Indeterminate => None,
        }
    }
}

/// 把进度画到这个窗口的任务栏按钮上。
///
/// 失败一律静默：任务栏是纯装饰，`ITaskbarList3` 在旧系统、精简版系统或者
/// explorer 刚崩过时都可能拿不到，那不该影响终端本身。
#[cfg(windows)]
pub fn apply(hwnd: isize, progress: TaskProgress) {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoUninitialize,
    };
    use windows_sys::core::{GUID, HRESULT};

    const CLSID_TASKBAR_LIST: GUID = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);
    const IID_TASKBAR_LIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

    #[repr(C)]
    struct Interface<T> {
        vtable: *const T,
    }
    #[repr(C)]
    struct IUnknownVTable {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    // ITaskbarList3 继承 ITaskbarList2 继承 ITaskbarList，vtable 按继承顺序
    // 平铺。只声明我们要用的三个，其余占位。
    #[repr(C)]
    struct TaskbarList3VTable {
        base: IUnknownVTable,
        hr_init: unsafe extern "system" fn(*mut c_void) -> HRESULT,
        _add_tab: usize,
        _delete_tab: usize,
        _activate_tab: usize,
        _set_active_alt: usize,
        _mark_fullscreen_window: usize,
        set_progress_value: unsafe extern "system" fn(*mut c_void, HWND, u64, u64) -> HRESULT,
        set_progress_state: unsafe extern "system" fn(*mut c_void, HWND, i32) -> HRESULT,
    }

    if hwnd == 0 {
        return;
    }
    let initialized = unsafe {
        CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32)
    };
    let should_uninitialize = initialized >= 0;
    if initialized < 0 && initialized != RPC_E_CHANGED_MODE {
        return;
    }
    let mut pointer: *mut c_void = std::ptr::null_mut();
    // SAFETY: 两个 GUID 是常量，`pointer` 是本帧的局部变量。
    let created = unsafe {
        CoCreateInstance(
            &CLSID_TASKBAR_LIST,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_TASKBAR_LIST3,
            &mut pointer,
        )
    };
    if created >= 0 && !pointer.is_null() {
        // SAFETY: CoCreateInstance 成功即 `pointer` 指向一个 ITaskbarList3；
        // 下面只按上面声明的 vtable 布局调用它自己的方法。
        unsafe {
            let interface = pointer as *mut Interface<TaskbarList3VTable>;
            let vtable = &*(*interface).vtable;
            // HrInit 必须先调；不调的话后续方法在部分系统上直接返回失败。
            if (vtable.hr_init)(pointer) >= 0 {
                let handle = hwnd as HWND;
                (vtable.set_progress_state)(pointer, handle, progress.taskbar_flag());
                if let Some(percent) = progress.percent() {
                    (vtable.set_progress_value)(pointer, handle, u64::from(percent), 100);
                }
            }
            (vtable.base.release)(pointer);
        }
    }
    if should_uninitialize {
        // SAFETY: 与上面成功的 CoInitializeEx 配对。
        unsafe { CoUninitialize() };
    }
}

#[cfg(not(windows))]
pub fn apply(_hwnd: isize, _progress: TaskProgress) {}

#[cfg(test)]
mod tests {
    use super::TaskProgress;

    /// 状态映射要宽容：规范外的码归到清除，而不是被当成非法丢掉——否则进度条
    /// 会永远停在最后一个状态上；部分 shell 集成实测就会发 `9;4;5;0`。
    #[test]
    fn unknown_states_clear_instead_of_sticking() {
        assert_eq!(TaskProgress::from_osc(0, None), TaskProgress::None);
        assert_eq!(TaskProgress::from_osc(5, Some(0)), TaskProgress::None);
        assert_eq!(TaskProgress::from_osc(200, None), TaskProgress::None);
        assert!(!TaskProgress::from_osc(5, Some(0)).is_active());
    }

    #[test]
    fn known_states_map_to_their_taskbar_shape() {
        assert_eq!(TaskProgress::from_osc(1, Some(42)), TaskProgress::Value(42));
        assert_eq!(TaskProgress::from_osc(3, None), TaskProgress::Indeterminate);
        assert_eq!(TaskProgress::from_osc(2, None), TaskProgress::Error(None));
        assert_eq!(TaskProgress::from_osc(4, Some(90)), TaskProgress::Paused(Some(90)));
        assert!(TaskProgress::from_osc(3, None).is_active());
    }

    /// 百分比越界要收进 0..=100：`SetProgressValue` 的分母写死 100，传进去的
    /// completed 比 total 大会画出一条撑满且不动的条。
    #[test]
    fn percentages_are_clamped() {
        assert_eq!(TaskProgress::from_osc(1, Some(200)), TaskProgress::Value(100));
        assert_eq!(TaskProgress::from_osc(4, Some(255)), TaskProgress::Paused(Some(100)));
    }
}
