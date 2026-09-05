use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use image::{ImageEncoder as _, RgbaImage};
use nebula_settings::AppIconName;

pub const FRAME_SIZES: [u32; 14] = [16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 256];
const ATLAS_WIDTH: u32 = 512;
const COVERAGE_PNG: &[u8] = include_bytes!("../../extra/logo/nebula-coverage.png");
const CACHE_LIMIT: usize = 128;

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
    FRAME_SIZES
        .into_iter()
        .chain([ATLAS_WIDTH])
        .find(|size| *size >= requested)
        .unwrap_or(ATLAS_WIDTH)
}

fn coverage(requested: u32) -> Option<RgbaImage> {
    static ATLAS: OnceLock<Option<RgbaImage>> = OnceLock::new();
    let atlas = ATLAS
        .get_or_init(|| {
            let image = image::load_from_memory(COVERAGE_PNG).ok()?.to_rgba8();
            let expected_height = FRAME_SIZES.iter().sum::<u32>() + ATLAS_WIDTH;
            (image.dimensions() == (ATLAS_WIDTH, expected_height)).then_some(image)
        })
        .as_ref()?;
    let size = requested.clamp(16, ATLAS_WIDTH);
    let stored_size = frame_size(size);
    let top: u32 = FRAME_SIZES.into_iter().take_while(|stored| *stored < stored_size).sum();
    let pixels = image::imageops::crop_imm(atlas, 0, top, stored_size, stored_size).to_image();
    Some(if stored_size == size {
        pixels
    } else {
        image::imageops::resize(&pixels, size, size, image::imageops::FilterType::Triangle)
    })
}

pub fn rgba(variant: AppIconName, requested: u32) -> Option<RgbaImage> {
    let mut pixels = coverage(requested)?;
    let palette = variant.palette();
    let colors = [palette.tile, palette.border, palette.mark];
    for pixel in pixels.pixels_mut() {
        let weights = [u32::from(pixel[0]), u32::from(pixel[1]), u32::from(pixel[2])];
        let total: u32 = weights.iter().sum();
        if total == 0 || pixel[3] == 0 {
            pixel.0 = [0; 4];
            continue;
        }
        for (channel, shift) in [16, 8, 0].into_iter().enumerate() {
            let mixed: u32 = colors
                .iter()
                .zip(weights)
                .map(|(color, weight)| ((color >> shift) & 255) * weight)
                .sum();
            pixel[channel] = ((mixed + total / 2) / total) as u8;
        }
    }
    Some(pixels)
}

pub fn png(variant: AppIconName, requested: u32) -> Option<Arc<[u8]>> {
    static PNGS: OnceLock<Mutex<HashMap<(AppIconName, u32), Arc<[u8]>>>> = OnceLock::new();
    let size = requested.clamp(16, ATLAS_WIDTH);
    let mut cache =
        PNGS.get_or_init(Default::default).lock().unwrap_or_else(|error| error.into_inner());
    if let Some(bytes) = cache.get(&(variant, size)) {
        return Some(bytes.clone());
    }
    let pixels = rgba(variant, size)?;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(pixels.as_raw(), size, size, image::ExtendedColorType::Rgba8)
    .ok()?;
    let bytes: Arc<[u8]> = bytes.into();
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert((variant, size), bytes.clone());
    Some(bytes)
}

#[cfg(feature = "gpui-shell")]
pub fn preview(variant: AppIconName, requested: u32) -> Option<Arc<gpui::Image>> {
    static IMAGES: OnceLock<Mutex<HashMap<(AppIconName, u32), Arc<gpui::Image>>>> = OnceLock::new();
    let size = requested.clamp(16, ATLAS_WIDTH);
    let mut cache =
        IMAGES.get_or_init(Default::default).lock().unwrap_or_else(|error| error.into_inner());
    if let Some(image) = cache.get(&(variant, size)) {
        return Some(image.clone());
    }
    let bytes = png(variant, size)?;
    let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()));
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert((variant, size), image.clone());
    Some(image)
}

#[cfg(windows)]
pub mod windows {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use nebula_settings::AppIconName;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, DestroyIcon, HICON, ICON_BIG, ICON_SMALL, SM_CXICON, SM_CXSMICON,
        SendMessageW, WM_SETICON,
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

