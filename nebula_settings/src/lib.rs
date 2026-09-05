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

mod app_icon;
pub use app_icon::{AppIconName, AppIconPalette};

/// Settings/data directory shared by every shell and renderer.
///
/// Resolution: `NEBULA_CONFIG_DIR` override, then `%APPDATA%\Nebula` on
/// Windows, `~/Library/Application Support/Nebula` on macOS, and
/// `$XDG_CONFIG_HOME/nebula` (or `~/.config/nebula`) on Linux. Relative XDG
/// paths are ignored as required by the specification.
pub fn settings_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("NEBULA_CONFIG_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }

    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir)
            .join("Nebula")
    }

    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library").join("Application Support"))
            .unwrap_or_else(std::env::temp_dir)
            .join("Nebula")
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir)
            .join("nebula")
    }
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

/// `nebula_settings.txt` 里的 `keybind=combo:Action` 行（行序即优先级，
/// 后写者胜）。两壳共读这一份；`RawSettings` 的哈希视图按 key 去重，表达
/// 不了多行 keybind，必须走行级解析。
pub fn keybind_pairs() -> Vec<(String, String)> {
    std::fs::read_to_string(settings_path())
        .map(|text| keybind_pairs_from_text(&text))
        .unwrap_or_default()
}

/// [`keybind_pairs`] 的纯文本解析，单测锁定。
pub fn keybind_pairs_from_text(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case("keybind") {
            continue;
        }
        let Some((combo, action)) = value.split_once(':') else { continue };
        let (combo, action) = (combo.trim(), action.trim());
        if combo.is_empty() || action.is_empty() {
            continue;
        }
        pairs.push((combo.to_owned(), action.to_owned()));
    }
    pairs
}

/// 整表替换全部 `keybind=` 行（删除旧行、按给定顺序追加到尾部），其余行
/// 原样保留（注释、未知键、行序）。键位编辑器的提交路径——逐行增删的
/// 语义由调用方（keymap.rs）先算好整表再落盘。
pub fn persist_keybinds(pairs: &[(String, String)]) -> std::io::Result<()> {
    let path = settings_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = apply_keybinds(&text, pairs);
    std::fs::create_dir_all(settings_dir())?;
    std::fs::write(&path, updated)
}

/// [`persist_keybinds`] 的纯文本变换，单测锁定。
pub fn apply_keybinds(text: &str, pairs: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        let is_keybind =
            line.split_once('=').is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("keybind"));
        if !is_keybind {
            out.push_str(line);
            out.push('\n');
        }
    }
    for (combo, action) in pairs {
        out.push_str("keybind=");
        out.push_str(combo);
        out.push(':');
        out.push_str(action);
        out.push('\n');
    }
    out
}

/// [`persist_keys`] 的纯文本变换，单测锁定行为。
pub fn apply_updates(text: &str, updates: &[(&str, String)]) -> String {
    let mut pending: Vec<(usize, &(&str, String))> = updates.iter().enumerate().collect();
    let mut out = String::with_capacity(text.len() + 64);

    for line in text.lines() {
        let key = line.split_once('=').map(|(key, _)| key.trim().to_ascii_lowercase());
        let hit = key
            .as_deref()
            .and_then(|key| pending.iter().position(|(_, (k, _))| k.eq_ignore_ascii_case(key)));
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
    Nebula,
    SilverLight,
    SteelDark,
    LimestoneLight,
    CoalDark,
    LinenLight,
    MossDark,
    /// 深色：Nord 配色（Arctic Ice Studio 公开色板）。出厂默认。
    ///
    /// 跟随系统外观时它的浅色对应是 [`Self::Paper`]（配对表在旧壳
    /// `display/ui/theme.rs::for_system_appearance`，两壳同一来源），所以改这一个
    /// `#[default]` 就同时定下了「默认深色 = Nord、默认浅色 = Paper」。
    #[default]
    Nord,
    /// 浅色：暖纸面 Paper 配色。跟随系统时作为 [`Self::Nord`] 的浅色成员。
    Paper,
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
            "Nord" => Self::Nord,
            "Paper" => Self::Paper,
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
            Self::Nord => "Nord",
            Self::Paper => "Paper",
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
                exact: None,
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
                exact: None,
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
                exact: None,
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
                exact: None,
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
                exact: None,
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
                exact: None,
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
                exact: None,
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
            Self::Nord => TermTheme {
                background: [0x2e, 0x34, 0x40],
                is_light: false,
                exact: Some(ExactTermColors {
                    foreground: [0xf1, 0xf6, 0xff],
                    ansi: [
                        [0x3b, 0x42, 0x52],
                        [0xbf, 0x61, 0x6a],
                        [0xa3, 0xbe, 0x8c],
                        [0xeb, 0xcb, 0x8b],
                        [0x81, 0xa1, 0xc1],
                        [0xb4, 0x8e, 0xad],
                        [0x88, 0xc0, 0xd0],
                        [0xe5, 0xe9, 0xf0],
                        [0x4c, 0x56, 0x6a],
                        [0xbf, 0x61, 0x6a],
                        [0xa3, 0xbe, 0x8c],
                        [0xeb, 0xcb, 0x8b],
                        [0x81, 0xa1, 0xc1],
                        [0xb4, 0x8e, 0xad],
                        [0x8f, 0xbc, 0xbb],
                        [0xec, 0xef, 0xf4],
                    ],
                    cursor: Some([0xe5, 0xe9, 0xf0]),
                    cursor_text: Some([0x2e, 0x34, 0x40]),
                    cursor_stroke: Some([0x88, 0xc0, 0xd0]),
                    selection_foreground: Some([0x2e, 0x34, 0x40]),
                    selection_background: Some([0xe5, 0xe9, 0xf0]),
                }),
                // Nord 色板没有 Nebula 的 Powerline 槽位；以下只用它的
                // surface / ANSI / token 色为 Nebula 提示符保持同主题可读性。
                powerline: [
                    [0x3b, 0x42, 0x52],
                    [0xec, 0xef, 0xf4],
                    [0x4c, 0x56, 0x6a],
                    [0xe5, 0xe9, 0xf0],
                    [0x43, 0x4c, 0x5e],
                    [0x88, 0xc0, 0xd0],
                    [0x2e, 0x34, 0x40],
                    [0x7b, 0x82, 0x94],
                ],
            },
            Self::Paper => TermTheme {
                background: [0xfc, 0xfb, 0xf9],
                is_light: true,
                exact: Some(ExactTermColors {
                    foreground: [0x1a, 0x1a, 0x1a],
                    ansi: [
                        [0x1a, 0x1a, 0x1a],
                        [0xa3, 0x3a, 0x3a],
                        [0x2b, 0x5a, 0x38],
                        [0xa8, 0x5a, 0x20],
                        [0x4a, 0x7a, 0x8a],
                        [0x4a, 0x3a, 0x6a],
                        [0x3a, 0x7a, 0x6a],
                        [0x47, 0x46, 0x46],
                        [0x8c, 0x8a, 0x80],
                        [0xc3, 0x6a, 0x6a],
                        [0x6b, 0x9a, 0x78],
                        [0xc8, 0x8a, 0x50],
                        [0x7a, 0x9a, 0xaa],
                        [0x8a, 0x7a, 0x9a],
                        [0x6a, 0xba, 0xaa],
                        [0x2f, 0x2e, 0x2e],
                    ],
                    // Paper 色板没有声明 cursor / selection；None 必须保留，
                    // 不能把截图近似值冒充成主题自带数据。
                    cursor: None,
                    cursor_text: None,
                    cursor_stroke: None,
                    selection_foreground: None,
                    selection_background: None,
                }),
                powerline: [
                    [0xe0, 0xdf, 0xd5],
                    [0x1a, 0x1a, 0x1a],
                    [0xf5, 0xf4, 0xf0],
                    [0x47, 0x46, 0x46],
                    [0xc1, 0xbe, 0xb5],
                    [0x2b, 0x5a, 0x38],
                    [0xfc, 0xfb, 0xf9],
                    [0x8c, 0x8a, 0x80],
                ],
            },
        }
    }

    /// 主题自带的终端卡几何。
    ///
    /// 归属裁定：卡几何跟**主题**走，不跟全局设置走。于是「浮起的圆角卡」和
    /// 「铺满到边 + 一条竖线」不是两条渲染路径、也不是两套页面，只是同一条
    /// 路径在不同主题下的取值。切主题即换形态；用户的显式设置覆盖这里。
    ///
    /// [`ThemeName::Nord`] 是这套抽象的第一个实验者：半径与卡缝双双归零，
    /// 终端铺满整个右侧区域，侧栏与终端之间靠一条 1px 竖线分界——卡缝归零后
    /// 两块深色面板会糊成一片，竖线是这种形态下唯一的结构分界。
    pub fn card_geometry(self) -> ThemeCardGeometry {
        match self {
            Self::Nord => {
                ThemeCardGeometry { radius: 0.0, gutter: 0.0, shadow: false, divider: 1.0 }
            },
            _ => ThemeCardGeometry {
                radius: DEFAULT_PANE_CARD_RADIUS,
                gutter: DEFAULT_PANE_CARD_GUTTER,
                shadow: false,
                // 有卡缝就已经把两块面板分开了，再加竖线是重复的分界。
                divider: 0.0,
            },
        }
    }
}

