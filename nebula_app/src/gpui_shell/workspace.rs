//! Nebula 主工作区：左侧垂直 Tab 侧边栏 + 主内容区（终端 / 设置页）。
//!
//! 布局对齐 nebula_app 的产品形态（TABS 侧边栏、每项图标 + 标题 + 关闭、
//! 激活高亮、主区圆角卡片）。设置页与旧壳同形态：一个单例特殊 tab。
//! 终端实例由本视图直接持有；会话清理走显式 `shutdown` +
//! `TerminalView::drop` 兜底。
//!
//! ## 分屏 pane 生命周期合同
//!
//! - 每个 Terminal tab 持有 `panes`（实体属主）+ `tree`（`nebula_split`
//!   布局树，叶 = pane id）+ `focused`。不变式：panes 的 id 集合 == 树的
//!   叶集合。
//! - 关一个 pane：`tree.remove_leaf` 裁定结局——`WasRoot` 关整个 tab；
//!   `Collapsed(id)` 由兄弟子树收编空间、焦点交给其首叶。被摘 pane 立即
//!   显式 `shutdown`，实体随 `panes` 移除而释放，`Drop` 兜底。
//! - 关整个 tab（侧栏 ×、最后 pane 退出）：逐 pane `shutdown`。
//! - PTY 尺寸：pane 矩形由布局树裁定，`TerminalElement` prepaint 回写
//!   `set_layout`，resize 合并/提交合同（burst + settle）原样生效——分屏
//!   拖拽期间 PTY 跟随提交比例，松手后一次落定，与旧壳语义一致。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::{path::Path, time::Duration};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Bounds, ClipboardItem, Context, Entity,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, ObjectFit, ParentElement as _, Pixels, Render, RenderImage,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription,
    Window, canvas, div, ease_out_quint, img, px, relative, size,
};
use image::Frame;

use crate::display::color::Rgb;
use crate::gpui_shell::prelude::*;
use crate::gpui_shell::settings_pane::{SettingsPane, SettingsPaneEvent};
use crate::gpui_shell::terminal::view::{SidebarActivity, TerminalView, TerminalViewEvent};
use gpui_component::Root;
use gpui_component::input::InputEvent;
use nebula_split::{DIVIDER_GAP, HIT_SLOP, RemoveOutcome, SplitDirection, SplitNav, SplitTree};

mod file_tree;
mod key_actions;
mod residency;
mod tab_drag;
mod tab_menu;
mod tab_scroll;
mod top_tabs;

use tab_drag::{TabDrag, TabDragAxis};

gpui::actions!(
    nebula_workspace,
    [
        NewTerminal,
        CloseActiveTerminal,
        ToggleSidebar,
        OpenSettings,
        ToggleCommandPalette,
        CloseCommandPalette,
        ToggleShellPicker,
        CommandPaletteUp,
        CommandPaletteDown,
        ToggleFileTree,
        ToggleGitPanel,
        SplitRight,
        SplitDown,
        RenameActiveTab,
        ToggleZoom,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        SelectNextTab,
        SelectPreviousTab,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        CopySelection,
        PasteClipboard,
        ToggleFullscreen,
        OpenQuickJump
    ]
);

/// 命令面板 / Shell 选择器罩层的 keymap context。Esc 必须挂在这里，
/// 不能挂全局：否则 CC/Codex 的终止对话键到不了 PTY。
const PALETTE_KEY_CONTEXT: &str = "NebulaCommandPalette";

/// 工作区静态默认绑定的 combo 集（[`init`] 的镜像）。撤销已失效的自定义
/// 注入时要排除：gpui 的 NoAction 打在静态默认键上会误杀基础功能。
const STATIC_DEFAULT_COMBOS: &[&str] = &[
    "ctrl-shift-t",
    "ctrl-shift-w",
    "ctrl-shift-b",
    "ctrl-,",
    "ctrl-shift-p",
    "ctrl-k",
    "ctrl-shift-f",
    "escape",
    "ctrl-shift-d",
    "ctrl-shift-s",
    "f2",
    "ctrl-shift-enter",
    "ctrl-alt-left",
    "ctrl-alt-right",
    "ctrl-alt-up",
    "ctrl-alt-down",
    "ctrl-tab",
    "ctrl-shift-tab",
    "ctrl-shift-g",
    "ctrl-=",
    "ctrl-+",
    "ctrl--",
    "ctrl-0",
    "ctrl-shift-c",
    "ctrl-shift-v",
    "alt-enter",
    "ctrl-shift-o",
];

/// `keybind=` 自定义表（两壳共读）中 config::Action → GPUI 工作区动作的
/// 映射。仍跳过未接线的动作（prompt 跳转、搜索、新建窗口）：编辑器可读写，
/// 这里不注入。`CreateNewWindow` 旧壳是同进程 `CreateWindow`，不是再 spawn
/// 一份 `nebula --gpui`；GPUI `run_shell` 单窗（一份 AI hook + 会话保存）。
fn custom_workspace_binding(combo: &str, action: &crate::config::Action) -> Option<KeyBinding> {
    use crate::config::Action;
    let combo = gpui_binding_combo(combo);
    match action {
        Action::ToggleCommandPalette => Some(KeyBinding::new(&combo, ToggleCommandPalette, None)),
        Action::ToggleShellPicker => Some(KeyBinding::new(&combo, ToggleShellPicker, None)),
        Action::CreateNewTab => Some(KeyBinding::new(&combo, NewTerminal, None)),
        Action::CloseTab => Some(KeyBinding::new(&combo, CloseActiveTerminal, None)),
        Action::ToggleFilesPanel => Some(KeyBinding::new(&combo, ToggleFileTree, None)),
        Action::ToggleGitPanel => Some(KeyBinding::new(&combo, ToggleGitPanel, None)),
        Action::SplitRight => Some(KeyBinding::new(&combo, SplitRight, None)),
        Action::SplitDown => Some(KeyBinding::new(&combo, SplitDown, None)),
        Action::ToggleZoom => Some(KeyBinding::new(&combo, ToggleZoom, None)),
        Action::FocusPaneLeft => Some(KeyBinding::new(&combo, FocusPaneLeft, None)),
        Action::FocusPaneRight => Some(KeyBinding::new(&combo, FocusPaneRight, None)),
        Action::FocusPaneUp => Some(KeyBinding::new(&combo, FocusPaneUp, None)),
        Action::FocusPaneDown => Some(KeyBinding::new(&combo, FocusPaneDown, None)),
        Action::SelectNextTab => Some(KeyBinding::new(&combo, SelectNextTab, None)),
        Action::SelectPreviousTab => Some(KeyBinding::new(&combo, SelectPreviousTab, None)),
        Action::IncreaseFontSize => Some(KeyBinding::new(&combo, IncreaseFontSize, None)),
        Action::DecreaseFontSize => Some(KeyBinding::new(&combo, DecreaseFontSize, None)),
        Action::ResetFontSize => Some(KeyBinding::new(&combo, ResetFontSize, None)),
        Action::Copy => Some(KeyBinding::new(&combo, CopySelection, None)),
        Action::Paste => Some(KeyBinding::new(&combo, PasteClipboard, None)),
        Action::ToggleFullscreen => Some(KeyBinding::new(&combo, ToggleFullscreen, None)),
        Action::OpenQuickJump => Some(KeyBinding::new(&combo, OpenQuickJump, None)),
        // `none` 禁用键：gpui 的 NoAction 绑定在最高优先级命中时吞掉按键，
        // 与旧壳 keybind=combo:none 的语义一致。
        Action::None => Some(KeyBinding::new(&combo, gpui::NoAction, None)),
        _ => None,
    }
}

/// 存储格式 combo（`ctrl+shift+t`）→ gpui 绑定串（`ctrl-shift-t`）。键名
/// 两套体系同构（小写命名键 + 单字符）；digitN 折回数字，plus/minus 折回
/// `+`/`-`（`+` 是存储分隔符，必须先占位再替换）。
fn gpui_binding_combo(combo: &str) -> String {
    combo
        .replace("plus", "\u{1}")
        .replace("minus", "\u{2}")
        .replace('+', "-")
        .replace("digit", "")
        .replace('\u{1}', "+")
        .replace('\u{2}', "-")
}

/// 注册工作区快捷键；在 `gpui_component::init` 之后调用一次。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-w", CloseActiveTerminal, None),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, None),
        KeyBinding::new("ctrl-k", ToggleShellPicker, None),
        KeyBinding::new("ctrl-shift-f", ToggleFileTree, None),
        // Esc 只在命令/Shell 面板打开时关面板。绑成 `None` 会在终端聚焦时
        // 抢走按键，CC/Codex 收不到 0x1b（旧壳无 overlay 时 Esc 一定进 PTY）。
        KeyBinding::new("escape", CloseCommandPalette, Some(PALETTE_KEY_CONTEXT)),
        // 分屏（旧壳 nebula_key_bindings 同键位）：ctrl+shift+d 左右、
        // ctrl+shift+s 上下、ctrl+shift+enter 缩放、ctrl+alt+方向切聚焦。
        KeyBinding::new("ctrl-shift-d", SplitRight, None),
        KeyBinding::new("ctrl-shift-s", SplitDown, None),
        // F2 重命名活动标签（旧壳同键位）；右键菜单的键帽读的就是这条。
        KeyBinding::new("f2", RenameActiveTab, None),
        KeyBinding::new("ctrl-shift-enter", ToggleZoom, None),
        KeyBinding::new("ctrl-alt-left", FocusPaneLeft, None),
        KeyBinding::new("ctrl-alt-right", FocusPaneRight, None),
        KeyBinding::new("ctrl-alt-up", FocusPaneUp, None),
        KeyBinding::new("ctrl-alt-down", FocusPaneDown, None),
        KeyBinding::new("ctrl-tab", SelectNextTab, None),
        KeyBinding::new("ctrl-shift-tab", SelectPreviousTab, None),
        KeyBinding::new("ctrl-shift-g", ToggleGitPanel, None),
        KeyBinding::new("ctrl-=", IncreaseFontSize, None),
        KeyBinding::new("ctrl-+", IncreaseFontSize, None),
        KeyBinding::new("ctrl--", DecreaseFontSize, None),
        KeyBinding::new("ctrl-0", ResetFontSize, None),
        KeyBinding::new("ctrl-shift-c", CopySelection, None),
        KeyBinding::new("ctrl-shift-v", PasteClipboard, None),
        KeyBinding::new("alt-enter", ToggleFullscreen, None),
        KeyBinding::new("ctrl-shift-o", OpenQuickJump, None),
    ]);
}

/// 一个终端 pane：视图实体 + 宿主订阅。id 即 `TerminalView::pane_id`
/// （AI hook 的 `NEBULA_PANE_ID` 同源），全工作区唯一、终生不复用。
struct TerminalPane {
    id: u64,
    view: Entity<TerminalView>,
    _subscription: Subscription,
}

enum WorkspaceTab {
    Terminal {
        /// pane 实体属主（无序存储，按 id 查找）。见模块头的生命周期合同。
        panes: Vec<TerminalPane>,
        /// 分屏布局树（`nebula_split` 共享权威实现），叶 = pane id。
        tree: SplitTree<u64>,
        /// 聚焦 pane：键盘输入焦点与 split/close 动作的作用对象。
        focused: u64,
        /// 缩放：聚焦 pane 临时满卡（ctrl+shift+enter，旧壳 ToggleZoom）；
        /// 任何结构性操作（split/close/导航/点击别的 pane）都先解除。
        zoomed: bool,
    },
    Settings {
        view: Entity<SettingsPane>,
        _subscription: Subscription,
    },
    /// 只读图片查看 tab（文件树双击图片进入；旧壳 open_image_tab 同形态）。
    Image {
        view: Entity<crate::gpui_shell::doc_tabs::ImageTabView>,
    },
    /// Markdown/文本文档 tab（文件树双击可读文本进入；旧壳 doc tab 同形态）。
    Document {
        view: Entity<crate::gpui_shell::doc_tabs::DocTabView>,
    },
    /// 源码查看 tab（tree-sitter 高亮 + 行级虚拟化，只读）。
    Code {
        view: Entity<crate::gpui_shell::code_tab::CodeTabView>,
    },
}

impl WorkspaceTab {
    fn is_settings(&self) -> bool {
        matches!(self, Self::Settings { .. })
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }

    /// 聚焦 pane 的视图（Terminal tab）；其余 tab 类型返回 None。
    fn focused_view(&self) -> Option<&Entity<TerminalView>> {
        match self {
            Self::Terminal { panes, focused, .. } => {
                panes.iter().find(|pane| pane.id == *focused).map(|pane| &pane.view)
            },
            _ => None,
        }
    }
}

/// 进行中的分隔条拖拽：目标 Split 节点以树路径寻址（`nebula_split::Divider`
/// 同语义）。指针→比例的换算依赖 [`NebulaWorkspace::split_bounds`] 记录的
/// 该节点上一帧视口矩形。
struct SplitDrag {
    tab: usize,
    path: Vec<bool>,
    direction: SplitDirection,
}

/// Split 视口 / pane 矩形的帧记录（canvas prepaint 回写，`Rc<RefCell>`
/// 避免 paint 阶段反向 update 实体）。键：(tab 下标, 节点路径)。
type SplitBoundsStore = Rc<RefCell<HashMap<(usize, Vec<bool>), Bounds<Pixels>>>>;
/// 键：pane id（方向导航要拿所有叶子的屏幕矩形算最近邻）。
type PaneBoundsStore = Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>;

fn decode_sidebar_logo(
    bytes: &[u8],
    tint: Option<([u8; 3], bool)>,
    target_size: u32,
) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
    if let Some((ink, preserve_luma)) = tint {
        for pixel in rgba.chunks_exact_mut(4) {
            let luma = if preserve_luma { u16::from(pixel[0]) } else { 255 };
            pixel[0] = (u16::from(ink[0]) * luma / 255) as u8;
            pixel[1] = (u16::from(ink[1]) * luma / 255) as u8;
            pixel[2] = (u16::from(ink[2]) * luma / 255) as u8;
        }
    }
    // 直接复用旧壳的 Lanczos3 物理像素预缩放与 alpha 质量中心校正。
    // 先 tint 再缩放，避免 1024px 原图在 GPUI paint 阶段临时压到十几个
    // 逻辑像素时产生灰边、锯齿与非整数 DPI 采样。
    let (prepared, width, height) = crate::display::prepare_ai_logo_texture(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
        target_size,
    );
    let mut rgba = image::RgbaImage::from_raw(width, height, prepared)?;
    // GPUI 的原始帧使用 BGRA；与壁纸解码走同一通道转换。
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([Frame::new(rgba)])))
}

fn sidebar_logo_images(
    target_size: u32,
) -> HashMap<(crate::display::AiLogo, bool), Arc<RenderImage>> {
    use crate::display::AiLogo;

    let mut images = HashMap::new();
    if let Some(image) =
        decode_sidebar_logo(include_bytes!("../../../extra/logo/ai_claude.png"), None, target_size)
    {
        images.insert((AiLogo::Claude, true), image.clone());
        images.insert((AiLogo::Claude, false), image);
    }
    for (logo, bytes, preserve_luma) in [
        (AiLogo::OpenAi, include_bytes!("../../../extra/logo/ai_openai.png").as_slice(), false),
        (AiLogo::OpenCode, include_bytes!("../../../extra/logo/ai_opencode.png").as_slice(), true),
        (AiLogo::Pi, include_bytes!("../../../extra/logo/ai_pi.png").as_slice(), false),
    ] {
        if let Some(image) =
            decode_sidebar_logo(bytes, Some(([236, 239, 245], preserve_luma)), target_size)
        {
            images.insert((logo, true), image);
        }
        if let Some(image) =
            decode_sidebar_logo(bytes, Some(([35, 40, 50], preserve_luma)), target_size)
        {
            images.insert((logo, false), image);
        }
    }
    if let Some(image) = decode_sidebar_logo(
        include_bytes!("../../../extra/logo/ai_grok_light.png"),
        None,
        target_size,
    ) {
        images.insert((AiLogo::Grok, true), image);
    }
    if let Some(image) = decode_sidebar_logo(
        include_bytes!("../../../extra/logo/ai_grok_dark.png"),
        None,
        target_size,
    ) {
        images.insert((AiLogo::Grok, false), image);
    }
    images
}

