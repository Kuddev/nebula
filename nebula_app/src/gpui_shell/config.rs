//! 读取用户配置：`nebula.toml` + `nebula_settings.txt`，与 `nebula_app`
//! 共享同一套文件与语义。
//!
//! 两个来源、一个优先级：`nebula_settings.txt`（设置界面持久化的运行时
//! 意图，经共享 crate `nebula-settings` 读取）覆盖 `nebula.toml` 的对应
//! 字段；主题对终端色表的影响永远在最后叠加（镜像旧壳 `apply_term_colors`
//! 的次序：用户配色是 defaults，主题裁定背景/浅色替换/Powerline 槽位）。
//!
//! 解析全程宽容：未知字段忽略、非法值回退默认，任何用户配置都不会阻止
//! 启动。toml 查找路径与主应用 `config::installed_config` 一致；
//! `general.import`（及顶层 `import`）按主应用语义值级合并。
//!
//! 尚未对齐的字段（记录在案，后续步骤处理）：
//! - `font.offset` / `font.glyph_offset`（cell 度量微调）
//! - `colors.cursor` 的 `CellForeground`/`CellBackground` 反色语义
//! - `follow_system_theme`（系统外观联动切主题）
//! - 配置热重载（主应用用 notify 监视；本壳目前启动读一次）

use std::path::{Path, PathBuf};

use gpui::Global;
use nebula_settings::{CursorShapeName, RuntimeSettings};
use nebula_terminal::vte::ansi::CursorShape;
use serde::Deserialize;

use crate::gpui_shell::terminal::colors::Palette;

/// 应用启动时装载一次的全局设置。
pub struct Settings {
    pub font_family: String,
    pub font_bold_family: String,
    pub font_italic_family: String,
    pub font_bold_italic_family: String,
    /// GPUI 逻辑像素（配置里是 pt，1pt = 4/3 px @96dpi）。
    pub font_size_px: f32,
    pub palette: Palette,
    pub cursor_shape: Option<CursorShape>,
    pub cursor_blink: Option<bool>,
    /// 选区完成即复制（旧壳 `copy_on_select` 设置）。
    pub copy_on_select: bool,
    /// 实际加载的 toml 配置文件（诊断用；None 表示无 toml）。
    pub source_path: Option<PathBuf>,
}

impl Global for Settings {}

impl Settings {
    pub fn load() -> Self {
        let runtime = RuntimeSettings::load();
        let path = find_config_file();
        let raw = path
            .as_deref()
            .map(load_merged_toml)
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        let raw: RawConfig = raw.try_into().unwrap_or_default();

        // 字体：settings.txt（设置界面）覆盖 toml，最后落内置默认。
        let normal_family = runtime
            .font_family
            .clone()
            .or_else(|| raw.font.normal.family.clone())
            .unwrap_or_else(|| default_font_family().to_string());
        let secondary = |desc: &RawFontDesc| -> String {
            desc.family.clone().unwrap_or_else(|| normal_family.clone())
        };
        // 字号语义（对齐旧壳写盘）：settings.txt 的 font_size 是**逻辑像素**
        // （设置 spinner/Ctrl+滚轮持久化时已除 scale）；toml 的 font.size
        // 才是 pt（1pt = 4/3 px @96dpi）。
        let font_size_px = runtime
            .font_size_px
            .unwrap_or_else(|| raw.font.size.unwrap_or(11.25).clamp(4.0, 96.0) * 4.0 / 3.0);

        // 配色：toml 覆盖内置默认，主题裁定背景/浅色替换/Powerline 槽位，
        // 用户的背景覆盖色（设置页取色器）最后压轴——与旧壳同序。
        let mut palette = build_palette(&raw.colors);
        apply_theme(&mut palette, runtime.theme);
        if let Some(background) = runtime.background {
            palette.background = rgba8(background);
        }

        Settings {
            font_bold_family: secondary(&raw.font.bold),
            font_italic_family: secondary(&raw.font.italic),
            font_bold_italic_family: secondary(&raw.font.bold_italic),
            font_size_px,
            palette,
            cursor_shape: runtime.cursor_shape.map(|shape| match shape {
                CursorShapeName::Block => CursorShape::Block,
                CursorShapeName::Beam => CursorShape::Beam,
                CursorShapeName::Underline => CursorShape::Underline,
                CursorShapeName::Hollow => CursorShape::HollowBlock,
            }),
            cursor_blink: runtime.cursor_blink,
            copy_on_select: runtime.copy_on_select,
            font_family: normal_family,
            source_path: path,
        }
    }

