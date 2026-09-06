use super::*;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
pub(super) const THEME_VALUES: [&str; 9] = [
    "Nebula",
    "SilverLight",
    "SteelDark",
    "LimestoneLight",
    "CoalDark",
    "LinenLight",
    "MossDark",
    "Nord",
    "Paper",
];

pub(super) const REPOSITORY_URL: &str = "https://github.com/Kuddev/nebula";
pub(super) const BUG_REPORT_TEMPLATE: &str = "bug_report.yml";

/// 左侧分区的稳定路由表。2026-08-28 产品裁定：默认 GPUI 导航收敛为常用项，
/// 暂时隐藏“AI 供应商”和“备份”；页面实现与索引继续保留。后续恢复入口时只改
/// [`HIDDEN_NAV_SECTIONS`]，不得删除或重排这里的条目。
pub(super) const SECTION_IDS: [&str; 10] = [
    "application",
    "appearance",
    "profiles",
    "providers",
    "ssh",
    "network",
    "interaction",
    "keymap",
    "advanced",
    "backup",
];

/// Bilingual search aliases for the stable section routes. Search is a route
/// finder, so a query such as "font", "opacity", or "更新" lands on the
/// section that owns the control instead of merely filtering the current page.
pub(super) const SECTION_SEARCH_TERMS: [&str; 10] = [
    "application app 应用 update 更新 version 版本 github support 支持",
    "appearance 外观 theme 主题 font 字体 opacity 透明度 background 背景 cursor 光标 icon 图标",
    "profiles 配置文件 shell terminal 终端 completion 补全 startup 启动",
    "providers provider ai 供应商 模型 api",
    "ssh host 主机 remote 远程 connection 连接",
    "network 网络 proxy 代理 connectivity 连接",
    "interaction 交互 copy 复制 paste 粘贴 tab 标签 panel 面板",
    "keymap key binding shortcut 按键映射 快捷键",
    "advanced 高级 session 会话 tray 托盘 restore 恢复",
    "backup 备份 export 导出 restore 恢复",
];

pub(super) const HIDDEN_NAV_SECTIONS: &[usize] = &[3, 9];

/// 保留原来的分组展开顺序，组名不再渲染；数组里仍保存稳定的 [`SECTION_IDS`]
/// 下标，不复制设置状态或路由。
pub(super) const NAV_GROUPS: [(&str, &[usize]); 3] =
    [("workspace", &[0, 1, 2, 6, 7]), ("connections", &[3, 4, 5]), ("system", &[8, 9])];

pub(super) fn section_label(index: usize, language: crate::display::UiLanguage) -> &'static str {
    match SECTION_IDS.get(index).copied() {
        Some("application") => language.tr("settings.sidebar.application"),
        Some("appearance") => language.tr("settings.sidebar.appearance"),
        Some("profiles") => language.tr("settings.sidebar.profiles"),
        Some("providers") => language.tr("settings.sidebar.providers"),
        Some("ssh") => language.tr("settings.sidebar.ssh"),
        Some("network") => language.tr("settings.sidebar.network"),
        Some("interaction") => language.tr("settings.sidebar.interaction"),
        Some("keymap") => language.tr("settings.sidebar.keymap"),
        Some("advanced") => language.tr("settings.sidebar.advanced"),
        Some("backup") => language.tr("settings.sidebar.backup"),
        _ => "",
    }
}

pub(super) fn is_nav_section_visible(index: usize) -> bool {
    !HIDDEN_NAV_SECTIONS.contains(&index)
}

pub(super) fn visible_nav_sections() -> impl Iterator<Item = usize> {
    NAV_GROUPS
        .iter()
        .flat_map(|(_, sections)| sections.iter().copied())
        .filter(|index| is_nav_section_visible(*index))
}

// 导航使用组件的小字号，与搜索菜单一致；尺寸是逻辑像素，由窗口统一处理 DPI。
pub(super) const SETTINGS_NAV_WIDTH: f32 = 208.0;
pub(super) const SETTINGS_NAV_ROW_HEIGHT: f32 = 34.0;
pub(super) const SETTINGS_NAV_ICON_SIZE: f32 = 16.0;
pub(super) const SETTINGS_HEADER_HEIGHT: f32 = 74.0;

