use std::collections::HashMap;

use ahash::RandomState;
use crossfont::{
    BitmapBuffer, Error as RasterizerError, FontDesc, FontKey, GlyphKey, Metrics, Rasterize,
    RasterizedGlyph, Size, Slant, Style, Weight,
};
use log::{error, info};
use unicode_width::UnicodeWidthChar;

use crate::config::font::{Font, FontDescription};
use crate::config::ui_config::Delta;
use crate::gl::types::*;

use super::builtin_font;
use super::font_rasterizer::Rasterizer;

/// `LoadGlyph` allows for copying a rasterized glyph into graphics memory.
pub trait LoadGlyph {
    /// Load the rasterized glyph into GPU memory.
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph;

    /// Clear any state accumulated from previous loaded glyphs.
    ///
    /// This can, for instance, be used to reset the texture Atlas.
    fn clear(&mut self);
}

#[derive(Copy, Clone, Debug)]
pub struct Glyph {
    pub tex_id: GLuint,
    pub multicolor: bool,
    pub top: i16,
    pub left: i16,
    pub width: i16,
    pub height: i16,
    pub uv_bot: f32,
    pub uv_left: f32,
    pub uv_width: f32,
    pub uv_height: f32,
}

/// Naïve glyph cache.
///
/// Currently only keyed by `char`, and thus not possible to hold different
/// representations of the same code point.
pub struct GlyphCache {
    /// Cache of buffered glyphs.
    cache: HashMap<GlyphKey, Glyph, RandomState>,

    /// Rasterizer for loading new glyphs.
    rasterizer: Rasterizer,

    /// Regular font.
    pub font_key: FontKey,

    /// Bold font.
    pub bold_key: FontKey,

    /// Italic font.
    pub italic_key: FontKey,

    /// Bold italic font.
    pub bold_italic_key: FontKey,

    /// Embedded Maple face reserved for UI and terminal icon codepoints.
    pub symbol_key: FontKey,

    /// Font size.
    pub font_size: crossfont::Size,

    /// The UI font role's size: chrome
    /// typography rasterizes at this size and never follows the terminal
    /// zoom. Kept alongside the terminal size — `GlyphKey` carries the size,
    /// so both roles share one atlas.
    ui_font_size: crossfont::Size,

    /// Font offset.
    font_offset: Delta<i8>,

    /// Glyph offset.
    glyph_offset: Delta<i8>,

    /// Font metrics.
    metrics: Metrics,

    /// Size [`Self::metrics`] was measured at. `draw_doc_text` raises
    /// [`Self::font_size`] for a heading run without touching the metrics —
    /// baseline anchoring is built on the base ones — so anything comparing a
    /// glyph against `metrics` must rescale by `font_size / metrics_size`.
    metrics_size: crossfont::Size,

    /// The UI font role's metrics, truly rasterized at [`Self::ui_font_size`].
    ui_metrics: Metrics,

    /// Effective terminal cell width — `compute_cell_size`'s product for the
    /// active cell-width mode (already floored/rounded, `font.offset.x`
    /// folded in). Built-in glyphs fill exactly this width so box-drawing
    /// strokes and Powerline separators never drift a pixel under the
    /// relaxed (round) mode. Refreshed by the display whenever the font or
    /// cell-width mode changes.
    cell_width: usize,

    /// Effective cell width for the UI domain. The UI role is pinned to the
    /// compact (floor) mode, so this is `floor(ui_metrics.average_advance +
    /// font.offset.x)` — the same value the chrome grid lays UI columns at.
    ui_cell_width: usize,

    /// While set, [`Self::load_glyph`] bakes the UI role's descent into glyph
    /// anchors instead of the terminal's. The UI text paths flip this around
    /// their draws; see [`Self::begin_ui_domain`].
    ui_domain: bool,

    /// The terminal face displaced by an active [`Self::begin_preview_face`],
    /// restored on `end_preview_face`. `None` outside a preview draw.
    preview_restore: Option<FontKey>,

    /// Whether to use the built-in font for box drawing characters.
    builtin_box_drawing: bool,