    /// 引擎 Term 的启动配置（默认光标形状/闪烁来自运行时设置）。
    pub fn term_config(&self) -> nebula_terminal::term::Config {
        let mut config = nebula_terminal::term::Config::default();
        if let Some(shape) = self.cursor_shape {
            config.default_cursor_style.shape = shape;
        }
        if let Some(blinking) = self.cursor_blink {
            config.default_cursor_style.blinking = blinking;
        }
        config
    }
}

/// 主题叠加在 toml 配色之上（镜像旧壳 `apply_term_colors` 的范围与次序）：
/// 背景永远替换；Powerline 槽位 16..=23 替换；浅色主题另替换前景与
/// ANSI-16。dim 表与 bright_foreground 有意不动——与旧壳逐字段一致。
fn apply_theme(palette: &mut Palette, theme: nebula_settings::ThemeName) {
    let term = theme.term_theme();
    palette.background = rgba8(term.background);
    for (i, color) in term.powerline.into_iter().enumerate() {
        set_indexed(palette, nebula_settings::POWERLINE_SLOT0 + i as u8, rgba8(color));
    }
    if term.is_light {
        palette.foreground = rgba8(nebula_settings::LIGHT_FOREGROUND);
        for (i, color) in nebula_settings::LIGHT_ANSI.into_iter().enumerate() {
            palette.ansi[i] = rgba8(color);
        }
    }
}

fn set_indexed(palette: &mut Palette, index: u8, color: gpui::Rgba) {
    palette.indexed.retain(|(existing, _)| *existing != index);
    palette.indexed.push((index, color));
}

fn rgba8(color: nebula_settings::Rgb8) -> gpui::Rgba {
    gpui::Rgba {
        r: color[0] as f32 / 255.0,
        g: color[1] as f32 / 255.0,
        b: color[2] as f32 / 255.0,
        a: 1.0,
    }
}

fn default_font_family() -> &'static str {
    #[cfg(windows)]
    {
        "Maple Mono Normal NF CN"
    }
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        "monospace"
    }
}

