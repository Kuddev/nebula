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
        let is_keybind = line
            .split_once('=')
            .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("keybind"));
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
    /// 系统外观变化时自动在主题家族的深浅成员间切换（默认关，尊重显式选择）。
    pub follow_system_theme: bool,
    pub font_family: Option<String>,
    /// **逻辑像素**（旧壳写盘语义：设置页 spinner 与 Ctrl+滚轮缩放持久化时
    /// 已除以 scale factor）。`None` = 跟随 nebula.toml 的 `font.size`（pt）。
    pub font_size_px: Option<f32>,
    pub cursor_shape: Option<CursorShapeName>,
    pub cursor_blink: Option<bool>,
    pub copy_on_select: bool,
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
    pub tab_reveal: TabRevealName,
    pub density: DensityName,
    pub new_tab_position: NewTabPositionName,
    pub cell_width_mode: CellWidthModeName,
    /// 新会话欢迎屏 fastfetch（默认关：启动速度优先于观感，旧壳裁定）。
    pub fetch: bool,
    pub keep_session: bool,
    pub restore_session: bool,
    pub resume_ai: bool,
    /// 常驻系统托盘图标。
    pub tray: bool,
    pub blur: bool,
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
}

/// 旧壳默认快速终端热键。
pub const DEFAULT_QUICK_TERMINAL_HOTKEY: &str = "ctrl+`";
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 230.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 170.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 420.0;

impl RuntimeSettings {
    pub fn load() -> Self {
        Self::from_raw(&RawSettings::load())
    }

    pub fn from_raw(raw: &RawSettings) -> Self {
        Self {
            language: raw
                .value("language")
                .and_then(LanguagePref::from_settings)
                .unwrap_or_default(),
            theme: raw.value("theme").and_then(ThemeName::from_prompt_name).unwrap_or_default(),
            follow_system_theme: raw.bool_on("follow_system_theme").unwrap_or(false),
            font_family: raw.value("font_family").map(str::to_owned),
            font_size_px: raw.f32("font_size").map(|size| size.clamp(4.0, 96.0)),
            cursor_shape: raw.value("cursor_shape").and_then(CursorShapeName::from_settings),
            cursor_blink: raw.bool_on("cursor_blink"),
            copy_on_select: raw.bool_on("copy_on_select").unwrap_or(false),
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
            tab_reveal: raw
                .value("tab_reveal")
                .and_then(TabRevealName::from_settings)
                .unwrap_or_default(),
            density: raw.value("density").and_then(DensityName::from_settings).unwrap_or_default(),
            new_tab_position: raw
                .value("new_tab_position")
                .and_then(NewTabPositionName::from_settings)
                .unwrap_or_default(),
            cell_width_mode: raw
                .value("cell_width_mode")
                .and_then(CellWidthModeName::from_settings)
                .unwrap_or_default(),
            fetch: raw.bool_on("fetch").unwrap_or(false),
            keep_session: raw.bool_on("keep_session").unwrap_or(false),
            restore_session: raw.bool_on("restore_session").unwrap_or(true),
            resume_ai: raw.bool_on("resume_ai").unwrap_or(true),
            tray: raw.bool_on("tray").unwrap_or(true),
            blur: raw.bool_on("blur").unwrap_or(true),
            opacity: raw.f32("opacity").map(|o| o.clamp(0.0, 1.0)).unwrap_or(1.0),
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
            background_image_alignment: raw
                .value("background_image_alignment")
                .map(str::to_owned),
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
             cjk_bold_regular=1\n\
             tab_reveal=instant\n\
             density=compact\n\
             new_tab_position=end\n\
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
        assert!(!settings.powerline);
        assert_eq!(settings.shell.as_deref(), Some("pwsh"));
        assert_eq!(settings.accept, AcceptKeyName::Tab);
        assert_eq!(settings.completion_style, CompletionStyleName::Popup);
        assert_eq!(settings.tab_reveal, TabRevealName::Instant);
        assert_eq!(settings.density, DensityName::Compact);
        assert_eq!(settings.new_tab_position, NewTabPositionName::End);
        assert_eq!(settings.cell_width_mode, CellWidthModeName::Relaxed);
        assert!(settings.fetch);
        assert!(settings.keep_session);
        assert!(!settings.restore_session);
        assert!(!settings.resume_ai);
        assert!(!settings.tray);
        assert!(!settings.blur);
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
        assert_eq!(settings.theme, ThemeName::Nebula);
        assert_eq!(settings.font_family, None);
        assert!(!settings.copy_on_select);
        assert!(settings.powerline);
        assert!(settings.ghost);
        assert_eq!(settings.accept, AcceptKeyName::Both);
        assert_eq!(settings.completion_style, CompletionStyleName::Inline);
        assert!(settings.cjk_bold_regular);
        assert_eq!(settings.tab_reveal, TabRevealName::Slide);
        assert_eq!(settings.density, DensityName::Standard);
        assert_eq!(settings.new_tab_position, NewTabPositionName::AfterCurrent);
        assert_eq!(settings.cell_width_mode, CellWidthModeName::Compact);
        assert!(!settings.fetch);
        assert!(!settings.keep_session);
        assert!(settings.restore_session);
        assert!(settings.resume_ai);
        assert!(settings.tray);
        assert!(settings.blur);
        assert!((settings.opacity - 1.0).abs() < 1e-6);
        assert_eq!(settings.background, None);
        assert!(!settings.panel_resize);
        assert_eq!(settings.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(settings.ssh_proxy_mode, ProxyModeName::Off);
        assert_eq!(settings.quick_terminal_hotkey, DEFAULT_QUICK_TERMINAL_HOTKEY);
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
        assert_eq!(
            rewritten,
            "theme=Nebula\n# comment\nkeybind=ctrl+y:Paste\n"
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
