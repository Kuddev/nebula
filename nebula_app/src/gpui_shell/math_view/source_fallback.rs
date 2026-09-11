//! Multiline source presentation while a formula is pending or cannot fit.

use gpui::{
    App, Bounds, ContentMask, Pixels, SharedString, Size, TextRun, Window, WrappedLine, px, size,
};

pub(super) struct SourceFallback {
    lines: Vec<WrappedLine>,
    pub(super) size: Size<Pixels>,
}

impl SourceFallback {
    pub(super) fn shape(
        source: &SharedString,
        font_size: Pixels,
        mut run: TextRun,
        max_width: f32,
        line_height: Pixels,
        window: &Window,
    ) -> Self {
        let text = match crate::text_preview::multiline_source(source) {
            std::borrow::Cow::Borrowed(_) => source.clone(),
            std::borrow::Cow::Owned(text) => text.into(),
        };
        run.len = text.len();
        let wrap_width = max_width.is_finite().then(|| px(max_width.max(8.0)));
        let lines = window
            .text_system()
            .shape_text(
                text,
                font_size,
                &[run],
                wrap_width,
                Some(crate::text_preview::MAX_SOURCE_PREVIEW_LINES),
            )
            .map(|lines| lines.into_vec())
            .unwrap_or_default();
        let mut measured = size(px(0.0), px(0.0));
        for line in &lines {
            let line_size = line.size(line_height);
            measured.width = measured.width.max(line_size.width);
            measured.height += line_size.height;
        }
        measured.height = measured.height.max(line_height);
        Self { lines, size: measured }
    }

    pub(super) fn paint(
        &self,
        bounds: Bounds<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let mut origin = bounds.origin;
            for line in &self.lines {
                if origin.y >= bounds.bottom() {
                    break;
                }
                let _ = line.paint(
                    origin,
                    line_height,
                    gpui::TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                );
                origin.y += line.size(line_height).height;
            }
        });
    }
}