/// GPUI `Bounds` → `nebula_split::Rect`（同为窗口逻辑像素坐标系）。
fn to_split_rect(bounds: &Bounds<Pixels>) -> nebula_split::Rect {
    nebula_split::Rect::new(
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

/// 纯树手术（dock 的核心）：把 `source` 整树挂到 `target` 树的 `nav` 侧，
/// 根级 50/50 分割（旧壳 `dock_tab_into_active` 的布局公式）。
fn dock_tree(target: SplitTree<u64>, source: SplitTree<u64>, nav: SplitNav) -> SplitTree<u64> {
    let (direction, src_first) = match nav {
        SplitNav::Left => (SplitDirection::LeftRight, true),
        SplitNav::Right => (SplitDirection::LeftRight, false),
        SplitNav::Up => (SplitDirection::TopBottom, true),
        SplitNav::Down => (SplitDirection::TopBottom, false),
    };
    let (first, second) = if src_first { (source, target) } else { (target, source) };
    SplitTree::Split {
        direction,
        ratio: 0.5,
        preview_ratio: None,
        dragging: false,
        first: Box::new(first),
        second: Box::new(second),
    }
}

/// 侧栏 tab 行高与行距（与 `render_sidebar` 的 `h(px(TAB_ROW_H))`、
/// `gap_2`(8px) 同源）；受约束拖拽按此步距换算让位槽位。
pub(super) const TAB_ROW_H: f32 = 34.0;
pub(super) const TAB_ROW_PITCH: f32 = TAB_ROW_H + 8.0;
const SIDE_PANEL_SLOT_W: f32 = 328.0;

/// 侧栏 tab 行的关闭按钮边长。旧壳 `chrome_tab_layout` 取
/// `max(row_h * 0.58, 16)`；`Button::xsmall()` 的 `size_5` 恰好落在这个数上，
/// 所以两个壳的 × 命中区同尺寸。垂直位置**必须**由 flex 居中给出：曾用
/// `absolute().top(px(3.0))` 硬写，34px 行里整枚按钮偏上 4px（用户报的
/// "删除按钮偏移了"）。
const TAB_CLOSE_SIZE: f32 = 20.0;

/// TABS 标题行右侧 `+` / `⋯`：命中区对照旧壳 `chrome.rs` 的 `plus_sz = 20`，
/// 中间 `s(2)`。旧壳三点只占 15 宽，是因为它画的是瘦 chevron；竖三点要
/// 方格才不被裁。Lucide viewBox 有内边距，图标铺满 20px 命中区，笔画才
/// 接近旧壳 `push_add` 的 6px 臂 / `push_more` 的 2.8px 点；`Button::xsmall()`
/// 里图标只有 12px，看起来会小一圈。
const SIDEBAR_PLUS_SIZE: f32 = 20.0;
const SIDEBAR_MENU_W: f32 = 20.0;
const SIDEBAR_HEADER_ICON: f32 = 18.0;

/// 侧栏文字的字号档位，**乘在终端字号上**（旧壳 chrome 同源）：标签行走
/// BODY(1.0)——旧壳 `draw_ui_text_tracked(.., 1.0, ..)`，分组标题与右侧
/// shell 短标各压一档。硬编码 `text_sm`(14px) 时侧栏比旧壳整体小一号，
/// 且用户调大字号也不跟——那是用户报的"按钮比旧版小太多"。
const SIDEBAR_TITLE_SCALE: f32 = 0.82;
const SIDEBAR_TAG_SCALE: f32 = 0.80;

/// 侧栏 tab 标签的水平预算（逻辑 px）：外层 `p_2`、行内 `px_2`、行内
/// `gap_2` 与右侧状态槽都从侧栏宽里扣掉，剩下的除以实测 cell 宽就是可用
/// 列数。旧壳同一算法（可用像素跨度 ÷ cell_w = 列），所以省略号出现的
/// 位置两壳一致。
const TAB_STATUS_SLOT_W: f32 = 52.0;
const TAB_LABEL_ICON_W: f32 = 16.0;
const TAB_LABEL_ICON_SIZE: f32 = 15.0;

/// 拖拽启动阈值（逻辑 px）：按住不动/轻微抖动是点击，越过才进入拖拽。
const TAB_DRAG_THRESHOLD: f32 = 4.0;

#[derive(Clone)]
enum WorkspacePaletteAction {
    Shared(crate::display::command_palette::PaletteAction),
    RunAiSession {
        command: String,
        cwd: Option<std::path::PathBuf>,
    },
    /// 启动器混排的 SSH 主机行（数据源 = 共享主机列表权威）。
    LaunchSshHost(String),
    /// 新建终端弹窗里的一台已检测 shell（旧壳 `ProfileRow::Shell`）。
    ///
    /// 旧壳这份菜单还会混入 `nebula.toml` 的 config profiles，GPUI 壳目前
    /// 整体没有消费 profiles（设置页的「配置文件」分区也只有终端与补全
    /// 设置），所以这里只列检测到的 shell——旧壳注释同样承认「没有 profile
    /// 时检测结果本身就能填满菜单」。
    LaunchShell(crate::shell_detect::DetectedShell),
}

#[derive(Clone)]
struct WorkspacePaletteRow {
    group_order: usize,
    group: String,
    label: String,
    hint: String,
    search: String,
    action: WorkspacePaletteAction,
    /// 行首的彩色品牌图标（只有 shell/配置档行有）。旧壳的 shell 菜单同样
    /// 是「图标 + 名字 + 灰色命令行」三段式，光靠文字分不清 pwsh 与 5.1。
    icon: Option<std::sync::Arc<gpui::RenderImage>>,
    /// SSH 主机行用 Nerd Font 码位（旧壳 `os_icons`），不是品牌 PNG。
    icon_glyph: Option<char>,
}

/// 精确 pane id 必须严格路由：已关闭 pane 的迟到事件不能污染当前活跃
/// pane；只有环境链路确实丢失 `NEBULA_PANE_ID` 的事件才允许回退到活跃
/// tab 的聚焦 pane。
fn ai_hook_target_pane(
    pane_ids: &[u64],
    event_pane: Option<u64>,
    active_focused: Option<u64>,
) -> Option<u64> {
    match event_pane {
        Some(pane_id) => pane_ids.contains(&pane_id).then_some(pane_id),
        None => active_focused,
    }
}

/// AI 接续是会话回放的独立闸门：关掉时 pane 的 cwd/launch/layout 仍可
/// 恢复，但绝不把 AgentSession 转成会敲进新 shell 的命令。
fn restored_agent_command(
    resume_ai: bool,
    agent: Option<&crate::session::AgentSession>,
) -> Option<String> {
    if resume_ai { agent.and_then(|agent| agent.resume_command()) } else { None }
}

fn open_in_file_manager(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn workspace_ui_language() -> crate::display::UiLanguage {
    match nebula_settings::RuntimeSettings::load().language {
        nebula_settings::LanguagePref::System => crate::display::LanguagePreference::System,
        nebula_settings::LanguagePref::ZhCn => crate::display::LanguagePreference::ZhCn,
        nebula_settings::LanguagePref::EnUs => crate::display::LanguagePreference::EnUs,
    }
    .resolved()
}

fn new_tab_insert_index(
    position: nebula_settings::NewTabPositionName,
    active: usize,
    tab_count: usize,
) -> usize {
    match position {
        nebula_settings::NewTabPositionName::AfterCurrent if tab_count > 0 => {
            active.saturating_add(1).min(tab_count)
        },
        nebula_settings::NewTabPositionName::AfterCurrent
        | nebula_settings::NewTabPositionName::End => tab_count,
    }
}

fn ai_session_palette_rows(
    sessions: impl IntoIterator<Item = crate::ai_sessions::AiSession>,
) -> Vec<WorkspacePaletteRow> {
    let sessions: Vec<_> = sessions.into_iter().collect();
    let lead_source = sessions.first().map(|session| session.source);
    let mut rows = Vec::new();
    for session in sessions {
        let group_order = usize::from(lead_source.is_some_and(|source| source != session.source));
        let group = crate::display::command_palette::source_group_label(session.source);
        let place = session.place_label();
        let time = crate::ai_sessions::relative_label(session.modified);
        let location = if place.is_empty() { time } else { format!("{place} · {time}") };
        let source = session.source.display_name();
        let search = format!("{} {} {}", session.title, session.project, session.source.label());
        let cwd = (!session.project.trim().is_empty())
            .then(|| std::path::PathBuf::from(session.project.trim()))
            .filter(|path| path.is_dir());
        if let Some(command) = session.resume_command() {
            rows.push(WorkspacePaletteRow {
                group_order,
                group: group.clone(),
                label: session.title.clone(),
                hint: format!("恢复 · {source} · {location}"),
                search: format!("恢复 resume {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
            });
        }
        if let Some(command) = session.fork_command() {
            rows.push(WorkspacePaletteRow {
                group_order,
                group: group.clone(),
                label: format!("分叉 · {}", session.title),
                hint: format!("{source} · {location}"),
                search: format!("分叉 fork {search}"),
                action: WorkspacePaletteAction::RunAiSession { command, cwd: cwd.clone() },
                icon: None,
                icon_glyph: None,
            });
        }
    }
    rows
}

/// 新建终端弹窗的行：已检测 shell + SSH 主机，分组对照旧壳
/// `CommandPalette::open_profiles`（推荐 / 所有 Shell / SSH 主机）。
/// 三点菜单与 Ctrl+K 打开的是这份列表，不是通用命令面板。
fn shell_palette_rows(
    shells: Vec<crate::shell_detect::DetectedShell>,
    ssh_hosts: impl IntoIterator<Item = String>,
    default_shell_id: &str,
    language: crate::display::UiLanguage,
    scale_factor: f32,
) -> Vec<WorkspacePaletteRow> {
    const SHELL_ICON_PX: f32 = 22.0;
    let recommended = language.pick("推荐", "Recommended");
    let all_shells = language.pick("所有 Shell", "All shells");
    let ssh_group = language.pick("SSH 主机", "SSH hosts");
    let mut rows: Vec<WorkspacePaletteRow> = shells
        .into_iter()
        .map(|shell| {
            let is_default = shell.id == default_shell_id;
            WorkspacePaletteRow {
                group_order: if is_default { 0 } else { 1 },
                group: if is_default { recommended.to_owned() } else { all_shells.to_owned() },
                label: shell.name.clone(),
                hint: shell.program.clone(),
                search: format!("{} {} shell profile", shell.name, shell.id).to_lowercase(),
                icon: crate::gpui_shell::widgets::shell_brand_image(
                    &shell.id,
                    SHELL_ICON_PX,
                    scale_factor,
                ),
                icon_glyph: None,
                action: WorkspacePaletteAction::LaunchShell(shell),
            }
        })
        .collect();
    if let Some(position) = rows.iter().position(|row| row.group_order == 0) {
        let default_row = rows.remove(position);
        rows.insert(0, default_row);
    }
    let ssh_icons = ssh_host_icon_ids();
    rows.extend(ssh_hosts.into_iter().map(|host| {
        let glyph = crate::display::ui::os_icons::resolve(ssh_icons.get(&host).map(String::as_str))
            .glyph;
        WorkspacePaletteRow {
            group_order: 2,
            group: ssh_group.to_owned(),
            label: host.clone(),
            hint: "SSH".to_owned(),
            search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
            action: WorkspacePaletteAction::LaunchSshHost(host),
            icon: None,
            icon_glyph: Some(glyph),
        }
    }));
    rows
}

fn ssh_host_icon_ids() -> std::collections::HashMap<String, String> {
    let mut icons = crate::ssh_profiles::SshProfiles::load(
        &crate::display::nebula_data_dir().join("ssh_profiles.json"),
    )
    .map(|profiles| profiles.icons())
    .unwrap_or_default();
    if std::env::var_os("NEBULA_CONFIG_DIR").is_some() {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let user = std::path::PathBuf::from(appdata).join("Nebula").join("ssh_profiles.json");
            if let Ok(profiles) = crate::ssh_profiles::SshProfiles::load(&user) {
                for (host, icon) in profiles.icons() {
                    icons.entry(host).or_insert(icon);
                }
            }
        }
    }
    icons
}

/// 标签的用户可编辑元数据：重命名与色标（旧壳 `TabEntry::custom_name` /
/// `custom_color` 的对应物，字段与共享 session v4 的 `TabSession` 同名同义，
/// 所以导出/恢复不需要转换层）。
///
/// 与 `tabs` **同下标**。长度必须一致，所以增删移三种结构性改动只允许走
/// `insert_tab_at` / `remove_tab_at` / `move_tab`——别处直接 `self.tabs.push`
/// 会让色条和名字错位到邻居身上。
#[derive(Clone, Debug, Default)]
struct TabMeta {
    /// 用户重命名过的标签名；`None` = 跟着 cwd/文件名自动走。
    custom_name: Option<String>,
    /// 色标（右键菜单的标签颜色）；`None` = 不画色条。
    color: Option<Rgb>,
    /// 本 Tab 创建时实际采用的 shell 短标。默认 shell 是“新建时参数”，
    /// 不是全局实时主题；设置改变后既有 PTY 不会换进程，这个标签也不能
    /// 跟着全局值漂移。非终端 Tab 为 `None`。
    shell_tag: Option<SharedString>,
    /// 与旧壳 `TabEntry::launch` 同义：保存“这个 Tab 创建时实际采用什么
    /// 启动方式”。共享 session v4 已有完整 schema，GPUI 只需把它保留下来，
    /// 不能在快照时把所有本地 Tab 都降级成 `None`。
    launch: Option<crate::session::LaunchSession>,
    /// 后台 tab 响过 BEL（旧壳 `has_bell`）。激活即清。
    has_bell: bool,
}

/// 侧栏行内重命名的活动状态（旧壳 `nebula_tab_rename` 同形态：被编辑的那
/// 一行原地变输入框，而不是弹一个对话框）。Enter 提交；Esc / 失焦取消
/// （对照 `input/chrome.rs` 点在框外 = `CancelRename`）；提交空串 = 恢复
/// 自动标签名。
struct TabRename {
    ix: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// 两种 tab 布局共用的只读展示数据。状态与动作仍由 `NebulaWorkspace`
/// 持有；这里只集中 cwd 标题、程序图标、AI 活动和用户元数据的解释。
struct TabPresentation {
    title: SharedString,
    is_settings: bool,
    activity: SidebarActivity,
    logo_image: Option<Arc<RenderImage>>,
    program_glyph: Option<&'static str>,
    shell_tag: Option<SharedString>,
    color: Option<Rgb>,
    renaming: Option<Entity<InputState>>,
}

/// 旧壳 `TabRequest::CommitRename`（`window_context.rs` ~871-880）：
/// trim；空串 → `custom_name = None`（恢复自动名）；非空 → `Some(trimmed)`。
fn apply_commit_rename(meta: &mut TabMeta, buffer: &str) {
    let trimmed = buffer.trim();
    meta.custom_name = if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) };
}

/// 旧壳 `TabRequest::CancelRename`（`window_context.rs` ~896-901）：
/// 丢掉重命名缓冲，`custom_name` 保持进入编辑前的值。
fn apply_cancel_rename(_meta: &mut TabMeta) {}

pub struct NebulaWorkspace {
    tabs: Vec<WorkspaceTab>,
    /// 与 `tabs` 同下标的用户元数据，见 [`TabMeta`]。
    tab_meta: Vec<TabMeta>,
    /// 正在行内重命名的标签，见 [`TabRename`]。
    tab_rename: Option<TabRename>,
    next_pane_id: u64,
    active: usize,
    sidebar_collapsed: bool,
    /// 只折叠 TABS 分区，不影响整个左栏；与旧壳分区标题的 chevron 同义。
    tabs_section_collapsed: bool,
    /// 标签栏布局：默认沿用左侧栏；Top 将同一组 tab 放进 48px 标题栏。
    tabs_position: nebula_settings::TabsPositionName,
    /// 运行时持久化的侧栏逻辑宽；布局、初始窗口和折叠动画必须同源。
    sidebar_width: f32,
    /// 首次手动切换后才启用折叠动画：启动帧保持静止落位（旧壳同感，
    /// spring 构造时直接初始化在端点上）。
    sidebar_fold_armed: bool,
    /// 同理，tab 列表的折叠动画也只在首次手动切换后启用。
    tabs_fold_armed: bool,
    /// 折叠/展开动画期间冻结视口高度：旧壳 `tabs_avail` 来自面板剩余高度，
    /// 绝不把卷帘裁剪量回去再算窗口，否则溢出列表会被锁成一行滚动区。
    tabs_fold_frozen: bool,
    tabs_fold_seq: u64,
    /// 旧壳 `nebula_tabs_scroll`：TABS 溢出时的整行窗口起点。
    tabs_scroll: usize,
    /// 列表视口（逻辑 px），由 canvas 回写；0 表示尚未量到。
    tabs_viewport_h: f32,
    tabs_list_width: f32,
    tabs_list_origin: gpui::Point<gpui::Pixels>,
    tabs_scroll_grab: Option<f32>,
    tabs_list_hot: bool,
    /// 顶栏 tab 的水平滚动；ScrollHandle 负责内容边界钳制。
    top_tabs_scroll: gpui::ScrollHandle,
    /// 开窗时反推的目标网格（含小屏收拢）；首个终端按它 spawn。
    initial_grid: (u16, u16),
    /// 进行中的 tab 拖拽（含未过阈值的待命态）；见 [`TabDrag`]。
    tab_drag: Option<TabDrag>,
    /// 进行中的分隔条拖拽；预览比例写进树节点，松手提交（吸附整格）。
    split_drag: Option<SplitDrag>,
    /// 进行中的侧栏拖宽（设置「面板拖拽调节」开启时才有入口）；宽度实时
    /// 生效，松手写盘 `sidebar_w`（旧壳同合同）。
    sidebar_resizing: bool,
    /// Split 节点视口的帧记录（拖拽换算与提交吸附用）。
    split_bounds: SplitBoundsStore,
    /// pane 矩形的帧记录（ctrl+alt+方向 的最近邻导航用）。
    pane_bounds: PaneBoundsStore,
    /// GPUI presentation over the shared old-shell command catalog.
    command_palette_open: bool,
    command_palette_input: Entity<InputState>,
    command_palette_selected: usize,
    /// Git/SVN 提交信息输入（GPUI 输入组件）；提交动作直达共享模型
    /// `vcs_commit_message`，不经旧壳的内部输入状态机。
    git_commit_input: Entity<InputState>,
    /// Git 树"丢弃改动"的二次确认（路径）；任何其他 VCS 操作都清掉它。
    vcs_discard_confirm: Option<String>,
    /// 命令面板的行覆盖：`None` = 常规命令目录，`Some` = 某个专用列表
    /// （AI 会话恢复、新建终端的 Shell 选择）。关闭面板时必须清掉，
    /// 否则下一次 Ctrl+Shift+P 会打开上一次那份专用列表。
    palette_override: Option<Vec<WorkspacePaletteRow>>,
    /// 三点 / Ctrl+K 打开的是旧壳 `PaletteMode::Profiles`，要画
    /// 全部/SSH/Shell 芯片；Ctrl+Shift+P 的命令目录不走这条。
    shell_picker_open: bool,
    launcher_filter: crate::display::command_palette::LauncherFilter,
    _command_palette_subscription: Subscription,
    /// Shared old-shell drawer model. GPUI owns only presentation and polling;
    /// filesystem traversal, expansion state, ignore marking and throttling
    /// remain in `display::side_panel`.
    side_panel: crate::display::side_panel::SidePanel,
    side_panel_polling: bool,
    side_panel_anim_armed: bool,
    /// 文件树右键：画在 workspace 根上，不进抽屉子孙树。见 `file_tree.rs`。
    file_tree_menu: Option<file_tree::FileTreeContextMenu>,
    /// 标签右键：同样画在根上。挂进标签行会让每一行都渲染同一份菜单，
    /// popover 阴影按标签数叠厚。见 `tab_menu.rs` 模块头。
    tab_menu: Option<tab_menu::TabContextMenu>,
    /// 复用旧壳随包分发的 AI 品牌图，不用近似字体图标替代。
    sidebar_logo_images: HashMap<(crate::display::AiLogo, bool), Arc<RenderImage>>,
    /// 品牌图缓存对应的整数物理像素边长；窗口跨 DPI 显示器时据此重建。
    sidebar_logo_target_px: u32,
    /// 跟随系统深浅：OS 外观切换的监听（旧壳 ThemeChanged 的对应物）。
    _appearance_sub: Subscription,
    /// 本会话已注入的自定义键位 combo（gpui 绑定串）。键位表没有删除
    /// API，撤销只能靠后注的 NoAction 盖掉；这份清单就是撤销的依据。
    custom_keybinds_applied: Vec<String>,
    /// 侧栏「运行中」spinner 的相位（0..1，旧壳 `SPINNER_PERIOD` 800ms 一
    /// 圈）与上一帧时刻。旧壳吃共享 motion frame，GPUI 用下一帧回调推进。
    spinner_phase: f32,
    spinner_last: std::time::Instant,
    /// 最近一次写盘的会话快照（1 Hz 自动保存的去重 + 退出收尾的素材）。
    last_saved_session: Option<crate::session::Session>,
    /// 系统关闭按钮可能连续送来多次 should-close；确认框在场时只保留一份。
    window_close_confirm_open: bool,
    /// `keep_session` 关窗后 HWND 已隐藏、PTY 仍在；托盘 / mux ATTACH 用来捞回。
    window_hidden: bool,
    /// 开窗时记下，mux `tab.new` 需要从 pump 拿到 `&mut Window`。
    window_handle: gpui::AnyWindowHandle,
    /// runtime API 里必须等窗口的命令（目前是 `tab.new`）。
    runtime_pending: Vec<std::sync::Arc<crate::runtime_api::RuntimeDispatch>>,
}

impl NebulaWorkspace {
    /// 所有新建标签共用同一个插入口，避免本地/SSH/设置各自解释配置。
    fn insert_new_tab(&mut self, tab: WorkspaceTab) {
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(at, tab, TabMeta::default());
        self.active = at;
        self.reveal_active_tab();
    }

    /// `tabs` + `tab_meta` 的唯一插入口（见 [`TabMeta`] 的同下标合同）。
    fn insert_tab_at(&mut self, at: usize, tab: WorkspaceTab, meta: TabMeta) {
        let at = at.min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.tab_meta.insert(at, meta);
        self.clamp_tabs_scroll();
    }

    /// `tabs` + `tab_meta` 的唯一移除口；返回被摘的 tab 供调用方回收会话。
    fn remove_tab_at(&mut self, ix: usize) -> Option<(WorkspaceTab, TabMeta)> {
        if ix >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(ix);
        // 长度不齐时（理论上不会）宁可给默认元数据，也不要 panic。
        let meta =
            if ix < self.tab_meta.len() { self.tab_meta.remove(ix) } else { TabMeta::default() };
        self.clamp_tabs_scroll();
        Some((tab, meta))
    }

    /// 当前标签的元数据（下标越界时给一份默认值，读取点因此不必各自判空）。
    fn meta(&self, ix: usize) -> TabMeta {
        self.tab_meta.get(ix).cloned().unwrap_or_default()
    }

    pub fn new(
        window: &mut Window,
        ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
        shell_events: std::sync::mpsc::Receiver<crate::gpui_shell::GpuiShellEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 启动相关设置只取样一次：本次开窗的恢复决策不能被恢复过程中的
        // 文件变化拆成互相矛盾的 restore/resume 状态。
        let runtime = nebula_settings::RuntimeSettings::load();
        let sidebar_width = runtime.sidebar_width;
        let initial_grid = Self::size_window_to_default_grid(window, cx, sidebar_width);
        let this = cx.entity().downgrade();
        let appearance_sub = window.observe_window_appearance(move |_, cx| {
            if let Some(workspace) = this.upgrade() {
                workspace.update(cx, |workspace, cx| workspace.apply_runtime_settings(cx));
            }
        });
        let command_palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索命令…"));
        let git_commit_input = cx.new(|cx| InputState::new(window, cx).placeholder("提交信息…"));
        let command_palette_subscription = cx.subscribe_in(
            &command_palette_input,
            window,
            |this: &mut Self,
             _: &Entity<InputState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<Self>| {
                match event {
                    InputEvent::Change => {
                        this.command_palette_selected = 0;
                        cx.notify();
                    },
                    InputEvent::PressEnter { .. } => {
                        this.run_selected_palette_action(window, cx);
                    },
                    _ => {},
                }
            },
        );
        let sidebar_logo_target_px =
            (TAB_LABEL_ICON_SIZE * window.scale_factor()).round().max(1.0) as u32;
        let mut this = Self {
            tabs: Vec::new(),
            tab_meta: Vec::new(),
            tab_rename: None,
            next_pane_id: 1,
            active: 0,
            sidebar_collapsed: false,
            tabs_section_collapsed: false,
            tabs_position: runtime.tabs_position,
            sidebar_width,
            sidebar_fold_armed: false,
            tabs_fold_armed: false,
            tabs_fold_frozen: false,
            tabs_fold_seq: 0,
            tabs_scroll: 0,
            tabs_viewport_h: 0.0,
            tabs_list_width: 0.0,
            tabs_list_origin: gpui::point(px(0.0), px(0.0)),
            tabs_scroll_grab: None,
            tabs_list_hot: false,
            top_tabs_scroll: gpui::ScrollHandle::new(),
            initial_grid,
            tab_drag: None,
            split_drag: None,
            sidebar_resizing: false,
            split_bounds: Rc::new(RefCell::new(HashMap::new())),
            pane_bounds: Rc::new(RefCell::new(HashMap::new())),
            command_palette_open: false,
            command_palette_input,
            command_palette_selected: 0,
            git_commit_input,
            vcs_discard_confirm: None,
            palette_override: None,
            shell_picker_open: false,
            launcher_filter: crate::display::command_palette::LauncherFilter::All,
            _command_palette_subscription: command_palette_subscription,
            side_panel: crate::display::side_panel::SidePanel::new(),
            side_panel_polling: false,
            side_panel_anim_armed: false,
            file_tree_menu: None,
            tab_menu: None,
            sidebar_logo_images: sidebar_logo_images(sidebar_logo_target_px),
            sidebar_logo_target_px,
            _appearance_sub: appearance_sub,
            custom_keybinds_applied: Vec::new(),
            spinner_phase: 0.0,
            spinner_last: std::time::Instant::now(),
            last_saved_session: None,
            window_close_confirm_open: false,
            window_hidden: false,
            window_handle: window.window_handle(),
            runtime_pending: Vec::new(),
        };
        // 配置装载错误的驻留横幅（消息栏层：用户要去修文件，必须看见）。
        // 只在开窗时呈现一次；设置页 persist 的重载不重复弹。
        if let Some(notice) = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .and_then(|settings| settings.load_notice.clone())
        {
            crate::gpui_shell::toast::banner(
                window,
                cx,
                crate::display::ToastKind::Warning,
                notice,
            );
        }
        // 会话恢复（共享 v4 schema + 崩溃断路器）：restore_session 关闭时
        // 用短路保证不碰 session.json（不加载、隔离、标记或回放），直接落
        // 到出厂的单终端。恢复开启但 resume_ai 关闭时仍回放布局/cwd，只不
        // 注入 AI 接续命令。1 Hz 自动保存随后照常启动。
        if !runtime.restore_session || !this.try_restore_session(runtime.resume_ai, window, cx) {
            this.add_terminal(window, cx);
        }
        Self::start_ai_hook_pump(ai_events, cx);
        Self::start_agent_screen_watchdog(cx);
        Self::start_shell_event_pump(shell_events, cx);
        this.start_session_autosave(cx);
        this.apply_custom_keybinds(cx);
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| workspace.should_close_window(window, cx))
                .unwrap_or(true)
        });
        this
    }

    /// 默认窗口尺寸 = 旧壳默认画布 116×30 的反推（`display` 的
    /// `Dimensions` 默认值）。画布按配置基准字号定形；持久化缩放只参与
    /// 随后的实际行列反推，不能把缩放后的 116 列全加到启动窗宽上。
    /// 布局链横向：网格 + 侧栏 + 卡缝 p_2×2(16) +
    /// 终端水平内边距 24；纵向：网格 + 标题栏 34（gpui-component
    /// TITLE_BAR_HEIGHT）+ 卡缝 16 + 终端垂直内边距 16。各加 2px 余量让
    /// 浮点 floor 不缩行列；放不下的屏幕按 95% 工作区收拢（网格随之变小，
    /// 与旧壳"开不下就小"同义）。
    fn size_window_to_default_grid(
        window: &mut Window,
        cx: &mut App,
        sidebar_width: f32,
    ) -> (u16, u16) {
        let (cell_w, line_h) = TerminalView::cell_metrics(window, cx);
        let (startup_cell_w, startup_line_h) = TerminalView::startup_cell_metrics(window, cx);
        // 标签栏位置只改变 chrome 内部布局，不能改变产品的默认外窗几何。
        // 顶栏模式仍保留与侧栏模式相同的横向预算，让两种模式启动时宽高一致。
        let chrome_w = sidebar_width + 16.0 + 24.0 + 2.0;
        let chrome_h = 34.0 + 16.0 + 16.0 + 2.0;
        let mut w =
            f32::from(TerminalView::DEFAULT_GRID_COLUMNS) * f32::from(startup_cell_w) + chrome_w;
        let mut h =
            f32::from(TerminalView::DEFAULT_GRID_LINES) * f32::from(startup_line_h) + chrome_h;
        if let Some(display) = cx.primary_display() {
            let bounds = display.bounds().size;
            w = w.min(f32::from(bounds.width) * 0.95);
            h = h.min(f32::from(bounds.height) * 0.95);
        }
        window.resize(size(px(w), px(h)));
        // 反推收拢后的目标网格：终端 spawn 直接用它，出生即最终几何，
        // 启动路径零 ConPTY resize（resize 竞态会打乱 shell 首屏输出的
        // 坐标缓存，参见 set_layout 的启动稳定闸）。
        let cols = ((w - chrome_w) / f32::from(cell_w) + 0.001).floor().max(2.0) as u16;
        let rows = ((h - chrome_h) / f32::from(line_h) + 0.001).floor().max(2.0) as u16;
        (cols, rows)
    }

    /// `LaunchSession::Default` 的口语短标。
    ///
    /// 这里不能再读运行时的“当前默认 Shell”：`Default` 已经是这个 Tab 创建
    /// 时冻结下来的启动身份，PTY 侧实际走的是引擎默认 PowerShell。恢复时若
    /// 重新读取设置，只会让右侧短标漂成新的默认值，和仍在运行/恢复出来的
    /// 进程身份相互矛盾。
    fn default_shell_tag() -> SharedString {
        crate::shell_detect::shell_short_tag("powershell").into()
    }

    /// 冻结“新建这一刻”的默认 Shell 为共享 v4 launch 身份。
    ///
    /// 旧壳通过 `TabLaunch::Shell` 保存同样的 name/program/args；GPUI 以前只
    /// 保存 UI 短标，冷恢复时因此失去了真正的启动命令。检测失败才保留
    /// `Default`，让跨机器工作区按 schema 的既有降级规则使用当地默认值。
    fn configured_local_launch(cx: &App) -> crate::session::LaunchSession {
        let shell_id = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .and_then(|settings| settings.shell_id.clone());
        let Some(shell_id) = shell_id.filter(|id| !id.trim().is_empty()) else {
            return crate::session::LaunchSession::Default;
        };
        let Some(detected) = crate::shell_detect::detect_shells()
            .into_iter()
            .find(|shell| shell.id.eq_ignore_ascii_case(&shell_id))
        else {
            return crate::session::LaunchSession::Default;
        };
        let shell = detected.shell();
        crate::session::LaunchSession::Shell {
            name: detected.name,
            program: shell.program().to_owned(),
            args: shell.args().to_vec(),
        }
    }

    /// 把共享会话 launch 还原为一次 GPUI PTY 启动。只有首 Pane 使用 Tab 的
    /// launch；其它分屏继续沿用旧壳合同，按当前默认 Shell 重建。
    fn terminal_launch_from_session(
        launch: &crate::session::LaunchSession,
        cwd: Option<std::path::PathBuf>,
    ) -> crate::gpui_shell::terminal::view::TerminalLaunch {
        use crate::gpui_shell::terminal::view::TerminalLaunch;
        use crate::session::LaunchSession;

        match launch {
            LaunchSession::Default => TerminalLaunch::Local { cwd, shell: None, shell_name: None },
            LaunchSession::Shell { name, program, args } => TerminalLaunch::Local {
                cwd,
                shell: Some(nebula_terminal::tty::Shell::new(program.clone(), args.clone())),
                shell_name: Some(name.clone()),
            },
            LaunchSession::Profile { name, command, args, cwd: profile_cwd, shell_id } => {
                let profile = crate::config::ui_config::Profile {
                    name: name.clone(),
                    command: command.clone(),
                    args: args.clone(),
                    cwd: profile_cwd.as_deref().and_then(crate::session::valid_dir),
                    shell_id: shell_id.clone(),
                    terminal_profile_id: None,
                };
                TerminalLaunch::Local {
                    cwd: profile.cwd.clone().or(cwd),
                    shell: Some(profile.shell()),
                    shell_name: profile.shell_id.clone().or_else(|| Some(profile.name.clone())),
                }
            },
            LaunchSession::Ssh { host } => TerminalLaunch::Ssh { destination: host.clone() },
        }
    }

    fn launch_shell_tag(launch: &crate::session::LaunchSession) -> Option<SharedString> {
        match launch {
            crate::session::LaunchSession::Default => Some(Self::default_shell_tag()),
            crate::session::LaunchSession::Shell { name, .. } => {
                Some(crate::shell_detect::shell_short_tag(name).into())
            },
            crate::session::LaunchSession::Ssh { .. } => Some("ssh".into()),
            crate::session::LaunchSession::Profile { .. } => None,
        }
    }

    /// 把 `keybind=` 自定义表注入 gpui 键位表（两壳共读同一份文件）。gpui
    /// 绑定按注册逆序匹配，后注即覆盖：自定义同 combo 压过静态默认、`none`
    /// 行用 NoAction 吞键——与旧壳「后写行先匹配」的语义对齐。
    ///
    /// 键位表没有删除 API：撤销旧注入靠对失效 combo 后注 NoAction。排除
    /// 两类——仍在生效集里的（后注 NoAction 会盖掉刚注入的新绑定）与静态
    /// 默认键（误杀基础功能）。启动时与设置页每次 `Changed` 后各跑一次。
    fn apply_custom_keybinds(&mut self, cx: &mut Context<Self>) {
        let mut to_bind: Vec<KeyBinding> = Vec::new();
        let mut applied: Vec<String> = Vec::new();
        for (combo, action) in nebula_settings::keybind_pairs() {
            // 解析失败的行（手编文件里残留）跳过：旧壳读表同样静默容错。
            let Some(action) = crate::display::keymap::parse_action(&action) else { continue };
            if crate::display::keymap::parse_combo(&combo).is_none() {
                continue;
            }
            let Some(binding) = custom_workspace_binding(&combo, &action) else { continue };
            let gpui_combo = gpui_binding_combo(&combo);
            applied.push(gpui_combo.clone());
            to_bind.push(binding);
        }
        let stale = self
            .custom_keybinds_applied
            .iter()
            .filter(|combo| {
                !applied.contains(combo) && !STATIC_DEFAULT_COMBOS.contains(&combo.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        for combo in stale {
            to_bind.push(KeyBinding::new(&combo, gpui::NoAction, None));
        }
        self.custom_keybinds_applied = applied;
        if !to_bind.is_empty() {
            cx.bind_keys(to_bind);
        }
    }

    /// 设置或系统外观变化后的统一热应用：重载全局 `Settings`（主题经
    /// follow_system 折算）、逐终端刷新、重建 chrome 令牌。
    fn apply_runtime_settings(&mut self, cx: &mut Context<Self>) {
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        cx.set_global(settings);
        for tab in &self.tabs {
            if let WorkspaceTab::Terminal { panes, .. } = tab {
                for pane in panes {
                    pane.view.update(cx, |view, cx| view.apply_settings(cx));
                }
            }
        }
        crate::gpui_shell::theme::apply_chrome_theme(cx);
        let runtime = nebula_settings::RuntimeSettings::load();
        self.sidebar_width = runtime.sidebar_width;
        self.tabs_position = runtime.tabs_position;
        self.sidebar_resizing = false;
        self.reveal_if_tray_disabled(cx);
        cx.notify();
    }

    fn add_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 旧壳合同（window_context `spawn_tab` 一族）：新 tab 的 cwd 先取
        // 设置页的「启动目录」（存在且是目录才算数），否则继承聚焦 pane
        // 的本地 cwd——`startup_directory=` 因此在两壳有同一效果。
        let cwd = Self::startup_directory().or_else(|| {
            self.tabs
                .get(self.active)
                .and_then(WorkspaceTab::focused_view)
                .and_then(|view| view.read(cx).local_cwd())
        });
        self.add_terminal_at(cwd, None, window, cx);
    }

    /// 设置页「启动目录」：非空且确实存在的目录才生效（旧壳同判定）。
    fn startup_directory() -> Option<std::path::PathBuf> {
        let dir = nebula_settings::RuntimeSettings::load().startup_directory?;
        let dir = dir.trim();
        if dir.is_empty() {
            return None;
        }
        let path = std::path::PathBuf::from(dir);
        path.is_dir().then_some(path)
    }

    /// 创建一个 pane 实体（分配 id、spawn 会话、挂宿主订阅）；调用方决定
    /// 它进新 tab 还是接进某棵分屏树。
    fn new_pane(
        &mut self,
        grid: (u16, u16),
        launch: crate::gpui_shell::terminal::view::TerminalLaunch,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TerminalPane {
        let pane_id = self.next_pane_id;
        self.next_pane_id = self.next_pane_id.saturating_add(1);
        let view = cx.new(|cx| TerminalView::new(pane_id, grid, launch, window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_terminal_event);
        if let Some(command) = command {
            view.update(cx, |view, cx| view.run_command(command, cx));
        }
        TerminalPane { id: pane_id, view, _subscription: subscription }
    }

    /// 现网格（聚焦终端）或开窗反推的目标网格：让新 pane 的 PTY 出生即
    /// 接近最终几何，启动接近零 resize。
    fn inherited_grid(&self, cx: &App) -> (u16, u16) {
        self.tabs
            .iter()
            .find_map(|tab| {
                tab.focused_view().map(|view| {
                    let view = view.read(cx);
                    (view.grid_cols() as u16, view.grid_rows() as u16)
                })
            })
            .unwrap_or(self.initial_grid)
    }

    fn add_terminal_at(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 默认 shell 只在“创建新 Tab”的这一刻取样，并把实际 program/args
        // 一起冻结进 Tab launch。设置页随后改默认值只影响下一次创建；冷
        // 恢复也按本 Tab 的 launch 重建，不会把混合工作区抹成同一种 shell。
        let launch_session = Self::configured_local_launch(cx);
        self.add_terminal_with(launch_session, cwd, command, window, cx);
    }

    /// 用一份指定的 launch 身份建 Tab。默认路径（`add_terminal_at`）冻结
    /// 设置里的默认 shell；Shell 选择弹窗则把用户挑中的那台传进来。
    fn add_terminal_with(
        &mut self,
        launch_session: crate::session::LaunchSession,
        cwd: Option<std::path::PathBuf>,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shell_tag = Self::launch_shell_tag(&launch_session);
        let grid = self.inherited_grid(cx);
        let launch = Self::terminal_launch_from_session(&launch_session, cwd);
        let pane = self.new_pane(grid, launch, command, window, cx);
        let focused = pane.id;
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane.id),
            panes: vec![pane],
            focused,
            zoomed: false,
        };
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(
            at,
            tab,
            TabMeta { shell_tag, launch: Some(launch_session), ..TabMeta::default() },
        );
        self.active = at;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 打开一个 SSH tab（启动器/设置页共用入口）：russh 直连业务层，连接
    /// 阶段/失败原因由业务层回流到 pane（横幅 + grid）。非 config 源的
    /// 目的地自动进保存列表（旧壳 spawn_tab_ssh 同语义）。
    pub fn add_ssh_terminal(
        &mut self,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let mut lists = crate::gpui_shell::ssh_hosts::SshHostLists::load();
            if !lists.is_from_config(&destination) {
                lists.remember(&destination);
                if let Err(err) = lists.persist() {
                    log::warn!("持久化 SSH 主机列表失败: {err}");
                }
            }
        }
        let grid = self.inherited_grid(cx);
        let launch = crate::gpui_shell::terminal::view::TerminalLaunch::Ssh {
            destination: destination.clone(),
        };
        let pane = self.new_pane(grid, launch, None, window, cx);
        let focused = pane.id;
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane.id),
            panes: vec![pane],
            focused,
            zoomed: false,
        };
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(
            at,
            tab,
            TabMeta {
                shell_tag: Some("ssh".into()),
                launch: Some(crate::session::LaunchSession::Ssh { host: destination }),
                ..TabMeta::default()
            },
        );
        self.active = at;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 在聚焦 pane 上开分屏（ctrl+shift+d / ctrl+shift+s，对齐旧壳
    /// SplitRight/SplitDown）：新 pane 继承聚焦 pane 的 cwd，spawn 网格按
    /// 切割方向对半预估——首帧 prepaint 回写真实矩形后自动收敛。
    fn split_focused(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let Some(WorkspaceTab::Terminal { panes, focused, .. }) = self.tabs.get(active) else {
            return;
        };
        let focused = *focused;
        let Some(anchor) = panes.iter().find(|pane| pane.id == focused) else { return };
        let (cols, rows, cwd) = {
            let view = anchor.view.read(cx);
            (view.grid_cols() as u16, view.grid_rows() as u16, view.local_cwd())
        };
        let grid = match direction {
            SplitDirection::LeftRight => ((cols / 2).max(2), rows.max(2)),
            SplitDirection::TopBottom => (cols.max(2), (rows / 2).max(2)),
        };
        let launch = crate::gpui_shell::terminal::view::TerminalLaunch::Local {
            cwd,
            shell: None,
            shell_name: None,
        };
        let pane = self.new_pane(grid, launch, None, window, cx);
        let new_id = pane.id;
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed }) =
            self.tabs.get_mut(active)
        else {
            // new_pane 之后 tab 结构不可能已变（同一同步调用栈），但防御住：
            // 树上挂不进去就立即回收，不留孤儿 PTY。
            pane.view.read(cx).shutdown();
            return;
        };
        if !tree.split_leaf(*focused, new_id, direction, 0.5) {
            pane.view.read(cx).shutdown();
            return;
        }
        panes.push(pane);
        *focused = new_id;
        *zoomed = false;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 关一个 pane（pane 退出 / ctrl+shift+w）。树裁定结局：最后一个叶子
    /// 关整个 tab，否则兄弟收编、焦点交给幸存子树首叶。
    fn close_pane(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = match self.tabs.get_mut(tab_ix) {
            Some(WorkspaceTab::Terminal { tree, .. }) => tree.remove_leaf(pane_id),
            _ => return,
        };
        match outcome {
            RemoveOutcome::NotFound => {},
            RemoveOutcome::WasRoot => self.close_tab(tab_ix, window, cx),
            RemoveOutcome::Collapsed(next_focus) => {
                if let Some(WorkspaceTab::Terminal { panes, focused, zoomed, .. }) =
                    self.tabs.get_mut(tab_ix)
                {
                    if let Some(pos) = panes.iter().position(|pane| pane.id == pane_id) {
                        let pane = panes.remove(pos);
                        pane.view.read(cx).shutdown();
                    }
                    if *focused == pane_id {
                        *focused = next_focus;
                    }
                    *zoomed = false;
                }
                self.pane_bounds.borrow_mut().remove(&pane_id);
                if tab_ix == self.active {
                    self.focus_active(window, cx);
                }
                cx.notify();
            },
        }
    }

    /// 与旧壳 `busy_process_in` 同一判据，只把查询落到 GPUI 的 Pane 实体。
    fn busy_process_in_tab(&self, tab_ix: usize, pane_id: Option<u64>, cx: &App) -> Option<String> {
        let WorkspaceTab::Terminal { panes, .. } = self.tabs.get(tab_ix)? else { return None };
        panes
            .iter()
            .filter(|pane| pane_id.is_none_or(|id| pane.id == id))
            .find_map(|pane| pane.view.read(cx).busy_process())
    }

    /// 系统标题栏关闭的是整个窗口，必须把所有 Tab/Pane 都纳入同一份旧壳
    /// `busy_child(shell_pid)` 判据；只检查当前 Tab 会漏掉后台仍在编译的任务。
    fn busy_process_in_window(&self, cx: &App) -> Option<String> {
        (0..self.tabs.len()).find_map(|tab_ix| self.busy_process_in_tab(tab_ix, None, cx))
    }

    fn save_clean_window_session(&mut self, cx: &App) {
        let mut snapshot = self.snapshot_session(cx);
        crate::session::save_final(&mut snapshot);
        // Drop 不再用最多滞后一秒的自动保存副本覆盖刚写下的准确快照。
        self.last_saved_session = None;
    }

    /// GPUI 的 should-close 回调必须同步返回：无繁忙进程时直接允许系统关闭；
    /// 有繁忙进程时先返回 false，再由对话框确认回调显式移除窗口。
    fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.keep_session_on_close(window, cx) {
            return false;
        }
        let Some(process) = self.busy_process_in_window(cx) else {
            self.save_clean_window_session(cx);
            return true;
        };
        if self.window_close_confirm_open {
            return false;
        }
        self.window_close_confirm_open = true;

        let body: SharedString = format!("{process} 仍在运行，关闭窗口会中止它。").into();
        let confirm_workspace = cx.entity().downgrade();
        let close_workspace = confirm_workspace.clone();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let confirm_workspace = confirm_workspace.clone();
            let close_workspace = close_workspace.clone();
            center_confirm_dialog(dialog, window)
                .title("关闭窗口？")
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("关闭")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger)
                        .cancel_text("取消"),
                )
                .child(body.clone())
                .on_ok(move |_, window, cx| {
                    let _ = confirm_workspace.update(cx, |workspace, cx| {
                        workspace.save_clean_window_session(cx);
                        workspace.window_close_confirm_open = false;
                        // `remove_window` 是确认后的最终动作，不会重新触发
                        // should-close，从而避免再次弹出同一确认框。
                        window.remove_window();
                    });
                    true
                })
                .on_close(move |_, _, cx| {
                    let _ = close_workspace.update(cx, |workspace, cx| {
                        workspace.window_close_confirm_open = false;
                        cx.notify();
                    });
                })
        });
        false
    }

    fn request_close_pane(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(process) = self.busy_process_in_tab(tab_ix, Some(pane_id), cx) else {
            self.close_pane(tab_ix, pane_id, window, cx);
            return;
        };
        let body: SharedString = format!("{process} 仍在运行，关闭会中止它。").into();
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let workspace = workspace.clone();
            center_confirm_dialog(dialog, window)
                .title("关闭此分栏？")
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("关闭")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger)
                        .cancel_text("取消"),
                )
                .child(body.clone())
                .on_ok(move |_, window, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.close_pane(tab_ix, pane_id, window, cx);
                    });
                    true
                })
        });
    }

    fn request_close_tab(&mut self, tab_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(process) = self.busy_process_in_tab(tab_ix, None, cx) else {
            self.close_tab(tab_ix, window, cx);
            return;
        };
        let body: SharedString = format!("{process} 仍在运行，关闭会中止它。").into();
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let workspace = workspace.clone();
            center_confirm_dialog(dialog, window)
                .title("关闭此标签页？")
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("关闭")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger)
                        .cancel_text("取消"),
                )
                .child(body.clone())
                .on_ok(move |_, window, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.close_tab(tab_ix, window, cx);
                    });
                    true
                })
        });
    }

    /// 聚焦另一个 pane（点击上报或方向导航落点）。
    fn focus_pane(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let Some(WorkspaceTab::Terminal { panes, focused, .. }) = self.tabs.get_mut(tab_ix)
            else {
                return;
            };
            if *focused == pane_id || !panes.iter().any(|pane| pane.id == pane_id) {
                return;
            }
            *focused = pane_id;
        }
        if tab_ix == self.active {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// ctrl+alt+方向：按上一帧 pane 矩形找目标方向最近邻（垂直漂移 4 倍
    /// 惩罚，`nebula_split::nav_target` 与旧壳同式）。缩放态先退出缩放再
    /// 导航，pane 矩形下一帧才有——本次导航按缩放前记录的矩形执行。
    fn navigate_pane(&mut self, nav: SplitNav, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active;
        let Some(WorkspaceTab::Terminal { panes, focused, zoomed, .. }) = self.tabs.get_mut(active)
        else {
            return;
        };
        if panes.len() < 2 {
            return;
        }
        *zoomed = false;
        let focused_id = *focused;
        let bounds = self.pane_bounds.borrow();
        let rects: Vec<(u64, nebula_split::Rect)> = panes
            .iter()
            .filter_map(|pane| bounds.get(&pane.id).map(|b| (pane.id, to_split_rect(b))))
            .collect();
        drop(bounds);
        if let Some(target) = nebula_split::nav_target(&rects, focused_id, nav) {
            self.focus_pane(active, target, window, cx);
        } else {
            cx.notify();
        }
    }

    /// ctrl+shift+enter：聚焦 pane 满卡缩放开关（旧壳 ToggleZoom）。
    fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Terminal { panes, zoomed, .. }) = self.tabs.get_mut(self.active) {
            if panes.len() > 1 {
                *zoomed = !*zoomed;
                cx.notify();
            }
        }
    }

    /// 启动恢复：断路器跳闸就隔离现场并走干净路径；恢复成功弹一条
    /// 自动消失的提示（崩溃现场多一句来源说明）。返回是否恢复出了 tab。
    fn try_restore_session(
        &mut self,
        resume_ai: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::display::ToastKind;

        let Some(mut session) = crate::session::load() else { return false };
        if !crate::session::should_restore(&session) {
            if !session.tabs.is_empty() {
                // 连续几次启动都没活到第一次自动保存：把「一恢复就崩」的
                // 现场挪去隔离文件（唯一的诊断材料），本次干净启动。
                if let Some(path) = crate::session::quarantine() {
                    crate::gpui_shell::toast::banner(
                        window,
                        cx,
                        ToastKind::Warning,
                        format!("连续多次启动未完成恢复，已跳过；现场保存在 {}", path.display()),
                    );
                }
            }
            return false;
        }
        let crashed = crate::session::was_crash(&session);
        crate::session::mark_boot_attempt(&mut session);
        let mut restored = 0usize;
        for tab in &session.tabs {
            if self.restore_tab(tab, resume_ai, window, cx) {
                restored += 1;
            }
        }
        if restored == 0 {
            return false;
        }
        self.active = session.active_tab.min(self.tabs.len().saturating_sub(1));
        self.focus_active(window, cx);
        let text = if crashed {
            format!("上次未正常退出，已恢复 {restored} 个标签")
        } else {
            format!("已恢复 {restored} 个标签")
        };
        crate::gpui_shell::toast::toast(window, cx, ToastKind::Success, text);
        cx.notify();
        true
    }

    /// 恢复一个 Terminal tab：DFS 逐叶 spawn（消失目录回退默认 cwd、AI 会话
    /// 以安全 resume 命令接续、SSH launch 只作用于首 pane——launch 描述的
    /// 是「首 pane 怎么启动」，旧壳同义），再按持久化树的形状重建分屏树。
    fn restore_tab(
        &mut self,
        tab: &crate::session::TabSession,
        resume_ai: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::session::{LaunchSession, LayoutSession};

        let layout =
            tab.layout.clone().unwrap_or(LayoutSession::Pane { cwd: tab.cwd.clone(), agent: None });
        // v1-v3 / 早期 GPUI 快照没有 launch，按共享 schema 回退 Default；
        // v4 的 Shell/Profile/Ssh 必须原样用于首 Pane，不能再次读取当前默认。
        let saved_launch = tab.launch.clone().unwrap_or(LaunchSession::Default);
        let grid = self.initial_grid;
        let mut panes: Vec<TerminalPane> = Vec::new();
        for (index, leaf) in layout.leaves().into_iter().enumerate() {
            let LayoutSession::Pane { cwd, agent } = leaf else { continue };
            let launch = if index == 0 {
                Self::terminal_launch_from_session(&saved_launch, crate::session::valid_dir(cwd))
            } else {
                // 共享 v4 与旧壳只把 Tab 的 launch 赋给首 Pane；其它叶子没有
                // 独立启动身份，保持既有 Default 恢复语义。
                crate::gpui_shell::terminal::view::TerminalLaunch::Local {
                    cwd: crate::session::valid_dir(cwd),
                    shell: None,
                    shell_name: None,
                }
            };
            let command = restored_agent_command(resume_ai, agent.as_ref());
            let pane = self.new_pane(grid, launch, command, window, cx);
            // 冷恢复已经知道这段对话的 hook 身份：种回 view，右键「分叉
            // AI 会话」不必再等下一条带 session_id 的 hook。
            if let Some((source, session_id)) = agent.as_ref().and_then(|agent| {
                Some((agent.source.clone(), agent.session_id.clone()?))
            }) {
                pane.view.update(cx, |view, cx| view.seed_ai_session(source, session_id, cx));
            }
            panes.push(pane);
        }
        if panes.is_empty() {
            return false;
        }
        let mut ids = panes.iter().map(|pane| pane.id).collect::<Vec<_>>().into_iter();
        let (tree, _) = crate::gpui_shell::session_restore::tree_from_layout(&layout, &mut || {
            ids.next().unwrap_or(0)
        });
        let focused =
            panes.get(tab.active_pane).or_else(|| panes.first()).map(|pane| pane.id).unwrap_or(0);
        // 恢复期保持文件里的既有次序，不套「新标签插入位置」策略。
        // 重命名与色标随会话一起回来（旧壳同合同）。
        let at = self.tabs.len();
        self.insert_tab_at(
            at,
            WorkspaceTab::Terminal { panes, tree, focused, zoomed: false },
            TabMeta {
                custom_name: tab.custom_name.clone(),
                color: tab.color,
                shell_tag: Self::launch_shell_tag(&saved_launch),
                launch: Some(saved_launch),
                has_bell: false,
            },
        );
        true
    }

    /// 当前工作区 → 共享 v4 快照。设置/文档/图片 tab 不进会话（旧壳同
    /// 合同）；AI 会话身份优先取 hook 直报的精确 id，退而取可解析的前台
    /// 程序名（claude 无 id 恢复成 `--continue`，安全判定在 schema 层）。
    fn snapshot_session(&self, cx: &App) -> crate::session::Session {
        use crate::session::{AgentSession, LaunchSession, Session, TabSession};

        let mut tabs = Vec::new();
        let mut active_out = 0usize;
        for (ix, tab) in self.tabs.iter().enumerate() {
            let WorkspaceTab::Terminal { panes, tree, focused, .. } = tab else { continue };
            if ix == self.active {
                active_out = tabs.len();
            }
            let leaf_data = |id: u64| -> (String, Option<AgentSession>) {
                let Some(pane) = panes.iter().find(|pane| pane.id == id) else {
                    return (String::new(), None);
                };
                let view = pane.view.read(cx);
                let agent = view
                    .ai_session
                    .as_ref()
                    .map(|identity| AgentSession {
                        source: identity.source.clone(),
                        session_id: Some(identity.session_id.clone()),
                    })
                    .or_else(|| {
                        view.running_program
                            .as_deref()
                            .filter(|program| crate::ai_agents::AgentKind::parse(program).is_some())
                            .map(|program| AgentSession {
                                source: program.to_owned(),
                                session_id: None,
                            })
                    });
                (view.cwd.clone(), agent)
            };
            let layout = crate::gpui_shell::session_restore::layout_from_tree(tree, &leaf_data);
            let cwd = panes
                .iter()
                .find(|pane| pane.id == *focused)
                .map(|pane| pane.view.read(cx).cwd.clone())
                .unwrap_or_default();
            let meta = self.meta(ix);
            let first_leaf = tree.first_leaf();
            let launch = meta.launch.clone().unwrap_or_else(|| {
                // 兼容本次修复前已经在内存中的 Tab：SSH 仍可从首 Pane 取回；
                // 旧本地 Tab 已经没有身份信息，只能诚实落为 Default。
                panes
                    .iter()
                    .find(|pane| pane.id == first_leaf)
                    .and_then(|pane| pane.view.read(cx).ssh_destination.clone())
                    .map(|host| LaunchSession::Ssh { host })
                    .unwrap_or(LaunchSession::Default)
            });
            let active_pane = tree.leaves().iter().position(|id| id == focused).unwrap_or(0);
            tabs.push(TabSession {
                cwd,
                custom_name: meta.custom_name,
                color: meta.color,
                launch: Some(launch),
                layout: Some(layout),
                active_pane,
            });
        }
        Session::new(active_out, tabs)
    }

    /// 1 Hz 自动保存（无变化跳过；崩溃/强杀最多丢一秒）。首笔成功写盘的
    /// 快照 `boot_attempts` 恒为 0，即旧壳「活过第一秒就解除断路器」。
    fn start_session_autosave(&self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(1000)).await;
                let alive = this.update(cx, |workspace, cx| {
                    let snapshot = workspace.snapshot_session(cx);
                    if workspace.last_saved_session.as_ref() != Some(&snapshot) {
                        crate::session::save(&snapshot);
                        workspace.last_saved_session = Some(snapshot);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_ai_hook_pump(
        receiver: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(75)).await;
                let mut events = Vec::new();
                while events.len() < 64 {
                    match receiver.try_recv() {
                        Ok(event) => events.push(event),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                if events.is_empty() {
                    continue;
                }
                if this
                    .update(cx, |workspace, cx| {
                        for event in events {
                            workspace.handle_ai_hook(event, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// 1 Hz 遍历所有终端 pane 跑屏幕看门狗（旧壳 `refresh_agent_screen_states`
    /// 的调度对应物）：纠正丢边的 hook 状态、给无 hook 客户端补位。非
    /// agent pane 在 view 侧一行判断就退出，代价可忽略。
    fn start_agent_screen_watchdog(cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(1000)).await;
                let alive = this.update(cx, |workspace, cx| {
                    let views: Vec<_> = workspace
                        .tabs
                        .iter()
                        .filter_map(|tab| match tab {
                            WorkspaceTab::Terminal { panes, .. } => Some(panes.iter()),
                            _ => None,
                        })
                        .flatten()
                        .map(|pane| pane.view.clone())
                        .collect();
                    for view in views {
                        view.update(cx, |view, cx| view.refresh_agent_screen_state(cx));
                    }
                    workspace.publish_tray_agents(cx);
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn handle_ai_hook(&mut self, event: crate::ai_hook::AiHookEvent, cx: &mut Context<Self>) {
        let pane_ids: Vec<u64> = self
            .tabs
            .iter()
            .flat_map(|tab| match tab {
                WorkspaceTab::Terminal { panes, .. } => {
                    panes.iter().map(|pane| pane.id).collect::<Vec<_>>()
                },
                _ => Vec::new(),
            })
            .collect();
        let active_focused = self.tabs.get(self.active).and_then(|tab| match tab {
            WorkspaceTab::Terminal { focused, .. } => Some(*focused),
            _ => None,
        });
        let Some(target_id) = ai_hook_target_pane(&pane_ids, event.pane, active_focused) else {
            return;
        };
        let target = self.tabs.iter().find_map(|tab| match tab {
            WorkspaceTab::Terminal { panes, .. } => {
                panes.iter().find(|pane| pane.id == target_id).map(|pane| pane.view.clone())
            },
            _ => None,
        });
        if let Some(view) = target {
            view.update(cx, |view, cx| view.handle_ai_hook(&event, cx));
        }
    }

    /// 在新 tab 里继续一段进行中的 AI 会话（旧壳 `fork_ai_session` 合同）：
    /// 不克隆 PTY，而是按源 tab 的 shell 重开一个终端，把官方 fork 命令敲进
    /// 去。SSH tab 不分叉——往认证提示里注入命令会打错协议层；新 tab 继承
    /// 源 tab 的 cwd 与色标，命名「{Agent} 分叉」。
    fn fork_ai_session(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.tabs.get(ix).and_then(WorkspaceTab::focused_view) else { return };
        let (command, cwd, agent) = {
            let view = view.read(cx);
            let Some(command) = view.ai_fork_command() else { return };
            let agent = view
                .ai_session
                .as_ref()
                .and_then(|identity| crate::ai_agents::AgentKind::parse(&identity.source));
            (command, view.local_cwd(), agent)
        };
        let launch_session = match self.meta(ix).launch {
            // Default 与旧壳一致取「分叉这一刻」的默认 shell，不是源 tab
            // 创建时的快照。
            None | Some(crate::session::LaunchSession::Default) => {
                Self::configured_local_launch(cx)
            },
            Some(shell @ crate::session::LaunchSession::Shell { .. }) => shell,
            // Profile 可能直接把 agent 当启动命令，SSH 会把命令注入认证
            // 提示——两者都不分叉（旧壳同合同）。
            Some(
                crate::session::LaunchSession::Profile { .. }
                | crate::session::LaunchSession::Ssh { .. },
            ) => return,
        };
        let color = self.meta(ix).color;
        self.activate_tab(ix, window, cx);

        let shell_tag = Self::launch_shell_tag(&launch_session);
        let grid = self.inherited_grid(cx);
        let launch = Self::terminal_launch_from_session(&launch_session, cwd);
        let pane = self.new_pane(grid, launch, Some(command), window, cx);
        let focused = pane.id;
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane.id),
            panes: vec![pane],
            focused,
            zoomed: false,
        };
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(
            at,
            tab,
            TabMeta {
                custom_name: agent.map(|agent| format!("{} 分叉", agent.display_name())),
                color,
                shell_tag,
                launch: Some(launch_session),
                has_bell: false,
            },
        );
        self.active = at;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 设置页是单例 tab（旧壳同形态）：已开则激活，未开则新建。
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // TODO(debug-layout): 临时诊断打点，定位设置页打不开的回归后删除。
        eprintln!("[nebula:gpui] open_settings invoked");
        if let Some(ix) = self.tabs.iter().position(WorkspaceTab::is_settings) {
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| SettingsPane::new(window, cx));
        eprintln!("[nebula:gpui] SettingsPane constructed");
        let subscription = cx.subscribe_in(&view, window, Self::on_settings_event);
        self.insert_new_tab(WorkspaceTab::Settings { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 调试/验收后门：`NEBULA_GPUI_OPEN_DOC=路径` 时启动即打开该文档，
    /// 与文件树双击同一条路由（公式渲染等文档 UI 的免点击验收）。
    pub fn open_document_at_startup(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_document_path(path, window, cx);
    }

    /// 文件路由（旧壳 `input/chrome.rs` 双击合同）：图片 → 图片 tab；
    /// Markdown → 文档 tab（TextView 富渲染）；其余可读文本（txt/log/json
    /// 与源码）→ 代码 tab（行号 + 行级虚拟化，用户裁定 txt 同代码一样）；
    /// 都不认的交系统处理器。
    fn open_document_path(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_markdown = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
            });
        if crate::display::image_viewer::viewable_file(&path) {
            self.open_image_tab(path, window, cx);
        } else if is_markdown {
            self.open_doc_tab(path, window, cx);
        } else if crate::gpui_shell::code_tab::viewable_file(&path)
            || crate::display::markdown_view::viewable_file(&path)
        {
            self.open_code_tab(path, window, cx);
        } else {
            open_in_file_manager(&path);
        }
    }

    /// 同一路径复用已开 tab（激活 + 重读盘，旧壳 open_image_tab 同语义）。
    fn open_image_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Image { view } if view.read(cx).path == path),
        ) {
            if let Some(WorkspaceTab::Image { view }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload(cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::doc_tabs::ImageTabView::new(path, cx));
        self.insert_new_tab(WorkspaceTab::Image { view });
        self.focus_active(window, cx);
        cx.notify();
    }

    fn open_doc_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Document { view } if view.read(cx).path == path),
        ) {
            if let Some(WorkspaceTab::Document { view }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| {
                    view.reload();
                    cx.notify();
                });
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|_| crate::gpui_shell::doc_tabs::DocTabView::new(path));
        self.insert_new_tab(WorkspaceTab::Document { view });
        self.focus_active(window, cx);
        cx.notify();
    }

    fn open_code_tab(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(
            |tab| matches!(tab, WorkspaceTab::Code { view } if view.read(cx).path == path),
        ) {
            if let Some(WorkspaceTab::Code { view }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload(window, cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::code_tab::CodeTabView::new(path, window, cx));
        self.insert_new_tab(WorkspaceTab::Code { view });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 按视图实体反查 (tab 下标, pane id)。
    fn locate_pane(&self, entity_id: gpui::EntityId) -> Option<(usize, u64)> {
        self.tabs.iter().enumerate().find_map(|(ix, tab)| match tab {
            WorkspaceTab::Terminal { panes, .. } => panes
                .iter()
                .find(|pane| pane.view.entity_id() == entity_id)
                .map(|pane| (ix, pane.id)),
            _ => None,
        })
    }

    fn on_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // 侧边栏标题渲染在本视图里，直接重绘。
            TerminalViewEvent::TitleChanged => cx.notify(),
            TerminalViewEvent::Exited => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.close_pane(tab_ix, pane_id, window, cx);
                }
            },
            TerminalViewEvent::FocusRequested => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.focus_pane(tab_ix, pane_id, window, cx);
                }
            },
            // SSH 连接卡片的取消/关闭：这个 pane 除了这条连接没有别的
            // 内容（旧壳 TabRequest::Close 同一裁定）。
            TerminalViewEvent::RequestClose => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.request_close_pane(tab_ix, pane_id, window, cx);
                }
            },
            TerminalViewEvent::FontSizeChanged => self.apply_runtime_settings(cx),
            TerminalViewEvent::Bell => {
                if let Some((tab_ix, _)) = self.locate_pane(view.entity_id())
                    && tab_ix != self.active
                {
                    if let Some(meta) = self.tab_meta.get_mut(tab_ix) {
                        meta.has_bell = true;
                    }
                    cx.notify();
                }
            },
        }
    }

    /// 设置改动：全局 `Settings` 已由设置页重载，这里热应用到所有终端
    /// 并联动窗口 chrome 主题深浅；SSH 连接请求转开新 tab。
    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsPane>,
        event: &SettingsPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsPaneEvent::Changed => {
                self.apply_runtime_settings(cx);
                // 键位编辑器可能改了 keybind= 表：注入/撤销随之热更新。
                self.apply_custom_keybinds(cx);
            },
            SettingsPaneEvent::LaunchSsh(host) => {
                self.add_ssh_terminal(host.clone(), window, cx);
            },
        }
    }

    /// 终端应用惯例：最后一个 Tab 关闭即退出应用。整 tab 关闭（侧栏 ×）
    /// 逐 pane 回收会话；实体引用清零后 `TerminalView::drop` 再兜底。
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab, _meta)) = self.remove_tab_at(ix) else { return };
        if let WorkspaceTab::Terminal { panes, .. } = &tab {
            let mut bounds = self.pane_bounds.borrow_mut();
            for pane in panes {
                pane.view.read(cx).shutdown();
                bounds.remove(&pane.id);
            }
        }

        if self.tabs.is_empty() {
            // 一路关标签关到空 = 正常退出：落一份 clean 空会话（下次启动
            // 走干净路径，不算崩溃）。Drop 兜底不再重写。
            let mut empty = crate::session::Session::new(0, Vec::new());
            crate::session::save_final(&mut empty);
            self.last_saved_session = None;
            cx.quit();
            return;
        }
        if ix < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            if let Some(meta) = self.tab_meta.get_mut(ix) {
                meta.has_bell = false;
            }
            self.reveal_active_tab();
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    /// ctrl+shift+w（对齐旧壳 CloseTab 语义）：tab 有分屏时关聚焦 pane，
    /// 单 pane 时关整个 tab；设置 tab 直接关 tab。
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(WorkspaceTab::Terminal { panes, focused, .. }) if panes.len() > 1 => {
                let (tab_ix, pane_id) = (self.active, *focused);
                self.request_close_pane(tab_ix, pane_id, window, cx);
            },
            Some(WorkspaceTab::Terminal { .. }) => {
                self.request_close_tab(self.active, window, cx);
            },
            Some(_) => self.close_tab(self.active, window, cx),
            None => {},
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = match self.tabs.get(self.active) {
            Some(tab @ WorkspaceTab::Terminal { .. }) => match tab.focused_view() {
                Some(view) => view.read(cx).focus_handle.clone(),
                None => return,
            },
            Some(WorkspaceTab::Settings { view, .. }) => view.read(cx).focus_handle(cx),
            // 图片/文档/代码查看 tab 没有键盘焦点语义（滚轮/拖拽直达元素）。
            Some(
                WorkspaceTab::Image { .. }
                | WorkspaceTab::Document { .. }
                | WorkspaceTab::Code { .. },
            )
            | None => return,
        };
        window.defer(cx, move |window, _| window.focus(&focus));
    }

    fn active_local_cwd(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.tabs
            .get(self.active)
            .and_then(WorkspaceTab::focused_view)
            .and_then(|view| view.read(cx).local_cwd())
    }

    fn toggle_side_panel(
        &mut self,
        view: crate::display::side_panel::PanelView,
        cx: &mut Context<Self>,
    ) {
        self.side_panel_anim_armed = true;
        self.side_panel.toggle(view);
        self.file_tree_menu = None;
        if !self.side_panel.open {
            cx.notify();
            return;
        }

        let cwd = self.active_local_cwd(cx);
        self.side_panel.sync(cwd);

        // The shared model builds snapshots on a worker and exposes a cheap,
        // throttled `sync`. GPUI polls only while the drawer is open; it does
        // not move filesystem business logic into the render function.
        if !self.side_panel_polling {
            self.side_panel_polling = true;
            let executor = cx.background_executor().clone();
            cx.spawn(async move |this, cx| {
                loop {
                    executor.timer(Duration::from_millis(100)).await;
                    let keep_polling = this
                        .update(cx, |workspace, cx| {
                            if !workspace.side_panel.open {
                                workspace.side_panel_polling = false;
                                return false;
                            }
                            let cwd = workspace.active_local_cwd(cx);
                            if workspace.side_panel.sync(cwd) {
                                cx.notify();
                            }
                            true
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn toggle_file_tree(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(crate::display::side_panel::PanelView::Files, cx);
    }

    fn toggle_git_tree(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(crate::display::side_panel::PanelView::Git, cx);
    }

    /// 提交按钮/Enter：读 GPUI 输入框的消息直达共享模型（git 提交暂存区、
    /// svn 提交工作副本），成功入队后清空输入。
    fn submit_vcs_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.git_commit_input.read(cx).value().trim().to_string();
        if message.is_empty() {
            return;
        }
        self.side_panel.vcs_commit_message(&message);
        self.git_commit_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// The catalog itself is owned by the old/shared command model. This
    /// presentation advertises only actions whose execution path is already
    /// wired in GPUI; unsupported rows remain in the shared catalog and appear
    /// automatically when their host service is connected.
    fn palette_action_supported(action: &crate::display::command_palette::PaletteAction) -> bool {
        use crate::display::command_palette::PaletteAction;
        matches!(
            action,
            PaletteAction::NewTab
                | PaletteAction::CopyCwd
                | PaletteAction::RevealCwd
                | PaletteAction::CloseTab
                | PaletteAction::NextTab
                | PaletteAction::PrevTab
                | PaletteAction::ToggleSidebar
                | PaletteAction::OpenSettings
                | PaletteAction::ToggleGhost
                | PaletteAction::CycleAccept
                | PaletteAction::CycleCompletionStyle
                | PaletteAction::ToggleFilesPanel
                | PaletteAction::OpenAiSessionPicker
                | PaletteAction::SelectTheme(_)
                | PaletteAction::SplitRight
                | PaletteAction::SplitDown
                | PaletteAction::ToggleGitPanel
                | PaletteAction::ExportWorkspace
        )
    }

    fn palette_action_available(
        action: &crate::display::command_palette::PaletteAction,
        has_local_cwd: bool,
    ) -> bool {
        use crate::display::command_palette::PaletteAction;

        Self::palette_action_supported(action)
            && (has_local_cwd
                || !matches!(action, PaletteAction::CopyCwd | PaletteAction::RevealCwd))
    }

    fn filtered_palette_rows(&self, cx: &App) -> Vec<WorkspacePaletteRow> {
        let query = self.command_palette_input.read(cx).value().to_ascii_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        let has_local_cwd = self.active_local_cwd(cx).is_some();
        let language = workspace_ui_language();
        let rows = self.palette_override.clone().unwrap_or_else(|| {
            let mut rows: Vec<WorkspacePaletteRow> = crate::display::command_palette::catalog()
                .iter()
                .filter(|item| Self::palette_action_available(&item.action, has_local_cwd))
                .map(|item| {
                    let (group_order, group) =
                        crate::display::command_palette::command_group_metadata(
                            &item.action,
                            language,
                            has_local_cwd,
                            false,
                        );
                    WorkspacePaletteRow {
                        group_order,
                        group,
                        label: item.label.to_owned(),
                        hint: item.hint.to_owned(),
                        search: item.search.to_owned(),
                        action: WorkspacePaletteAction::Shared(item.action.clone()),
                        icon: None,
                        icon_glyph: None,
                    }
                })
                .collect();
            // 启动器混排（旧壳 ⌘K 裁定）：SSH 主机与命令同列，置顶/隐藏
            // 次序由共享 merge 权威裁定。
            let ssh_icons = ssh_host_icon_ids();
            rows.extend(
                crate::gpui_shell::ssh_hosts::SshHostLists::load().merged().into_iter().map(
                    |host| {
                        let glyph = crate::display::ui::os_icons::resolve(
                            ssh_icons.get(&host).map(String::as_str),
                        )
                        .glyph;
                        WorkspacePaletteRow {
                            group_order: usize::MAX,
                            group: language.pick("SSH 主机", "SSH HOSTS").to_owned(),
                            label: host.clone(),
                            hint: "SSH".to_owned(),
                            search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
                            action: WorkspacePaletteAction::LaunchSshHost(host),
                            icon: None,
                            icon_glyph: Some(glyph),
                        }
                    },
                ),
            );
            rows
        });
        let mut rows: Vec<_> = rows
            .into_iter()
            .filter(|row| {
                if self.shell_picker_open {
                    let keep = match self.launcher_filter {
                        crate::display::command_palette::LauncherFilter::All => true,
                        crate::display::command_palette::LauncherFilter::Ssh => {
                            matches!(row.action, WorkspacePaletteAction::LaunchSshHost(_))
                        },
                        crate::display::command_palette::LauncherFilter::Shell => {
                            matches!(row.action, WorkspacePaletteAction::LaunchShell(_))
                        },
                    };
                    if !keep {
                        return false;
                    }
                }
                words.is_empty()
                    || words.iter().all(|word| row.search.to_ascii_lowercase().contains(word))
            })
            .collect();
        rows.sort_by_key(|row| row.group_order);
        rows
    }

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette_open {
            self.close_command_palette(window, cx);
            return;
        }
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.palette_override = None;
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.command_palette_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn dismiss_palette_state(&mut self) {
        self.command_palette_open = false;
        self.palette_override = None;
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.command_palette_open {
            return;
        }
        self.dismiss_palette_state();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn move_command_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.command_palette_open {
            return;
        }
        let len = self.filtered_palette_rows(cx).len();
        if len == 0 {
            self.command_palette_selected = 0;
        } else {
            self.command_palette_selected =
                (self.command_palette_selected as isize + delta).rem_euclid(len as isize) as usize;
        }
        cx.notify();
    }

    fn run_selected_palette_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = self
            .filtered_palette_rows(cx)
            .get(self.command_palette_selected)
            .map(|item| item.action.clone());
        let Some(action) = action else { return };
        match action {
            WorkspacePaletteAction::Shared(action) => self.run_palette_action(action, window, cx),
            WorkspacePaletteAction::RunAiSession { command, cwd } => {
                self.dismiss_palette_state();
                self.add_terminal_at(cwd, Some(command), window, cx);
            },
            WorkspacePaletteAction::LaunchSshHost(host) => {
                self.dismiss_palette_state();
                self.add_ssh_terminal(host, window, cx);
            },
            WorkspacePaletteAction::LaunchShell(detected) => {
                self.launch_palette_shell(detected, window, cx);
            },
        }
    }

    fn open_ai_session_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.palette_override = Some(ai_session_palette_rows(crate::ai_sessions::scan(30)));
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.command_palette_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// 三点 / Ctrl+K：旧壳 `NewTabMenu` → `open_shell_menu` → `PaletteMode::Profiles`。
    fn open_shell_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default_shell_id = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .and_then(|settings| settings.shell_id.clone())
            .unwrap_or_else(|| "powershell".to_owned());
        let language = workspace_ui_language();
        let rows = shell_palette_rows(
            crate::shell_detect::detect_shells(),
            crate::gpui_shell::ssh_hosts::SshHostLists::load().merged(),
            &default_shell_id,
            language,
            window.scale_factor().max(0.5),
        );
        self.palette_override = Some(rows);
        self.shell_picker_open = true;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.command_palette_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn toggle_shell_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette_open && self.shell_picker_open {
            self.close_command_palette(window, cx);
            return;
        }
        self.open_shell_palette(window, cx);
    }

    fn set_launcher_filter(
        &mut self,
        filter: crate::display::command_palette::LauncherFilter,
        cx: &mut Context<Self>,
    ) {
        if !self.shell_picker_open || self.launcher_filter == filter {
            return;
        }
        self.launcher_filter = filter;
        self.command_palette_selected = 0;
        cx.notify();
    }

    fn launcher_chip_counts(
        &self,
    ) -> [(crate::display::command_palette::LauncherFilter, usize); 3] {
        use crate::display::command_palette::LauncherFilter;
        let rows = self.palette_override.as_deref().unwrap_or(&[]);
        let shell = rows
            .iter()
            .filter(|row| matches!(row.action, WorkspacePaletteAction::LaunchShell(_)))
            .count();
        let ssh = rows
            .iter()
            .filter(|row| matches!(row.action, WorkspacePaletteAction::LaunchSshHost(_)))
            .count();
        [
            (LauncherFilter::All, shell + ssh),
            (LauncherFilter::Ssh, ssh),
            (LauncherFilter::Shell, shell),
        ]
    }

    /// 从弹窗选中的 shell 起一个新终端。走共享 v4 launch 身份，因此冷恢复
    /// 拿得回真正的启动命令，侧栏短标也跟着对。
    fn launch_palette_shell(
        &mut self,
        detected: crate::shell_detect::DetectedShell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_palette_state();
        let shell = detected.shell();
        let launch = crate::session::LaunchSession::Shell {
            name: detected.name.clone(),
            program: shell.program().to_owned(),
            args: shell.args().to_vec(),
        };
        let cwd = Self::startup_directory().or_else(|| {
            self.tabs
                .get(self.active)
                .and_then(WorkspaceTab::focused_view)
                .and_then(|view| view.read(cx).local_cwd())
        });
        self.add_terminal_with(launch, cwd, None, window, cx);
    }

    fn run_palette_action(
        &mut self,
        action: crate::display::command_palette::PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::display::command_palette::PaletteAction;

        if action == PaletteAction::OpenAiSessionPicker {
            self.open_ai_session_palette(window, cx);
            return;
        }
        self.dismiss_palette_state();
        match action {
            PaletteAction::NewTab => self.add_terminal(window, cx),
            PaletteAction::CopyCwd => {
                if let Some(path) = self.active_local_cwd(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ));
                }
                self.focus_active(window, cx);
            },
            PaletteAction::RevealCwd => {
                if let Some(path) = self.active_local_cwd(cx) {
                    open_in_file_manager(&path);
                }
                self.focus_active(window, cx);
            },
            PaletteAction::CloseTab => self.close_active(window, cx),
            PaletteAction::SplitRight => {
                self.split_focused(SplitDirection::LeftRight, window, cx);
            },
            PaletteAction::SplitDown => {
                self.split_focused(SplitDirection::TopBottom, window, cx);
            },
            PaletteAction::LaunchSsh(host) => {
                self.add_ssh_terminal(host, window, cx);
            },
            PaletteAction::NextTab if !self.tabs.is_empty() => {
                self.activate_tab((self.active + 1) % self.tabs.len(), window, cx);
            },
            PaletteAction::PrevTab if !self.tabs.is_empty() => {
                self.activate_tab(
                    (self.active + self.tabs.len() - 1) % self.tabs.len(),
                    window,
                    cx,
                );
            },
            PaletteAction::ToggleSidebar => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
                self.sidebar_fold_armed = true;
                self.focus_active(window, cx);
            },
            PaletteAction::OpenSettings => self.open_settings(window, cx),
            PaletteAction::ToggleFilesPanel => {
                self.toggle_file_tree(cx);
                self.focus_active(window, cx);
            },
            PaletteAction::ToggleGitPanel => {
                self.toggle_git_tree(cx);
                self.focus_active(window, cx);
            },
            PaletteAction::ExportWorkspace => self.export_workspace(window, cx),
            PaletteAction::ToggleGhost => {
                let value = !nebula_settings::RuntimeSettings::load().ghost;
                let _ = nebula_settings::persist_keys(&[("ghost", (value as u8).to_string())]);
                self.apply_runtime_settings(cx);
                self.focus_active(window, cx);
            },
            PaletteAction::CycleAccept => {
                let runtime = nebula_settings::RuntimeSettings::load();
                let next = match runtime.accept.settings_value() {
                    "right" => "tab",
                    "tab" => "both",
                    _ => "right",
                };
                let _ = nebula_settings::persist_keys(&[("accept", next.to_owned())]);
                self.apply_runtime_settings(cx);
                self.focus_active(window, cx);
            },
            PaletteAction::CycleCompletionStyle => {
                let runtime = nebula_settings::RuntimeSettings::load();
                let next = if runtime.completion_style.settings_value() == "inline" {
                    "popup"
                } else {
                    "inline"
                };
                let _ = nebula_settings::persist_keys(&[("completion_style", next.to_owned())]);
                self.apply_runtime_settings(cx);
                self.focus_active(window, cx);
            },
            PaletteAction::SelectTheme(theme) => {
                let name = nebula_settings::ThemeName::from_prompt_name(theme.prompt_name())
                    .unwrap_or_default();
                let _ = nebula_settings::persist_keys(
                    &crate::gpui_shell::theme::theme_card_persist_updates(name),
                );
                self.apply_runtime_settings(cx);
                self.focus_active(window, cx);
            },
            _ => self.focus_active(window, cx),
        }
        cx.notify();
    }

    /// 完整标签文本。**不在这里截断**：可见宽度是布局问题，字符数上限会在
    /// 窄侧栏下漏出、在宽侧栏下白扔字符。截断由 `render_sidebar` 按实测
    /// cell 宽换算成列数后交给旧壳的 `truncate_tab_label`（带省略号）。
    fn tab_title(&self, ix: usize, cx: &App) -> SharedString {
        if let Some(custom) = self.meta(ix).custom_name {
            return custom.into();
        }
        match &self.tabs[ix] {
            WorkspaceTab::Settings { .. } => "设置".into(),
            WorkspaceTab::Image { view } => view.read(cx).title.clone().into(),
            WorkspaceTab::Document { view } => view.read(cx).title.clone().into(),
            WorkspaceTab::Code { view } => view.read(cx).title.clone().into(),
            tab @ WorkspaceTab::Terminal { panes, .. } => {
                // 标签 = 聚焦 pane 的 cwd 末级目录名（旧壳 chrome_tab_label
                // 规则）；分屏时缀 pane 计数，一眼可见这行是一组。
                let label = match tab.focused_view() {
                    Some(view) => view.read(cx).tab_label(),
                    None => String::from("shell"),
                };
                let mut head = label;
                if panes.len() > 1 {
                    head.push_str(&format!(" ⊞{}", panes.len()));
                }
                head.into()
            },
        }
    }

    fn render_command_palette(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let selected_bg = theme.list_active;
        let hover_bg = theme.list_hover;
        let muted = theme.muted_foreground;
        let items = self.filtered_palette_rows(cx);
        if self.command_palette_selected >= items.len() {
            self.command_palette_selected = items.len().saturating_sub(1);
        }

        let mut rows = Vec::new();
        let mut previous_group: Option<String> = None;
        for (ix, item) in items.into_iter().enumerate() {
            if previous_group.as_deref() != Some(item.group.as_str()) {
                previous_group = Some(item.group.clone());
                rows.push(
                    h_flex()
                        .h(px(26.0))
                        .px_3()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(item.group.clone())
                        .into_any_element(),
                );
            }
            let action = item.action.clone();
            let selected = ix == self.command_palette_selected;
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("command-palette-row-{ix}")))
                    .h(px(36.0))
                    .w_full()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .when(selected, |row| row.bg(selected_bg))
                    .hover(|row| row.bg(hover_bg))
                    // 品牌图标只有 shell 行有；命令行不留空槽，否则整份
                    // 命令目录会平白多出一列缩进。
                    .when_some(item.icon.clone(), |row, image| {
                        row.child(gpui::StyledImage::object_fit(
                            img(image).size(px(22.0)).flex_shrink_0(),
                            gpui::ObjectFit::Contain,
                        ))
                    })
                    .when_some(item.icon_glyph, |row, glyph| {
                        row.child(
                            div()
                                .size(px(22.0))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_family(crate::font_install::REQUIRED_FONT_FAMILY)
                                .text_size(px(16.0))
                                .child(glyph.to_string()),
                        )
                    })
                    .child(div().flex_1().text_sm().child(item.label))
                    .when(!item.hint.is_empty(), |row| {
                        row.child(div().text_xs().text_color(muted).child(item.hint))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.command_palette_selected = ix;
                        match action.clone() {
                            WorkspacePaletteAction::Shared(action) => {
                                this.run_palette_action(action, window, cx);
                            },
                            WorkspacePaletteAction::RunAiSession { command, cwd } => {
                                this.dismiss_palette_state();
                                this.add_terminal_at(cwd, Some(command), window, cx);
                            },
                            WorkspacePaletteAction::LaunchSshHost(host) => {
                                this.dismiss_palette_state();
                                this.add_ssh_terminal(host, window, cx);
                            },
                            WorkspacePaletteAction::LaunchShell(detected) => {
                                this.launch_palette_shell(detected, window, cx);
                            },
                        }
                    }))
                    .into_any_element(),
            );
        }

        div()
            .absolute()
            .inset_0()
            .occlude()
            .key_context(PALETTE_KEY_CONTEXT)
            .bg(theme.overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_command_palette(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        this.move_command_palette_selection(-1, cx);
                        cx.stop_propagation();
                    },
                    "down" => {
                        this.move_command_palette_selection(1, cx);
                        cx.stop_propagation();
                    },
                    "escape" => {
                        this.close_command_palette(window, cx);
                        cx.stop_propagation();
                    },
                    _ => {},
                }
            }))
            .child(
                v_flex()
                    .absolute()
                    .top(px(76.0))
                    .left_1_2()
                    .ml(px(-290.0))
                    .w(px(580.0))
                    .max_h(px(520.0))
                    .p_2()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .child(Input::new(&self.command_palette_input))
                    .when(self.shell_picker_open, |panel| {
                        let language = workspace_ui_language();
                        let selected_filter = self.launcher_filter;
                        panel.child(
                            h_flex().w_full().gap_1().px_1().children(
                                self.launcher_chip_counts().into_iter().map(|(filter, count)| {
                                    let selected = selected_filter == filter;
                                    let label: SharedString =
                                        format!("{} {count}", filter.label(language)).into();
                                    h_flex()
                                        .id(SharedString::from(format!(
                                            "launcher-chip-{filter:?}"
                                        )))
                                        .h(px(26.0))
                                        .px_2()
                                        .items_center()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when(selected, |chip| chip.bg(selected_bg))
                                        .hover(|chip| chip.bg(hover_bg))
                                        .child(div().text_xs().child(label))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_launcher_filter(filter, cx);
                                        }))
                                }),
                            ),
                        )
                    })
                    // gap 与行宽都要落在滚动区内层：`overflow_y_scrollbar` 把
                    // 外层样式搬到它自建的容器上（详注见 settings_pane.rs 的
                    // 设置正文），写在它之前的 gap_1 对行间距无效，行上的
                    // w_full 也会回落 max-content 使每行宽度参差。
                    .child(
                        v_flex()
                            .max_h(px(430.0))
                            .overflow_y_scrollbar()
                            .child(v_flex().w_full().gap_1().children(rows)),
                    ),
            )
            .into_any_element()
    }

    fn select_side_panel_view(
        &mut self,
        view: crate::display::side_panel::PanelView,
        cx: &mut Context<Self>,
    ) {
        if self.side_panel.view == view {
            return;
        }
        self.file_tree_menu = None;
        self.side_panel.toggle(view);
        let cwd = self.active_local_cwd(cx);
        self.side_panel.sync(cwd);
        cx.notify();
    }

    fn render_side_panel_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::display::side_panel::PanelView;

        let files = self.side_panel.view == PanelView::Files;
        let git = self.side_panel.view == PanelView::Git;
        let git_count = self
            .side_panel
            .git()
            .map(|snapshot| snapshot.unstaged.len() + snapshot.staged.len())
            .unwrap_or(0);
        h_flex()
            .gap_1()
            .child(
                Button::new("side-panel-files")
                    .icon(IconName::FolderClosed)
                    .label("文件")
                    .small()
                    .selected(files)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_side_panel_view(PanelView::Files, cx);
                    })),
            )
            .child(
                Button::new("side-panel-git")
                    .label("Git")
                    .icon(IconName::GitHub)
                    .small()
                    .selected(git)
                    .when(git_count > 0, |button| button.label(format!("Git {git_count}")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_side_panel_view(PanelView::Git, cx);
                    })),
            )
    }

    fn render_git_tree(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.list_hover;
        let selected_bg = theme.list_active;
        let mono_family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
        // Git 视图看的是终端当前目录，不是目录树浏览到的位置（`vcs_root`）。
        let root = self.side_panel.vcs_root().map(Path::to_path_buf);
        let selected = self.side_panel.selected.clone();
        let git = self.side_panel.git().cloned();
        let vcs = git.as_ref().map(|info| info.vcs);
        let op_running = self.side_panel.op_running();
        let op_error = self.side_panel.op_error();
        let mut rows = Vec::new();

        if let Some(info) = git.as_ref() {
            use crate::display::side_panel::VcsKind;
            /// 分组决定行内操作（VS Code 的 SCM 行合同）。
            #[derive(Clone, Copy, PartialEq)]
            enum RowOps {
                /// 变更组：暂存 + 丢弃（untracked 不给丢弃——restore 不删新文件）。
                Unstaged,
                /// 已暂存组：取消暂存。
                Staged,
                /// 冲突组 / SVN（无暂存区）：无行内操作。
                None,
            }
            let is_git = info.vcs == VcsKind::Git;
            let conflict_paths: std::collections::HashSet<&str> =
                info.conflicts.iter().map(|(_, path)| path.as_str()).collect();
            // VS Code 三组模型：合并冲突 → 已暂存 → 变更。冲突路径从后两组
            // 过滤（数据层为旧壳兼容把它们同时留在原列表里）。
            let mut sections: Vec<(&str, Vec<&(char, String)>, RowOps)> = Vec::new();
            if !info.conflicts.is_empty() {
                sections.push(("合并冲突", info.conflicts.iter().collect(), RowOps::None));
            }
            let not_conflicted =
                |(_, path): &&(char, String)| !conflict_paths.contains(path.as_str());
            match info.vcs {
                VcsKind::Git => {
                    sections.push((
                        "已暂存",
                        info.staged.iter().filter(not_conflicted).collect(),
                        RowOps::Staged,
                    ));
                    sections.push((
                        "变更",
                        info.unstaged.iter().filter(not_conflicted).collect(),
                        RowOps::Unstaged,
                    ));
                },
                VcsKind::Svn => sections.push((
                    "修改",
                    info.unstaged.iter().filter(not_conflicted).collect(),
                    RowOps::None,
                )),
            }
            let clean = sections.iter().all(|(_, entries, _)| entries.is_empty());
            if clean {
                rows.push(
                    div()
                        .py_2()
                        .px_2()
                        .text_sm()
                        .text_color(muted)
                        .child("没有更改")
                        .into_any_element(),
                );
            }
            let discard_confirm = self.vcs_discard_confirm.clone();
            for (section, entries, ops) in sections {
                if entries.is_empty() {
                    continue;
                }
                rows.push(
                    h_flex()
                        .h(px(26.0))
                        .px_2()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(section)
                        .child(div().ml_2().child(entries.len().to_string()))
                        .into_any_element(),
                );
                for (index, (status, relative_path)) in entries.into_iter().enumerate() {
                    let path = root
                        .as_ref()
                        .map(|root| root.join(relative_path))
                        .unwrap_or_else(|| std::path::PathBuf::from(relative_path));
                    let selected_row = selected.as_ref() == Some(&path);
                    let status_color = match status {
                        'A' | '?' => theme.success,
                        'D' | '!' => theme.danger,
                        'C' | 'U' => theme.danger,
                        _ => theme.warning,
                    };
                    // VS Code 式路径拆分：文件名主体 + 灰色父目录。
                    let (file_name, parent) = match relative_path.rfind('/') {
                        Some(pos) => {
                            (relative_path[pos + 1..].to_owned(), relative_path[..pos].to_owned())
                        },
                        None => (relative_path.clone(), String::new()),
                    };
                    let row_group =
                        SharedString::from(format!("vcs-row-actions-{section}-{index}"));
                    let open_path = path.clone();
                    let stage_path = relative_path.clone();
                    let unstage_path = relative_path.clone();
                    let discard_path = relative_path.clone();
                    let discard_armed = discard_confirm.as_deref() == Some(relative_path.as_str());
                    let can_discard = ops == RowOps::Unstaged && *status != '?';
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!(
                                "git-tree-row-{section}-{index}-{relative_path}"
                            )))
                            .group(row_group.clone())
                            .h(px(30.0))
                            .w_full()
                            .px_2()
                            .gap_2()
                            .items_center()
                            .rounded_md()
                            .when(selected_row, |row| row.bg(selected_bg))
                            .hover(|row| row.bg(hover))
                            .child(
                                div()
                                    .w(px(14.0))
                                    .flex_shrink_0()
                                    .font_family(mono_family.clone())
                                    .text_sm()
                                    .text_color(status_color)
                                    .child(status.to_string()),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .items_center()
                                    .child(div().flex_shrink_0().text_sm().child(file_name.clone()))
                                    .when(!parent.is_empty(), |line| {
                                        line.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(parent.clone()),
                                        )
                                    }),
                            )
                            .when(can_discard, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "vcs-discard-{section}-{index}"
                                    )))
                                    .map(|button| {
                                        if discard_armed {
                                            button.label("确认丢弃").danger().xsmall()
                                        } else {
                                            button
                                                .icon(IconName::Undo2)
                                                .ghost()
                                                .xsmall()
                                                .tooltip("丢弃改动")
                                        }
                                    })
                                    .when(!discard_armed, |button| {
                                        button
                                            .invisible()
                                            .group_hover(row_group.clone(), |button| {
                                                button.visible()
                                            })
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if this.vcs_discard_confirm.as_deref()
                                                == Some(discard_path.as_str())
                                            {
                                                this.vcs_discard_confirm = None;
                                                this.side_panel.git_discard_path(&discard_path);
                                            } else {
                                                this.vcs_discard_confirm =
                                                    Some(discard_path.clone());
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .when(ops == RowOps::Unstaged && is_git, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "vcs-stage-{section}-{index}"
                                    )))
                                    .icon(IconName::Plus)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("暂存")
                                    .invisible()
                                    .group_hover(row_group.clone(), |button| button.visible())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.git_stage_path(&stage_path);
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .when(ops == RowOps::Staged, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "vcs-unstage-{section}-{index}"
                                    )))
                                    .icon(IconName::Minus)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("取消暂存")
                                    .invisible()
                                    .group_hover(row_group.clone(), |button| button.visible())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.vcs_discard_confirm = None;
                                            this.side_panel.git_unstage_path(&unstage_path);
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.side_panel.selected = Some(path.clone());
                                cx.notify();
                            }))
                            .on_double_click(cx.listener(move |this, _, window, cx| {
                                // VS Code 点变更行看内容；diff 视图未落地前
                                // 先开文件本体（源码 tab / 文档 tab 自动路由）。
                                this.open_document_path(open_path.clone(), window, cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
        }

        let summary = git.as_ref().map(|info| {
            h_flex()
                .h(px(30.0))
                .items_center()
                .gap_2()
                .child(div().font_family(mono_family).text_sm().text_color(muted).child("\u{ea68}"))
                .child(div().flex_1().min_w_0().text_sm().truncate().child(
                    if info.branch.is_empty() {
                        "(no branch)".to_owned()
                    } else {
                        info.branch.clone()
                    },
                ))
                .when(info.ahead > 0, |row| {
                    row.child(
                        div().text_xs().text_color(theme.primary).child(format!("↑{}", info.ahead)),
                    )
                })
                .child(div().text_xs().text_color(theme.success).child(format!("+{}", info.plus)))
                .child(div().text_xs().text_color(theme.danger).child(format!("−{}", info.minus)))
        });

        // 操作面（旧壳四按钮的 GPUI 形态；SVN 无暂存区/推送语义，砍两钮、
        // 「拉取」改「更新」）。提交输入直达共享模型 `vcs_commit_message`，
        // 其余按钮各对一个共享入口；op_error 驻留在摘要之下。
        let (unstaged_len, staged_len, ahead) = git
            .as_ref()
            .map(|info| (info.unstaged.len(), info.staged.len(), info.ahead))
            .unwrap_or((0, 0, 0));
        let commit_ready = {
            use crate::display::side_panel::VcsKind;
            !op_running
                && match vcs {
                    Some(VcsKind::Git) => staged_len > 0,
                    Some(VcsKind::Svn) => unstaged_len > 0,
                    None => false,
                }
        };
        let commit_row = git.as_ref().map(|_| {
            h_flex()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&self.git_commit_input)))
                .child(
                    Button::new("vcs-commit")
                        .label("提交")
                        .small()
                        .disabled(!commit_ready)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_vcs_commit(window, cx);
                        })),
                )
        });
        let action_strip = git.as_ref().map(|_| {
            use crate::display::side_panel::VcsKind;
            let is_git = vcs == Some(VcsKind::Git);
            h_flex()
                .gap_1()
                .items_center()
                .when(is_git, |row| {
                    row.child(
                        Button::new("git-stage-all")
                            .label("全部暂存")
                            .small()
                            .disabled(op_running || unstaged_len == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.git_stage_all();
                                cx.notify();
                            })),
                    )
                })
                .when(is_git, |row| {
                    row.child(
                        Button::new("git-push")
                            .label(if ahead > 0 {
                                SharedString::from(format!("推送 ↑{ahead}"))
                            } else {
                                SharedString::from("推送")
                            })
                            .small()
                            .disabled(op_running || ahead == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.git_push();
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new("vcs-pull")
                        .label(if is_git { "拉取" } else { "更新" })
                        .small()
                        .disabled(op_running)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.side_panel.git_pull();
                            cx.notify();
                        })),
                )
                .when(op_running, |row| row.child(Spinner::new().xsmall()))
        });

        v_flex()
            .h_full()
            .w(px(320.0))
            .flex_shrink_0()
            .my_2()
            .mr_2()
            .p_2()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            // 与文件树抽屉同一套紧凑投影，避免右侧抽屉语言再出现 shadow_lg。
            .shadow(gpui_component::popover_shadow(theme.is_dark()))
            .occlude()
            .child(view_switch)
            .when_some(summary, |panel, summary| panel.child(summary))
            .when_some(op_error, |panel, error| {
                panel.child(div().text_xs().text_color(theme.danger).child(error))
            })
            .when_some(commit_row, |panel, row| panel.child(row))
            .when_some(action_strip, |panel, row| panel.child(row))
            .when(git.is_none(), |panel| {
                panel.child(
                    div().py_3().text_sm().text_color(muted).child("当前目录不在 Git/SVN 仓库中"),
                )
            })
            .when_some(self.side_panel.root_notice(), |panel, notice| {
                panel.child(div().text_xs().text_color(theme.warning).child(notice.to_owned()))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().gap_1().children(rows)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new("git-tree-refresh")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("刷新 Git 状态")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.request_refresh();
                                let cwd = this.active_local_cwd(cx);
                                this.side_panel.sync(cwd);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-tree-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("关闭 Git 状态")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_git_tree(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_side_panel_slot(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.side_panel_anim_armed && !self.side_panel.open {
            return div().into_any_element();
        }
        let open = self.side_panel.open;
        let panel = match self.side_panel.view {
            crate::display::side_panel::PanelView::Files => self.render_file_tree(cx),
            crate::display::side_panel::PanelView::Git => self.render_git_tree(cx),
        };
        div()
            .h_full()
            .flex()
            .justify_end()
            .flex_shrink_0()
            .overflow_hidden()
            .child(panel)
            .with_animation(
                ("side-panel-slide", open as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| {
                    let progress = if open { t } else { 1.0 - t };
                    slot.w(px(SIDE_PANEL_SLOT_W * progress))
                },
            )
            .into_any_element()
    }

    fn duplicate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let Some(view) = tab.focused_view() else { return };
        let (ssh, cwd) = {
            let view = view.read(cx);
            (view.ssh_destination.clone(), view.local_cwd())
        };
        if let Some(destination) = ssh {
            self.add_ssh_terminal(destination, window, cx);
        } else {
            self.add_terminal_at(cwd, None, window, cx);
        }
    }

    /// 进入行内重命名：对照旧壳 `TabRequest::BeginRename`
    /// （`window_context.rs` ~854-868）。预填 `custom_name`，否则
    /// `chrome_tab_label`（cwd 末级，不含分屏后缀）。已在编辑别的行时丢掉
    /// 前一次缓冲（不提交），与旧壳覆盖 `nebula_tab_rename` 同合同。
    fn begin_rename(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // 覆盖前一次编辑：只丢缓冲，不走 Commit（否则会把当时显示的目录名
        // 冻成 custom_name）。不要调用 `cancel_rename`——它会 `focus_active`
        // 把焦点延迟抢回终端，紧接着的输入框 focus 会被下一帧冲掉。
        let _ = self.tab_rename.take();
        let current = self.meta(ix).custom_name.unwrap_or_else(|| self.rename_prefill(ix, cx));
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                    // 点在框外 = CancelRename（`input/chrome.rs` ~359-368），
                    // 不是 Commit。Blur 提交会把自动目录名冻成 custom_name。
                    InputEvent::Blur => this.cancel_rename(window, cx),
                    _ => {},
                }
            },
        );
        // 旧壳 `nebula_tab_rename_select_all = true`：set_value 后全选再 focus。
        // `InputState::select_all` 是 `pub(super)`，对外走公开的 `SelectAll` action。
        input.update(cx, |state, cx| {
            state.set_value(current, window, cx);
            state.focus(window, cx);
        });
        self.tab_rename = Some(TabRename { ix, input, _subscription: subscription });
        cx.on_next_frame(window, |this, window, cx| {
            let Some(rename) = this.tab_rename.as_ref() else { return };
            if !rename.input.read(cx).focus_handle(cx).is_focused(window) {
                return;
            }
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });
        cx.notify();
    }

    /// BeginRename 预填：有 custom 用 custom，否则终端用聚焦 pane 的
    /// `tab_label()`（cwd 末级，对齐 `chrome_tab_label`），其它 tab 用标题。
    fn rename_prefill(&self, ix: usize, cx: &App) -> String {
        match self.tabs.get(ix) {
            Some(tab @ WorkspaceTab::Terminal { .. }) => tab
                .focused_view()
                .map(|view| view.read(cx).tab_label())
                .unwrap_or_else(|| self.tab_title(ix, cx).to_string()),
            _ => self.tab_title(ix, cx).to_string(),
        }
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.tab_rename.take() else { return };
        let name = rename.input.read(cx).value();
        if let Some(meta) = self.tab_meta.get_mut(rename.ix) {
            apply_commit_rename(meta, &name);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.tab_rename.take() else { return };
        if let Some(meta) = self.tab_meta.get_mut(rename.ix) {
            apply_cancel_rename(meta);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 标签色标：同色再点一次即取消（旧壳菜单里选中的那枚色块 = 当前色）。
    fn set_tab_color(&mut self, ix: usize, color: Option<Rgb>, cx: &mut Context<Self>) {
        if let Some(meta) = self.tab_meta.get_mut(ix) {
            meta.color = color;
            cx.notify();
        }
    }

    /// 导出整个窗口为工作区文件（旧壳 `export_workspace(None)`）。
    fn export_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let export = self.snapshot_session(cx);
        if export.tabs.is_empty() {
            return;
        }
        self.prompt_save_workspace(export, "workspace", window, cx);
    }

    /// 导出单个标签为工作区文件（旧壳 `export_workspace(Some(ix))` 同合同：
    /// 只导出可恢复的终端标签，文件名取标签名，扩展名 `.nebula-workspace.json`，
    /// 落盘走共享 v4 schema）。
    fn export_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.snapshot_session(cx);
        // snapshot 只收终端标签，所以下标要按「第几个可导出标签」重算。
        let exportable_before = self
            .tabs
            .iter()
            .take(ix)
            .filter(|tab| matches!(tab, WorkspaceTab::Terminal { .. }))
            .count();
        let Some(tab) = session.tabs.get(exportable_before).cloned() else { return };
        let stem = tab
            .custom_name
            .clone()
            .or_else(|| {
                tab.cwd.rsplit(['/', '\\']).find(|part| !part.is_empty()).map(str::to_owned)
            })
            .unwrap_or_else(|| "tab".to_owned());
        let export = crate::session::Session::new(0, vec![tab]);
        self.prompt_save_workspace(export, &stem, window, cx);
    }

    fn prompt_save_workspace(
        &self,
        export: crate::session::Session,
        stem: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stem: String = stem
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        let directory = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let prompt =
            cx.prompt_for_new_path(&directory, Some(&format!("{stem}.nebula-workspace.json")));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else { return };
            let result = crate::session::save_to(&path, &export);
            let _ = this.update_in(cx, |_, window, cx| match result {
                Ok(()) => crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Success,
                    format!("已导出到 {}", path.display()),
                ),
                Err(error) => crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Warning,
                    format!("工作区导出失败：{error}"),
                ),
            });
        })
        .detach();
    }

    /// 侧栏等宽标签的 cell 宽：与终端元素同一套度量法（塑形一个 "M" 取
    /// advance），列数换算与省略号都建立在它上面。字体缺失时回落 0.6em，
    /// 只影响截断位置、不会画错。
    fn sidebar_cell_width(&self, window: &mut Window, family: &SharedString, size_px: f32) -> f32 {
        let shaped = window.text_system().shape_line(
            SharedString::new_static("M"),
            px(size_px),
            &[gpui::TextRun {
                len: 1,
                font: gpui::font(family.clone()),
                color: gpui::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let width = f32::from(shaped.width);
        if width > 0.5 { width } else { size_px * 0.6 }
    }

    /// 旧壳 `icons::push_spinner` 的 canvas 复刻：暗轨道 + 绕行亮弧（占
    /// 整圈 1/3），半径 5.5、笔画 0.30r，中性灰（spinner 表达「还在跑」，
    /// 不抢品牌色）。phase 由 render 侧的帧循环推进。
    fn spinner(phase: f32, track: gpui::Rgba, head: gpui::Rgba) -> impl IntoElement {
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let side = f32::from(bounds.size.width);
                let (cx, cy) = (ox + side * 0.5, oy + side * 0.5);
                let radius = 5.5_f32;
                let stroke = (radius * 0.30).max(1.0);
                // 点铺在轨道中线上：外缘正好落在 radius 上。
                let mid = radius - stroke * 0.5;
                // 与旧壳 `push_spinner` 完全同式：相邻圆点约重叠 50%，既不
                // 留珠链缝，也不以过密叠加制造额外模糊。
                let steps =
                    ((mid * std::f32::consts::TAU / (stroke * 0.5)).ceil() as usize).clamp(24, 96);
                const ARC: f32 = 0.34;
                for step in 0..steps {
                    let at = step as f32 / steps as f32;
                    let angle = at * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                    let behind = (phase - at).rem_euclid(1.0);
                    let t = (1.0 - behind / ARC).clamp(0.0, 1.0);
                    // 旧壳在 RGB 域插值，不在 HSL 域绕色相；两端均已预合成
                    // 为不透明色，圆点相交处不会积累 alpha。
                    let mix = |a: f32, b: f32| a + (b - a) * t;
                    let c: gpui::Hsla = gpui::Rgba {
                        r: mix(track.r, head.r),
                        g: mix(track.g, head.g),
                        b: mix(track.b, head.b),
                        a: 1.0,
                    }
                    .into();
                    let x0 = cx + mid * angle.cos() - stroke * 0.5;
                    let y0 = cy + mid * angle.sin() - stroke * 0.5;
                    // 旧壳不对组成圆环的每个点单独做像素吸附。圆周点本来就落
                    // 在连续坐标上，强制取整会让相邻点忽近忽远、半径跳变，低
                    // DPI 下尤其容易显成锯齿珠链；交给 GPUI 统一抗锯齿才与
                    // `icons::push_spinner` 的几何一致。
                    window.paint_quad(
                        gpui::fill(
                            Bounds::new(gpui::point(px(x0), px(y0)), size(px(stroke), px(stroke))),
                            c,
                        )
                        .corner_radii(px(stroke * 0.5)),
                    );
                }
            },
        )
        .size(px(11.0))
    }

    fn tab_presentation(&self, ix: usize, cx: &App, dark: bool) -> TabPresentation {
        let active = ix == self.active;
        let title = self.tab_title(ix, cx);
        let is_settings = self.tabs[ix].is_settings();
        let is_terminal = self.tabs[ix].is_terminal();
        let (program, activity) = self.tabs[ix]
            .focused_view()
            .map(|entity| {
                let view = entity.read(cx);
                let program = view
                    .running_program
                    .clone()
                    .or_else(|| view.ai_session.as_ref().map(|identity| identity.source.clone()))
                    .or_else(|| view.ssh_destination.as_ref().map(|_| "ssh".to_owned()));
                (program, view.sidebar_activity())
            })
            .unwrap_or((None, SidebarActivity::Idle));
        let activity = if !active && self.meta(ix).has_bell && activity == SidebarActivity::Idle {
            SidebarActivity::Done
        } else {
            activity
        };
        let logo_image = program
            .as_deref()
            .and_then(crate::display::ai_logo_for_program)
            .and_then(|logo| self.sidebar_logo_images.get(&(logo, dark)).cloned());
        let program_glyph = program
            .as_deref()
            .filter(|_| logo_image.is_none())
            .map(crate::display::program_icon)
            .or_else(|| match &self.tabs[ix] {
                WorkspaceTab::Document { .. } => Some("\u{eb1d}"),
                WorkspaceTab::Code { view } => {
                    Some(crate::display::side_panel::file_type_icon(&view.read(cx).title))
                },
                WorkspaceTab::Image { view } => {
                    Some(crate::display::side_panel::file_type_icon(&view.read(cx).title))
                },
                _ => None,
            });
        let meta = self.meta(ix);
        let shell_tag = (is_terminal && activity == SidebarActivity::Idle)
            .then_some(meta.shell_tag.clone())
            .flatten()
            .filter(|tag| !tag.is_empty());
        let renaming = self
            .tab_rename
            .as_ref()
            .filter(|rename| rename.ix == ix)
            .map(|rename| rename.input.clone());
        TabPresentation {
            title,
            is_settings,
            activity,
            logo_image,
            program_glyph,
            shell_tag,
            color: meta.color,
            renaming,
        }
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // 数量 chip 数字：旧壳独用 ink_faint（比 ink_dim 再暗一档），chip 背
        // 景 surface 洗色不变——数字不该和标题抢层级。
        let faint = crate::gpui_shell::theme::faint_ink(cx);
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = theme.list_hover;
        let dark = theme.is_dark();
        // 运行程序图标是 Nerd Font 字位，chrome 的 UI 字体没有——用终端等
        // 宽字体渲染（旧壳 chrome 同理，其内置字体本就带全部图标字位）。
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let mono_family: SharedString = settings
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Cascadia Mono"))
            .into();
        // 旧壳合同（display/mod.rs `ui_font_px`）：chrome 锚定**配置字号**
        // （nebula.toml `font.size` 默认 11.25pt = 15px）。终端的持久化缩放
        // （`font_size=` 键）只影响终端网格，侧栏不得跟着变粗/变大；
        // 固定 14px 的旧毛病（比旧壳小一号）也不能回潮。
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        // 等宽 advance 的唯一事实源：与终端元素同款——塑形一个 "M" 量出来，
        // 而不是按 0.6em 猜。列数换算和省略号都建立在这个数上。
        let cell_w = self.sidebar_cell_width(window, &mono_family, label_px);
        // 旧壳标题是同一个 tracked run：`"{chevron}  TABS"`。T 的起点
        // 因而位于箭头起点之后三个完整字位（箭头自身 + 两个空格），不是
        // Tailwind `gap_2` 的固定 8px。按标题字号实测 advance，再补回旧壳
        // 0.65px/字的 tracking，字号和 DPI 改变时呼吸位仍保持同一比例。
        let section_title_cell_w =
            self.sidebar_cell_width(window, &mono_family, label_px * SIDEBAR_TITLE_SCALE);
        let tabs_disclosure_slot_w = (section_title_cell_w + 0.65) * 3.0;

        // 受约束拖拽的渲染参数：激活后被拖行骑指针位移，落点槽位由位移换算。
        let drag = self
            .tab_drag
            .as_ref()
            .filter(|d| d.active && d.axis == TabDragAxis::Vertical)
            .map(|d| (d.source, Self::drag_slot(d, self.tabs.len()), d.offset));

        // 本次渲染里是否有「运行中」行（spinner 帧循环的开关）。
        let items_running = std::cell::Cell::new(false);
        // 折叠只裁剪槽位，不改窗口算法：旧壳 `tabs_avail` 与 `tabs_open`
        // 分开——折起来时行矩形为零，但可用高度仍按面板剩余算。
        let (tabs_scroll, tabs_show) = self.tabs_visible_window();
        // 行的确定宽度：侧栏宽 − 侧栏 p_2 两边 − 列表右侧滚动条留白。
        // 与下面 `label_avail` 同一份减法口径，两者不能各算一套。
        let row_w =
            (self.sidebar_width - 16.0 - tab_scroll::TAB_SCROLL_GUTTER).max(1.0);
        let items = (0..self.tabs.len())
            .filter(|&ix| tab_scroll::index_visible(ix, tabs_scroll, tabs_show))
            .map(|ix| {
            let active = ix == self.active;
            let TabPresentation {
                title,
                is_settings,
                activity,
                logo_image,
                program_glyph,
                shell_tag,
                color: tab_color,
                renaming,
            } = self.tab_presentation(ix, cx, dark);
            let hover_group: SharedString = format!("sidebar-tab-hover-{ix}").into();
            // 可用列数 = （行宽 − 行内 px_2 − 行内 gap − 状态槽 − 行首图标槽）
            // ÷ cell 宽。基准取上面的 `row_w`（已扣掉侧栏 p_2 与滚动条留白），
            // 与行的实际宽度同源——否则算出的列数会比行能容纳的多出一格，
            // 截断后的标题反过来把行撑开。省略号由旧壳同一份
            // `truncate_tab_label` 追加，两壳的裁切位置因此一致。
            let has_icon = is_settings || logo_image.is_some() || program_glyph.is_some();
            let label_avail = row_w
                - 16.0
                - TAB_STATUS_SLOT_W
                - 8.0
                - if has_icon { TAB_LABEL_ICON_W + 8.0 } else { 0.0 };
            let label_cols = (label_avail / cell_w).floor().max(1.0) as usize;
            let title: SharedString = crate::display::truncate_tab_label(&title, label_cols).into();
            // 用户明确设置过的标签色：行左侧一条竖光条（旧壳 strip，位置与
            // 尺寸同源：左内缩 4、上下各留 7、宽 2.5）。默认标签不占这层
            // 视觉层级。
            let strip = tab_color.map(|color| gpui::Rgba {
                r: color.r as f32 / 255.0,
                g: color.g as f32 / 255.0,
                b: color.b as f32 / 255.0,
                a: 1.0,
            });
            let status_color = if active { active_fg } else { muted };
            let resting_status: Option<gpui::AnyElement> = match activity {
                SidebarActivity::Running => {
                    items_running.set(true);
                    let (track, head) =
                        crate::gpui_shell::theme::sidebar_spinner_colors(cx, active);
                    Some(Self::spinner(self.spinner_phase, track, head).into_any_element())
                },
                // 回合完成、等下一条指令：旧壳蓝点语义——不转圈，留一个
                // 「有结果没看」的痕迹。
                SidebarActivity::Done => {
                    Some(div().size(px(6.0)).rounded_full().bg(theme.primary).into_any_element())
                },
                // 停在授权/提问上：比「完成」更强，必须换形状而不是换色
                // （旧壳教训：两态共用圆点在界面上根本分不出来）。
                SidebarActivity::Attention => Some(
                    Icon::new(IconName::TriangleAlert)
                        .xsmall()
                        .text_color(theme.warning)
                        .into_any_element(),
                ),
                SidebarActivity::Failed => Some(
                    Icon::new(IconName::CircleX)
                        .xsmall()
                        .text_color(theme.danger)
                        .into_any_element(),
                ),
                SidebarActivity::Idle => shell_tag.map(|tag| {
                    div()
                        .font_family(mono_family.clone())
                        .text_size(px(label_px * SIDEBAR_TAG_SCALE))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(status_color)
                        .child(tag)
                        .into_any_element()
                }),
            };
            // 三类行位移（旧壳 tab_drag_draw_y 的语义）：被拖行骑指针，
            // 源与落点之间的行向反方向让一个槽位，其余不动。存储顺序在
            // 拖拽期间不变，释放时一次性提交。
            let (dragged, shift) = match drag {
                Some((src, _, _)) if ix == src => (true, 0.0),
                Some((src, tgt, _)) if src < tgt && ix > src && ix <= tgt => {
                    (false, -TAB_ROW_PITCH)
                },
                Some((src, tgt, _)) if src > tgt && ix >= tgt && ix < src => (false, TAB_ROW_PITCH),
                _ => (false, 0.0),
            };
            let row = h_flex()
                .id(("sidebar-tab", ix))
                .group(hover_group.clone())
                .relative()
                // 旧壳 `layout.tabs[i]` 的命中矩形覆盖整条可见行。
                //
                // 这里必须是**显式像素宽**，不能用 `w_full`：行在
                // `tab_scroll::wrap_tabs_scroll_list` 的 overflow_hidden 容器
                // 里，百分比宽度到不了这一层，会回落成 shrink-to-fit——表现
                // 就是行宽跟着文件名长短变，带图标的行还整体右移一个图标宽
                // （用户 08-19 报的侧栏 tab 宽度乱跳）。双击重命名"看起来正常"
                // 只是因为 Input 恰好把容器撑满，不是宽度真的对了。
                .w(px(row_w))
                // 内容再宽也不许把行撑开：截断后的标题若比测量值宽一两像素，
                // 撑开的行会重新引入上面那个症状。
                .overflow_hidden()
                .gap_2()
                .px_2()
                .h(px(TAB_ROW_H))
                .items_center()
                // 旧壳 pill 圆角 = UI_CORNER_RADIUS_LOGICAL(8)，rounded_md(6)
                // 偏小一圈，选中水洗的轮廓形状会不一样。
                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
                // GPUI 默认文本样式可能把侧栏整行带到中等/粗体；旧壳
                // tab chrome 使用终端 Regular face，所有子文本从这里继承常规字重。
                .font_weight(FontWeight::NORMAL)
                .cursor_pointer()
                .when(active, |item| item.bg(active_bg).text_color(active_fg))
                .when(!active && !dragged, |item| {
                    item.text_color(muted).hover(|style| style.bg(hover_bg))
                })
                .when(!active && dragged, |item| item.text_color(muted).bg(hover_bg))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_tab(ix, window, cx);
                }))
                // 受约束拖拽：按下待命（源下标 + 按点 Y），移动阈值与让位
                // 由 update_tab_drag 驱动；激活后的指针独占见 render 根部
                // 的透明罩层。
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.tab_drag = Some(TabDrag {
                            source: ix,
                            press_x: f32::from(event.position.x),
                            press_y: f32::from(event.position.y),
                            axis: TabDragAxis::Vertical,
                            pitch: TAB_ROW_PITCH,
                            offset: 0.0,
                            active: false,
                            dock: None,
                        });
                    }),
                )
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close_tab(ix, window, cx);
                    }),
                )
                .on_double_click(cx.listener(move |this, _, window, cx| {
                    // 旧壳 `ChromeHit::Tab` + DoubleClick → BeginRename。
                    cx.stop_propagation();
                    this.begin_rename(ix, window, cx);
                }))
                .when_some(strip, |row, color| {
                    row.child(
                        div()
                            .absolute()
                            .left(px(4.0))
                            .top(px(7.0))
                            .w(px(2.5))
                            .h(px(TAB_ROW_H - 14.0))
                            .rounded_full()
                            .bg(color),
                    )
                })
                .when(is_settings, |row| {
                    row.child(
                        div().w(px(TAB_LABEL_ICON_W)).flex_shrink_0().flex().justify_center().child(
                            Icon::new(IconName::Settings)
                                .small()
                                .text_color(if active { active_fg } else { muted }),
                        ),
                    )
                })
                .when_some(logo_image, |row, image| {
                    row.child(
                        img(image)
                            .size(px(TAB_LABEL_ICON_SIZE))
                            .flex_shrink_0()
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when_some(program_glyph, |row, glyph| {
                    row.child(
                        div()
                            .w(px(TAB_LABEL_ICON_W))
                            .flex_shrink_0()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(if active { active_fg } else { muted })
                            .child(glyph),
                    )
                })
                // 标签走终端字体 + 终端字号（旧壳 chrome 同源）。文本已按列
                // 截断，这里只需要不换行；再叠一层 `truncate()` 会把省略号
                // 自己裁掉（用户报的"直接截断"）。重命名中的那一行原地换成
                // 输入框：点进去不该触发选中/拖拽，所以自己吃掉 mouse_down。
                .child(match renaming {
                    Some(input) => div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.cancel_rename(window, cx);
                            }
                        }))
                        .child(
                            Input::new(&input)
                                .w_full()
                                .text_size(px(label_px))
                                .font_family(mono_family.clone()),
                        )
                        .into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono_family.clone())
                        .text_size(px(label_px))
                        // 标签标题本身使用 Light；活动态只换前景/背景色，
                        // 不再靠更粗字重强调，避免选中行看起来突然加粗。
                        .font_weight(FontWeight::LIGHT)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(title)
                        .into_any_element(),
                })
                .child(
                    div()
                        .relative()
                        .w(px(TAB_STATUS_SLOT_W))
                        .h_full()
                        .flex_shrink_0()
                        .when_some(resting_status, |slot, status| {
                            slot.child(
                                h_flex()
                                    .absolute()
                                    .inset_0()
                                    .justify_end()
                                    .items_center()
                                    .group_hover(hover_group.clone(), |item| item.invisible())
                                    .child(status),
                            )
                        })
                        .child(
                            // 关闭按钮跟状态徽章共用同一个居中槽位：位置由
                            // flex 给出，不再硬写 top 偏移。
                            h_flex()
                                .absolute()
                                .inset_0()
                                .justify_end()
                                .items_center()
                                .invisible()
                                .group_hover(hover_group, |slot| slot.visible())
                                .child(
                                    Button::new(("close-tab", ix))
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.request_close_tab(ix, window, cx);
                                        })),
                                ),
                        ),
                )
                // 右键只记锚点：菜单由 workspace 根上唯一一份宿主画。挂
                // `.context_menu()` 会让每个标签行都渲染同一个 PopupMenu，
                // 阴影按标签数叠厚（见 `tab_menu.rs` 模块头）。
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_tab_context_menu(ix, event.position, window, cx);
                    }),
                );
            if dragged {
                // 骑指针 + 提到最上层画（deferred 只延后绘制、不动布局），
                // 阴影给"拿起来"的抬升感。
                gpui::deferred(row.top(px(drag.map(|(_, _, off)| off).unwrap_or(0.0))).shadow_md())
                    .into_any_element()
            } else if shift != 0.0 {
                // 让位滑动：进位方向 ease-out 滑入（旧壳是双向弹簧；回位
                // 这里先直落，违和再补逐帧插值）。设置「标签动画=立即」时
                // 直接落位（旧壳 TabRevealMotion::Instant 的 Snap 语义）。
                if nebula_settings::RuntimeSettings::load().tab_reveal
                    == nebula_settings::TabRevealName::Instant
                {
                    row.top(px(shift)).into_any_element()
                } else {
                    row.with_animation(
                        ("tab-make-way", ix),
                        Animation::new(Duration::from_millis(120)).with_easing(ease_out_quint()),
                        move |row, t| row.top(px(shift * t)),
                    )
                    .into_any_element()
                }
            } else {
                row.into_any_element()
            }
        })
        .collect::<Vec<_>>();

        let header_group: SharedString = "sidebar-tabs-header-hover".into();
        let count: SharedString = self.tabs.len().to_string().into();

        let sidebar = v_flex()
            .w(px(self.sidebar_width))
            .h_full()
            .flex_shrink_0()
            // workspace 根保持透明，侧栏自己只铺一层壳色；否则终端卡会
            // 叠到根底色上，把 Acrylic 的目标透明度二次增浓。
            .bg(theme.background)
            .p_2()
            .gap_2()
            // 待命阶段（未过阈值）的指针跟踪；激活后由根部罩层独占接管。
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.update_tab_drag(event, window, cx);
            }))
            .child(
                h_flex()
                    .id("sidebar-tabs-toggle")
                    .group(header_group.clone())
                    .w_full()
                    .h(px(34.0))
                    .pb_1()
                    // 旧壳标题文字从 panel_x + 16px 起；侧栏根已有 8px
                    // padding，这里再补 8px，箭头不会贴住左边缘。
                    .pl_2()
                    .items_center()
                    .cursor_pointer()
                    // `ChromeHit::TabsSection` 命中整条 tabs_header。监听必须
                    // 挂在标题行根节点，右侧空白区同样可以折叠，而不是只有
                    // 箭头、标题和数量这一小段能点。
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_tabs_section(cx);
                    }))
                    .child(
                        // 箭头、TABS 和计数仍作为一个排版段；折叠命中已经
                        // 提升到外层整行。右侧 +/⋯ 自己停止冒泡。
                        h_flex()
                            .h_full()
                            .items_center()
                            .pr_1()
                            .child(
                                // 折叠三角用组件库线性 Chevron（lucide 细线），
                                // 不再用 Nerd Font 实心字位——后者在侧栏标题
                                // 上偏重、和右侧 +/⋯ 的 SVG 不一套语言。
                                h_flex()
                                    .w(px(tabs_disclosure_slot_w))
                                    .h_full()
                                    .flex_shrink_0()
                                    .items_center()
                                    .child(
                                        Icon::new(if self.tabs_section_collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        })
                                        .xsmall()
                                        .text_color(muted),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(muted)
                                    .child("TABS"),
                            )
                            .child(
                                // chip 尺寸贴旧壳公式：h = max(cell_h×0.82×1.18, 11)
                                // ≈ 18-19px（不是 22），1 位数宽 max(adv+8, h×1.25)。
                                // 数字 ink_faint。
                                h_flex()
                                    .ml_2()
                                    .h(px((label_px * 1.22 * SIDEBAR_TITLE_SCALE * 1.18).max(11.0)))
                                    .min_w(px(label_px * 0.62 + 8.0))
                                    .px_2()
                                    .justify_center()
                                    .items_center()
                                    .rounded_full()
                                    .bg(theme.muted)
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(faint)
                                    .child(count),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(2.0))
                            .child(
                                // 旧壳 `ChromeHit::NewTab`：直接开设置里的默认
                                // shell，不经过选择器。三点才是 NewTabMenu。
                                h_flex()
                                    .id("sidebar-new-tab")
                                    .size(px(SIDEBAR_PLUS_SIZE))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .invisible()
                                    .group_hover(header_group, |button| button.visible())
                                    .hover(|button| button.bg(hover_bg).text_color(theme.foreground))
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new(
                                            "新建终端 (Ctrl+Shift+T)",
                                        )
                                        .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.add_terminal(window, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::Plus).with_size(px(SIDEBAR_HEADER_ICON)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .id("sidebar-tabs-menu")
                                    .w(px(SIDEBAR_MENU_W))
                                    .h(px(SIDEBAR_PLUS_SIZE))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(|button| button.bg(hover_bg).text_color(theme.foreground))
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new("新建终端 (Ctrl+K)")
                                            .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_shell_palette(window, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::EllipsisVertical)
                                            .with_size(px(SIDEBAR_HEADER_ICON)),
                                    ),
                            ),
                    ),
            )
            .child(self.render_tabs_section(items, cx));
        // spinner 帧循环（旧壳 motion frame 的对应物）：有任何行在
        // 「运行中」时推进相位；notify → 下一次 render 再续帧。
        if items_running.get() {
            cx.on_next_frame(window, |this, _, cx| {
                let now = std::time::Instant::now();
                let dt = now - this.spinner_last;
                this.spinner_last = now;
                this.spinner_phase = (this.spinner_phase + dt.as_secs_f32() / 0.8).rem_euclid(1.0);
                cx.notify();
            });
        }
        sidebar
    }

    /// 与旧壳 `nebula_tabs_section_open` 同义：点标题整行折叠/展开。
    /// 卷帘只裁剪槽位，视口高度在动画期间冻结，避免量到裁剪高后把溢出
    /// 列表锁成一行滚动区。
    fn toggle_tabs_section(&mut self, cx: &mut Context<Self>) {
        self.tabs_section_collapsed = !self.tabs_section_collapsed;
        // 卷帘一动，菜单锚定的那一行就不在原处了。
        self.tab_menu = None;
        self.tabs_fold_armed = true;
        self.tabs_fold_seq = self.tabs_fold_seq.wrapping_add(1).max(1);
        let seq = self.tabs_fold_seq;
        self.tabs_fold_frozen = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update(cx, |this, cx| {
                if this.tabs_fold_seq == seq {
                    this.tabs_fold_frozen = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Tab 列表槽位。展开时列表是侧栏 `v_flex` 的 `flex_1` 子项，视口等于
    /// 面板剩余高度（旧壳 `tabs_avail`）。折叠动画只按**上次量到的剩余
    /// 高度**卷帘，绝不按全部行高，也不把裁剪高度写回窗口。
    fn render_tabs_section<I>(&self, items: I, cx: &mut Context<Self>) -> gpui::AnyElement
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        let collapsed = self.tabs_section_collapsed;
        let list = self.wrap_tabs_scroll_list(items.into_iter().map(|item| item.into_any_element()), cx);
        if collapsed && !self.tabs_fold_frozen {
            return div().into_any_element();
        }
        if !self.tabs_fold_armed || !self.tabs_fold_frozen {
            return if collapsed { div().into_any_element() } else { list };
        }
        let slot_h = self.tabs_viewport_h;
        let (from, to) = if collapsed { (slot_h, 0.0) } else { (0.0, slot_h) };
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(list)
            .with_animation(
                ("tabs-fold", collapsed as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| {
                    let height = from + (to - from) * t;
                    if !collapsed && t >= 1.0 {
                        slot
                    } else {
                        slot.max_h(px(height))
                    }
                },
            )
            .into_any_element()
    }

    /// 侧栏槽位：宽度在 0..持久化宽度间以 ease-out 滑动，近似旧壳
    /// response=0.14 的 swift-out 弹簧；内容保持固定宽、由槽位裁剪，
    /// 终端卡随 flex 布局自然滑移收编空间（对齐旧壳"卡骑在折叠动画上"
    /// 的观感）。动画按方向换 key 重启，端点随运行时设置变化。
    fn render_sidebar_slot(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let collapsed = self.sidebar_collapsed;
        if !self.sidebar_fold_armed {
            return if collapsed {
                div().into_any_element()
            } else {
                self.render_sidebar(window, cx).into_any_element()
            };
        }
        let width = self.sidebar_width;
        let (from, to) = if collapsed { (width, 0.0) } else { (0.0, width) };
        div()
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .child(self.render_sidebar(window, cx))
            .with_animation(
                ("sidebar-fold", collapsed as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| slot.w(px(from + (to - from) * t)),
            )
            .into_any_element()
    }

    /// Terminal tab 的内容区：单叶直渲、缩放态聚焦 pane 满卡，否则按
    /// 分屏树递归铺陈。
    fn render_terminal_tab(&self, tab_ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(WorkspaceTab::Terminal { tree, panes, focused, zoomed }) = self.tabs.get(tab_ix)
        else {
            return div().into_any_element();
        };
        if *zoomed || tree.is_leaf() {
            let view = panes
                .iter()
                .find(|pane| pane.id == *focused)
                .or_else(|| panes.first())
                .map(|pane| pane.view.clone());
            return div().size_full().children(view).into_any_element();
        }
        let mut path = Vec::new();
        self.render_split_node(tab_ix, tree, &mut path, panes, *focused, cx)
    }

    /// 递归渲染分屏树。Split 节点 = flex 容器：first 占 `relative(ratio)`、
    /// 分隔条固定 `DIVIDER_GAP`、second 吃剩余（与 `nebula_split::layout`
    /// 的切割数学等价到 ±divider×ratio ≤ 2px；比例在提交时按真实视口吸附
    /// 整格，不累积漂移）。节点视口与 pane 矩形经 canvas prepaint 回写
    /// 帧记录，供拖拽换算与方向导航消费。
    fn render_split_node(
        &self,
        tab_ix: usize,
        node: &SplitTree<u64>,
        path: &mut Vec<bool>,
        panes: &[TerminalPane],
        focused: u64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            SplitTree::Leaf(id) => {
                let id = *id;
                let view = panes.iter().find(|pane| pane.id == id).map(|pane| pane.view.clone());
                let store = self.pane_bounds.clone();
                let is_focused = id == focused;
                let dim = gpui::black().opacity(crate::display::NEBULA_UNFOCUSED_SPLIT_DIM);
                let veil = div().absolute().inset_0().bg(dim);
                div()
                    .size_full()
                    .relative()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                store.borrow_mut().insert(id, bounds);
                            },
                            |_, _, _, _| (),
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .children(view)
                    // 与旧壳一致：不用焦点描边，仅给非活动 pane 覆 30% 黑色 veil。
                    .when(!is_focused, |pane| pane.child(veil))
                    .into_any_element()
            },
            SplitTree::Split { direction, ratio, preview_ratio, dragging, first, second } => {
                let direction = *direction;
                let show_ratio = (*preview_ratio).unwrap_or(*ratio);
                let dragging = *dragging;
                let key = (tab_ix, path.clone());
                let store = self.split_bounds.clone();
                let probe = gpui::canvas(
                    move |bounds, _, _| {
                        store.borrow_mut().insert(key.clone(), bounds);
                    },
                    |_, _, _, _| (),
                )
                .absolute()
                .inset_0();

                path.push(false);
                let first_el = self.render_split_node(tab_ix, first, path, panes, focused, cx);
                path.pop();
                path.push(true);
                let second_el = self.render_split_node(tab_ix, second, path, panes, focused, cx);
                path.pop();

                let drag_path = path.clone();
                let divider_id =
                    SharedString::from(format!("split-divider-{tab_ix}-{drag_path:?}"));
                let bar_color =
                    if dragging { cx.theme().primary } else { cx.theme().border.opacity(0.35) };
                // 视觉线 DIVIDER_GAP，命中区向两侧各扩 HIT_SLOP（旧壳同参）。
                let hit = div()
                    .id(divider_id)
                    .absolute()
                    .map(|hit| match direction {
                        SplitDirection::LeftRight => hit
                            .top_0()
                            .bottom_0()
                            .left(px(-HIT_SLOP))
                            .w(px(DIVIDER_GAP + HIT_SLOP * 2.0))
                            .cursor_col_resize(),
                        SplitDirection::TopBottom => hit
                            .left_0()
                            .right_0()
                            .top(px(-HIT_SLOP))
                            .h(px(DIVIDER_GAP + HIT_SLOP * 2.0))
                            .cursor_row_resize(),
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.split_drag =
                                Some(SplitDrag { tab: tab_ix, path: drag_path.clone(), direction });
                            cx.notify();
                        }),
                    );
                let divider = div()
                    .relative()
                    .flex_shrink_0()
                    .map(|bar| match direction {
                        SplitDirection::LeftRight => bar.w(px(DIVIDER_GAP)).h_full(),
                        SplitDirection::TopBottom => bar.h(px(DIVIDER_GAP)).w_full(),
                    })
                    .bg(bar_color)
                    .child(hit);

                div()
                    .size_full()
                    .relative()
                    .flex()
                    .map(|el| match direction {
                        SplitDirection::LeftRight => el.flex_row(),
                        SplitDirection::TopBottom => el.flex_col(),
                    })
                    .child(probe)
                    .child(
                        div()
                            .min_w_0()
                            .min_h_0()
                            .map(|el| match direction {
                                SplitDirection::LeftRight => el.h_full().w(relative(show_ratio)),
                                SplitDirection::TopBottom => el.w_full().h(relative(show_ratio)),
                            })
                            .child(first_el),
                    )
                    .child(divider)
                    .child(div().flex_1().min_w_0().min_h_0().child(second_el))
                    .into_any_element()
            },
        }
    }

    /// 分隔条拖拽的指针移动：指针位置 → 原始比例 → 预览曲线（常规带跟手、
    /// 关闭区钉边示意"松手即关"）。PTY 尺寸不追预览——`set_layout` 的
    /// resize 合并合同把拖拽期间的矩形变化收敛到松手后一次提交。
    fn update_split_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &self.split_drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            let position = event.position;
            self.finish_split_drag(position, window, cx);
            return;
        }
        let raw = match self.split_drag_raw_ratio(event.position, window, cx) {
            Some(raw) => raw,
            None => return,
        };
        let (tab, path) = (drag.tab, drag.path.clone());
        if let Some(WorkspaceTab::Terminal { tree, .. }) = self.tabs.get_mut(tab) {
            if let Some(SplitTree::Split { preview_ratio, dragging, .. }) = tree.node_mut(&path) {
                *preview_ratio = Some(nebula_split::preview_ratio(raw));
                *dragging = true;
                cx.notify();
            }
        }
    }

    /// 当前拖拽的指针位置 → 未钳制原始比例（依赖上一帧节点视口记录）。
    fn split_drag_raw_ratio(
        &self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<f32> {
        let drag = self.split_drag.as_ref()?;
        let viewport = self.split_bounds.borrow().get(&(drag.tab, drag.path.clone())).copied()?;
        let (cell_w, line_h) = TerminalView::cell_metrics(window, cx);
        let cell = match drag.direction {
            SplitDirection::LeftRight => f32::from(cell_w),
            SplitDirection::TopBottom => f32::from(line_h),
        };
        Some(nebula_split::drag_ratio(
            drag.direction,
            to_split_rect(&viewport),
            DIVIDER_GAP,
            cell,
            f32::from(position.x),
            f32::from(position.y),
        ))
    }

    /// 松手：关闭区按被挤压侧收尾（仅当那一侧是单叶——子树不做整树误杀，
    /// 按钳制带提交）；常规带比例吸附整数单元格后写回提交值。
    fn finish_split_drag(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw = self.split_drag_raw_ratio(position, window, cx);
        let Some(drag) = self.split_drag.take() else { return };
        let viewport = self.split_bounds.borrow().get(&(drag.tab, drag.path.clone())).copied();
        let (cell_w, line_h) = TerminalView::cell_metrics(window, cx);
        let cell = match drag.direction {
            SplitDirection::LeftRight => f32::from(cell_w),
            SplitDirection::TopBottom => f32::from(line_h),
        };

        // 先裁定拖拽关闭；借用两段式（close_pane 需要 &mut self）。
        let mut close_target: Option<u64> = None;
        if let (Some(raw), Some(WorkspaceTab::Terminal { tree, .. })) =
            (raw, self.tabs.get_mut(drag.tab))
        {
            if let Some(squeezed_second) = nebula_split::drag_close_target(raw) {
                if let Some(SplitTree::Split { first, second, .. }) = tree.node_mut(&drag.path) {
                    let child = if squeezed_second { second.as_ref() } else { first.as_ref() };
                    if let SplitTree::Leaf(id) = child {
                        close_target = Some(*id);
                    }
                }
            }
        }
        if let Some(pane_id) = close_target {
            // remove_leaf 会顺带塌缩本 Split 节点；预览状态随节点一起消亡。
            self.request_close_pane(drag.tab, pane_id, window, cx);
            cx.notify();
            return;
        }

        if let Some(WorkspaceTab::Terminal { tree, .. }) = self.tabs.get_mut(drag.tab) {
            if let Some(SplitTree::Split { ratio, preview_ratio, dragging, .. }) =
                tree.node_mut(&drag.path)
            {
                if let (Some(raw), Some(viewport)) = (raw, viewport) {
                    let vp = to_split_rect(&viewport);
                    let extent = match drag.direction {
                        SplitDirection::LeftRight => vp.w,
                        SplitDirection::TopBottom => vp.h,
                    };
                    *ratio = nebula_split::commit_ratio(
                        nebula_split::preview_ratio(raw),
                        extent,
                        DIVIDER_GAP,
                        cell,
                    );
                }
                *preview_ratio = None;
                *dragging = false;
            }
        }
        cx.notify();
    }
}