    /// 全宽（2 列）字形在 bold run 里改用 Regular 字形（2026-07-28 裁定，
    /// 任务 #4）。DirectWrite 会把 CJK 粗体 fallback 到雅黑 Bold 真字形，
    /// 小字号下与周围 Regular 对比过重、随粗体词分布"时有时无"地发闷。
    /// 终端圈的成熟处理是宽字形不上粗字重——粗体语义仍由 ANSI 亮色映射
    /// 承担。默认开，`nebula_settings.txt` 的 `cjk_bold_regular=0` 关闭。
    pub wide_bold_use_regular: bool,
}

/// `font_family` 允许逗号分隔的多级列表（issue #33）：第一个是主族，其余
/// 按序作为缺字形时的 fallback 层级，内置 Maple 永远垫底。空段丢弃。
pub(crate) fn split_font_families(family: &str) -> Vec<&str> {
    family.split(',').map(str::trim).filter(|family| !family.is_empty()).collect()
}

/// The face actually loaded as the primary terminal font — the first entry
/// of the comma-separated list, or the raw string when there is no list.
pub(crate) fn primary_font_family(family: &str) -> &str {
    split_font_families(family).first().copied().unwrap_or(family)
}

impl GlyphCache {
    #[cfg(windows)]
    pub fn private_font_families(&self) -> Vec<String> {
        self.rasterizer.private_font_families()
    }

    #[cfg(windows)]
    pub fn system_font_families(&self) -> Vec<crate::font_install::SystemFontFamily> {
        self.rasterizer.system_font_families()
    }

