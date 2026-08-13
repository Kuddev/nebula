use log::{debug, warn};
use winit::raw_window_handle::RawDisplayHandle;

use nebula_terminal::term::ClipboardType;

#[cfg(any(feature = "x11", target_os = "macos", windows))]
use copypasta::ClipboardContext;
use copypasta::ClipboardProvider;
use copypasta::nop_clipboard::NopClipboardContext;
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use copypasta::wayland_clipboard;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use copypasta::x11_clipboard::{Primary as X11SelectionClipboard, X11ClipboardContext};

pub struct Clipboard {
    clipboard: Box<dyn ClipboardProvider>,
    selection: Option<Box<dyn ClipboardProvider>>,
}

impl Clipboard {
    pub unsafe fn new(display: RawDisplayHandle) -> Self {
        match display {
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            RawDisplayHandle::Wayland(display) => {
                let (selection, clipboard) = unsafe {
                    wayland_clipboard::create_clipboards_from_external(display.display.as_ptr())
                };
                Self { clipboard: Box::new(clipboard), selection: Some(Box::new(selection)) }
            },
            _ => Self::default(),
        }
    }

    /// Used for tests, to handle missing clipboard provider when built without the `x11`
    /// feature, and as default clipboard value.
    pub fn new_nop() -> Self {
        Self { clipboard: Box::new(NopClipboardContext::new().unwrap()), selection: None }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        #[cfg(any(target_os = "macos", windows))]
        return Self { clipboard: Box::new(ClipboardContext::new().unwrap()), selection: None };

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        return Self {
            clipboard: Box::new(ClipboardContext::new().unwrap()),
            selection: Some(Box::new(X11ClipboardContext::<X11SelectionClipboard>::new().unwrap())),
        };

        #[cfg(not(any(feature = "x11", target_os = "macos", windows)))]
        return Self::new_nop();
    }
}

impl Clipboard {
    pub fn store(&mut self, ty: ClipboardType, text: impl Into<String>) {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            (ClipboardType::Selection, None) => return,
            _ => &mut self.clipboard,
        };

        // Windows: OpenClipboard races with clipboard listeners (IMEs, the
        // Win+V cloud clipboard, managers, remote-desktop tools) and fails
        // with OSError(5) 拒绝访问. The holder releases within milliseconds,
        // so short backoff retries resolve virtually all of these; only a
        // persistent failure is worth logging.
        let text = text.into();
        let mut last_err = None;
        for delay_ms in [0u64, 15, 40, 80] {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            match clipboard.set_contents(text.clone()) {
                Ok(()) => return,
                Err(err) => last_err = Some(err),
            }
        }
        if let Some(err) = last_err {
            warn!("Unable to store text in clipboard: {err}");
        }
    }

    pub fn load(&mut self, ty: ClipboardType) -> String {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            _ => &mut self.clipboard,
        };

        match clipboard.get_contents() {
            Err(err) => {
                debug!("Unable to load text from clipboard: {err}");
                String::new()
            },
            Ok(text) => text,
        }
    }
}

/// 剪贴板里的截图（无文本、只有位图，Win+Shift+S 的产物）转成 PNG 字节。
/// 图片粘贴回退用：本地 pane 落成临时文件粘路径，SSH pane 经 SFTP 上传。
/// 只认 CF_DIB 的 24/32bpp BI_RGB / BI_BITFIELDS——Windows 截图与主流应用
/// 复制出来的就是这两种；其余罕见编码返回 `None`，粘贴回退安静放弃。
#[cfg(windows)]
pub fn clipboard_image_png() -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_DIB: u32 = 8;

    // OpenClipboard 与剪贴板监听者（IME、Win+V、管理器）竞争，参照
    // `Clipboard::store` 的短退避重试。
    let mut opened = false;
    for delay_ms in [0u64, 15, 40] {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            opened = true;
            break;
        }
    }
    if !opened {
        return None;
    }

    // 从这里起任何路径都必须 CloseClipboard：拿数据后立即复制到自有缓冲。
    let dib = unsafe {
        if IsClipboardFormatAvailable(CF_DIB) == 0 {
            CloseClipboard();
            return None;
        }
        let handle: HANDLE = GetClipboardData(CF_DIB);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let size = GlobalSize(handle);
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() || size == 0 {
            GlobalUnlock(handle);
            CloseClipboard();
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        CloseClipboard();
        bytes
    };

    dib_to_png(&dib)
}

