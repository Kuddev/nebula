//! Bounded local thumbnails. All filesystem, image and PDF work runs off the UI thread.

use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EDGE: u32 = 640;
const MAX_CACHE_ITEMS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
struct FileKey {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
}

pub(crate) struct Thumbnail {
    pub(crate) png: Arc<[u8]>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pages: Option<u32>,
}

pub(crate) fn supports(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "pdf" | "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
        )
    })
}

pub(crate) fn load(path: &Path) -> Result<Arc<Thumbnail>, String> {
    static CACHE: OnceLock<Mutex<VecDeque<(FileKey, Arc<Thumbnail>)>>> = OnceLock::new();
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err("File exceeds preview limits".into());
    }
    let key = FileKey {
        path: path.to_owned(),
        modified: metadata.modified().ok(),
        length: metadata.len(),
    };
    let cache = CACHE.get_or_init(Default::default);
    {
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(index) = cache.iter().position(|(cached, _)| cached == &key) {
            let hit = cache.remove(index).unwrap();
            let result = hit.1.clone();
            cache.push_back(hit);
            return Ok(result);
        }
    }
    let pdf = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    let (bytes, pages) = if pdf {
        pdf_first_page(path)?
    } else {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("File exceeds preview limits".into());
        }
        (bytes, None)
    };
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let original = reader.decode().map_err(|error| error.to_string())?;
    let width = original.width();
    let height = original.height();
    let reduced = original.thumbnail(MAX_EDGE, MAX_EDGE);
    let mut png = Cursor::new(Vec::new());
    reduced.write_to(&mut png, image::ImageFormat::Png).map_err(|error| error.to_string())?;
    let thumbnail = Arc::new(Thumbnail { png: png.into_inner().into(), width, height, pages });
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    cache.retain(|(cached, _)| cached.path != key.path);
    cache.push_back((key, thumbnail.clone()));
    while cache.len() > MAX_CACHE_ITEMS {
        cache.pop_front();
    }
    Ok(thumbnail)
}

#[cfg(windows)]
fn pdf_first_page(path: &Path) -> Result<(Vec<u8>, Option<u32>), String> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::{
        StorageFile,
        Streams::{DataReader, InMemoryRandomAccessStream},
    };
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    let render = || -> windows::core::Result<(Vec<u8>, Option<u32>)> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        let _apartment = Apartment;
        let file =
            StorageFile::GetFileFromPathAsync(&path.to_string_lossy().as_ref().into())?.get()?;
        let pdf = PdfDocument::LoadFromFileAsync(&file)?.get()?;
        let pages = pdf.PageCount()?;
        let page = pdf.GetPage(0)?;
        let size = page.Size()?;
        let scale = MAX_EDGE as f32 / size.Width.max(size.Height).max(1.0);
        let options = PdfPageRenderOptions::new()?;
        options.SetDestinationWidth((size.Width * scale).round().max(1.0) as u32)?;
        options.SetDestinationHeight((size.Height * scale).round().max(1.0) as u32)?;
        let stream = InMemoryRandomAccessStream::new()?;
        page.RenderWithOptionsToStreamAsync(&stream, &options)?.get()?;
        let length = stream.Size()?.min(8 * 1024 * 1024) as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(length)?.get()?;
        let mut bytes = vec![0; length as usize];
        reader.ReadBytes(&mut bytes)?;
        page.Close()?;
        Ok((bytes, Some(pages)))
    };
    render().map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn pdf_first_page(path: &Path) -> Result<(Vec<u8>, Option<u32>), String> {
    use std::process::{Command, Stdio};
    let output = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("sips");
        command.args(["-s", "format", "png"]).arg(path).arg("--out").arg(output.path());
        command
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = {
        let mut command = Command::new("pdftoppm");
        command
            .args(["-f", "1", "-l", "1", "-singlefile", "-scale-to", "640", "-png"])
            .arg(path)
            .stdout(Stdio::from(output.reopen().map_err(|error| error.to_string())?));
        command
    };
    let mut child = command.stderr(Stdio::null()).spawn().map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) if status.success() => break,
            Some(_) => return Err("PDF preview is unavailable".into()),
            None if started.elapsed().as_secs() >= 5 => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("PDF preview timed out".into());
            },
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    Ok((std::fs::read(output.path()).map_err(|error| error.to_string())?, None))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn native_pdf_thumbnail_renders_the_first_page() {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 120] /Contents 4 0 R >>",
            "<< /Length 28 >>\nstream\n1 0 0 rg 10 10 100 100 re f\nendstream",
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = pdf.len();
        pdf.push_str("xref\n0 5\n0000000000 65535 f \n");
        for offset in &offsets[1..] {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("first-page.pdf");
        std::fs::write(&path, pdf).unwrap();
        let preview = load(&path).unwrap();
        assert_eq!(preview.pages, Some(1));
        let pixels = image::load_from_memory(&preview.png).unwrap().to_rgb8();
        let middle = pixels.get_pixel(pixels.width() / 2, pixels.height() / 2);
        assert!(middle[0] > 200 && middle[1] < 50 && middle[2] < 50);
    }
    use super::*;
    #[test]
    fn thumbnails_are_bounded_cached_and_invalidated_by_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        image::RgbaImage::from_pixel(1600, 800, image::Rgba([255, 0, 0, 255])).save(&path).unwrap();
        let first = load(&path).unwrap();
        let decoded = image::load_from_memory(&first.png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (640, 320));
        assert!(Arc::ptr_eq(&first, &load(&path).unwrap()));
        image::RgbaImage::from_pixel(32, 24, image::Rgba([0, 0, 255, 255])).save(&path).unwrap();
        let second = load(&path).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!((second.width, second.height), (32, 24));
    }
}