    #[cfg(windows)]
    pub fn add_private_font(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Vec<String>, crossfont::Error> {
        self.rasterizer.add_private_font(path)
    }

    #[cfg(windows)]
    pub fn refresh_private_fonts(&mut self) -> Vec<String> {
        self.rasterizer.refresh_private_fonts()
    }

    /// 在不改变已配置终端字体的前提下，试着加载一个字体族。
    ///
    /// 字体目录纳入系统字体后，目录里的条目不再个个都是「已经成功加载过」
    /// 的——任意系统族都可能加载失败。选择因此必须先过这一关再提交。
    pub fn family_loads(&mut self, family: &str, size: Size) -> bool {
        Self::font_family_available(&mut self.rasterizer, family, size)
    }

    /// Check a font family without changing the configured terminal font.
    /// 系统族与导入的私有族走同一条查找路径（`load_family_font`）。
    pub fn font_family_available(rasterizer: &mut Rasterizer, family: &str, size: Size) -> bool {
        #[cfg(windows)]
        {
            rasterizer.load_family_font(family, Slant::Normal, Weight::Normal, size).is_ok()
        }
        #[cfg(not(windows))]
        {
            let description = FontDesc::new(
                family,
                Style::Description { slant: Slant::Normal, weight: Weight::Normal },
            );
            rasterizer.load_font(&description, size).is_ok()
        }
    }

    /// Load a system font for WYSIWYG preview rendering without changing the
    /// configured terminal font. Returns the `FontKey` on success so the caller
    /// can temporarily swap `Self::font_key` for drawing.
    ///
    /// 字体目录混了系统族与导入的私有族（`font_catalog`），查找统一走
    /// `load_family_font`（系统在前、私有兜底），与生效字体同一条路径。
    fn preview_font_key(&mut self, family: &str) -> Option<FontKey> {
        #[cfg(windows)]
        {
            self.rasterizer
                .load_family_font(family, Slant::Normal, Weight::Normal, self.ui_font_size)
                .ok()
        }
        #[cfg(not(windows))]
        {
            let description = FontDesc::new(
                family,
                Style::Description { slant: Slant::Normal, weight: Weight::Normal },
            );
            self.rasterizer.load_font(&description, self.ui_font_size).ok()
        }
    }

    /// 接下来的 chrome 文本改用 `family` 的真实字形绘制——字体选择器的行内
    /// 预览（WYSIWYG）。返回 `false` 表示这个族当前加载不了，调用方照常用
    /// 界面字体画：预览缺一行好过整行空白。
    ///
    /// 缓存键 [`GlyphKey`] 带 `font_key`，各族字形互不覆盖；`load_glyph` 的
    /// 溢出压缩照旧生效，于是预览呈现的正是**选中之后**的真实效果（比例
    /// 字体在固定网格里的挤压也如实展示）。必须与 [`Self::end_preview_face`]
    /// 配对。
    pub fn begin_preview_face(&mut self, family: &str) -> bool {
        let Some(key) = self.preview_font_key(family) else { return false };
        self.preview_restore = Some(self.font_key);
        self.font_key = key;
        true
    }

    pub fn end_preview_face(&mut self) {
        if let Some(key) = self.preview_restore.take() {
            self.font_key = key;
        }
    }

    pub fn new(mut rasterizer: Rasterizer, font: &Font) -> Result<GlyphCache, crossfont::Error> {
        let (regular, bold, italic, bold_italic) = Self::compute_font_keys(font, &mut rasterizer)?;
        #[cfg(windows)]
        let symbol_key = rasterizer.load_embedded_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            Slant::Normal,
            Weight::Normal,
            font.size(),
        )?;
        #[cfg(not(windows))]
        let symbol_key = regular;

        let metrics = GlyphCache::load_font_metrics(&mut rasterizer, font, regular)?;
        Ok(Self {
            cache: Default::default(),
            rasterizer,
            font_size: font.size(),
            // The UI role starts at the terminal size; the display pins it to
            // the config base size right after construction and on every font
            // change (`set_ui_font_size`).
            ui_font_size: font.size(),
            font_key: regular,
            bold_key: bold,
            italic_key: italic,
            bold_italic_key: bold_italic,
            symbol_key,
            font_offset: font.offset,
            glyph_offset: font.glyph_offset,
            metrics,
            metrics_size: font.size(),
            ui_metrics: metrics,
            // Seed the effective widths from the loaded metrics (compact mode
            // = floor, matching the display's initial `compute_cell_size`).
            // The display overwrites both on every font / mode change.
            cell_width: (metrics.average_advance as i32 + font.offset.x as i32).max(1) as usize,
            ui_cell_width: (metrics.average_advance as i32 + font.offset.x as i32).max(1) as usize,
            ui_domain: false,
            preview_restore: None,
            builtin_box_drawing: font.builtin_box_drawing,
            wide_bold_use_regular: true,
        })
    }

    // Load font metrics and adjust for glyph offset.
    fn load_font_metrics(
        rasterizer: &mut Rasterizer,
        font: &Font,
        key: FontKey,
    ) -> Result<Metrics, crossfont::Error> {
        // Need to load at least one glyph for the face before calling metrics.
        // The glyph requested here ('m' at the time of writing) has no special
        // meaning.
        rasterizer.get_glyph(GlyphKey { font_key: key, character: 'm', size: font.size() })?;

        let mut metrics = rasterizer.metrics(key, font.size())?;
        metrics.strikeout_position += font.glyph_offset.y as f32;
        Ok(metrics)
    }

    fn load_glyphs_for_font<L: LoadGlyph>(&mut self, font: FontKey, loader: &mut L) {
        let size = self.font_size;

        // Cache all ascii characters.
        for i in 32u8..=126u8 {
            self.get(GlyphKey { font_key: font, character: i as char, size }, loader, true);
        }
    }