#[cfg(not(windows))]
pub fn clipboard_image_png() -> Option<Vec<u8>> {
    None
}

/// CF_DIB（BITMAPINFOHEADER + 裸像素）→ PNG。独立成纯函数以便测试。
#[cfg(windows)]
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    let u32_at = |offset: usize| -> Option<u32> {
        dib.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header_size = u32_at(0)? as usize;
    if header_size < 40 || dib.len() < header_size {
        return None;
    }
    let width = u32_at(4)? as i32;
    let raw_height = u32_at(8)? as i32;
    let bit_count = dib.get(14..16).map(|b| u16::from_le_bytes([b[0], b[1]]))? as usize;
    let compression = u32_at(16)?;

    // 0 = BI_RGB；3 = BI_BITFIELDS（截图常见，BGRA 顺序的默认掩码）。老式
    // BITMAPINFOHEADER(40) 的 BITFIELDS 掩码在头之后另占 12 字节；V4/V5 头
    // 已把掩码含在 header_size 里。
    if !(compression == 0 || compression == 3) || !(bit_count == 24 || bit_count == 32) {
        return None;
    }
    let masks = if compression == 3 && header_size == 40 { 12 } else { 0 };
    let data_offset = header_size + masks;

    let top_down = raw_height < 0;
    let width_px = usize::try_from(width).ok().filter(|w| *w > 0)?;
    let height_px = usize::try_from(raw_height.abs()).ok().filter(|h| *h > 0)?;
    // DIB 行按 4 字节对齐。
    let stride = (width_px * bit_count).div_ceil(32) * 4;
    let pixels = dib.get(data_offset..data_offset + stride * height_px)?;

    let mut rgba = Vec::with_capacity(width_px * height_px * 4);
    for row in 0..height_px {
        let source_row = if top_down { row } else { height_px - 1 - row };
        let line = &pixels[source_row * stride..source_row * stride + stride];
        for x in 0..width_px {
            let (b, g, r, a) = if bit_count == 32 {
                let p = &line[x * 4..x * 4 + 4];
                // 图标以外的 32bpp DIB 常把 alpha 通道整个写 0（截图即是）；
                // 全 0 当不透明处理，否则粘出来是一张全透明图。
                (p[0], p[1], p[2], p[3])
            } else {
                let p = &line[x * 3..x * 3 + 3];
                (p[0], p[1], p[2], 255)
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    if bit_count == 32 && rgba.iter().skip(3).step_by(4).all(|a| *a == 0) {
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, width_px as u32, height_px as u32, ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

#[cfg(all(test, windows))]
mod tests {
    use super::dib_to_png;

    /// 手工构造一张 2×2 的 32bpp bottom-up DIB，验证行序翻转与 BGRA→RGBA。
    #[test]
    fn dib_round_trips_bottom_up_bgra() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&2i32.to_le_bytes()); // width
        dib.extend_from_slice(&2i32.to_le_bytes()); // height (bottom-up)
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bit count
        dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        dib.extend_from_slice(&[0u8; 20]); // 剩余头字段
        // 像素（bottom-up：文件里第一行是图像的最后一行）。BGRA。
        // 图像期望：左上红、右上绿 / 左下蓝、右下白。
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 255, 255, 0]); // 蓝、白（底行）
        dib.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]); // 红、绿（顶行）

        let png = dib_to_png(&dib).expect("dib decodes");
        let decoded = image::load_from_memory(&png).expect("png decodes").to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255], "top-left red");
        assert_eq!(decoded.get_pixel(1, 0).0, [0, 255, 0, 255], "top-right green");
        assert_eq!(decoded.get_pixel(0, 1).0, [0, 0, 255, 255], "bottom-left blue");
        assert_eq!(decoded.get_pixel(1, 1).0, [255, 255, 255, 255], "bottom-right white");
    }

    #[test]
    fn unsupported_formats_are_rejected() {
        assert!(dib_to_png(&[]).is_none());
        let mut dib = vec![0u8; 40];
        dib[0] = 40; // header size ok
        dib[14] = 8; // 8bpp 调色板图不支持
        assert!(dib_to_png(&dib).is_none());
    }
}