/// 查找配置文件；顺序与 `nebula_app::config::installed_config` 一致。
/// `NEBULA_GPUI_CONFIG` 用于隔离测试：绝不能往用户真实配置目录写测试文件，
/// 正式版 Nebula 正在监视它做热重载。
fn find_config_file() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("NEBULA_GPUI_CONFIG") {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }

    #[cfg(windows)]
    {
        dirs::config_dir().map(|p| p.join("nebula").join("nebula.toml")).filter(|p| p.exists())
    }

    #[cfg(not(windows))]
    {
        let file_name = "nebula.toml";
        if let Ok(xdg_home) = std::env::var("XDG_CONFIG_HOME") {
            let base = PathBuf::from(xdg_home);
            for candidate in [base.join("nebula").join(file_name), base.join(file_name)] {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            for candidate in [
                home.join(".config/nebula").join(file_name),
                home.join(format!(".{file_name}")),
            ] {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        let etc = PathBuf::from("/etc/nebula").join(file_name);
        etc.exists().then_some(etc)
    }
}

/// 读取主文件并按主应用语义合并 imports：imports 先加载，主文件最后覆盖。
fn load_merged_toml(path: &Path) -> toml::Value {
    let main = read_toml(path);

    let imports: Vec<String> = [main.get("general").and_then(|g| g.get("import")), main.get("import")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut merged = toml::Value::Table(Default::default());
    for import in imports {
        let import_path = resolve_import_path(&import, path);
        if import_path.exists() {
            merged = merge_values(merged, read_toml(&import_path));
        } else {
            eprintln!("[nebula:gpui] config import not found: {}", import_path.display());
        }
    }
    merge_values(merged, main)
}

fn read_toml(path: &Path) -> toml::Value {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("[nebula:gpui] failed to read config {}: {err}", path.display());
            return toml::Value::Table(Default::default());
        },
    };
    // toml 0.9：`Value: FromStr` 解析的是单个 TOML 值；文档必须走 `Table`。
    match text.parse::<toml::Table>() {
        Ok(table) => toml::Value::Table(table),
        Err(err) => {
            eprintln!("[nebula:gpui] failed to parse config {}: {err}", path.display());
            toml::Value::Table(Default::default())
        },
    }
}

/// `~/` 展开为 home；相对路径相对于主配置文件所在目录。
fn resolve_import_path(import: &str, base_config: &Path) -> PathBuf {
    let mut path = PathBuf::from(import);
    if let Ok(stripped) = path.strip_prefix("~/") {
        if let Some(home) = home::home_dir() {
            path = home.join(stripped);
        }
    }
    if path.is_relative() {
        if let Some(dir) = base_config.parent() {
            path = dir.join(path);
        }
    }
    path
}

/// TOML 值级递归合并：表递归，其余类型 `other` 覆盖 `base`。
fn merge_values(base: toml::Value, other: toml::Value) -> toml::Value {
    match (base, other) {
        (toml::Value::Table(mut base), toml::Value::Table(other)) => {
            for (key, value) in other {
                let merged = match base.remove(&key) {
                    Some(existing) => merge_values(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            toml::Value::Table(base)
        },
        (_, other) => other,
    }
}

// ---------------------------------------------------------------------------
// TOML 字段子集（与 nebula_app/src/config 的 TOML 形状兼容）
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    font: RawFont,
    colors: RawColors,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFont {
    normal: RawFontDesc,
    bold: RawFontDesc,
    italic: RawFontDesc,
    bold_italic: RawFontDesc,
    /// pt，允许整数或浮点。
    size: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFontDesc {
    family: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawColors {
    primary: RawPrimary,
    cursor: RawCellColors,
    selection: RawCellColors,
    normal: RawAnsi8,
    bright: RawAnsi8,
    dim: Option<RawAnsi8>,
    indexed_colors: Vec<RawIndexed>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPrimary {
    foreground: Option<String>,
    background: Option<String>,
    bright_foreground: Option<String>,
    dim_foreground: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCellColors {
    // CellForeground/CellBackground 等特殊值 parse 失败自然落回默认。
    foreground: Option<String>,
    background: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAnsi8 {
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawIndexed {
    index: Option<u16>,
    color: Option<String>,
}

/// 解析 `#rrggbb` / `0xrrggbb`。
fn parse_rgb(text: &str) -> Option<gpui::Rgba> {
    let hex = text.strip_prefix('#').or_else(|| text.strip_prefix("0x"))?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(gpui::Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: 1.0,
    })
}

fn build_palette(raw: &RawColors) -> Palette {
    let mut palette = Palette::default();
    let set = |slot: &mut gpui::Rgba, value: &Option<String>| {
        if let Some(rgba) = value.as_deref().and_then(parse_rgb) {
            *slot = rgba;
        }
    };

    set(&mut palette.foreground, &raw.primary.foreground);
    set(&mut palette.background, &raw.primary.background);
    // 主应用语义：bright/dim foreground 未配置时派生自 foreground。
    palette.bright_foreground = raw
        .primary
        .bright_foreground
        .as_deref()
        .and_then(parse_rgb)
        .unwrap_or(palette.foreground);
    palette.dim_foreground = raw
        .primary
        .dim_foreground
        .as_deref()
        .and_then(parse_rgb)
        .unwrap_or_else(|| Palette::dim_of(palette.foreground));

    set(&mut palette.cursor, &raw.cursor.background);
    if let Some(selection) = raw.selection.background.as_deref().and_then(parse_rgb) {
        // 用户显式选区色保持主应用的不透明语义。
        palette.selection = selection;
    }

    let ansi8 = |group: &RawAnsi8| -> [Option<gpui::Rgba>; 8] {
        [
            group.black.as_deref().and_then(parse_rgb),
            group.red.as_deref().and_then(parse_rgb),
            group.green.as_deref().and_then(parse_rgb),
            group.yellow.as_deref().and_then(parse_rgb),
            group.blue.as_deref().and_then(parse_rgb),
            group.magenta.as_deref().and_then(parse_rgb),
            group.cyan.as_deref().and_then(parse_rgb),
            group.white.as_deref().and_then(parse_rgb),
        ]
    };

    for (i, color) in ansi8(&raw.normal).into_iter().enumerate() {
        if let Some(color) = color {
            palette.ansi[i] = color;
        }
    }
    for (i, color) in ansi8(&raw.bright).into_iter().enumerate() {
        if let Some(color) = color {
            palette.ansi[8 + i] = color;
        }
    }
    if let Some(dim) = &raw.dim {
        for (i, color) in ansi8(dim).into_iter().enumerate() {
            if let Some(color) = color {
                palette.dim[i] = color;
            }
        }
    } else {
        // 主应用语义：dim 未配置时由 normal 推导。
        for i in 0..8 {
            palette.dim[i] = Palette::dim_of(palette.ansi[i]);
        }
    }

    for indexed in &raw.indexed_colors {
        if let (Some(index), Some(color)) =
            (indexed.index, indexed.color.as_deref().and_then(parse_rgb))
        {
            if (16..=255).contains(&index) {
                palette.indexed.push((index as u8, color));
            }
        }
    }

    palette
}