    /// Computes font keys for (Regular, Bold, Italic, Bold Italic).
    fn compute_font_keys(
        font: &Font,
        rasterizer: &mut Rasterizer,
    ) -> Result<(FontKey, FontKey, FontKey, FontKey), crossfont::Error> {
        let size = font.size();

        // `family` 可以是逗号分隔的多级列表：主族承载四个样式 key 的加载，
        // 其余族按序组成缺字形 fallback 链（issue #33）。
        let normal_font = Self::primary_description(font.normal());
        #[cfg(windows)]
        let fallback_families: Vec<String> = split_font_families(&font.normal().family)
            .into_iter()
            .skip(1)
            .map(str::to_owned)
            .collect();

        // Load regular font.
        let regular_desc = Self::make_desc(&normal_font, Slant::Normal, Weight::Normal);

        let regular = Self::load_regular_font(
            rasterizer,
            &regular_desc,
            &normal_font.family,
            Slant::Normal,
            Weight::Normal,
            size,
        )?;
        #[cfg(windows)]
        Self::register_fallback_chain(
            rasterizer,
            regular,
            &fallback_families,
            Slant::Normal,
            Weight::Normal,
            size,
        );

        // Helper to load a description if it is not the `regular_desc`.
        let mut load_or_regular = |desc: FontDesc, family: &str, slant: Slant, weight: Weight| {
            if desc == regular_desc {
                regular
            } else {
                let key = Self::load_regular_font(rasterizer, &desc, family, slant, weight, size)
                    .unwrap_or(regular);
                #[cfg(windows)]
                if key != regular {
                    Self::register_fallback_chain(
                        rasterizer,
                        key,
                        &fallback_families,
                        slant,
                        weight,
                        size,
                    );
                }
                key
            }
        };

        // Load bold font.
        let bold_font = Self::primary_description(&font.bold());
        let bold_desc = Self::make_desc(&bold_font, Slant::Normal, Weight::Bold);
        let bold = load_or_regular(bold_desc, &bold_font.family, Slant::Normal, Weight::Bold);

        // Load italic font.
        let italic_font = Self::primary_description(&font.italic());
        let italic_desc = Self::make_desc(&italic_font, Slant::Italic, Weight::Normal);
        let italic =
            load_or_regular(italic_desc, &italic_font.family, Slant::Italic, Weight::Normal);

        // Load bold italic font.
        let bold_italic_font = Self::primary_description(&font.bold_italic());
        let bold_italic_desc = Self::make_desc(&bold_italic_font, Slant::Italic, Weight::Bold);
        let bold_italic = load_or_regular(
            bold_italic_desc,
            &bold_italic_font.family,
            Slant::Italic,
            Weight::Bold,
        );

        Ok((regular, bold, italic, bold_italic))
    }

    /// The description with `family` reduced to the primary entry of a
    /// comma-separated fallback list.
    fn primary_description(desc: &FontDescription) -> FontDescription {
        FontDescription {
            family: primary_font_family(&desc.family).to_owned(),
            style: desc.style.clone(),
        }
    }