    pub fn set_window(hwnd: HWND, variant: AppIconName) {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        ICONS.with(|cache| {
            let mut cache = cache.borrow_mut();
            for (slot, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
                let size = unsafe { GetSystemMetricsForDpi(metric, dpi) }.clamp(16, 256) as u32;
                let icon = cache.entry((variant, size)).or_insert_with(|| {
                    let handle =
                        super::png(variant, size).map_or(std::ptr::null_mut(), |bytes| unsafe {
                            CreateIconFromResourceEx(
                                bytes.as_ptr(),
                                bytes.len() as u32,
                                1,
                                0x00030000,
                                size as i32,
                                size as i32,
                                0,
                            )
                        });
                    NativeIcon(handle)
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

    fn ico_png(bytes: &[u8], size: u32) -> &[u8] {
        let count = u16::from_le_bytes(bytes[4..6].try_into().unwrap()) as usize;
        for index in 0..count {
            let entry = &bytes[6 + index * 16..22 + index * 16];
            let width = if entry[0] == 0 { 256 } else { u32::from(entry[0]) };
            if width == size {
                let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
                let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
                return &bytes[offset..offset + length];
            }
        }
        panic!("missing frame {size}");
    }

    #[test]
    fn all_25_palettes_share_a_small_coverage_atlas() {
        assert_eq!(AppIconName::ALL.len(), 25);
        assert_eq!(AppIconName::default(), AppIconName::Titanium);
        assert!(COVERAGE_PNG.len() + include_bytes!("../windows/nebula.ico").len() <= 96 * 1024);
        for variant in AppIconName::ALL {
            for size in FRAME_SIZES {
                let pixels = rgba(variant, size).expect("palette image");
                assert_eq!(pixels.dimensions(), (size, size));
                assert_eq!(pixels.get_pixel(0, 0).0, [0; 4]);
                assert!(pixels.pixels().any(|pixel| pixel[3] > 0 && pixel[3] < 255));
            }
        }
    }

    #[test]
    fn runtime_pixels_match_the_default_and_legacy_exports() {
        for (variant, bytes) in [
            (AppIconName::Titanium, include_bytes!("../windows/nebula.ico").as_slice()),
            (AppIconName::SilverViolet, include_bytes!("../windows/nebula-light.ico").as_slice()),
            (AppIconName::GraphiteViolet, include_bytes!("../windows/nebula-dark.ico").as_slice()),
        ] {
            for size in FRAME_SIZES {
                let expected = image::load_from_memory(ico_png(bytes, size)).unwrap().to_rgba8();
                assert_eq!(rgba(variant, size).unwrap(), expected);
            }
        }
    }

    #[test]
    fn fractional_scale_sizes_are_exact_and_have_no_color_overshoot() {
        for variant in AppIconName::ALL {
            let palette = variant.palette();
            for size in [18, 25, 35, 50, 60, 72, 100, 144, 192, 288, 384] {
                let pixels = rgba(variant, size).unwrap();
                assert_eq!(pixels.dimensions(), (size, size));
                for pixel in pixels.pixels() {
                    if pixel[3] == 0 {
                        assert_eq!(pixel.0, [0; 4]);
                        continue;
                    }
                    for (channel, shift) in [16, 8, 0].into_iter().enumerate() {
                        let values = [palette.tile, palette.border, palette.mark]
                            .map(|color| ((color >> shift) & 255) as u8);
                        assert!(
                            (*values.iter().min().unwrap()..=*values.iter().max().unwrap())
                                .contains(&pixel[channel])
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn encoded_png_is_lossless_and_cached() {
        for variant in AppIconName::ALL {
            let first = png(variant, 32).unwrap();
            assert!(Arc::ptr_eq(&first, &png(variant, 32).unwrap()));
            assert_eq!(
                image::load_from_memory(&first).unwrap().to_rgba8(),
                rgba(variant, 32).unwrap()
            );
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
                    assert!(!cached.contains(&handles));
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
        unsafe { DestroyWindow(hwnd) };
    }
}
