//! Nebula 运行时用户设置（`nebula_settings.txt`）与主题终端色表的共享读取。
//!
//! 这是两个 UI 壳同读的权威实现：设置文件的路径解析、键值语义，以及每个
//! 主题对终端色表的影响（背景替换、浅色主题的前景/ANSI-16 替换、Powerline
//! 提示符槽位）。设置界面写入的 `nebula_settings.txt` 是运行时用户意图的
//! 权威来源，优先级高于 `nebula.toml` 的对应字段。
//!
//! 同步事实（记录在案）：
//! - `nebula_app/src/display/ui/theme.rs` 是旧壳体内的既有实现；本 crate 的
//!   色值与其逐字一致。旧壳按 P4 裁定冻结保留，新 UI 一律读这里；改主题
//!   色值时两处同步，直到 P3 接入完成后旧壳引用此 crate。
//! - `nebula_terminal::tty::windows` 里有一份私有的同款路径/键值读取；引擎
//!   不能依赖 UI 侧 crate（依赖方向是铁律），属有意保留的最小重复。
//! - `follow_system_theme` 的系统外观联动暂未在此实现（消费方自行处理）。

use std::collections::HashMap;
use std::path::PathBuf;

/// 设置目录：`NEBULA_CONFIG_DIR` 覆盖（便携/测试隔离，指向目录本身）→
/// `%APPDATA%\Nebula` → `$HOME/.config/Nebula` → 临时目录。
/// 与引擎 tty 层、旧壳、注入的 shell 提示符脚本使用同一套解析。
pub fn settings_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("NEBULA_CONFIG_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Nebula")
}

pub fn settings_path() -> PathBuf {
    settings_dir().join("nebula_settings.txt")
}

/// `nebula_settings.txt` 的键值视图。行格式 `key=value`；键大小写不敏感，
/// 值两端修剪；空值视为未设置。
#[derive(Default)]
pub struct RawSettings {
    values: HashMap<String, String>,
}

impl RawSettings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .map(|text| Self::from_text(&text))
            .unwrap_or_default()
    }

    pub fn from_text(text: &str) -> Self {
        let mut values = HashMap::new();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        Self { values }
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        self.values
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn f32(&self, key: &str) -> Option<f32> {
        self.value(key)?.parse().ok()
    }

    /// `1/true/yes/on` → true；`0/false/no/off` → false；其余 `None`。
    pub fn bool_on(&self, key: &str) -> Option<bool> {
        match self.value(key)?.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }
}

/// 把若干键值写回 `nebula_settings.txt`：已有键**原地替换**（保留行序、
/// 未知键与原样格式），缺失键追加到尾部。全量读-改-写；与旧壳的全量覆盖
/// 写并存时后写者胜——与旧壳多窗口的既有语义一致。
pub fn persist_keys(updates: &[(&str, String)]) -> std::io::Result<()> {
    let path = settings_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = apply_updates(&text, updates);
    std::fs::create_dir_all(settings_dir())?;
    std::fs::write(&path, updated)
}

