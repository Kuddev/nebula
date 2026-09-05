use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use nebula_settings::AppIconName;

pub const FRAME_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

fn selection() -> &'static AtomicU8 {
    static SELECTION: OnceLock<AtomicU8> = OnceLock::new();
    SELECTION.get_or_init(|| AtomicU8::new(nebula_settings::RuntimeSettings::load().app_icon as u8))
}

pub fn selected() -> AppIconName {
    AppIconName::ALL[selection().load(Ordering::Relaxed) as usize]
}

pub fn set_selected(variant: AppIconName) -> bool {
    selection().swap(variant as u8, Ordering::Relaxed) != variant as u8
}

pub fn frame_size(requested: u32) -> u32 {
    FRAME_SIZES.into_iter().find(|size| *size >= requested).unwrap_or(256)
}

#[cfg(windows)]
pub fn png(variant: AppIconName, requested: u32) -> Option<&'static [u8]> {
    windows::png(variant, frame_size(requested))
}

#[cfg(not(windows))]
pub fn png(variant: AppIconName, requested: u32) -> Option<&'static [u8]> {
    let bytes: &[u8] = match variant {
        AppIconName::SilverViolet => include_bytes!("../windows/nebula.ico"),
        AppIconName::GraphiteViolet => include_bytes!("../windows/nebula-dark.ico"),
        AppIconName::Titanium => include_bytes!("../windows/nebula-titanium.ico"),
    };
    ico_png(bytes, requested)
}

#[cfg(any(not(windows), test))]
fn ico_png(bytes: &[u8], requested: u32) -> Option<&[u8]> {
    if bytes.get(..4)? != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
    let size = frame_size(requested);
    for index in 0..count {
        let entry = bytes.get(6 + index * 16..22 + index * 16)?;
        let width = if entry[0] == 0 { 256 } else { u32::from(entry[0]) };
        if width != size || entry[0] != entry[1] {
            continue;
        }
        let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
        let frame = bytes.get(offset..offset.checked_add(length)?)?;
        return frame.starts_with(b"\x89PNG\r\n\x1a\n").then_some(frame);
    }
    None
}

#[cfg(feature = "gpui-shell")]
pub fn preview(variant: AppIconName) -> Option<std::sync::Arc<gpui::Image>> {
    static IMAGES: OnceLock<[Option<std::sync::Arc<gpui::Image>>; 3]> = OnceLock::new();
    IMAGES.get_or_init(|| {
        AppIconName::ALL.map(|variant| {
            png(variant, 256).map(|bytes| {
                std::sync::Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()))
            })
        })
    })[variant as usize]
        .clone()
}