/// 主题明确携带的终端色表。`None` 字段表示主题没有声明该值，调用方应保留
/// 用户配置或宿主默认值；这对没有 cursor/selection 数据的 Paper 很重要。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ExactTermColors {
    pub foreground: Rgb8,
    pub ansi: [Rgb8; 16],
    pub cursor: Option<Rgb8>,
    pub cursor_text: Option<Rgb8>,
    pub cursor_stroke: Option<Rgb8>,
    pub selection_foreground: Option<Rgb8>,
    pub selection_background: Option<Rgb8>,
}

/// 一个主题对终端色表的影响。浅色主题额外用 [`LIGHT_FOREGROUND`] 与
/// [`LIGHT_ANSI`] 替换前景与 ANSI-16（配置的暗底配色在浅底上不可读）。
/// dim 表与 bright_foreground 有意不动——与旧壳逐字段一致。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TermTheme {
    pub background: Rgb8,
    pub is_light: bool,
    /// 只有自带完整终端 palette 的主题使用；现有七个主题保持 `None`，继续
    /// 走原来的背景/浅色替换合同。
    pub exact: Option<ExactTermColors>,
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
    Hollow,
}

impl CursorShapeName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "block" => Some(Self::Block),
            "beam" | "bar" => Some(Self::Beam),
            "underline" => Some(Self::Underline),
            "hollow" => Some(Self::Hollow),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Beam => "beam",
            Self::Underline => "underline",
            Self::Hollow => "hollow",
        }
    }
}

/// 下面的小枚举都是 `nebula_settings.txt` 的键值域，取值与旧壳设置页
/// 逐字一致（parse 宽容：未知值回退默认）。

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum LanguagePref {
    #[default]
    System,
    ZhCn,
    EnUs,
}

impl LanguagePref {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim() {
            "system" => Some(Self::System),
            "zh-CN" => Some(Self::ZhCn),
            "en-US" => Some(Self::EnUs),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum AcceptKeyName {
    Right,
    Tab,
    #[default]
    Both,
}

impl AcceptKeyName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "right" => Some(Self::Right),
            "tab" => Some(Self::Tab),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Tab => "tab",
            Self::Both => "both",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CompletionStyleName {
    #[default]
    Inline,
    Popup,
}

impl CompletionStyleName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inline" | "ghost" => Some(Self::Inline),
            "popup" | "menu" | "list" => Some(Self::Popup),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Popup => "popup",
        }
    }
}

/// 标签栏位置。这里只提供产品当前支持的两种布局，避免把设置扩成未实现的
/// 通用停靠系统。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TabsPositionName {
    #[default]
    Sidebar,
    Top,
}

impl TabsPositionName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sidebar" => Some(Self::Sidebar),
            "top" => Some(Self::Top),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Top => "top",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TabRevealName {
    #[default]
    Slide,
    Instant,
}

impl TabRevealName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "slide" => Some(Self::Slide),
            "instant" => Some(Self::Instant),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Slide => "slide",
            Self::Instant => "instant",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum DensityName {
    #[default]
    Standard,
    Compact,
}

impl DensityName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "standard" => Some(Self::Standard),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Compact => "compact",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum NewTabPositionName {
    #[default]
    AfterCurrent,
    End,
}

impl NewTabPositionName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "after_current" => Some(Self::AfterCurrent),
            "end" => Some(Self::End),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::AfterCurrent => "after_current",
            Self::End => "end",
        }
    }
}

