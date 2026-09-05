//! Bounded decode/cache state for terminal image protocols.
//!
//! Encoded bytes arrive through the PTY, but decoding is serialized on the
//! background executor. This prevents a burst of compressed image bombs from
//! expanding concurrently on the UI thread or exhausting process memory.

use std::collections::VecDeque;
use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, ImageFormat};

const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_IMAGE_ENCODED_BYTES: usize = 12 * 1024 * 1024;
const MAX_IMAGE_DECODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_IMAGES: usize = 16;
const MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_QUEUED_IMAGES: usize = 16;
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct PendingInlineImage {
    sequence: u64,
    data: Arc<Vec<u8>>,
    abs_line: usize,
    display_width: f32,
    display_height: f32,
    row_span: usize,
}

#[derive(Clone)]
pub(super) struct InlineImage {
    pub image: Arc<RenderImage>,
    pub abs_line: usize,
    /// Display size reported by the terminal protocol, in device pixels.
    pub display_width: f32,
    pub display_height: f32,
    pub row_span: usize,
    decoded_bytes: usize,
}

#[derive(Default)]
pub(super) struct InlineImageStore {
    queued: VecDeque<PendingInlineImage>,
    queued_bytes: usize,
    decoding: bool,
    next_sequence: u64,
    images: VecDeque<(u64, InlineImage)>,
    decoded_bytes: usize,
}

impl InlineImageStore {
    pub fn enqueue(
        &mut self,
        data: Arc<Vec<u8>>,
        abs_line: usize,
        display_width: f32,
        display_height: f32,
        row_span: usize,
    ) -> Result<(), &'static str> {
        if data.len() > MAX_IMAGE_ENCODED_BYTES {
            return Err("encoded terminal image exceeds 12 MiB");
        }
        if !display_width.is_finite()
            || !display_height.is_finite()
            || display_width <= 0.0
            || display_height <= 0.0
        {
            return Err("terminal image has an invalid display size");
        }
        if self.queued.len() >= MAX_QUEUED_IMAGES {
            return Err("too many terminal images are waiting to decode");
        }
        let next_bytes = self
            .queued_bytes
            .checked_add(data.len())
            .ok_or("terminal image queue size overflow")?;
        if next_bytes > MAX_QUEUED_BYTES {
            return Err("terminal image decode queue exceeds 32 MiB");
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.queued.push_back(PendingInlineImage {
            sequence,
            data,
            abs_line,
            display_width,
            display_height,
            row_span: row_span.max(1),
        });
        self.queued_bytes = next_bytes;
        Ok(())
    }

    pub fn start_next(&mut self) -> Option<PendingInlineImage> {
        if self.decoding {
            return None;
        }
        let pending = self.queued.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(pending.data.len());
        self.decoding = true;
        Some(pending)
    }

    pub fn finish(&mut self, result: Result<(u64, InlineImage), String>) -> Result<(), String> {
        self.decoding = false;
        let (sequence, image) = result?;
        if image.decoded_bytes > MAX_IMAGE_DECODED_BYTES {
            return Err("decoded terminal image exceeds 64 MiB".to_owned());
        }

        while self.images.len() >= MAX_CACHED_IMAGES
            || self.decoded_bytes.saturating_add(image.decoded_bytes) > MAX_CACHE_BYTES
        {
            let Some((_, evicted)) = self.images.pop_front() else { break };
            self.decoded_bytes = self.decoded_bytes.saturating_sub(evicted.decoded_bytes);
        }
        self.decoded_bytes += image.decoded_bytes;
        self.images.push_back((sequence, image));
        Ok(())
    }

    /// Drop images whose reserved rows have left scrollback, then return cheap
    /// Arc-backed clones for this frame. Terminal output is chronological, so
    /// FIFO eviction is also the useful recency order here.
    pub fn frame_images(&mut self, scrollback_floor: usize) -> Vec<InlineImage> {
        self.images
            .retain(|(_, image)| image.abs_line.saturating_add(image.row_span) > scrollback_floor);
        self.decoded_bytes = self.images.iter().map(|(_, image)| image.decoded_bytes).sum();
        self.images.iter().map(|(_, image)| image.clone()).collect()
    }
}

pub(super) fn decode(pending: PendingInlineImage) -> Result<(u64, InlineImage), String> {
    let (render_image, decoded_bytes) = decode_bytes(pending.data.as_slice())?;
    Ok((
        pending.sequence,
        InlineImage {
            image: render_image,
            abs_line: pending.abs_line,
            display_width: pending.display_width,
            display_height: pending.display_height,
            row_span: pending.row_span,
            decoded_bytes,
        },
    ))
}