/// [`persist_keys`] 的纯文本变换，单测锁定行为。
pub fn apply_updates(text: &str, updates: &[(&str, String)]) -> String {
    let mut pending: Vec<(usize, &(&str, String))> = updates.iter().enumerate().collect();
    let mut out = String::with_capacity(text.len() + 64);

    for line in text.lines() {
        let key = line.split_once('=').map(|(key, _)| key.trim().to_ascii_lowercase());
        let hit = key.as_deref().and_then(|key| {
            pending.iter().position(|(_, (k, _))| k.eq_ignore_ascii_case(key))
        });
        match hit {
            Some(ix) => {
                let (_, (key, value)) = pending.remove(ix);
                out.push_str(key);
                out.push('=');
                out.push_str(value);
            },
            None => out.push_str(line),
        }
        out.push('\n');
    }

    // 追加缺失键，保持调用方给出的次序。
    pending.sort_by_key(|(order, _)| *order);
    for (_, (key, value)) in pending {
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

pub type Rgb8 = [u8; 3];

/// 主题标识；`nebula_settings.txt` 里 `theme=` 持久化 [`Self::prompt_name`]。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ThemeName {
    #[default]
    Nebula,
    SilverLight,
    SteelDark,
    LimestoneLight,
    CoalDark,
    LinenLight,
    MossDark,
}

impl ThemeName {
    pub fn from_prompt_name(name: &str) -> Option<Self> {
        Some(match name {
            "Nebula" => Self::Nebula,
            "SilverLight" => Self::SilverLight,
            "SteelDark" => Self::SteelDark,
            "LimestoneLight" => Self::LimestoneLight,
            "CoalDark" => Self::CoalDark,
            "LinenLight" => Self::LinenLight,
            "MossDark" => Self::MossDark,
            _ => return None,
        })
    }

    pub fn prompt_name(self) -> &'static str {
        match self {
            Self::Nebula => "Nebula",
            Self::SilverLight => "SilverLight",
            Self::SteelDark => "SteelDark",
            Self::LimestoneLight => "LimestoneLight",
            Self::CoalDark => "CoalDark",
            Self::LinenLight => "LinenLight",
            Self::MossDark => "MossDark",
        }
    }

    /// 主题对终端色表的全部影响（旧壳 `apply_term_colors` 的数据形态）。
    pub fn term_theme(self) -> TermTheme {
        // 背景与 is_light 来自各主题 palette()；powerline 为提示符段色
        // （icon bg/fg、path bg/fg、branch bg/fg、time bg/fg）。
        match self {
            Self::Nebula => TermTheme {
                background: [15, 17, 26],
                is_light: false,
                powerline: [
                    [57, 75, 112],
                    [192, 202, 245],
                    [41, 52, 82],
                    [169, 177, 214],
                    [47, 79, 79],
                    [139, 213, 202],
                    [29, 33, 46],
                    [100, 116, 139],
                ],
            },
            Self::SilverLight => TermTheme {
                background: [255, 255, 255],
                is_light: true,
                powerline: [
                    [229, 231, 235],
                    [55, 65, 81],
                    [243, 244, 246],
                    [55, 65, 81],
                    [224, 242, 254],
                    [3, 105, 161],
                    [249, 250, 251],
                    [107, 114, 128],
                ],
            },
            Self::SteelDark => TermTheme {
                background: [26, 28, 36],
                is_light: false,
                powerline: [
                    [71, 85, 105],
                    [241, 245, 249],
                    [51, 65, 85],
                    [203, 213, 225],
                    [59, 82, 73],
                    [163, 184, 153],
                    [40, 44, 56],
                    [148, 163, 184],
                ],
            },
            Self::LimestoneLight => TermTheme {
                background: [255, 255, 255],
                is_light: true,
                powerline: [
                    [214, 211, 209],
                    [250, 250, 249],
                    [231, 229, 228],
                    [68, 64, 60],
                    [200, 198, 167],
                    [41, 37, 36],
                    [235, 233, 230],
                    [163, 160, 151],
                ],
            },
            Self::CoalDark => TermTheme {
                background: [23, 23, 23],
                is_light: false,
                powerline: [
                    [82, 82, 82],
                    [245, 245, 245],
                    [64, 64, 64],
                    [212, 212, 212],
                    [74, 79, 65],
                    [181, 181, 166],
                    [48, 48, 48],
                    [115, 115, 115],
                ],
            },
            Self::LinenLight => TermTheme {
                background: [255, 255, 255],
                is_light: true,
                powerline: [
                    [212, 212, 208],
                    [255, 255, 255],
                    [229, 229, 223],
                    [63, 63, 63],
                    [181, 196, 177],
                    [45, 45, 45],
                    [236, 236, 230],
                    [176, 179, 176],
                ],
            },
            Self::MossDark => TermTheme {
                background: [30, 33, 30],
                is_light: false,
                powerline: [
                    [75, 85, 72],
                    [240, 253, 244],
                    [59, 66, 56],
                    [220, 252, 231],
                    [60, 79, 60],
                    [187, 247, 208],
                    [42, 47, 42],
                    [107, 114, 107],
                ],
            },
        }
    }
}