/// 新进程启动时如何选择承载新标签的窗口。
///
/// 默认保留 Nebula 既有的单实例行为：优先交给任意虚拟桌面上最近使用的
/// 窗口。`UseExisting` 进一步限定为当前虚拟桌面；找不到合适窗口时，两者
/// 都会回退为创建新窗口，不能把启动请求静默丢弃。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum WindowingBehaviorName {
    UseNew,
    #[default]
    UseAnyExisting,
    UseExisting,
}

impl WindowingBehaviorName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "use_new" | "usenew" => Some(Self::UseNew),
            "use_any_existing" | "useanyexisting" => Some(Self::UseAnyExisting),
            "use_existing" | "useexisting" => Some(Self::UseExisting),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::UseNew => "use_new",
            Self::UseAnyExisting => "use_any_existing",
            Self::UseExisting => "use_existing",
        }
    }
}

/// 侧栏版本控制视图的数据源：自动探测（就近的 .svn 提示 + git 优先），
/// 或强制只认 Git / SVN（混合仓库、或想屏蔽其中一种时用）。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum VcsDisplayName {
    #[default]
    Auto,
    Git,
    Svn,
}

impl VcsDisplayName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "git" => Some(Self::Git),
            "svn" => Some(Self::Svn),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Git => "git",
            Self::Svn => "svn",
        }
    }
}

/// 终端 BEL（`^G`）的通知方式：关 / 闪烁 / 声音 / 两者。
/// 缺省 `Both`：旧壳给 AI CLI 回合结束的可听提示默认是开的。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum BellModeName {
    None,
    Visual,
    Audible,
    #[default]
    Both,
}

impl BellModeName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(Self::None),
            "visual" => Some(Self::Visual),
            "audible" => Some(Self::Audible),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Visual => "visual",
            Self::Audible => "audible",
            Self::Both => "both",
        }
    }

    pub fn visual(self) -> bool {
        matches!(self, Self::Visual | Self::Both)
    }

    pub fn audible(self) -> bool {
        matches!(self, Self::Audible | Self::Both)
    }
}

/// 窗口背景模糊材质：关 / Mica 系列 / Aero / Acrylic。
///
/// 五档对应不同的 DWM 成本模型，落点在
/// `gpui_shell::wallpaper::apply_windows_accent_policy`：
/// - `None`：AccentPolicy 全零 + `DWMSBT_NONE`，透明交换链直接按内容 alpha 合成。
/// - `Mica` / `MicaAlt`：系统合成器提供壁纸 backdrop、模糊和色调，
///   Nebula 不读取壁纸文件，也不在客户区仿画材质；不逐帧采样其他窗口。
/// - `Aero`：实时 BlurBehind + 半透明深色玻璃色调。
/// - `Acrylic`：`ACCENT_ENABLE_ACRYLICBLURBEHIND`。DWM 对窗口后方**实时
///   内容**逐帧高斯模糊 + 噪点 + 饱和度。观感最好，也是唯一能透出后方其他
///   窗口的档位；2026-08-22 实测这一档把 dwm.exe 顶到 28% 均值。
///
/// # 缺省为什么是 Mica 而不是 Acrylic
///
/// 2026-08-22 用户实测：3K@165Hz 下 Acrylic 档 `nebula.exe` 22.7% +
/// `dwm.exe` 28.3%，关掉模糊后卡顿立刻好转。默认档必须是性能安全的那个，
/// 想要实时透视的用户显式选高质量。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
/// 窗口背景材质。**按 DWM 每帧成本递增排列**，不是质量递进——五者是五套成本
/// 模型，不存在"越靠后越好"：
///
/// - `None`：无材质。窗口按不透明度直接透出后方内容，不模糊。
/// - `Mica`：Windows 系统壁纸 backdrop；由 DWM 合成，不含后方其他窗口。
/// - `MicaAlt`：Mica 的更强色调变体，适合带标签栏的窗口。
/// - `Aero`：实时模糊窗口**后方的真实内容**，并叠加 Win32 深色玻璃色调。
/// - `Acrylic`：实时模糊 + tint/噪点/饱和度，最贵。
pub enum BlurModeName {
    None,
    #[default]
    Mica,
    MicaAlt,
    Aero,
    Acrylic,
}

impl BlurModeName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "0" | "false" | "no" => Some(Self::None),
            "mica" => Some(Self::Mica),
            "mica-alt" => Some(Self::MicaAlt),
            "aero" | "blurbehind" => Some(Self::Aero),
            "acrylic" => Some(Self::Acrylic),
            // 旧壳的布尔开关：`blur=1` 只表达"我要模糊"，并不表达要哪种材质。
            // 迁到 Mica 而非 Acrylic——本次改动的初衷就是降 GPU 占用，把存量
            // 用户留在最贵的那一档等于没修。想要实时透视的人显式改成 aero。
            "1" | "true" | "yes" | "on" => Some(Self::Mica),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Mica => "mica",
            Self::MicaAlt => "mica-alt",
            Self::Aero => "aero",
            Self::Acrylic => "acrylic",
        }
    }

    /// 是否需要窗口内容保留透明像素（除 `None` 外都要，否则材质被自己盖住）。
    pub fn enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CellWidthModeName {
    #[default]
    Compact,
    Relaxed,
}

impl CellWidthModeName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "relaxed" => Some(Self::Relaxed),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Relaxed => "relaxed",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ProxyModeName {
    #[default]
    Off,
    System,
    Custom,
}

impl ProxyModeName {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "system" => Some(Self::System),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::System => "system",
            Self::Custom => "custom",
        }
    }
}