    /// 为 `primary` 注册用户配置的多级 fallback 链（issue #33）：每个族先查
    /// 系统集合，再查私有/导入集合；链尾始终补上内置 Maple 兜底，中文与
    /// Nerd Font 图标的覆盖面不因换字体而回退。
    #[cfg(windows)]
    fn register_fallback_chain(
        rasterizer: &mut Rasterizer,
        primary: FontKey,
        families: &[String],
        slant: Slant,
        weight: Weight,
        size: Size,
    ) {
        let mut chain = Vec::new();
        for family in families {
            match rasterizer.load_family_font(family, slant, weight, size) {
                Ok(key) => {
                    if key != primary && !chain.contains(&key) {
                        chain.push(key);
                    }
                },
                Err(error) => error!("fallback font {family} unavailable: {error}"),
            }
        }
        if let Ok(maple) = rasterizer.load_embedded_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            slant,
            weight,
            size,
        ) && maple != primary
            && !chain.contains(&maple)
        {
            chain.push(maple);
        }
        rasterizer.set_fallback_chain(primary, chain);
    }

    fn load_regular_font(
        rasterizer: &mut Rasterizer,
        description: &FontDesc,
        family: &str,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<FontKey, crossfont::Error> {
        #[cfg(windows)]
        let preferred = rasterizer.load_preferred_font(description, family, slant, weight, size);
        #[cfg(not(windows))]
        let preferred = rasterizer.load_font(description, size);

        match preferred {
            Ok(font) => Ok(font),
            Err(err) => {
                error!("{err}");

                #[cfg(windows)]
                let fallback_desc = FontDesc::new(
                    "Cascadia Code",
                    Style::Description { slant: Slant::Normal, weight: Weight::Normal },
                );
                #[cfg(not(windows))]
                let fallback_desc =
                    Self::make_desc(Font::default().normal(), Slant::Normal, Weight::Normal);

                rasterizer.load_font(&fallback_desc, size)
            },
        }
    }

    fn make_desc(desc: &FontDescription, slant: Slant, weight: Weight) -> FontDesc {
        let style = if let Some(ref spec) = desc.style {
            Style::Specific(spec.to_owned())
        } else {
            Style::Description { slant, weight }
        };
        FontDesc::new(desc.family.clone(), style)
    }

    #[inline]
    pub fn font_key_for(&self, character: char, text_key: FontKey) -> FontKey {
        if is_private_use(character) { self.symbol_key } else { text_key }
    }

    /// Get a glyph from the font.
    ///
    /// If the glyph has never been loaded before, it will be rasterized and inserted into the
    /// cache.
    ///
    /// # Errors
    ///
    /// This will fail when the glyph could not be rasterized. Usually this is due to the glyph
    /// not being present in any font.
    pub fn get<L>(&mut self, glyph_key: GlyphKey, loader: &mut L, show_missing: bool) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        // 宽字形的 bold 降级发生在缓存键之前：bold run 里的 CJK 与 Regular
        // 共享同一条缓存/atlas 条目，粗细一致由构造保证。bold_key 本就回落
        // 到 regular 的配置下这里是无操作。
        let mut glyph_key = glyph_key;
        if self.wide_bold_use_regular && glyph_key.character.width() == Some(2) {
            if glyph_key.font_key == self.bold_key {
                glyph_key.font_key = self.font_key;
            } else if glyph_key.font_key == self.bold_italic_key {
                glyph_key.font_key = self.italic_key;
            }
        }

        // Try to load glyph from cache.
        if let Some(glyph) = self.cache.get(&glyph_key) {
            return *glyph;
        };

        // Rasterize the glyph using the built-in font for special characters or the user's font
        // for everything else.
        let rasterized = self
            .builtin_box_drawing
            .then(|| {
                // Built-in glyphs are DRAWN from metrics rather than
                // rasterized from the face, so they must follow the active
                // domain like every other glyph — the cursor-shape previews
                // (│ █ ▁) in the settings dropdown are UI text.
                let metrics = if self.ui_domain { &self.ui_metrics } else { &self.metrics };
                let cell_width = if self.ui_domain { self.ui_cell_width } else { self.cell_width };
                builtin_font::builtin_glyph(
                    glyph_key.character,
                    metrics,
                    cell_width,
                    &self.font_offset,
                    &self.glyph_offset,
                )
            })
            .flatten()
            .map_or_else(|| self.rasterizer.get_glyph(glyph_key), Ok);

        let glyph = match rasterized {
            Ok(rasterized) => self.load_glyph(loader, rasterized),
            // Load fallback glyph.
            Err(RasterizerError::MissingGlyph(rasterized)) if show_missing => {
                // Use `\0` as "missing" glyph to cache it only once.
                let missing_key = GlyphKey { character: '\0', ..glyph_key };
                if let Some(glyph) = self.cache.get(&missing_key) {
                    *glyph
                } else {
                    // If no missing glyph was loaded yet, insert it as `\0`.
                    let glyph = self.load_glyph(loader, rasterized);
                    self.cache.insert(missing_key, glyph);

                    glyph
                }
            },
            Err(_) => self.load_glyph(loader, Default::default()),
        };

        // Cache rasterized glyph.
        *self.cache.entry(glyph_key).or_insert(glyph)
    }

    /// Load glyph into the atlas.
    ///
    /// This will apply all transforms defined for the glyph cache to the rasterized glyph before
    pub fn load_glyph<L>(&self, loader: &mut L, mut glyph: RasterizedGlyph) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        // 2026-07-28 用户报告：任务列表里的"①"和后面的汉字叠在一起。根因
        // 是 East Asian Ambiguous（①②★№…）：unicode-width 按窄（1 列）分
        // 格，可主等宽字体没有这些字形，DirectWrite 落到 CJK 字体的全宽字
        // 形（≈2 列墨迹），画出来右半截压进邻格。终端侧不能单方面把格子改
        // 成 2 列——应用程序（Claude Code 等）按 1 列排版，格宽不一致会让
        // 整行错位。所以在装载时把溢出位图等比缩进一列：CPU、一次、进缓存
        // （绘制期 GPU 拉伸是被清晰度铁律禁止的）。1.4× 容差放过斜体悬伸
        // 这类正常越界，只捕获真正的全宽 fallback 字形。
        {
            let metrics = if self.ui_domain { &self.ui_metrics } else { &self.metrics };
            // 2026-08-05 用户报告：换成自带的导入字体后，Markdown 标题里
            // "Native" 的 N 正常、ative 明显小一圈，设置页标题的 l 也坍缩。
            // 根因就在这条判据：`draw_doc_text` 为标题把 font_size 抬到
            // 1.7 倍却不动 metrics（基线锚定按基准算），于是放大后的字形
            // 一律超过基准 advance×1.4，被当成全宽 fallback 压回一列——
            // 墨迹宽的字母压得多、窄的（i l t）几乎不压，同一行大小失衡。
            // 阈值必须跟着真正栅格化的字号走。字形墨迹本来就宽的字体（比如
            // 用户导入的 JetBrains Maple Mono）触发得更早，所以换字体才显形。
            let size_ratio = if self.ui_domain {
                1.0
            } else {
                self.font_size.as_px() / self.metrics_size.as_px().max(1.0)
            };
            let advance = metrics.average_advance as f32 * size_ratio;
            // Private Use Area（Nerd Font 图标、powerline 分隔符 U+E0B0…）
            // 刻意画满甚至越出格子拼接徽章，绝不能缩（2026-07-28 用户反馈：
            // 提示符图标全变小了）。只处理真实的 East Asian Ambiguous 文字。
            let private_use = matches!(u32::from(glyph.character),
                0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
            if glyph.character.width() == Some(1)
                && !private_use
                && advance > 0.0
                && glyph.width as f32 > advance * 1.4
            {
                shrink_glyph_to_advance(&mut glyph, advance);
            }
        }

        glyph.left += i32::from(self.glyph_offset.x);
        glyph.top += i32::from(self.glyph_offset.y);
        // The descent baked into the anchor comes from the glyph's domain:
        // UI-role text (any scale) anchors in the UI metrics so chrome never
        // moves with the terminal zoom; everything else (grid cells, doc
        // text) keeps the terminal domain its baseline math is built on.
        let metrics = if self.ui_domain { &self.ui_metrics } else { &self.metrics };
        glyph.top -= metrics.descent as i32;

        // The metrics of zero-width characters are based on rendering
        // the character after the current cell, with the anchor at the
        // right side of the preceding character. Since we render the
        // zero-width characters inside the preceding character, the
        // anchor has been moved to the right by one cell.
        if glyph.character.width() == Some(0) {
            glyph.left += metrics.average_advance as i32;
        }

        // Add glyph to cache.
        loader.load_glyph(&glyph)
    }

    /// Reset currently cached data in both GL and the registry to default state.
    pub fn reset_glyph_cache<L: LoadGlyph>(&mut self, loader: &mut L) {
        loader.clear();
        self.cache = Default::default();

        self.load_common_glyphs(loader);
    }

    /// Update the inner font size.
    ///
    /// NOTE: To reload the renderers's fonts [`Self::reset_glyph_cache`] should be called
    /// afterwards.
    pub fn update_font_size(&mut self, font: &Font) -> Result<(), crossfont::Error> {
        // Update dpi scaling.
        self.font_offset = font.offset;
        self.glyph_offset = font.glyph_offset;

        // Recompute font keys.
        let (regular, bold, italic, bold_italic) =
            Self::compute_font_keys(font, &mut self.rasterizer)?;
        #[cfg(windows)]
        let symbol_key = self.rasterizer.load_embedded_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            Slant::Normal,
            Weight::Normal,
            font.size(),
        )?;
        #[cfg(not(windows))]
        let symbol_key = regular;

        let metrics = GlyphCache::load_font_metrics(&mut self.rasterizer, font, regular)?;

        info!("Font size changed to {:?} px", font.size().as_px());

        self.font_size = font.size();
        self.font_key = regular;
        self.bold_key = bold;
        self.italic_key = italic;
        self.bold_italic_key = bold_italic;
        self.symbol_key = symbol_key;
        self.metrics = metrics;
        self.metrics_size = font.size();
        self.builtin_box_drawing = font.builtin_box_drawing;

        Ok(())
    }

    pub fn font_metrics(&self) -> crossfont::Metrics {
        self.metrics
    }

    /// Pin the effective terminal cell width. Call this after every font or
    /// cell-width-mode change, with the width the grid will lay columns at
    /// (`compute_cell_size`'s product). Built-in glyphs then fill exactly
    /// this width instead of re-flooring `average_advance` from the metrics.
    pub fn set_cell_width(&mut self, cell_width: usize) {
        self.cell_width = cell_width.max(1);
    }

    /// Pin the effective UI-domain cell width. The UI role is always compact
    /// (floor), so callers pass `compute_cell_size(.., Compact).0 as usize`.
    pub fn set_ui_cell_width(&mut self, cell_width: usize) {
        self.ui_cell_width = cell_width.max(1);
    }

    /// Metrics the regular face produces when truly rasterized at `size`.
    ///
    /// The UI anchor system steps chrome text by the REAL advance of the UI
    /// base font size; deriving it by scaling the terminal metrics ignores
    /// per-size hinting and drifts a fraction of a pixel per column, which
    /// reads as hairline seams once the terminal is zoomed.
    pub fn metrics_at(&mut self, size: crossfont::Size) -> Option<crossfont::Metrics> {
        // A glyph at the target size must be loaded before metrics resolve
        // (same contract as `load_font_metrics`).
        self.rasterizer
            .get_glyph(GlyphKey { font_key: self.font_key, character: 'm', size })
            .ok()?;
        self.rasterizer.metrics(self.font_key, size).ok()
    }

    /// Pin the UI font role to `size`:
    /// chrome typography rasterizes at this size regardless of the terminal
    /// zoom. Returns the role's metrics; on rasterizer failure the role
    /// degrades to the terminal font so chrome never goes blank.
    pub fn set_ui_font_size(&mut self, size: crossfont::Size) -> crossfont::Metrics {
        match self.metrics_at(size) {
            Some(metrics) => {
                self.ui_font_size = size;
                self.ui_metrics = metrics;
            },
            None => {
                self.ui_font_size = self.font_size;
                self.ui_metrics = self.metrics;
            },
        }
        self.ui_metrics
    }

    /// The UI font role's size. Shares the atlas with the terminal font —
    /// `GlyphKey` carries the size, so both roles' glyphs coexist.
    pub fn ui_font_size(&self) -> crossfont::Size {
        self.ui_font_size
    }

    /// The UI font role's metrics (real rasterized values, not scaled
    /// approximations of the terminal metrics).
    pub fn ui_font_metrics(&self) -> crossfont::Metrics {
        self.ui_metrics
    }

    /// Glyphs loaded until [`Self::end_ui_domain`] bake the UI role's descent
    /// into their anchor instead of the terminal's — a UI glyph positioned
    /// with the terminal descent rides upward as the zoom grows and jitters
    /// one pixel per zoom notch. The only cache-key overlap between the two
    /// domains happens at the base zoom, where both descents are equal.
    pub fn begin_ui_domain(&mut self) {
        self.ui_domain = true;
    }

    pub fn end_ui_domain(&mut self) {
        self.ui_domain = false;
    }

    /// Prefetch glyphs that are almost guaranteed to be loaded anyways.
    pub fn load_common_glyphs<L: LoadGlyph>(&mut self, loader: &mut L) {
        self.load_glyphs_for_font(self.font_key, loader);
        self.load_glyphs_for_font(self.bold_key, loader);
        self.load_glyphs_for_font(self.italic_key, loader);
        self.load_glyphs_for_font(self.bold_italic_key, loader);
    }
}