/// 一个主题对终端色表的影响。浅色主题额外用 [`LIGHT_FOREGROUND`] 与
/// [`LIGHT_ANSI`] 替换前景与 ANSI-16（配置的暗底配色在浅底上不可读）。
/// dim 表与 bright_foreground 有意不动——与旧壳逐字段一致。
pub struct TermTheme {
    pub background: Rgb8,
    pub is_light: bool,
    /// Powerline 提示符段色，发布到 256 色表 [`POWERLINE_SLOT0`]`..+8`。
    pub powerline: [Rgb8; 8],
}

/// Powerline 槽位起点（索引色 16..=23）。
pub const POWERLINE_SLOT0: u8 = 16;

/// 浅色主题的替换前景（#24292f）。
pub const LIGHT_FOREGROUND: Rgb8 = [36, 41, 47];

/// 浅色主题的 ANSI-16 替换表（GitHub Primer Light 派生；BrightWhite 刻意
/// 用灰——纯白会消失在白底终端上）。
pub const LIGHT_ANSI: [Rgb8; 16] = [
    [36, 41, 47],    // black #24292f
    [207, 34, 46],   // red #cf222e
    [26, 127, 55],   // green #1a7f37
    [154, 103, 0],   // yellow #9a6700
    [9, 105, 218],   // blue #0969da
    [130, 80, 223],  // magenta #8250df
    [27, 124, 131],  // cyan #1b7c83
    [110, 119, 129], // white #6e7781
    [87, 96, 106],   // bright black #57606a
    [164, 14, 38],   // bright red #a40e26
    [45, 164, 78],   // bright green #2da44e
    [191, 135, 0],   // bright yellow #bf8700
    [33, 139, 255],  // bright blue #218bff
    [164, 117, 249], // bright magenta #a475f9
    [49, 146, 170],  // bright cyan #3192aa
    [140, 149, 159], // bright white #8c959f
];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CursorShapeName {
    Block,
    Beam,
    Underline,
}

/// 新 UI 消费的运行时设置子集。字段随消费方增长；未设置的键保持 `None`
/// 让调用方自选回退。
pub struct RuntimeSettings {
    pub theme: ThemeName,
    pub font_family: Option<String>,
    /// pt（与设置界面一致）。
    pub font_size_pt: Option<f32>,
    pub cursor_shape: Option<CursorShapeName>,
    pub cursor_blink: Option<bool>,
    pub copy_on_select: bool,
    pub powerline: bool,
}

impl RuntimeSettings {
    pub fn load() -> Self {
        Self::from_raw(&RawSettings::load())
    }