/// 新 UI 消费的运行时设置。字段与出厂默认逐项对照旧壳
/// `nebula_settings_load`；`Option` 字段的 `None` = 键未设置，调用方自选
/// 回退（如 font_family 回落 nebula.toml）。
pub struct RuntimeSettings {
    pub language: LanguagePref,
    pub theme: ThemeName,
    pub app_icon: AppIconName,
    /// 系统外观变化时自动在主题家族的深浅成员间切换（默认关，尊重显式选择）。
    pub follow_system_theme: bool,
    pub font_family: Option<String>,
    /// **逻辑像素**（旧壳写盘语义：设置页 spinner 与 Ctrl+滚轮缩放持久化时
    /// 已除以 scale factor）。`None` = 跟随 nebula.toml 的 `font.size`（pt）。
    pub font_size_px: Option<f32>,
    pub cursor_shape: Option<CursorShapeName>,
    pub cursor_blink: Option<bool>,
    pub copy_on_select: bool,
    /// 裸 shell 风险粘贴确认：开 = 换行、提权命令或控制字符先确认；关 = 直接粘贴。
    pub multiline_paste_confirm: bool,
    /// 标签页关闭按钮（叉号）是否渲染：关 = 不渲染，仍可用中键关闭。
    pub tab_close_visible: bool,
    pub powerline: bool,
    /// 默认 shell 的原始 id（`shell=` 原文：powershell/bash/cmd/pwsh/WSL
    /// 发行版等）。解析归 shell 检测层，这里只做持久化往返。
    pub shell: Option<String>,
    pub startup_directory: Option<String>,
    /// AI 内联补全（ghost text）。
    pub ghost: bool,
    pub accept: AcceptKeyName,
    pub completion_style: CompletionStyleName,
    /// 全宽字形（CJK 等）bold run 用 Regular 字形（粗体提亮不加粗）。
    pub cjk_bold_regular: bool,
    pub tabs_position: TabsPositionName,
    pub tab_reveal: TabRevealName,
    pub density: DensityName,
    pub new_tab_position: NewTabPositionName,
    pub windowing_behavior: WindowingBehaviorName,
    pub cell_width_mode: CellWidthModeName,
    /// 侧栏版本控制视图的数据源（auto/git/svn）。
    pub vcs_display: VcsDisplayName,
    /// 终端 BEL：关 / 闪烁 / 声音 / 两者（缺省两者）。
    pub bell: BellModeName,
    /// 新会话欢迎屏 fastfetch（默认关：启动速度优先于观感，旧壳裁定）。
    pub fetch: bool,
    /// Check GitHub Releases after startup. Manual checks remain available
    /// from the Application settings page when this is disabled.
    pub auto_check_updates: bool,
    pub keep_session: bool,
    pub restore_session: bool,
    pub resume_ai: bool,
    /// 常驻系统托盘图标。
    pub tray: bool,
    pub blur: BlurModeName,
    pub opacity: f32,
    /// 终端背景覆盖色（设置页取色器写入，优先于主题背景）。
    pub background: Option<Rgb8>,
    /// 壁纸路径（空 = 无壁纸）。fit/alignment 存原文，解析归渲染层
    /// （旧壳 `renderer::image` 的 parse 是权威记号表）。
    pub background_image: Option<String>,
    /// 壁纸自身透明度，独立于窗口 opacity（保文字对比度，旧壳同语义）。
    pub background_image_opacity: f32,
    pub background_image_fit: Option<String>,
    pub background_image_alignment: Option<String>,
    /// 壁纸铺满整窗（含侧栏/标题栏）而非仅终端卡。
    pub background_image_cover_chrome: bool,
    pub panel_resize: bool,
    /// 左侧 Tab 栏逻辑宽；与旧壳的持久化键、钳制范围共用。
    pub sidebar_width: f32,
    pub ssh_proxy_mode: ProxyModeName,
    pub ssh_proxy_url: String,
    pub ssh_proxy_no_proxy: String,
    pub quick_terminal_hotkey: String,
    /// 终端卡圆角（逻辑像素）。`None` = 跟随主题自带的几何
    /// （见 [`ThemeName::card_geometry`]）。两壳同径：旧壳的
    /// `UI_SHELL_RADIUS_LOGICAL` 直接引用 [`DEFAULT_PANE_CARD_RADIUS`]，
    /// 不再各写一份字面量。
    pub pane_card_radius: Option<f32>,
    /// 终端卡与周围 chrome 的卡缝（逻辑像素）。**只作用于左 / 右 / 下三边**，
    /// 上边恒为零（08-26 裁定：侧栏 / 终端卡 / 右侧抽屉三列顶边都贴 chrome
    /// 下沿）。四边各自的值由 GPUI 壳的 `PaneCardStyle::margin` 表达。
    pub pane_card_gutter: Option<f32>,
    /// 终端卡投影。`shadow` 关掉 + 半径与卡缝归零，就是「铺满到边」的形态。
    pub pane_card_shadow: Option<bool>,
    /// 侧栏与终端之间的竖线宽度（逻辑像素，0 = 不画）。卡缝归零后两块深色
    /// 面板会糊成一片，竖线是那种铺满形态下唯一的结构分界；有卡缝时它是
    /// 多余的，所以默认按主题给（见 [`ThemeName::card_geometry`]）。
    pub pane_card_divider: Option<f32>,
}

/// 主题自带的终端卡几何。
///
/// 四个键一组：`radius`（圆角）、`gutter`（卡与周围 chrome 的外间距）、
/// `shadow`（投影）、`divider`（侧栏与终端之间的竖线）。卡几何跟主题走：
/// 切主题就换形态，不需要用户再去设置里调一次。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeCardGeometry {
    pub radius: f32,
    pub gutter: f32,
    pub shadow: bool,
    pub divider: f32,
}

/// 旧壳默认快速终端热键。
pub const DEFAULT_QUICK_TERMINAL_HOTKEY: &str = "ctrl+`";
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 230.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 170.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 420.0;
/// 终端卡几何的单一真源。旧壳的 `UI_SHELL_RADIUS_LOGICAL` 与 GPUI 壳的
/// `PaneCardStyle` 都从这里取默认值——半径曾经在 `display/mod.rs` 里写死、
/// 卡缝曾经在 `workspace.rs` 里写死，两处各写一份字面量就是那圈白边的来源。
///
/// 上限 28 是让半径不超过卡最窄边一半之前的实用上界；0 与卡缝 0 搭配就是
/// 「铺满到窗口边、无圆角」的形态（[`ThemeName::Nord`] 用的就是这组值）。
pub const DEFAULT_PANE_CARD_RADIUS: f32 = 14.0;
pub const MIN_PANE_CARD_RADIUS: f32 = 0.0;
pub const MAX_PANE_CARD_RADIUS: f32 = 28.0;
pub const DEFAULT_PANE_CARD_GUTTER: f32 = 8.0;
pub const MIN_PANE_CARD_GUTTER: f32 = 0.0;
pub const MAX_PANE_CARD_GUTTER: f32 = 32.0;
pub const MAX_PANE_CARD_DIVIDER: f32 = 4.0;
/// 系统材质启用且用户未明确设置透明度时，让基础层适度透出。
const SYSTEM_MATERIAL_OPACITY: f32 = 0.82;

impl RuntimeSettings {
    pub fn load() -> Self {
        Self::from_raw(&RawSettings::load())
    }