#[inline]
fn is_private_use(character: char) -> bool {
    matches!(character as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// Shrink an overflowing single-column fallback bitmap so its ink fits one
/// cell advance: uniform scale (a squashed ① reads worse than a small one),
/// box-filtered on the CPU once at load time, then horizontally centered in
/// the cell and re-seated on the baseline.
fn shrink_glyph_to_advance(glyph: &mut RasterizedGlyph, advance: f32) {
    let (src_w, src_h) = (glyph.width as usize, glyph.height as usize);
    if src_w == 0 || src_h == 0 {
        return;
    }
    let scale = advance / src_w as f32;
    let dst_w = ((src_w as f32 * scale).round() as usize).max(1);
    let dst_h = ((src_h as f32 * scale).round() as usize).max(1);
    let (bpp, src) = match &glyph.buffer {
        BitmapBuffer::Rgb(buffer) => (3usize, buffer),
        BitmapBuffer::Rgba(buffer) => (4usize, buffer),
    };
    if src.len() < src_w * src_h * bpp {
        // A lying bitmap header must not become an out-of-bounds read.
        return;
    }
    let mut dst = vec![0u8; dst_w * dst_h * bpp];
    let inv = 1.0 / scale;
    for dy in 0..dst_h {
        let sy_lo = (dy as f32 * inv) as usize;
        let sy_hi = (((dy + 1) as f32 * inv).ceil() as usize).clamp(sy_lo + 1, src_h);
        for dx in 0..dst_w {
            let sx_lo = (dx as f32 * inv) as usize;
            let sx_hi = (((dx + 1) as f32 * inv).ceil() as usize).clamp(sx_lo + 1, src_w);
            let samples = ((sy_hi - sy_lo) * (sx_hi - sx_lo)) as u32;
            for channel in 0..bpp {
                let mut sum = 0u32;
                for sy in sy_lo..sy_hi {
                    for sx in sx_lo..sx_hi {
                        sum += u32::from(src[(sy * src_w + sx) * bpp + channel]);
                    }
                }
                dst[(dy * dst_w + dx) * bpp + channel] = (sum / samples) as u8;
            }
        }
    }
    glyph.buffer = match &glyph.buffer {
        BitmapBuffer::Rgb(_) => BitmapBuffer::Rgb(dst),
        BitmapBuffer::Rgba(_) => BitmapBuffer::Rgba(dst),
    };
    glyph.width = dst_w as i32;
    glyph.height = dst_h as i32;
    // Center the now-narrower ink in its single cell; the fallback font's
    // full-width bearing has no meaning in this cell's coordinate space.
    glyph.left = ((advance - dst_w as f32) * 0.5).round() as i32;
    glyph.top = (glyph.top as f32 * scale).round() as i32;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn missing_font_family_is_not_reported_as_available() {
        let mut rasterizer = Rasterizer::new().expect("DirectWrite rasterizer");
        assert!(!GlyphCache::font_family_available(
            &mut rasterizer,
            "Nebula Missing Font Probe 8C0A651D",
            Size::new(11.25),
        ));
    }

    #[test]
    fn private_use_symbols_always_use_the_embedded_maple_key() {
        let rasterizer = Rasterizer::new().expect("DirectWrite rasterizer");
        let font = Font::default().with_family("Consolas");
        let cache = GlyphCache::new(rasterizer, &font).expect("glyph cache");

        assert_ne!(cache.font_key, cache.symbol_key);
        assert_eq!(cache.font_key_for('A', cache.font_key), cache.font_key);
        assert_eq!(cache.font_key_for('\u{ea83}', cache.font_key), cache.symbol_key);
        assert_eq!(cache.font_key_for('\u{f0000}', cache.font_key), cache.symbol_key);
    }
}
