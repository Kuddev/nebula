//! Static command metadata shared by both shell presentations.

use super::{NebulaTheme, PaletteAction, PaletteItem};

/// Theme commands use the same labels as the theme metadata.
const fn theme_item(theme: NebulaTheme, search: &'static str) -> PaletteItem {
    PaletteItem {
        label: theme.command_label(),
        hint: "",
        search,
        action: PaletteAction::SelectTheme(theme),
    }
}

/// The declaration order breaks equal fuzzy scores and orders empty queries.
pub(super) const ITEMS: &[PaletteItem] = &[
    PaletteItem {
        label: "新建标签页",
        hint: "Ctrl+Shift+T",
        search: "新建标签页 new tab xinjian biaoqianye",
        action: PaletteAction::NewTab,
    },
    PaletteItem {
        label: "复制路径",
        hint: "",
        search: "复制路径 copy path cwd working directory fuzhi lujing gongzuo mulu",
        action: PaletteAction::CopyCwd,
    },
    PaletteItem {
        label: "在资源管理器中显示",
        hint: "",
        search: "在资源管理器中显示 reveal open explorer file manager folder ziyuan guanliqi \
                 wenjianjia",
        action: PaletteAction::RevealCwd,
    },
    PaletteItem {
        label: "在常用目录中新建终端…",
        hint: "",
        search: "在常用目录中新建终端 new terminal in frequent directory changyong mulu",
        action: PaletteAction::OpenDirectoryPicker,
    },
    PaletteItem {
        // GPUI 已把 Tabs、Panes、目录、SSH 与 AI 会话投影进同一个母入口；
        // `OpenAiSessionPicker` 这个旧 action 名暂留给 legacy shell 兼容，避免
        // 为两个壳复制第二份命令目录。
        label: "快速跳转…",
        hint: "Ctrl+Shift+O",
        search: "快速跳转 标签 分屏 目录 SSH AI 会话 open quickly jump tab pane directory ssh resume ai session claude codex kuaisu tiaozhuan biaoqian fenping mulu huifu",
        action: PaletteAction::OpenAiSessionPicker,
    },
    PaletteItem {
        label: "关闭标签页",
        hint: "Ctrl+Shift+W",
        search: "关闭标签页 close tab guanbi",
        action: PaletteAction::CloseTab,
    },
    PaletteItem {
        label: "下一个标签页",
        hint: "Ctrl+Tab",
        search: "下一个标签页 next tab xiayige",
        action: PaletteAction::NextTab,
    },
    PaletteItem {
        label: "上一个标签页",
        hint: "Ctrl+Shift+Tab",
        search: "上一个标签页 previous prev tab shangyige",
        action: PaletteAction::PrevTab,
    },
    PaletteItem {
        label: "新建窗口",
        hint: "Ctrl+Shift+E",
        search: "新建窗口 new window xinjian chuangkou",
        action: PaletteAction::NewWindow,
    },
    PaletteItem {
        label: "左右分屏",
        hint: "Ctrl+Shift+D",
        search: "左右分屏 split right vertical zuoyou fenping",
        action: PaletteAction::SplitRight,
    },
    PaletteItem {
        label: "上下分屏",
        hint: "Ctrl+Shift+S",
        search: "上下分屏 split down horizontal shangxia fenping",
        action: PaletteAction::SplitDown,
    },
    PaletteItem {
        label: "导出工作区…",
        hint: "",
        search: "导出工作区 export workspace save session daochu gongzuoqu",
        action: PaletteAction::ExportWorkspace,
    },
    PaletteItem {
        label: "打开工作区…",
        hint: "",
        search: "打开工作区 open import workspace load session dakai daoru gongzuoqu",
        action: PaletteAction::ImportWorkspace,
    },
    PaletteItem {
        label: "目录树面板",
        hint: "Ctrl+Shift+F",
        search: "目录树面板 files tree explorer panel mulushu wenjian",
        action: PaletteAction::ToggleFilesPanel,
    },
    PaletteItem {
        label: "显示标签侧栏",
        hint: "",
        search: "显示标签侧栏 toggle show hide tab sidebar xianshi biaoqian celan",
        action: PaletteAction::ToggleSidebar,
    },
    PaletteItem {
        label: "拖拽调节侧栏宽度",
        hint: "",
        search: "拖拽调节侧栏宽度 drag resize sidebar drawer panel width tuozhuai tiaojie kuandu",
        action: PaletteAction::TogglePanelResize,
    },
    PaletteItem {
        label: "Git 面板",
        hint: "Ctrl+Shift+G",
        search: "git 面板 status branch panel mianban",
        action: PaletteAction::ToggleGitPanel,
    },
    PaletteItem {
        label: "打开设置",
        hint: "",
        search: "打开设置 open settings preferences dakai shezhi",
        action: PaletteAction::OpenSettings,
    },
    PaletteItem {
        label: "打开配置文件",
        hint: "",
        search: "打开配置文件 open config file dakai peizhi wenjian",
        action: PaletteAction::OpenSettingsFile,
    },
    PaletteItem {
        label: "同步：推送设置到云端",
        hint: "",
        search: "同步推送设置到云端 webdav sync push upload settings tongbu tuisong",
        action: PaletteAction::SyncPush,
    },
    PaletteItem {
        label: "同步：从云端拉取设置",
        hint: "",
        search: "同步从云端拉取设置 webdav sync pull download settings tongbu laqu",
        action: PaletteAction::SyncPull,
    },
    PaletteItem {
        label: "切换行内补全 (Ghost)",
        hint: "",
        search: "切换行内补全 toggle ghost completion qiehuan buquan",
        action: PaletteAction::ToggleGhost,
    },
    PaletteItem {
        label: "切换补全接受键",
        hint: "",
        search: "切换补全接受键 cycle accept key completion jieshou",
        action: PaletteAction::CycleAccept,
    },
    PaletteItem {
        label: "切换补全样式（行内 / 弹窗）",
        hint: "",
        search: "切换补全样式 行内 弹窗 completion style inline popup list buquan yangshi",
        action: PaletteAction::CycleCompletionStyle,
    },
    PaletteItem {
        label: "选择背景图片…",
        hint: "",
        search: "选择背景图片 background image picture xuanze beijing tupian",
        action: PaletteAction::PickBackgroundImage,
    },
    PaletteItem {
        label: "切换背景色",
        hint: "",
        search: "切换背景色 cycle background color qiehuan beijingse",
        action: PaletteAction::CycleBackground,
    },
    PaletteItem {
        label: "恢复外观默认",
        hint: "",
        search: "恢复外观默认 reset appearance default huifu waiguan moren",
        action: PaletteAction::ResetAppearance,
    },
    theme_item(NebulaTheme::BreezeLight, "theme 主题 breeze light"),
    theme_item(NebulaTheme::BreezeDark, "theme 主题 breeze dark"),
    theme_item(NebulaTheme::MintLight, "theme 主题 mint light"),
    theme_item(NebulaTheme::MintDark, "theme 主题 mint dark"),
    theme_item(NebulaTheme::SilverLight, "theme 主题 silver light"),
    theme_item(NebulaTheme::Nord, "theme 主题 nord dark"),
    theme_item(NebulaTheme::Paper, "theme 主题 paper light"),
    theme_item(NebulaTheme::LimestoneLight, "theme 主题 limestone light"),
    theme_item(NebulaTheme::LinenLight, "theme 主题 linen light"),
    theme_item(NebulaTheme::CatppuccinMocha, "theme 主题 catppuccin mocha dark"),
    theme_item(NebulaTheme::CatppuccinLatte, "theme 主题 catppuccin latte light"),
    theme_item(NebulaTheme::CatppuccinFrappe, "theme 主题 catppuccin frappe frappé dark"),
    theme_item(NebulaTheme::CatppuccinMacchiato, "theme 主题 catppuccin macchiato dark"),
    theme_item(NebulaTheme::GlassLight, "theme 主题 glass light"),
    theme_item(NebulaTheme::GlassDark, "theme 主题 glass dark"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_commands_match_the_selectable_catalog() {
        let themes: Vec<_> = ITEMS
            .iter()
            .filter_map(|item| match item.action {
                PaletteAction::SelectTheme(theme) => Some(theme.prompt_name()),
                _ => None,
            })
            .collect();
        assert_eq!(themes, nebula_settings::ThemeName::BUILTIN_NAMES);
    }
}