    pub fn from_raw(raw: &RawSettings) -> Self {
        let raw_blur = raw.value("blur");
        let blur = raw_blur.and_then(BlurModeName::from_settings).unwrap_or_default();
        let configured_opacity = raw.f32("opacity").map(|opacity| opacity.clamp(0.0, 1.0));
        // 旧壳会把缺省值完整写成 `blur=1` + `opacity=1.00`。那不是用户主动
        // 选择的实色覆盖，却会把迁移后的 Mica 完全盖住；只迁移这组旧记号，
        // 新枚举值下显式设置的 1.00 仍保持全不透明。
        let legacy_material_default = raw_blur.is_some_and(|value| {
            matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        }) && configured_opacity == Some(1.0);
        let material_default = matches!(blur, BlurModeName::Mica | BlurModeName::MicaAlt)
            && (configured_opacity.is_none() || legacy_material_default);
        let opacity = if material_default {
            SYSTEM_MATERIAL_OPACITY
        } else {
            configured_opacity.unwrap_or(1.0)
        };

        Self {
            language: raw
                .value("language")
                .and_then(LanguagePref::from_settings)
                .unwrap_or_default(),
            theme: raw.value("theme").and_then(ThemeName::from_prompt_name).unwrap_or_default(),
            app_icon: raw
                .value("app_icon")
                .and_then(AppIconName::from_settings)
                .unwrap_or_default(),
            follow_system_theme: raw.bool_on("follow_system_theme").unwrap_or(false),
            font_family: raw.value("font_family").map(str::to_owned),
            font_size_px: raw.f32("font_size").map(|size| size.clamp(4.0, 96.0)),
            cursor_shape: raw.value("cursor_shape").and_then(CursorShapeName::from_settings),
            cursor_blink: raw.bool_on("cursor_blink"),
            copy_on_select: raw.bool_on("copy_on_select").unwrap_or(false),
            multiline_paste_confirm: raw.bool_on("multiline_paste_confirm").unwrap_or(true),
            tab_close_visible: raw.bool_on("tab_close_visible").unwrap_or(true),
            powerline: raw.bool_on("powerline").unwrap_or(true),
            shell: raw.value("shell").or_else(|| raw.value("executor")).map(str::to_owned),
            startup_directory: raw.value("startup_directory").map(str::to_owned),
            ghost: raw.value("ghost").map(|v| v != "0").unwrap_or(true),
            accept: raw.value("accept").and_then(AcceptKeyName::from_settings).unwrap_or_default(),
            completion_style: raw
                .value("completion_style")
                .and_then(CompletionStyleName::from_settings)
                .unwrap_or_default(),
            cjk_bold_regular: raw.bool_on("cjk_bold_regular").unwrap_or(true),
            tabs_position: raw
                .value("tabs_position")
                .and_then(TabsPositionName::from_settings)
                .unwrap_or_default(),
            tab_reveal: raw
                .value("tab_reveal")
                .and_then(TabRevealName::from_settings)
                .unwrap_or_default(),
            density: raw.value("density").and_then(DensityName::from_settings).unwrap_or_default(),
            new_tab_position: raw
                .value("new_tab_position")
                .and_then(NewTabPositionName::from_settings)
                .unwrap_or_default(),
            windowing_behavior: raw
                .value("windowing_behavior")
                .and_then(WindowingBehaviorName::from_settings)
                .unwrap_or_default(),
            cell_width_mode: raw
                .value("cell_width_mode")
                .and_then(CellWidthModeName::from_settings)
                .unwrap_or_default(),
            vcs_display: raw
                .value("vcs_display")
                .and_then(VcsDisplayName::from_settings)
                .unwrap_or_default(),
            bell: raw.value("bell").and_then(BellModeName::from_settings).unwrap_or_default(),
            fetch: raw.bool_on("fetch").unwrap_or(false),
            auto_check_updates: raw.bool_on("auto_check_updates").unwrap_or(true),
            keep_session: raw.bool_on("keep_session").unwrap_or(false),
            restore_session: raw.bool_on("restore_session").unwrap_or(true),
            resume_ai: raw.bool_on("resume_ai").unwrap_or(true),
            tray: raw.bool_on("tray").unwrap_or(true),
            blur,
            opacity,
            background: raw.value("background").and_then(parse_hex_rgb),
            background_image: raw
                .value("background_image")
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            // 默认 0.38：旧壳 display/settings 同值（壁纸压不过文字）。
            background_image_opacity: raw
                .f32("background_image_opacity")
                .map(|o| o.clamp(0.0, 1.0))
                .unwrap_or(0.38),
            background_image_fit: raw.value("background_image_fit").map(str::to_owned),
            background_image_alignment: raw.value("background_image_alignment").map(str::to_owned),
            background_image_cover_chrome: raw
                .bool_on("background_image_cover_chrome")
                .unwrap_or(false),
            panel_resize: raw.bool_on("panel_resize").unwrap_or(false),
            sidebar_width: raw
                .f32("sidebar_w")
                .map(|width| width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH))
                .unwrap_or(DEFAULT_SIDEBAR_WIDTH),
            ssh_proxy_mode: raw
                .value("ssh_proxy_mode")
                .and_then(ProxyModeName::from_settings)
                .unwrap_or_default(),
            ssh_proxy_url: raw.value("ssh_proxy_url").unwrap_or_default().to_owned(),
            ssh_proxy_no_proxy: raw.value("ssh_proxy_no_proxy").unwrap_or_default().to_owned(),
            quick_terminal_hotkey: raw
                .value("quick_terminal_hotkey")
                .unwrap_or(DEFAULT_QUICK_TERMINAL_HOTKEY)
                .to_owned(),
            // 这四项一律保留 `Option`：`None` 就是「用户没设过」，让
            // `ThemeName::card_geometry` 的主题默认生效。若在这里就 unwrap 成
            // 具体数字，切主题便再也换不了形态——那正是要避免的。
            pane_card_radius: raw
                .f32("pane_card_radius")
                .map(|r| r.clamp(MIN_PANE_CARD_RADIUS, MAX_PANE_CARD_RADIUS)),
            pane_card_gutter: raw
                .f32("pane_card_gutter")
                .map(|g| g.clamp(MIN_PANE_CARD_GUTTER, MAX_PANE_CARD_GUTTER)),
            pane_card_shadow: raw.bool_on("pane_card_shadow"),
            pane_card_divider: raw
                .f32("pane_card_divider")
                .map(|d| d.clamp(0.0, MAX_PANE_CARD_DIVIDER)),
        }
    }
}