#[cfg(windows)]
pub mod windows {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use nebula_settings::AppIconName;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LoadImageW,
        LookupIconIdFromDirectoryEx, RT_GROUP_ICON, RT_ICON, SM_CXICON, SM_CXSMICON, SendMessageW,
        WM_SETICON,
    };

    struct NativeIcon(HICON);

    impl Drop for NativeIcon {
        fn drop(&mut self) {
            unsafe { DestroyIcon(self.0) };
        }
    }

    thread_local! {
        static ICONS: RefCell<HashMap<(AppIconName, u32), NativeIcon>> = RefCell::new(HashMap::new());
    }

    fn resource_id(variant: AppIconName) -> u16 {
        0x101 + variant as u16
    }

    fn resource_bytes(identifier: u16, kind: *const u16) -> Option<&'static [u8]> {
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            if module.is_null() {
                return None;
            }
            let resource = FindResourceW(module, identifier as usize as *const u16, kind);
            if resource.is_null() {
                return None;
            }
            let length = SizeofResource(module, resource) as usize;
            let loaded = LoadResource(module, resource);
            if loaded.is_null() || length == 0 {
                return None;
            }
            let pointer = LockResource(loaded) as *const u8;
            if pointer.is_null() {
                return None;
            }
            Some(std::slice::from_raw_parts(pointer, length))
        }
    }

    pub(super) fn png(variant: AppIconName, size: u32) -> Option<&'static [u8]> {
        let group = resource_bytes(resource_id(variant), RT_GROUP_ICON)?;
        let identifier =
            unsafe { LookupIconIdFromDirectoryEx(group.as_ptr(), 1, size as i32, size as i32, 0) };
        if identifier <= 0 {
            return None;
        }
        let bytes = resource_bytes(identifier as u16, RT_ICON)?;
        bytes.starts_with(b"\x89PNG\r\n\x1a\n").then_some(bytes)
    }

    pub fn set_window(hwnd: HWND, variant: AppIconName) {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        ICONS.with(|cache| {
            let mut cache = cache.borrow_mut();
            for (slot, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
                let requested = unsafe { GetSystemMetricsForDpi(metric, dpi) }.max(16) as u32;
                let size = super::frame_size(requested);
                let icon = cache.entry((variant, size)).or_insert_with(|| {
                    NativeIcon(unsafe {
                        LoadImageW(
                            GetModuleHandleW(std::ptr::null()),
                            resource_id(variant) as usize as *const u16,
                            IMAGE_ICON,
                            size as i32,
                            size as i32,
                            0,
                        )
                    })
                });
                if !icon.0.is_null() {
                    unsafe { SendMessageW(hwnd, WM_SETICON, slot as usize, icon.0 as isize) };
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_selection_never_undersamples_within_budget() {
        for size in FRAME_SIZES {
            assert_eq!(frame_size(size), size);
        }
        assert_eq!(frame_size(17), 20);
        assert_eq!(frame_size(25), 32);
        assert_eq!(frame_size(144), 256);
        assert_eq!(frame_size(512), 256);
    }

    #[test]
    fn all_presets_have_exact_png_frames_and_fit_budget() {
        let assets: [&[u8]; 3] = [
            include_bytes!("../windows/nebula.ico"),
            include_bytes!("../windows/nebula-dark.ico"),
            include_bytes!("../windows/nebula-titanium.ico"),
        ];
        assert!(assets.iter().map(|bytes| bytes.len()).sum::<usize>() <= 64 * 1024);
        for bytes in assets {
            for size in FRAME_SIZES {
                let frame = ico_png(bytes, size).expect("exact PNG frame");
                let decoded = image::load_from_memory(frame).expect("valid PNG");
                assert_eq!((decoded.width(), decoded.height()), (size, size));
                assert_eq!(decoded.to_rgba8().get_pixel(0, 0)[3], 0);
            }
        }
    }

    #[test]
    fn corrupt_ico_is_rejected() {
        for length in 0..22 {
            assert!(ico_png(&vec![0; length], 16).is_none());
        }
        let mut bytes = include_bytes!("../windows/nebula.ico").to_vec();
        bytes[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(ico_png(&bytes, 16).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn executable_resources_match_presets_without_duplicate_pngs() {
        let assets: [&[u8]; 3] = [
            include_bytes!("../windows/nebula.ico"),
            include_bytes!("../windows/nebula-dark.ico"),
            include_bytes!("../windows/nebula-titanium.ico"),
        ];
        for (variant, bytes) in AppIconName::ALL.into_iter().zip(assets) {
            for size in FRAME_SIZES {
                assert_eq!(png(variant, size), ico_png(bytes, size));
                assert!(png(variant, size).is_some());
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_switch_reuses_handles_and_preserves_two_icon_sizes() {
        use windows_sys::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetIconInfo, ICON_BIG, ICON_SMALL, ICONINFO,
            SendMessageW, WM_GETICON,
        };
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let mut cached = Vec::new();
        for iteration in 0..10 {
            for (variant_index, variant) in AppIconName::ALL.into_iter().enumerate() {
                windows::set_window(hwnd, variant);
                let handles = [ICON_SMALL, ICON_BIG]
                    .map(|slot| unsafe { SendMessageW(hwnd, WM_GETICON, slot as usize, 0) });
                assert!(handles.iter().all(|handle| *handle != 0));
                assert_ne!(handles[0], handles[1]);
                if iteration == 0 {
                    cached.push(handles);
                    let sizes = handles.map(|handle| unsafe {
                        let mut info: ICONINFO = std::mem::zeroed();
                        assert_ne!(GetIconInfo(handle as _, &mut info), 0);
                        let mut bitmap: BITMAP = std::mem::zeroed();
                        assert_ne!(
                            GetObjectW(
                                info.hbmColor,
                                std::mem::size_of::<BITMAP>() as i32,
                                &mut bitmap as *mut BITMAP as _
                            ),
                            0
                        );
                        DeleteObject(info.hbmColor);
                        DeleteObject(info.hbmMask);
                        assert_eq!(bitmap.bmWidth, bitmap.bmHeight);
                        bitmap.bmWidth
                    });
                    assert!(sizes[0] < sizes[1]);
                } else {
                    assert_eq!(handles, cached[variant_index]);
                }
            }
        }
        assert_ne!(cached[0], cached[1]);
        assert_ne!(cached[1], cached[2]);
        unsafe { DestroyWindow(hwnd) };
    }
}