impl Drop for NebulaWorkspace {
    fn drop(&mut self) {
        // 正常退出的最后一笔：把最近的自动保存快照标上 `clean_exit` 落盘
        // （关窗/退出应用都会走到；强杀/断电走不到，崩溃判定据此）。
        // panic 展开也会进 Drop——那是崩溃现场，不能标成正常退出。
        if std::thread::panicking() {
            return;
        }
        if let Some(mut session) = self.last_saved_session.take() {
            crate::session::save_final(&mut session);
        }
    }
}

impl Render for NebulaWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_logo_target_px =
            (TAB_LABEL_ICON_SIZE * window.scale_factor()).round().max(1.0) as u32;
        if sidebar_logo_target_px != self.sidebar_logo_target_px {
            // GPUI 窗口可跨不同 DPI 的显示器；原纹理只在整数物理像素尺寸
            // 变化时重建，普通 render 不重复解码 PNG。
            self.sidebar_logo_images = sidebar_logo_images(sidebar_logo_target_px);
            self.sidebar_logo_target_px = sidebar_logo_target_px;
        }
        let content: Option<gpui::AnyElement> = match self.tabs.get(self.active) {
            Some(WorkspaceTab::Terminal { .. }) => Some(self.render_terminal_tab(self.active, cx)),
            Some(WorkspaceTab::Settings { view, .. }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            Some(WorkspaceTab::Image { view }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            Some(WorkspaceTab::Document { view }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            Some(WorkspaceTab::Code { view }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            None => None,
        };
        let files_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Files;
        let git_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Git;
        let settings_active =
            matches!(self.tabs.get(self.active), Some(WorkspaceTab::Settings { .. }));
        let top_tabs = self.tabs_position == nebula_settings::TabsPositionName::Top;
        // dock 预览：被拖 tab 悬于终端区时高亮目标半区（松手即挂到那侧）。
        let dock_preview =
            self.tab_drag.as_ref().filter(|drag| drag.active).and_then(|drag| drag.dock).and_then(
                |nav| {
                    self.active_terminal_area().map(|area| match nav {
                        SplitNav::Left => (area.x, area.y, area.w * 0.5, area.h),
                        SplitNav::Right => (area.x + area.w * 0.5, area.y, area.w * 0.5, area.h),
                        SplitNav::Up => (area.x, area.y, area.w, area.h * 0.5),
                        SplitNav::Down => (area.x, area.y + area.h * 0.5, area.w, area.h * 0.5),
                    })
                },
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            // 旧壳每个像素只画一次：整窗先透明清屏，再分别画标题栏、侧栏、
            // 卡外壳与卡底。根节点不能再铺一张半透明底。
            .bg(gpui::transparent_black())
            .text_color(cx.theme().foreground)
            // 兜底：未过阈值的按-放在任意位置松开都清掉待命拖拽
            //（点击语义不受影响——行的 on_click 在冒泡链更早触发）。
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.tab_drag.as_ref().is_some_and(|d| !d.active) {
                        this.tab_drag = None;
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.add_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseActiveTerminal, window, cx| {
                this.close_active(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                this.sidebar_fold_armed = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseCommandPalette, window, cx| {
                this.close_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleShellPicker, window, cx| {
                this.toggle_shell_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CommandPaletteUp, _, cx| {
                this.move_command_palette_selection(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &CommandPaletteDown, _, cx| {
                this.move_command_palette_selection(1, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFileTree, _, cx| {
                this.toggle_file_tree(cx);
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_focused(SplitDirection::LeftRight, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_focused(SplitDirection::TopBottom, window, cx);
            }))
            .on_action(cx.listener(|this, _: &RenameActiveTab, window, cx| {
                let ix = this.active;
                this.begin_rename(ix, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| {
                this.toggle_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
                this.navigate_pane(SplitNav::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
                this.navigate_pane(SplitNav::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
                this.navigate_pane(SplitNav::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
                this.navigate_pane(SplitNav::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextTab, window, cx| {
                this.select_adjacent_tab(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousTab, window, cx| {
                this.select_adjacent_tab(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleGitPanel, _, cx| {
                this.toggle_git_tree(cx);
            }))
            .on_action(cx.listener(|this, _: &IncreaseFontSize, _, cx| {
                this.bump_font_size(1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &DecreaseFontSize, _, cx| {
                this.bump_font_size(-1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &ResetFontSize, _, cx| {
                this.bump_font_size(0.0, cx);
            }))
            .on_action(cx.listener(|this, _: &CopySelection, window, cx| {
                this.copy_focused_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PasteClipboard, window, cx| {
                this.paste_focused_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFullscreen, window, _cx| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &OpenQuickJump, window, cx| {
                this.open_ai_session_palette(window, cx);
            }))
            .child(
                // 壁纸"铺满整窗"底层（chrome 之下）：卡外区域由这层负责，
                // 卡内切片由终端元素在卡底色之上重画（旧壳同一层模型）。
                gpui::canvas(
                    |_, _, _| (),
                    |bounds, _, window, cx| {
                        crate::gpui_shell::wallpaper::paint_wallpaper_under_chrome(
                            bounds, window, cx,
                        );
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                // 标题栏左上与旧壳同构：侧栏开关 + 齿轮，两枚裸图标，无应用名
                // 文案。图标用静态 PanelLeft（旧壳折叠/展开共用同一枚方块标）。
                //
                // 组件库默认 TitleBar 只有 34px，而普通 Button 的命中块是
                // 32px，上下各剩 1px，视觉上就会紧贴窗口顶边。旧壳合同是
                // 8px 外边距 + 40px 标题带，总高 48px；显式覆写后按钮垂直
                // 居中，上下各留 8px，且右侧窗口控制仍共享同一标题带。
                TitleBar::new()
                    .h(px(48.0))
                    .when(top_tabs, |bar| {
                        bar.child(self.render_top_title_bar(
                            files_active,
                            git_active,
                            settings_active,
                            window,
                            cx,
                        ))
                    })
                    .when(!top_tabs, |bar| {
                        bar.child(
                            h_flex()
                                // 旧壳两枚 32px 命中块之间固定留 8px；默认 Button
                                // 正好是 32px，`.small()` 会把热区缩成 24px。
                                .gap_2()
                                .items_center()
                                .occlude()
                                .child(
                                    Button::new("toggle-sidebar")
                                        .icon(IconName::PanelLeft)
                                        .ghost()
                                        // 侧栏是开关而非一次性动作：展开期间必须持续
                                        // 显示选中底，和旧壳 `left_sidebar_visible()` 同义。
                                        .selected(!self.sidebar_collapsed)
                                        .tooltip("折叠/展开侧边栏 (Ctrl+Shift+B)")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sidebar_collapsed = !this.sidebar_collapsed;
                                            this.sidebar_fold_armed = true;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("open-settings")
                                        .icon(IconName::Settings)
                                        .ghost()
                                        .selected(settings_active)
                                        .tooltip("设置 (Ctrl+,)")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_settings(window, cx);
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .h_full()
                                .items_center()
                                .gap_2()
                                .occlude()
                                .child(
                                    Button::new("toggle-file-tree")
                                        .icon(if files_active {
                                            IconName::FolderOpen
                                        } else {
                                            IconName::FolderClosed
                                        })
                                        .ghost()
                                        .selected(files_active)
                                        .tooltip("目录树 (Ctrl+Shift+F)")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_file_tree(cx);
                                        })),
                                )
                                .child(
                                    Button::new("toggle-git-tree")
                                        .icon(IconName::GitHub)
                                        .ghost()
                                        .selected(git_active)
                                        .tooltip("Git 状态 (Ctrl+Shift+G)")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_git_tree(cx);
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                // 不用 h_flex：它默认 items_center，会把子项高度压成内容高度。
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .when(!top_tabs, |row| {
                        row.child(self.render_sidebar_slot(window, cx)).when(
                            // 侧栏拖宽热区（旧壳 `panel_resize` 设置门控）：贴在
                            // 侧栏右缘、零布局宽，不挤压终端卡。
                            !self.sidebar_collapsed
                                && nebula_settings::RuntimeSettings::load().panel_resize,
                            |row| {
                                row.child(
                                    div().relative().w_0().h_full().flex_shrink_0().child(
                                        div()
                                            .id("sidebar-resize-handle")
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left(px(-3.0))
                                            .w(px(6.0))
                                            .cursor_col_resize()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.sidebar_resizing = true;
                                                    cx.notify();
                                                }),
                                            ),
                                    ),
                                )
                            },
                        )
                    })
                    .child(
                        // 终端卡（一体化外壳）：唯一的结构分界。圆角与旧壳卡
                        // 同源（UI_SHELL_RADIUS_LOGICAL=14），无描边——融合靠
                        // 壳色包围圆角卡本身，不靠线框。侧栏模式四边保留旧壳
                        // 8px 卡缝；顶部 tab 模式取消上边距，让 tab 底边与正文
                        // 直接相接，左右和底部卡缝不变。
                        div()
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .child(
                                gpui::canvas(
                                    |_, _, _| (),
                                    |bounds, _, window, cx| {
                                        crate::gpui_shell::theme::paint_shell_around_card(
                                            bounds, window, cx,
                                        );
                                    },
                                )
                                .absolute()
                                .inset_0(),
                            )
                            .when(top_tabs, |card| card.px_2().pb(px(8.0)))
                            .when(!top_tabs, |card| card.p_2())
                            .child(
                            div()
                                .size_full()
                                .rounded(crate::gpui_shell::theme::card_radius())
                                .bg(crate::gpui_shell::theme::card_content_bg(cx))
                                .overflow_hidden()
                                .child(
                                    // 壁纸层（卡底色之上、内容之下，覆盖整卡含
                                    // 内边距带）：卡模式按卡定位，铺满整窗模式画
                                    // 窗口锚定的卡内切片。
                                    gpui::canvas(
                                        |_, _, _| (),
                                        |bounds, _, window, cx| {
                                            crate::gpui_shell::wallpaper::paint_wallpaper_card(
                                                bounds, window, cx,
                                            );
                                        },
                                    )
                                    .absolute()
                                    .inset_0(),
                                )
                                .children(content),
                        ),
                    )
                    .child(self.render_side_panel_slot(cx)),
            )
            .when_some(dock_preview, |root, (x, y, w, h)| {
                root.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(w))
                        .h(px(h))
                        .rounded(crate::gpui_shell::theme::card_radius())
                        .border_2()
                        .border_color(cx.theme().primary)
                        .bg(cx.theme().primary.opacity(0.15)),
                )
            })
            .when(self.tab_drag.as_ref().is_some_and(|d| d.active), |root| {
                // 拖拽激活期间的全窗透明罩层：独占指针（occlude 挡掉下层
                // 命中），移动喂状态机、松开提交落位——等效指针捕获，指针
                // 划出侧栏甚至划到终端上都不会丢拖拽。
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_move(cx.listener(|this, event, window, cx| {
                            this.update_tab_drag(event, window, cx);
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.finish_tab_drag(window, cx);
                            }),
                        ),
                )
            })
            .children(self.tabs_scrollbar_drag_overlay(cx))
            .when(self.sidebar_resizing, |root| {
                // 侧栏拖宽罩层（同 split_drag 的指针捕获模式）：移动实时改宽
                // （夹在共享层的 170..420 之间），松手写盘 `sidebar_w`。
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .cursor_col_resize()
                        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                            let width = f32::from(event.position.x).clamp(
                                nebula_settings::MIN_SIDEBAR_WIDTH,
                                nebula_settings::MAX_SIDEBAR_WIDTH,
                            );
                            if (width - this.sidebar_width).abs() >= 0.5 {
                                this.sidebar_width = width;
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.sidebar_resizing = false;
                                if let Err(err) = nebula_settings::persist_keys(&[(
                                    "sidebar_w",
                                    format!("{:.0}", this.sidebar_width),
                                )]) {
                                    log::warn!("持久化侧栏宽度失败: {err}");
                                }
                                cx.notify();
                            }),
                        ),
                )
            })
            .when_some(
                self.split_drag.as_ref().map(|drag| drag.direction),
                |root, direction| {
                    // 分隔条拖拽罩层（同 tab_drag 的指针捕获模式）：整窗
                    // 保持 resize 光标，移动喂预览、松开提交/关闭。
                    root.child(
                        div()
                            .absolute()
                            .inset_0()
                            .occlude()
                            .map(|mask| match direction {
                                SplitDirection::LeftRight => mask.cursor_col_resize(),
                                SplitDirection::TopBottom => mask.cursor_row_resize(),
                            })
                            .on_mouse_move(cx.listener(|this, event, window, cx| {
                                this.update_split_drag(event, window, cx);
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, event: &gpui::MouseUpEvent, window, cx| {
                                    this.finish_split_drag(event.position, window, cx);
                                }),
                            ),
                    )
                },
            )
            .when(self.command_palette_open, |root| {
                root.child(self.render_command_palette(cx))
            })
            .when_some(self.render_file_tree_context_menu(), |root, menu| {
                root.child(menu)
            })
            .when_some(self.render_tab_context_menu(), |root, menu| root.child(menu))
            // 组件库的模态/通知层不会自己上屏：`Root::render` 只画宿主视图，
            // dialog/notification 两层由宿主显式挂。挂在最外层链尾＝盖住命令
            // 面板和所有拖拽罩层；dialog 在下、notification 在上，确认框弹着
            // 时仍看得见 toast。
            //
            // 少了这两行，`window.open_dialog` 只会把模态推进 `Root` 并抢走
            // 焦点而不画任何东西——终端看着就像卡死了。
            .children(Root::render_dialog_layer(window, cx))
            .children(crate::gpui_shell::toast::render_layer(window, cx))
    }
}

#[cfg(test)]
mod tests;