// 正文分组与表单继续沿用现有几何节奏。
pub(super) const SETTINGS_GROUP_GAP: f32 = 32.0;
pub(super) const SETTINGS_GROUP_TITLE_HEIGHT: f32 = 26.0;
pub(super) const SETTINGS_GROUP_TITLE_GAP: f32 = 16.0;
pub(super) const SETTINGS_ROW_HEIGHT: f32 = 48.0;
pub(super) const SETTINGS_ROW_GAP: f32 = 8.0;
/// 标准设置选择器的实际宽度。字体输入与 Select 共用，避免同列控件漂移。
pub(super) const SETTINGS_SELECT_WIDTH: f32 = 220.0;

/// 列表/触发条上的展示名：族名偶尔带着导入文件后缀，界面上剥掉。
pub(super) fn font_display_name(name: &str) -> String {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    for ext in [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return trimmed[..stem.len()].to_owned();
        }
    }
    trimmed.to_owned()
}

/// 导航图标。
///
/// 统一走路径字符串而不是 `IconName`：绝大多数取 lucide 的现成图标，但
/// 「按键映射」在 lucide 里没有对应项（原来错挂成 `ALargeSmall`，那是字号
/// 图标），只能自带一枚——两种来源用同一个类型表达，调用点就不必分叉。
pub(super) fn section_icon(index: usize) -> SharedString {
    use gpui_component::IconNamed as _;
    match index {
        0 => crate::gpui_shell::assets::nav::LAYOUT_GRID.into(),
        1 => IconName::Palette.path(),
        2 => IconName::Folder.path(),
        3 => IconName::Bot.path(),
        4 => IconName::SquareTerminal.path(),
        5 => IconName::Globe.path(),
        6 => crate::gpui_shell::assets::nav::MOUSE_POINTER.into(),
        7 => crate::gpui_shell::assets::nav::KEYMAP.into(),
        8 => crate::gpui_shell::assets::nav::SLIDERS.into(),
        _ => IconName::Inbox.path(),
    }
}

pub(super) fn chrome_theme(theme: ThemeName) -> crate::display::NebulaTheme {
    use crate::display::NebulaTheme;
    match theme {
        ThemeName::Nebula => NebulaTheme::Nebula,
        ThemeName::SilverLight => NebulaTheme::SilverLight,
        ThemeName::SteelDark => NebulaTheme::SteelDark,
        ThemeName::LimestoneLight => NebulaTheme::LimestoneLight,
        ThemeName::CoalDark => NebulaTheme::CoalDark,
        ThemeName::LinenLight => NebulaTheme::LinenLight,
        ThemeName::MossDark => NebulaTheme::MossDark,
        ThemeName::Nord => NebulaTheme::Nord,
        ThemeName::Paper => NebulaTheme::Paper,
    }
}

pub(super) fn rgb_hsla(r: u8, g: u8, b: u8) -> Hsla {
    GpuiRgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }
        .into()
}

pub(super) fn query_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

/// GitHub issue form 预填合同：模板、版本、平台、安装来源
/// 和诊断摘要都来自当前运行实例，用户只需补充复现步骤与实际表现。
pub(super) fn issue_url() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let platform = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        _ => "Other",
    };
    let install_source = if cfg!(debug_assertions) {
        "Built from source (cargo build / cargo run)"
    } else {
        "GitHub Release (.msi / .exe / portable archive)"
    };
    let build = if cfg!(debug_assertions) { "debug" } else { "release" };
    let logs = format!(
        "Reported from {} Settings ({} {version}).\n\nPlatform: {} {}\nBuild: {build}",
        crate::brand::NAME,
        crate::brand::NAME,
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let params = [
        ("template", BUG_REPORT_TEMPLATE),
        ("title", "[Bug] "),
        ("version", version),
        ("platform", platform),
        ("install_source", install_source),
        ("logs", logs.as_str()),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", query_component(value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{REPOSITORY_URL}/issues/new?{params}")
}