    pub fn from_raw(raw: &RawSettings) -> Self {
        Self {
            theme: raw.value("theme").and_then(ThemeName::from_prompt_name).unwrap_or_default(),
            font_family: raw.value("font_family").map(str::to_owned),
            font_size_pt: raw.f32("font_size").map(|size| size.clamp(4.0, 96.0)),
            cursor_shape: raw.value("cursor_shape").and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "block" => Some(CursorShapeName::Block),
                    "beam" | "bar" => Some(CursorShapeName::Beam),
                    "underline" => Some(CursorShapeName::Underline),
                    _ => None,
                }
            }),
            cursor_blink: raw.bool_on("cursor_blink"),
            copy_on_select: raw.bool_on("copy_on_select").unwrap_or(false),
            powerline: raw.bool_on("powerline").unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_settings_shape() {
        let raw = RawSettings::from_text(
            "language=system\n\
             theme=SilverLight\n\
             follow_system_theme=0\n\
             shell=powershell\n\
             font_family=Maple Mono Normal NF CN\n\
             font_size=16.3\n\
             cursor_shape=beam\n\
             cursor_blink=1\n\
             copy_on_select=1\n\
             powerline=0\n\
             startup_directory=\n",
        );
        let settings = RuntimeSettings::from_raw(&raw);
        assert_eq!(settings.theme, ThemeName::SilverLight);
        assert_eq!(settings.font_family.as_deref(), Some("Maple Mono Normal NF CN"));
        assert_eq!(settings.font_size_pt, Some(16.3));
        assert_eq!(settings.cursor_shape, Some(CursorShapeName::Beam));
        assert_eq!(settings.cursor_blink, Some(true));
        assert!(settings.copy_on_select);
        assert!(!settings.powerline);
        // 空值 = 未设置。
        assert_eq!(raw.value("startup_directory"), None);
    }

    #[test]
    fn defaults_when_file_content_is_absent_or_junk() {
        let settings = RuntimeSettings::from_raw(&RawSettings::from_text("theme=NoSuchTheme\n"));
        assert_eq!(settings.theme, ThemeName::Nebula);
        assert_eq!(settings.font_family, None);
        assert!(!settings.copy_on_select);
        assert!(settings.powerline);
    }

    #[test]
    fn theme_names_roundtrip() {
        for theme in [
            ThemeName::Nebula,
            ThemeName::SilverLight,
            ThemeName::SteelDark,
            ThemeName::LimestoneLight,
            ThemeName::CoalDark,
            ThemeName::LinenLight,
            ThemeName::MossDark,
        ] {
            assert_eq!(ThemeName::from_prompt_name(theme.prompt_name()), Some(theme));
        }
    }

    #[test]
    fn light_themes_replace_foreground_dark_themes_do_not() {
        assert!(ThemeName::SilverLight.term_theme().is_light);
        assert!(ThemeName::LimestoneLight.term_theme().is_light);
        assert!(ThemeName::LinenLight.term_theme().is_light);
        assert!(!ThemeName::Nebula.term_theme().is_light);
        assert!(!ThemeName::SteelDark.term_theme().is_light);
        // 浅色主题终端底全白，背景由主题裁定而非用户配色。
        assert_eq!(ThemeName::SilverLight.term_theme().background, [255, 255, 255]);
        assert_eq!(ThemeName::Nebula.term_theme().background, [15, 17, 26]);
    }

    #[test]
    fn keys_are_case_insensitive_and_trimmed() {
        let raw = RawSettings::from_text("  Theme = SilverLight \nFONT_SIZE=12\n");
        assert_eq!(raw.value("theme"), Some("SilverLight"));
        assert_eq!(raw.f32("font_size"), Some(12.0));
    }

    #[test]
    fn apply_updates_replaces_in_place_and_appends_missing() {
        let text = "language=system\ntheme=Nebula\nshell=powershell\n";
        let updated = apply_updates(
            text,
            &[("theme", "SilverLight".into()), ("cursor_blink", "1".into())],
        );
        assert_eq!(
            updated,
            "language=system\ntheme=SilverLight\nshell=powershell\ncursor_blink=1\n"
        );
    }

    #[test]
    fn apply_updates_preserves_unknown_lines_and_matches_case_insensitively() {
        let text = "# comment survives\nTHEME=CoalDark\nkeybind=ctrl+x=Copy\n";
        let updated = apply_updates(text, &[("theme", "MossDark".into())]);
        // 键名按调用方写法输出，注释与奇形行原样保留。
        assert_eq!(updated, "# comment survives\ntheme=MossDark\nkeybind=ctrl+x=Copy\n");
    }

    #[test]
    fn apply_updates_on_empty_file_appends_all() {
        let updated =
            apply_updates("", &[("theme", "Nebula".into()), ("font_size", "12.5".into())]);
        assert_eq!(updated, "theme=Nebula\nfont_size=12.5\n");
    }
}
