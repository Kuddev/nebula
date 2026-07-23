use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::{
    FILEOPENDIALOGOPTIONS, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, FileOpenDialog, SIGDN, SIGDN_FILESYSPATH,
};
use windows_sys::core::{GUID, HRESULT, PCWSTR, PWSTR};
use winit::raw_window_handle::RawWindowHandle;

const FILE_OPEN_DIALOG_IID: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
const HRESULT_CANCELLED: HRESULT = 0x800704c7_u32 as HRESULT;

#[repr(C)]
struct Interface<T> {
    vtable: *const T,
}

#[repr(C)]
struct IUnknownVTable {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
struct IModalWindowVTable {
    base: IUnknownVTable,
    show: unsafe extern "system" fn(this: *mut c_void, owner: HWND) -> HRESULT,
}

// windows-sys 只提供原始 COM 函数，因此这里保留 IFileDialog 的精确 vtable 顺序。
#[repr(C)]
struct IFileDialogVTable {
    base: IModalWindowVTable,
    _set_file_types: usize,
    _set_file_type_index: usize,
    _get_file_type_index: usize,
    _advise: usize,
    _unadvise: usize,
    set_options:
        unsafe extern "system" fn(this: *mut c_void, options: FILEOPENDIALOGOPTIONS) -> HRESULT,
    get_options: unsafe extern "system" fn(
        this: *mut c_void,
        options: *mut FILEOPENDIALOGOPTIONS,
    ) -> HRESULT,
    _set_default_folder: usize,
    _set_folder: usize,
    _get_folder: usize,
    _get_current_selection: usize,
    _set_file_name: usize,
    _get_file_name: usize,
    set_title: unsafe extern "system" fn(this: *mut c_void, title: PCWSTR) -> HRESULT,
    _set_ok_button_label: usize,
    _set_file_name_label: usize,
    get_result: unsafe extern "system" fn(this: *mut c_void, item: *mut *mut c_void) -> HRESULT,
    _add_place: usize,
    _set_default_extension: usize,
    _close: usize,
    _set_client_guid: usize,
    _clear_client_data: usize,
    _set_filter: usize,
}

#[repr(C)]
struct IFileOpenDialogVTable {
    base: IFileDialogVTable,
    _get_results: usize,
    _get_selected_items: usize,
}

#[repr(C)]
struct IShellItemVTable {
    base: IUnknownVTable,
    _bind_to_handler: usize,
    _get_parent: usize,
    get_display_name:
        unsafe extern "system" fn(this: *mut c_void, name_kind: SIGDN, name: *mut PWSTR) -> HRESULT,
    _get_attributes: usize,
    _compare: usize,
}

struct ComApartment {
    should_uninitialize: bool,
}

impl ComApartment {
    fn initialize() -> Option<Self> {
        let result = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        if result >= 0 {
            return Some(Self { should_uninitialize: true });
        }

        // 线程已有不同 apartment 时不能配对反初始化，但仍可沿用宿主的 COM 环境尝试打开。
        if result == RPC_E_CHANGED_MODE {
            return Some(Self { should_uninitialize: false });
        }

        warn_hresult("初始化目录选择器 COM 环境", result);
        None
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

struct ComPtr(*mut c_void);

impl ComPtr {
    fn from_raw(pointer: *mut c_void) -> Option<Self> {
        (!pointer.is_null()).then_some(Self(pointer))
    }

    unsafe fn vtable<T>(&self) -> &T {
        let interface = unsafe { &*self.0.cast::<Interface<T>>() };
        unsafe { &*interface.vtable }
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        unsafe {
            let vtable = self.vtable::<IUnknownVTable>();
            (vtable.release)(self.0);
        }
    }
}

struct TaskMemWide(PWSTR);

impl Drop for TaskMemWide {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

pub(super) fn pick(owner: RawWindowHandle, title: &str) -> Option<PathBuf> {
    let _apartment = ComApartment::initialize()?;
    let owner = match owner {
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    };

    let mut dialog_pointer = std::ptr::null_mut();
    let result = unsafe {
        CoCreateInstance(
            &FileOpenDialog,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &FILE_OPEN_DIALOG_IID,
            &mut dialog_pointer,
        )
    };
    if !succeeded("创建目录选择器", result) {
        return None;
    }
    let dialog = ComPtr::from_raw(dialog_pointer)?;
    let dialog_vtable = unsafe { dialog.vtable::<IFileOpenDialogVTable>() };

    let mut options = 0;
    let result = unsafe { (dialog_vtable.base.get_options)(dialog.0, &mut options) };
    if !succeeded("读取目录选择器选项", result) {
        return None;
    }
    let options =
        options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR;
    let result = unsafe { (dialog_vtable.base.set_options)(dialog.0, options) };
    if !succeeded("设置目录选择器选项", result) {
        return None;
    }

    let title = title.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let result = unsafe { (dialog_vtable.base.set_title)(dialog.0, title.as_ptr()) };
    if !succeeded("设置目录选择器标题", result) {
        return None;
    }

    let result = unsafe { (dialog_vtable.base.base.show)(dialog.0, owner) };
    if result == HRESULT_CANCELLED {
        return None;
    }
    if !succeeded("显示目录选择器", result) {
        return None;
    }

    let mut item_pointer = std::ptr::null_mut();
    let result = unsafe { (dialog_vtable.base.get_result)(dialog.0, &mut item_pointer) };
    if !succeeded("获取所选目录", result) {
        return None;
    }
    let item = ComPtr::from_raw(item_pointer)?;
    let item_vtable = unsafe { item.vtable::<IShellItemVTable>() };

    let mut path_pointer = std::ptr::null_mut();
    let result =
        unsafe { (item_vtable.get_display_name)(item.0, SIGDN_FILESYSPATH, &mut path_pointer) };
    if !succeeded("读取所选目录路径", result) || path_pointer.is_null() {
        return None;
    }

    // Shell 返回动态分配的 UTF-16 字符串，可完整保留长路径和 WSL UNC 路径。
    let path_pointer = TaskMemWide(path_pointer);
    unsafe { path_from_nul_terminated_wide(path_pointer.0) }
}

fn succeeded(operation: &str, result: HRESULT) -> bool {
    if result >= 0 {
        true
    } else {
        warn_hresult(operation, result);
        false
    }
}

fn warn_hresult(operation: &str, result: HRESULT) {
    log::warn!("{operation}失败，HRESULT=0x{:08X}", result as u32);
}

unsafe fn path_from_nul_terminated_wide(pointer: *const u16) -> Option<PathBuf> {
    if pointer.is_null() {
        return None;
    }

    let mut length = 0;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let units = unsafe { std::slice::from_raw_parts(pointer, length) };
    path_from_wide_units(units)
}

fn path_from_wide_units(units: &[u16]) -> Option<PathBuf> {
    (!units.is_empty()).then(|| PathBuf::from(OsString::from_wide(units)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::path_from_wide_units;

    #[test]
    fn keeps_wsl_localhost_unc_path() {
        let path = r"\\wsl.localhost\Ubuntu\home\用户\项目";
        let wide = path.encode_utf16().collect::<Vec<_>>();
        assert_eq!(path_from_wide_units(&wide), Some(PathBuf::from(path)));
    }

    #[test]
    fn keeps_legacy_wsl_unc_path() {
        let path = r"\\wsl$\Ubuntu\home\user\project";
        let wide = path.encode_utf16().collect::<Vec<_>>();
        assert_eq!(path_from_wide_units(&wide), Some(PathBuf::from(path)));
    }

    #[test]
    fn keeps_local_unicode_path() {
        let path = r"D:\普通目录\项目";
        let wide = path.encode_utf16().collect::<Vec<_>>();
        assert_eq!(path_from_wide_units(&wide), Some(PathBuf::from(path)));
    }
}