/// 解析 `#rrggbb`（旧壳 `parse_hex_rgb` 同款：# 前缀可省）。
pub fn parse_hex_rgb(value: &str) -> Option<Rgb8> {
    let hex = value.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

/// 格式化为 `#rrggbb`（写盘格式，与旧壳一致）。
pub fn format_hex_rgb(rgb: Rgb8) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_defaults_and_unknown_values_use_titanium() {
        for text in ["", "app_icon=\n", "app_icon=unknown\n", "app_icon=../custom.ico\n"] {
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(text));
            assert_eq!(settings.app_icon, AppIconName::Titanium);
        }
    }

    #[test]
    fn all_app_icons_round_trip_without_changing_other_settings() {
        for variant in AppIconName::ALL {
            let text = apply_updates(
                "# keep\ntheme=Nord\ncustom=preserved\napp_icon=silver-violet\n",
                &[("app_icon", variant.settings_value().to_owned())],
            );
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(&text));
            assert_eq!(settings.app_icon, variant);
            assert_eq!(settings.theme, ThemeName::Nord);
            assert!(text.contains("custom=preserved"));
            assert!(text.contains("# keep"));
            assert_eq!(text.matches("app_icon=").count(), 1);
        }
        assert_eq!(AppIconName::from_settings(" TITANIUM "), Some(AppIconName::Titanium));
        assert_eq!(AppIconName::from_settings("night-mint"), Some(AppIconName::NightMint));
        let numbers: std::collections::HashSet<_> =
            AppIconName::ALL.into_iter().map(|variant| variant.palette().number).collect();
        assert_eq!(numbers.len(), 25);
    }

    #[test]
    fn nord_carries_the_flush_card_geometry() {
        // Nord 是卡几何抽象的第一个实验者：半径与卡缝双双归零、靠一条竖线分界。
        // 这四个数一起构成「终端铺满整个右侧区域」的形态，任一项退回默认都会
        // 让它变回浮起的圆角卡——所以逐项钉死，而不是只断言 radius。
        let nord = ThemeName::Nord.card_geometry();
        assert_eq!(nord.radius, 0.0);
        assert_eq!(nord.gutter, 0.0);
        assert_eq!(nord.divider, 1.0);
        assert!(!nord.shadow);

        // 其余主题保持卡片形态，且**不画**竖线：有卡缝时两块面板已经分开了，
        // 再加一条线是重复的分界。
        for theme in [ThemeName::Nebula, ThemeName::Paper, ThemeName::CoalDark] {
            let geometry = theme.card_geometry();
            assert_eq!(geometry.radius, DEFAULT_PANE_CARD_RADIUS);
            assert_eq!(geometry.gutter, DEFAULT_PANE_CARD_GUTTER);
            assert_eq!(geometry.divider, 0.0);
        }
    }

    #[test]
    fn card_geometry_keys_stay_none_until_the_user_sets_them() {
        // `None` 的含义是「用户没设过」，主题默认因此能生效。若在解析处就
        // unwrap 成具体数字，切主题便再也换不了形态——那是这套抽象的失效模式。
        let untouched = RuntimeSettings::from_raw(&RawSettings::from_text("theme=Nord\n"));
        assert_eq!(untouched.pane_card_radius, None);
        assert_eq!(untouched.pane_card_gutter, None);
        assert_eq!(untouched.pane_card_shadow, None);
        assert_eq!(untouched.pane_card_divider, None);

        // 显式值覆盖主题默认，且越界值按范围钳制而不是被丢弃。
        let tuned = RuntimeSettings::from_raw(&RawSettings::from_text(
            "pane_card_radius=999\n\
             pane_card_gutter=12\n\
             pane_card_shadow=on\n\
             pane_card_divider=99\n",
        ));
        assert_eq!(tuned.pane_card_radius, Some(MAX_PANE_CARD_RADIUS));
        assert_eq!(tuned.pane_card_gutter, Some(12.0));
        assert_eq!(tuned.pane_card_shadow, Some(true));
        assert_eq!(tuned.pane_card_divider, Some(MAX_PANE_CARD_DIVIDER));
    }

    #[test]
    fn parses_real_settings_shape() {
        let raw = RawSettings::from_text(
            "language=zh-CN\n\
             theme=SilverLight\n\
             follow_system_theme=0\n\
             ghost=1\n\
             accept=tab\n\
             completion_style=popup\n\
             shell=pwsh\n\
             font_family=Maple Mono Normal NF CN\n\
             font_size=16.3\n\
             cursor_shape=beam\n\
             cursor_blink=1\n\
             copy_on_select=1\n\
             multiline_paste_confirm=0\n\
             tab_close_visible=0\n\
             bell=audible\n\
             cjk_bold_regular=1\n\
             tabs_position=top\n\
             tab_reveal=instant\n\
             density=compact\n\
             new_tab_position=end\n\
             windowing_behavior=use_existing\n\
             cell_width_mode=relaxed\n\
             fetch=1\n\
             powerline=0\n\
             keep_session=1\n\
             restore_session=0\n\
             resume_ai=0\n\
             tray=0\n\
             blur=0\n\
             opacity=0.87\n\
             background=#101216\n\
             panel_resize=1\n\
             sidebar_w=222\n\
             ssh_proxy_mode=custom\n\
             ssh_proxy_url=socks5://127.0.0.1:7890\n\
             quick_terminal_hotkey=alt+`\n\
             startup_directory=\n",
        );
        let settings = RuntimeSettings::from_raw(&raw);
        assert_eq!(settings.language, LanguagePref::ZhCn);
        assert_eq!(settings.theme, ThemeName::SilverLight);
        assert_eq!(settings.font_family.as_deref(), Some("Maple Mono Normal NF CN"));
        // font_size 键存的是逻辑像素（旧壳写盘语义），不做 pt 换算。
        assert_eq!(settings.font_size_px, Some(16.3));
        assert_eq!(settings.cursor_shape, Some(CursorShapeName::Beam));
        assert_eq!(settings.cursor_blink, Some(true));
        assert!(settings.copy_on_select);
        assert!(!settings.multiline_paste_confirm);
        assert!(!settings.tab_close_visible);
        assert_eq!(settings.bell, BellModeName::Audible);
        assert!(!settings.powerline);
        assert_eq!(settings.shell.as_deref(), Some("pwsh"));
        assert_eq!(settings.accept, AcceptKeyName::Tab);
        assert_eq!(settings.completion_style, CompletionStyleName::Popup);
        assert_eq!(settings.tabs_position, TabsPositionName::Top);
        assert_eq!(settings.tab_reveal, TabRevealName::Instant);
        assert_eq!(settings.density, DensityName::Compact);
        assert_eq!(settings.new_tab_position, NewTabPositionName::End);
        assert_eq!(settings.windowing_behavior, WindowingBehaviorName::UseExisting);
        assert_eq!(settings.cell_width_mode, CellWidthModeName::Relaxed);
        assert!(settings.fetch);
        assert!(settings.keep_session);
        assert!(!settings.restore_session);
        assert!(!settings.resume_ai);
        assert!(!settings.tray);
        assert_eq!(settings.blur, BlurModeName::None);
        assert!((settings.opacity - 0.87).abs() < 1e-6);
        assert_eq!(settings.background, Some([0x10, 0x12, 0x16]));
        assert!(settings.panel_resize);
        assert_eq!(settings.sidebar_width, 222.0);
        assert_eq!(settings.ssh_proxy_mode, ProxyModeName::Custom);
        assert_eq!(settings.ssh_proxy_url, "socks5://127.0.0.1:7890");
        assert_eq!(settings.quick_terminal_hotkey, "alt+`");
        // 空值 = 未设置。
        assert_eq!(raw.value("startup_directory"), None);
    }

    #[test]
    fn defaults_when_file_content_is_absent_or_junk() {
        let settings = RuntimeSettings::from_raw(&RawSettings::from_text("theme=NoSuchTheme\n"));
        // 出厂默认逐项对照旧壳 nebula_settings_load。
        assert_eq!(settings.language, LanguagePref::System);
        assert_eq!(settings.theme, ThemeName::Nord);
        assert_eq!(settings.font_family, None);
        assert!(!settings.copy_on_select);
        assert!(settings.multiline_paste_confirm, "多行粘贴确认默认开（上游兼容）");
        assert!(settings.tab_close_visible, "标签关闭按钮默认可见（上游兼容）");
        assert!(settings.powerline);
        assert!(settings.ghost);
        assert_eq!(settings.accept, AcceptKeyName::Both);
        assert_eq!(settings.completion_style, CompletionStyleName::Inline);
        assert!(settings.cjk_bold_regular);
        assert_eq!(settings.tabs_position, TabsPositionName::Sidebar);
        assert_eq!(settings.tab_reveal, TabRevealName::Slide);
        assert_eq!(settings.density, DensityName::Standard);
        assert_eq!(settings.new_tab_position, NewTabPositionName::AfterCurrent);
        assert_eq!(settings.windowing_behavior, WindowingBehaviorName::UseAnyExisting);
        assert_eq!(settings.cell_width_mode, CellWidthModeName::Compact);
        assert!(!settings.fetch);
        assert!(settings.auto_check_updates);
        assert!(!settings.keep_session);
        assert!(settings.restore_session);
        assert!(settings.resume_ai);
        assert!(settings.tray);
        assert_eq!(settings.blur, BlurModeName::Mica);
        assert!((settings.opacity - SYSTEM_MATERIAL_OPACITY).abs() < 1e-6);
        assert_eq!(settings.background, None);
        assert!(!settings.panel_resize);
        assert_eq!(settings.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(settings.ssh_proxy_mode, ProxyModeName::Off);
        assert_eq!(settings.quick_terminal_hotkey, DEFAULT_QUICK_TERMINAL_HOTKEY);
        assert_eq!(settings.bell, BellModeName::Both);
    }

    #[test]
    fn automatic_update_checks_can_be_disabled_without_changing_the_default() {
        assert!(RuntimeSettings::from_raw(&RawSettings::default()).auto_check_updates);
        assert!(
            RuntimeSettings::from_raw(&RawSettings::from_text("auto_check_updates=1\n"))
                .auto_check_updates
        );
        assert!(
            !RuntimeSettings::from_raw(&RawSettings::from_text("auto_check_updates=0\n"))
                .auto_check_updates
        );
    }

    #[test]
    fn bell_mode_parses_known_values_and_rejects_junk() {
        assert_eq!(BellModeName::from_settings("none"), Some(BellModeName::None));
        assert_eq!(BellModeName::from_settings("off"), Some(BellModeName::None));
        assert_eq!(BellModeName::from_settings("visual"), Some(BellModeName::Visual));
        assert_eq!(BellModeName::from_settings("audible"), Some(BellModeName::Audible));
        assert_eq!(BellModeName::from_settings("both"), Some(BellModeName::Both));
        assert_eq!(BellModeName::from_settings("loud"), None);
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("bell=loud\n")).bell,
            BellModeName::Both
        );
    }

    #[test]
    fn blur_mode_parses_mica_alt_and_migrates_legacy_bool() {
        assert_eq!(BlurModeName::from_settings("none"), Some(BlurModeName::None));
        assert_eq!(BlurModeName::from_settings("off"), Some(BlurModeName::None));
        assert_eq!(BlurModeName::from_settings("mica"), Some(BlurModeName::Mica));
        assert_eq!(BlurModeName::from_settings("mica-alt"), Some(BlurModeName::MicaAlt));
        assert_eq!(BlurModeName::from_settings("aero"), Some(BlurModeName::Aero));
        // 未公开 API 的名字（`ACCENT_ENABLE_BLURBEHIND`）也认，手改配置的人
        // 可能照着实现名写。
        assert_eq!(BlurModeName::from_settings("blurbehind"), Some(BlurModeName::Aero));
        assert_eq!(BlurModeName::from_settings("acrylic"), Some(BlurModeName::Acrylic));
        assert_eq!(BlurModeName::from_settings("frosted"), None);
        // 大小写与空白由 from_settings 归一（设置文件是手改的）。
        assert_eq!(BlurModeName::from_settings("  Acrylic "), Some(BlurModeName::Acrylic));
        assert_eq!(BlurModeName::from_settings(" AERO "), Some(BlurModeName::Aero));

        // 旧壳布尔值迁移：关保持关，开落到 Mica（不是最贵的 Acrylic）。
        assert_eq!(BlurModeName::from_settings("0"), Some(BlurModeName::None));
        assert_eq!(BlurModeName::from_settings("false"), Some(BlurModeName::None));
        assert_eq!(BlurModeName::from_settings("1"), Some(BlurModeName::Mica));
        assert_eq!(BlurModeName::from_settings("true"), Some(BlurModeName::Mica));
        assert!(
            (RuntimeSettings::from_raw(&RawSettings::from_text("blur=1\nopacity=1.00\n")).opacity
                - SYSTEM_MATERIAL_OPACITY)
                .abs()
                < 1e-6
        );
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("blur=mica\nopacity=1.00\n")).opacity,
            1.0
        );

        // 认不出的值回落到缺省档，而不是把窗口留在无模糊状态。
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("blur=frosted\n")).blur,
            BlurModeName::Mica
        );
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("blur=acrylic\n")).blur,
            BlurModeName::Acrylic
        );

        // settings_value 与 from_settings 必须互为逆运算，否则设置页存盘后
        // 重开会掉档。
        for mode in [
            BlurModeName::None,
            BlurModeName::Mica,
            BlurModeName::MicaAlt,
            BlurModeName::Aero,
            BlurModeName::Acrylic,
        ] {
            assert_eq!(BlurModeName::from_settings(mode.settings_value()), Some(mode));
        }

        assert!(!BlurModeName::None.enabled());
        assert!(BlurModeName::Mica.enabled());
        assert!(BlurModeName::MicaAlt.enabled());
        assert!(BlurModeName::Aero.enabled());
        assert!(BlurModeName::Acrylic.enabled());
    }

    #[test]
    fn tabs_position_parses_supported_values_and_defaults_to_sidebar() {
        assert_eq!(TabsPositionName::from_settings("sidebar"), Some(TabsPositionName::Sidebar));
        assert_eq!(TabsPositionName::from_settings("TOP"), Some(TabsPositionName::Top));
        assert_eq!(TabsPositionName::from_settings("bottom"), None);
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("tabs_position=bottom\n"))
                .tabs_position,
            TabsPositionName::Sidebar
        );
    }

    #[test]
    fn windowing_behavior_preserves_existing_default_and_accepts_terminal_aliases() {
        assert_eq!(
            WindowingBehaviorName::from_settings("use_new"),
            Some(WindowingBehaviorName::UseNew)
        );
        assert_eq!(
            WindowingBehaviorName::from_settings("useAnyExisting"),
            Some(WindowingBehaviorName::UseAnyExisting)
        );
        assert_eq!(
            WindowingBehaviorName::from_settings("useExisting"),
            Some(WindowingBehaviorName::UseExisting)
        );
        assert_eq!(WindowingBehaviorName::from_settings("reuse_everything"), None);
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::default()).windowing_behavior,
            WindowingBehaviorName::UseAnyExisting
        );
    }

    #[test]
    fn hex_rgb_roundtrip() {
        assert_eq!(parse_hex_rgb("#8bd5ca"), Some([0x8b, 0xd5, 0xca]));
        assert_eq!(parse_hex_rgb("8bd5ca"), Some([0x8b, 0xd5, 0xca]));
        assert_eq!(parse_hex_rgb("#nothex"), None);
        assert_eq!(format_hex_rgb([0x8b, 0xd5, 0xca]), "#8bd5ca");
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
            ThemeName::Nord,
            ThemeName::Paper,
        ] {
            assert_eq!(ThemeName::from_prompt_name(theme.prompt_name()), Some(theme));
        }
    }

    #[test]
    fn light_themes_replace_foreground_dark_themes_do_not() {
        assert!(ThemeName::SilverLight.term_theme().is_light);
        assert!(ThemeName::LimestoneLight.term_theme().is_light);
        assert!(ThemeName::LinenLight.term_theme().is_light);
        assert!(ThemeName::Paper.term_theme().is_light);
        assert!(!ThemeName::Nebula.term_theme().is_light);
        assert!(!ThemeName::SteelDark.term_theme().is_light);
        assert!(!ThemeName::Nord.term_theme().is_light);
        // 浅色主题终端底全白，背景由主题裁定而非用户配色。
        assert_eq!(ThemeName::SilverLight.term_theme().background, [255, 255, 255]);
        assert_eq!(ThemeName::Nebula.term_theme().background, [15, 17, 26]);
    }

    #[test]
    fn nord_and_paper_keep_their_declared_terminal_palettes() {
        let nord = ThemeName::Nord.term_theme().exact.expect("Nord exact colors");
        assert_eq!(nord.foreground, [0xf1, 0xf6, 0xff]);
        assert_eq!(nord.ansi[0], [0x3b, 0x42, 0x52]);
        assert_eq!(nord.ansi[15], [0xec, 0xef, 0xf4]);
        assert_eq!(nord.cursor, Some([0xe5, 0xe9, 0xf0]));
        assert_eq!(nord.cursor_stroke, Some([0x88, 0xc0, 0xd0]));
        assert_eq!(nord.selection_background, Some([0xe5, 0xe9, 0xf0]));

        let paper = ThemeName::Paper.term_theme().exact.expect("Paper exact colors");
        assert_eq!(paper.foreground, [0x1a, 0x1a, 0x1a]);
        assert_eq!(paper.ansi[0], [0x1a, 0x1a, 0x1a]);
        assert_eq!(paper.ansi[15], [0x2f, 0x2e, 0x2e]);
        assert_eq!(paper.cursor, None);
        assert_eq!(paper.selection_background, None);
    }

    #[test]
    fn existing_themes_keep_their_original_terminal_color_contract() {
        for theme in [
            ThemeName::Nebula,
            ThemeName::SilverLight,
            ThemeName::SteelDark,
            ThemeName::LimestoneLight,
            ThemeName::CoalDark,
            ThemeName::LinenLight,
            ThemeName::MossDark,
        ] {
            assert_eq!(theme.term_theme().exact, None, "{}", theme.prompt_name());
        }
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
        let updated =
            apply_updates(text, &[("theme", "SilverLight".into()), ("cursor_blink", "1".into())]);
        assert_eq!(
            updated,
            "language=system\ntheme=SilverLight\nshell=powershell\ncursor_blink=1\n"
        );
    }

    #[test]
    fn keybind_pairs_parse_and_rewrite() {
        let text = "theme=Nebula\nkeybind=ctrl+x:Copy\n# comment\nkeybind=alt+f1:SplitRight\n";
        assert_eq!(
            keybind_pairs_from_text(text),
            vec![
                ("ctrl+x".to_owned(), "Copy".to_owned()),
                ("alt+f1".to_owned(), "SplitRight".to_owned()),
            ]
        );
        let rewritten = apply_keybinds(text, &[("ctrl+y".to_owned(), "Paste".to_owned())]);
        assert_eq!(rewritten, "theme=Nebula\n# comment\nkeybind=ctrl+y:Paste\n");
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