pub(super) fn decode_bytes(data: &[u8]) -> Result<(Arc<RenderImage>, usize), String> {
    if data.len() > MAX_IMAGE_ENCODED_BYTES {
        return Err("encoded terminal image exceeds 12 MiB".to_owned());
    }
    let format = image::guess_format(data)
        .map_err(|error| format!("unsupported terminal image: {error}"))?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif) {
        return Err(format!("unsupported terminal image format: {format:?}"));
    }

    let (encoded_width, encoded_height) =
        image::ImageReader::with_format(std::io::Cursor::new(data), format)
            .into_dimensions()
            .map_err(|error| format!("failed to read terminal image dimensions: {error}"))?;
    let pixels = u64::from(encoded_width)
        .checked_mul(u64::from(encoded_height))
        .ok_or_else(|| "terminal image dimensions overflow".to_owned())?;
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err("terminal image exceeds 16 megapixels".to_owned());
    }
    let decoded_bytes = usize::try_from(pixels.checked_mul(4).unwrap_or(u64::MAX))
        .map_err(|_| "terminal image allocation size overflow".to_owned())?;
    if decoded_bytes > MAX_IMAGE_DECODED_BYTES {
        return Err("decoded terminal image exceeds 64 MiB".to_owned());
    }

    // `load_from_memory_with_format` decodes the first frame for animated
    // formats. Animation is deliberately outside this first bounded slice.
    let decoded = image::load_from_memory_with_format(data, format)
        .map_err(|error| format!("failed to decode terminal image: {error}"))?;
    let (actual_width, actual_height) = (decoded.width(), decoded.height());
    if actual_width != encoded_width || actual_height != encoded_height {
        return Err("terminal image dimensions changed while decoding".to_owned());
    }

    let mut bgra = decoded.into_rgba8();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let render_image = Arc::new(RenderImage::new([Frame::new(bgra)]));
    Ok((render_image, decoded_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(data: Vec<u8>) -> PendingInlineImage {
        PendingInlineImage {
            sequence: 7,
            data: Arc::new(data),
            abs_line: 11,
            display_width: 2.0,
            display_height: 1.0,
            row_span: 1,
        }
    }

    fn encoded_png() -> Vec<u8> {
        let pixels = image::RgbaImage::from_pixel(2, 1, image::Rgba([10, 20, 30, 255]));
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels).write_to(&mut output, ImageFormat::Png).unwrap();
        output.into_inner()
    }

    #[test]
    fn queue_rejects_unbounded_encoded_backlog() {
        let mut store = InlineImageStore::default();
        let chunk = Arc::new(vec![0; 3 * 1024 * 1024]);
        for _ in 0..10 {
            store.enqueue(chunk.clone(), 0, 1.0, 1.0, 1).unwrap();
        }
        assert!(store.enqueue(chunk, 0, 1.0, 1.0, 1).is_err());
    }

    #[test]
    fn decode_png_produces_one_bgra_frame() {
        let (sequence, decoded) = decode(pending(encoded_png())).unwrap();
        assert_eq!(sequence, 7);
        assert_eq!(decoded.image.frame_count(), 1);
        assert_eq!(decoded.image.as_bytes(0), Some([30, 20, 10, 255, 30, 20, 10, 255].as_slice()));
    }

    #[test]
    fn decode_gif_keeps_only_the_first_frame() {
        use image::codecs::gif::{GifEncoder, Repeat};

        let mut encoded = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut encoded);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            encoder
                .encode_frame(Frame::new(image::RgbaImage::from_pixel(
                    2,
                    1,
                    image::Rgba([255, 0, 0, 255]),
                )))
                .unwrap();
            encoder
                .encode_frame(Frame::new(image::RgbaImage::from_pixel(
                    2,
                    1,
                    image::Rgba([0, 255, 0, 255]),
                )))
                .unwrap();
        }

        let (_, decoded) = decode(pending(encoded)).unwrap();
        assert_eq!(decoded.image.frame_count(), 1);
        assert_eq!(decoded.image.as_bytes(0), Some([0, 0, 255, 255, 0, 0, 255, 255].as_slice()));
    }

    #[test]
    fn decode_rejects_mp4_bytes() {
        let Err(error) = decode(pending(b"\0\0\0\x18ftypmp42not-an-image".to_vec())) else {
            panic!("MP4 input was accepted as a terminal image");
        };
        assert!(error.contains("unsupported terminal image"));
    }

    #[test]
    fn cache_is_bounded_by_count() {
        let mut store = InlineImageStore::default();
        for sequence in 0..MAX_CACHED_IMAGES + 3 {
            let rgba = image::RgbaImage::new(1, 1);
            store
                .finish(Ok((
                    sequence as u64,
                    InlineImage {
                        image: Arc::new(RenderImage::new([Frame::new(rgba)])),
                        abs_line: sequence,
                        display_width: 1.0,
                        display_height: 1.0,
                        row_span: 1,
                        decoded_bytes: 4,
                    },
                )))
                .unwrap();
        }
        assert_eq!(store.frame_images(0).len(), MAX_CACHED_IMAGES);
    }

    #[test]
    fn cache_drops_an_image_after_its_reserved_rows_leave_scrollback() {
        let mut store = InlineImageStore::default();
        store
            .finish(Ok((
                0,
                InlineImage {
                    image: Arc::new(RenderImage::new([Frame::new(image::RgbaImage::new(1, 1))])),
                    abs_line: 4,
                    display_width: 1.0,
                    display_height: 1.0,
                    row_span: 2,
                    decoded_bytes: 4,
                },
            )))
            .unwrap();

        assert_eq!(store.frame_images(5).len(), 1);
        assert!(store.frame_images(6).is_empty());
        assert_eq!(store.decoded_bytes, 0);
    }
}
