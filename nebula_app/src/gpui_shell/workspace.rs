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
    Window, canvas, div, ease_out_quint, fill, img, px, relative, size,
};
use image::Frame;

use crate::display::color::Rgb;
use crate::gpui_shell::code_tab::CodeTabViewEvent;
use crate::gpui_shell::doc_tabs::DocTabViewEvent;
use crate::gpui_shell::prelude::*;
use crate::gpui_shell::settings_pane::{SettingsPane, SettingsPaneEvent};
use crate::gpui_shell::terminal::view::{SidebarActivity, TerminalView, TerminalViewEvent};
use gpui_component::Root;
use gpui_component::input::InputEvent;
use gpui_component::notification::Notification;
use nebula_split::{DIVIDER_GAP, HIT_SLOP, RemoveOutcome, SplitDirection, SplitNav, SplitTree};

mod agents;
mod command_manager;
mod file_tree;
mod key_actions;
mod notifications;
mod palette;
mod pane_header;
mod quick_jump;
mod quick_terminal;
mod remote_files;
mod residency;
mod send_to_chat;
mod session_persistence;
mod settings_navigation;
mod sidebar;
mod ssh_dialog;
mod tab_drag;
mod tab_menu;
mod tab_scroll;
mod top_tabs;
mod update_dialog;
mod vcs_panel;
mod window_titlebar;
pub(crate) mod windowing;

// 调用点分散在设置页与窗口层，原样再导出以免拆分波及它们。
pub(crate) use update_dialog::{open_update_dialog, show_update_notification};

use tab_drag::{TabDrag, TabDragAxis};

#[cfg(test)]
use agents::ai_hook_target_pane;
use agents::{ai_session_palette_rows, restored_agent_command};

gpui::actions!(
    nebula_workspace,
    [
        NewTerminal,
        NewWindow,
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
        MoveTabLeft,
        MoveTabRight,
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

/// 侧栏拖宽热区的宽度。热区中心由 [`sidebar_resize_offset_for`] 决定。
const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 6.0;

/// 侧栏槽位右缘到「用户眼里那条分界」的距离，热区与拖拽换算都用它。
///
/// 两种形态的分界不在同一个位置：主题画了竖线（Nord 这类铺满布局，卡缝 0 +
/// 1px 竖线）时分界就是那条线，它贴在槽位右缘上；没画线的浮起圆角卡（卡缝 8
/// + 无竖线）里分界是卡缝右侧的卡可见左缘。原来这里写死 8.0 只对后者成立，
/// Nord 成为出厂默认之后热区整整偏右 7.5px——鼠标停在线上不变形，得往右挪
/// 半个字符宽才拖得动。
fn sidebar_resize_offset_for(divider: f32, gutter: f32) -> f32 {
    if divider > 0.0 { divider * 0.5 } else { gutter }
}

fn sidebar_resize_visual_offset(cx: &App) -> f32 {
    let card = crate::gpui_shell::theme::PaneCardStyle::current(cx);
    sidebar_resize_offset_for(card.divider, card.margin.left)
}

/// 标题栏里的文件树 / Git 工具必须同时挡住原生拖窗命中和父级拖拽起手。
/// `occlude` 只屏蔽后方 hitbox，不会阻止 MouseDown 向 `TitleBar` 冒泡。
fn title_bar_panel_controls() -> gpui::Div {
    h_flex()
        .h_full()
        .items_center()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// 工作区静态默认绑定的 combo 集（[`init`] 的镜像）。撤销已失效的自定义
/// 注入时要排除：gpui 的 NoAction 打在静态默认键上会误杀基础功能。
const STATIC_DEFAULT_COMBOS: &[&str] = &[
    "ctrl-shift-t",
    "ctrl-shift-e",
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
    "ctrl-shift-pageup",
    "ctrl-shift-pagedown",
    "ctrl-shift-g",
    "ctrl-=",
    "ctrl-+",
    "ctrl--",
    "ctrl-0",
    "ctrl-shift-c",
    #[cfg(not(target_os = "macos"))]
    "ctrl-c",
    "ctrl-v",
    "ctrl-shift-v",
    "alt-enter",
    "ctrl-shift-o",
];

/// `keybind=` 自定义表（两壳共读）中 config::Action → GPUI 工作区动作的
/// 映射。仍跳过未接线的动作（prompt 跳转、搜索）：编辑器可读写，这里不
/// 注入。`CreateNewWindow` 复用同一个 GPUI App 和进程级 hook/runtime。
fn custom_workspace_binding(combo: &str, action: &crate::config::Action) -> Option<KeyBinding> {
    use crate::config::Action;
    let combo = gpui_binding_combo(combo);
    match action {
        Action::ToggleCommandPalette => Some(KeyBinding::new(&combo, ToggleCommandPalette, None)),
        Action::ToggleShellPicker => Some(KeyBinding::new(&combo, ToggleShellPicker, None)),
        Action::CreateNewTab => Some(KeyBinding::new(&combo, NewTerminal, None)),
        Action::CreateNewWindow => Some(KeyBinding::new(&combo, NewWindow, None)),
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
        Action::Copy => Some(KeyBinding::new(
            &combo,
            CopySelection,
            Some(crate::gpui_shell::terminal::KEY_CONTEXT),
        )),
        Action::Paste => Some(KeyBinding::new(
            &combo,
            PasteClipboard,
            Some(crate::gpui_shell::terminal::KEY_CONTEXT),
        )),
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
    cx.bind_keys(default_workspace_bindings());
    #[cfg(target_os = "macos")]
    bind_macos_command_keys(cx);
}

/// 工作区静态默认键位表；与 [`STATIC_DEFAULT_COMBOS`] 互为镜像。
fn default_workspace_bindings() -> Vec<KeyBinding> {
    [
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-e", NewWindow, None),
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
        // 标签位置左右移动（WT 的 moveTab forward/backward；键位取 VS Code
        // 的「移动编辑器」同构）。我们自己的回滚翻页只吃**不带 ctrl** 的
        // shift+pageup（view.rs 的滚动分支），两者不冲突。
        KeyBinding::new("ctrl-shift-pageup", MoveTabLeft, None),
        KeyBinding::new("ctrl-shift-pagedown", MoveTabRight, None),
        KeyBinding::new("ctrl-shift-g", ToggleGitPanel, None),
        KeyBinding::new("ctrl-=", IncreaseFontSize, None),
        KeyBinding::new("ctrl-+", IncreaseFontSize, None),
        KeyBinding::new("ctrl--", DecreaseFontSize, None),
        KeyBinding::new("ctrl-0", ResetFontSize, None),
        KeyBinding::new("ctrl-shift-c", CopySelection, None),
        // 复制优先（WT 语义）：终端聚焦时有选区复制并清选区，无选区经 handler
        // 的 `cx.propagate()` 落成 ^C。带终端上下文，重命名/输入框聚焦时
        // ctrl+c 归输入框自己（Input -> Copy）。
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", CopySelection, Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        // 终端粘贴只在终端焦点路径命中。Input 自己带 `Input -> Paste`；这里若
        // 无上下文，会因注册更晚而抢走弹窗/设置页输入框的 Ctrl+V。
        KeyBinding::new("ctrl-v", PasteClipboard, Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        KeyBinding::new(
            "ctrl-shift-v",
            PasteClipboard,
            Some(crate::gpui_shell::terminal::KEY_CONTEXT),
        ),
        KeyBinding::new("ctrl-shift-v", gpui_component::input::Paste, Some("Input")),
        KeyBinding::new("alt-enter", ToggleFullscreen, None),
        KeyBinding::new("ctrl-shift-o", OpenQuickJump, None),
    ]
    .into()
}

/// macOS 的原生修饰键是 ⌘：在 Ctrl 绑定之外**追加**一套 ⌘ 绑定，不替换。
/// 追加而非替换有两个原因：Ctrl+Shift 组合在 Mac 终端里没有别的含义，留着
/// 不碍事；而 ⌘C/⌘V 必须存在，否则 Mac 用户第一反应就是「复制粘贴坏了」。
/// 终端里的 Ctrl+C 仍然是 SIGINT——这里只绑 ⌘，不碰 Ctrl 的语义。
#[cfg(target_os = "macos")]
fn bind_macos_command_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTerminal, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-w", CloseActiveTerminal, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-shift-p", ToggleCommandPalette, None),
        KeyBinding::new("cmd-k", ToggleShellPicker, None),
        KeyBinding::new("cmd-shift-f", ToggleFileTree, None),
        KeyBinding::new("cmd-d", SplitRight, None),
        KeyBinding::new("cmd-shift-d", SplitDown, None),
        KeyBinding::new("cmd-shift-enter", ToggleZoom, None),
        KeyBinding::new("cmd-alt-left", FocusPaneLeft, None),
        KeyBinding::new("cmd-alt-right", FocusPaneRight, None),
        KeyBinding::new("cmd-alt-up", FocusPaneUp, None),
        KeyBinding::new("cmd-alt-down", FocusPaneDown, None),
        KeyBinding::new("cmd-shift-]", SelectNextTab, None),
        KeyBinding::new("cmd-shift-[", SelectPreviousTab, None),
        KeyBinding::new("cmd-shift-g", ToggleGitPanel, None),
        KeyBinding::new("cmd-=", IncreaseFontSize, None),
        KeyBinding::new("cmd-+", IncreaseFontSize, None),
        KeyBinding::new("cmd--", DecreaseFontSize, None),
        KeyBinding::new("cmd-0", ResetFontSize, None),
        KeyBinding::new("cmd-c", CopySelection, Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        KeyBinding::new("cmd-v", PasteClipboard, Some(crate::gpui_shell::terminal::KEY_CONTEXT)),
        KeyBinding::new("cmd-v", gpui_component::input::Paste, Some("Input")),
        KeyBinding::new("cmd-ctrl-f", ToggleFullscreen, None),
        KeyBinding::new("cmd-shift-o", OpenQuickJump, None),
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
        /// 广播输入：开启后聚焦 pane 的击键/文本同步到本 tab 其余 pane。
        /// 只活在内存里——绝不写进 session 快照，重启不该带回一个看不见的
        /// 「打一个字进四个 shell」模式。收敛到单 pane 时自动关。
        broadcast: bool,
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
        _subscription: Subscription,
    },
    /// 源码查看 tab（tree-sitter 高亮 + 行级虚拟化，只读）。
    Code {
        view: Entity<crate::gpui_shell::code_tab::CodeTabView>,
        _subscription: Subscription,
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
    /// Lightweight visual preview. The pane flex tree keeps using its committed
    /// ratio until mouse-up, so dragging never reflows terminal grids.
    preview_ratio: f32,
    close_target: Option<bool>,
    last_notified: std::time::Instant,
}

/// Split 视口 / pane 矩形的帧记录（canvas prepaint 回写，`Rc<RefCell>`
/// 避免 paint 阶段反向 update 实体）。键：(tab 下标, 节点路径)。
type SplitBoundsStore = Rc<RefCell<HashMap<(usize, Vec<bool>), Bounds<Pixels>>>>;
/// 键：pane id（方向导航要拿所有叶子的屏幕矩形算最近邻）。
type PaneBoundsStore = Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>;

fn decode_sidebar_logo(
    logo: crate::display::AiLogo,
    dark: bool,
    target_size: u32,
) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(logo.png(dark)).ok()?.into_rgba8();
    logo.tint_pixels(&mut rgba, if dark { [236, 239, 245] } else { [35, 40, 50] });
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
    for logo in AiLogo::ALL {
        for dark in [false, true] {
            if let Some(image) = decode_sidebar_logo(logo, dark, target_size) {
                images.insert((logo, dark), image);
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct SplitDragVisual {
    divider: nebula_split::Rect,
    close_area: Option<nebula_split::Rect>,
}

/// Compute the cheap split-drag overlay without changing either pane's layout.
/// `close_target` uses [`nebula_split::drag_close_target`] semantics: `false`
/// is the first (left/top) child, `true` the second (right/bottom) child.
fn split_drag_visual_geometry(
    direction: SplitDirection,
    viewport: nebula_split::Rect,
    preview_ratio: f32,
    close_target: Option<bool>,
) -> SplitDragVisual {
    let usable = match direction {
        SplitDirection::LeftRight => (viewport.w - DIVIDER_GAP).max(0.0),
        SplitDirection::TopBottom => (viewport.h - DIVIDER_GAP).max(0.0),
    };
    let split = usable * preview_ratio.clamp(0.0, 1.0);
    let divider = match direction {
        SplitDirection::LeftRight => {
            nebula_split::Rect::new(viewport.x + split, viewport.y, DIVIDER_GAP, viewport.h)
        },
        SplitDirection::TopBottom => {
            nebula_split::Rect::new(viewport.x, viewport.y + split, viewport.w, DIVIDER_GAP)
        },
    };
    let close_area = close_target.map(|second| match (direction, second) {
        (SplitDirection::LeftRight, false) => {
            nebula_split::Rect::new(viewport.x, viewport.y, split, viewport.h)
        },
        (SplitDirection::LeftRight, true) => nebula_split::Rect::new(
            divider.x + divider.w,
            viewport.y,
            (viewport.w - split - divider.w).max(0.0),
            viewport.h,
        ),
        (SplitDirection::TopBottom, false) => {
            nebula_split::Rect::new(viewport.x, viewport.y, viewport.w, split)
        },
        (SplitDirection::TopBottom, true) => nebula_split::Rect::new(
            viewport.x,
            divider.y + divider.h,
            viewport.w,
            (viewport.h - split - divider.h).max(0.0),
        ),
    });
    SplitDragVisual { divider, close_area }
}

/// 侧栏分界线在终端卡 canvas 里绘制，但视觉上属于整个窗口 chrome。
/// 返回窗口坐标中的一条连续矩形，避免标题栏与正文各画一段产生缩放接缝。
fn pane_card_divider_bounds(
    bounds: Bounds<Pixels>,
    divider: f32,
    scale_factor: f32,
) -> Option<Bounds<Pixels>> {
    // x=0 表示终端左侧已经没有侧栏（顶部 tab 或折叠终态），贴边线没有分隔意义。
    if divider <= 0.0 || bounds.origin.x <= px(0.0) {
        return None;
    }

    let scale = scale_factor.max(1.0);
    let width = (divider * scale).round().max(1.0) / scale;
    let height = bounds.size.height + bounds.origin.y;
    let mut origin = bounds.origin;
    origin.y = px(0.0);
    Some(Bounds::new(origin, size(px(width), height)))
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
/// 右侧抽屉槽位宽度 = 抽屉自身宽度。抽屉贴满右侧整条竖带（上下右都不留卡缝，
/// 左侧直接抵住终端卡），所以槽位里不再有额外的卡缝要算进来。
const SIDE_PANEL_SLOT_W: f32 = 320.0;

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

// Palette 是一个随手打开的 popover，不是占满工作流的设置页。宽高与内部节奏
// 单独命名，避免后续在渲染树里重新散落一批互不相干的魔数。
const PALETTE_PANEL_WIDTH: f32 = 580.0;
const PALETTE_PANEL_HEIGHT: f32 = 520.0;
const PALETTE_METADATA_MAX_WIDTH: f32 = 260.0;
/// 与 Shell 选择器的 launcher row 共用同一命中高度；不能只放大文字或底色，
/// 否则 hover、点击和键盘选中会再次出现三套几何。
const PALETTE_ROW_HEIGHT: f32 = 42.0;
const PALETTE_GROUP_HEADER_HEIGHT: f32 = 26.0;
const PALETTE_FILTER_BAR_HEIGHT: f32 = 26.0;
const PALETTE_ROW_GAP: f32 = 4.0;
const PALETTE_SCROLLBAR_CONTENT_GUTTER: f32 = 16.0;

#[derive(Clone)]
enum WorkspacePaletteAction {
    Shared(crate::display::command_palette::PaletteAction),
    /// 聚焦已经存在的工作区标签，不创建副本。
    FocusTab(usize),
    /// 先激活标签，再把焦点交给其中的明确 pane。
    FocusPane {
        tab: usize,
        pane: u64,
    },
    /// 在 frecency 目录中新建终端；已有目录中的 pane 由上面的 FocusPane 命中。
    OpenDirectory(std::path::PathBuf),
    RunAiSession {
        command: String,
        cwd: Option<std::path::PathBuf>,
    },
    /// 启动器混排的 SSH 主机行（数据源 = 共享主机列表权威）。
    LaunchSshHost(String),
    /// 新建终端弹窗里的一台已检测 shell（旧壳 `ProfileRow::Shell`）。
    LaunchShell(crate::shell_detect::DetectedShell),
    /// 设置页“导入终端目录”落盘的可执行文件快照。
    LaunchProfile(crate::config::ui_config::Profile),
}

#[derive(Clone, Copy)]
enum WorkspacePaletteHintStyle {
    /// 路径、会话来源、连接种类等补充身份信息。
    Metadata,
    /// 可直接触发该动作的键位；渲染为独立 keycap，不与普通说明混淆。
    Shortcut,
}

/// Quick Jump 把不同数据源当成一等 scope，而不是把 `AI`、`SSH`
/// 当搜索关键字。这里只有已经接通真实端到端动作的数据源；Recipe/Files 在
/// 后端可执行前不展示空壳入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickJumpFilter {
    All,
    Opened,
    Folders,
    Ssh,
    Agents,
}

impl QuickJumpFilter {
    const ALL: [Self; 5] = [Self::All, Self::Opened, Self::Folders, Self::Ssh, Self::Agents];

    fn label(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::All => language.pick("全部", "All"),
            Self::Opened => language.pick("已打开", "Opened"),
            Self::Folders => language.pick("文件夹", "Folders"),
            Self::Ssh => "SSH",
            Self::Agents => language.pick("智能体", "Agents"),
        }
    }

    fn placeholder(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::All => language.pick(
                "搜索标签页、分屏、目录、SSH 或 AI 会话…",
                "Search tabs, panes, folders, SSH or agent sessions...",
            ),
            Self::Opened => {
                language.pick("搜索已打开的标签页和分屏…", "Search open tabs and panes...")
            },
            Self::Folders => language.pick("搜索常用文件夹…", "Search frequent folders..."),
            Self::Ssh => language.pick("搜索 SSH 主机…", "Search SSH hosts..."),
            Self::Agents => language.pick("搜索智能体会话…", "Search agent sessions..."),
        }
    }

    fn matches(self, action: &WorkspacePaletteAction) -> bool {
        match self {
            Self::All => true,
            Self::Opened => matches!(
                action,
                WorkspacePaletteAction::FocusTab(_) | WorkspacePaletteAction::FocusPane { .. }
            ),
            Self::Folders => matches!(action, WorkspacePaletteAction::OpenDirectory(_)),
            Self::Ssh => matches!(action, WorkspacePaletteAction::LaunchSshHost(_)),
            Self::Agents => matches!(action, WorkspacePaletteAction::RunAiSession { .. }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspacePaletteFilter {
    Launcher(crate::display::command_palette::LauncherFilter),
    QuickJump(QuickJumpFilter),
}

impl WorkspacePaletteFilter {
    fn label(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::Launcher(filter) => filter.label(language),
            Self::QuickJump(filter) => filter.label(language),
        }
    }

    fn placeholder(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::Launcher(crate::display::command_palette::LauncherFilter::All) => language
                .pick("搜索 Shell、配置和 SSH 主机…", "Search shells, profiles and SSH hosts..."),
            Self::Launcher(crate::display::command_palette::LauncherFilter::Ssh) => {
                language.pick("搜索 SSH 主机…", "Search SSH hosts...")
            },
            Self::Launcher(crate::display::command_palette::LauncherFilter::Shell) => {
                language.pick("搜索 Shell 和配置…", "Search shells and profiles...")
            },
            Self::QuickJump(filter) => filter.placeholder(language),
        }
    }
}

#[derive(Clone)]
struct WorkspacePaletteRow {
    group_order: usize,
    group: String,
    label: String,
    hint: String,
    hint_style: WorkspacePaletteHintStyle,
    search: String,
    action: WorkspacePaletteAction,
    /// 行首的彩色品牌图标（只有 shell/配置档行有）。旧壳的 shell 菜单同样
    /// 是「图标 + 名字 + 灰色命令行」三段式，光靠文字分不清 pwsh 与 5.1。
    icon: Option<std::sync::Arc<gpui::RenderImage>>,
    /// SSH 主机行用 Nerd Font 码位（旧壳 `os_icons`），不是品牌 PNG。
    icon_glyph: Option<char>,
    /// 通用名词行使用组件库的线性图标；与品牌图片、系统 Nerd Font 图标互斥。
    icon_path: Option<SharedString>,
}

fn open_in_file_manager(path: &Path) {
    log_file_manager_spawn("open", path, crate::platform::file_manager::open(path));
}

/// 外部文件管理器的启动结果必须留痕。这两条路径此前都是 `let _ = …spawn()`：
/// explorer 一旦没弹窗（复用后台窗口、路径不存在、拒绝启动）界面上零反馈，
/// 只能靠猜——「点了没反应」的报障就是这么来的。
fn log_file_manager_spawn(kind: &str, path: &Path, result: std::io::Result<()>) {
    match result {
        Ok(()) => crate::display::nebula_debug_log(format!(
            "file_manager {kind} spawned path={}",
            path.display()
        )),
        Err(err) => crate::display::nebula_debug_log(format!(
            "file_manager {kind} FAILED path={} err={err}",
            path.display()
        )),
    }
}

/// “在文件管理器中显示”与单纯打开路径不是一个动作。Windows 的
/// `/select,` 必须和路径组成同一个 argv，避免空格与 Unicode 被二次解析。
fn reveal_in_file_manager(path: &Path) {
    log_file_manager_spawn("reveal", path, crate::platform::file_manager::reveal(path));
}

fn workspace_ui_language() -> crate::display::UiLanguage {
    crate::display::LanguagePreference::from(nebula_settings::RuntimeSettings::load().language)
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

fn settings_should_fold_sidebar(tabs_position: nebula_settings::TabsPositionName) -> bool {
    tabs_position == nebula_settings::TabsPositionName::Sidebar
}

/// 新建终端弹窗的行：已检测 shell + SSH 主机，分组对照旧壳
/// `CommandPalette::open_profiles`（推荐 / 所有 Shell / SSH 主机）。
/// 三点菜单与 Ctrl+K 打开的是这份列表，不是通用命令面板。
fn shell_palette_rows(
    shells: Vec<crate::shell_detect::DetectedShell>,
    profiles: Vec<crate::config::ui_config::Profile>,
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
                hint_style: WorkspacePaletteHintStyle::Metadata,
                search: format!("{} {} shell profile", shell.name, shell.id).to_lowercase(),
                icon: crate::gpui_shell::widgets::shell_brand_image(
                    &shell.id,
                    SHELL_ICON_PX,
                    scale_factor,
                ),
                icon_glyph: None,
                icon_path: None,
                action: WorkspacePaletteAction::LaunchShell(shell),
            }
        })
        .collect();
    rows.extend(profiles.into_iter().filter_map(|profile| {
        let id = profile.settings_id()?;
        let is_default = id.eq_ignore_ascii_case(default_shell_id);
        let icon_id = profile.shell_id.as_deref().unwrap_or(&id);
        let icon =
            crate::gpui_shell::widgets::shell_brand_image(icon_id, SHELL_ICON_PX, scale_factor);
        let label = profile.name.clone();
        let hint = profile.command.clone();
        Some(WorkspacePaletteRow {
            group_order: if is_default { 0 } else { 1 },
            group: if is_default { recommended.to_owned() } else { all_shells.to_owned() },
            search: format!("{} {} {} shell profile", profile.name, id, profile.command)
                .to_lowercase(),
            label,
            hint,
            hint_style: WorkspacePaletteHintStyle::Metadata,
            action: WorkspacePaletteAction::LaunchProfile(profile),
            icon,
            icon_glyph: None,
            icon_path: None,
        })
    }));
    if let Some(position) = rows.iter().position(|row| row.group_order == 0) {
        let default_row = rows.remove(position);
        rows.insert(0, default_row);
    }
    let ssh_icons = ssh_host_icon_ids();
    rows.extend(ssh_hosts.into_iter().map(|host| {
        let glyph =
            crate::display::ui::os_icons::resolve(ssh_icons.get(&host).map(String::as_str)).glyph;
        WorkspacePaletteRow {
            group_order: 2,
            group: ssh_group.to_owned(),
            label: host.clone(),
            hint: "SSH".to_owned(),
            hint_style: WorkspacePaletteHintStyle::Metadata,
            search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
            action: WorkspacePaletteAction::LaunchSshHost(host),
            icon: None,
            icon_glyph: Some(glyph),
            icon_path: None,
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
    /// 本 tab 的分屏数（Terminal tab 才 > 0）。`> 1` 时行首图标换成 2×2 分屏
    /// 标记、行尾挂一枚数量胶囊；见 [`pane_header::split_badge`]。
    pane_count: usize,
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
    /// Window-level Settings surface. It intentionally lives outside `tabs`:
    /// opening preferences must not alter tab order, session state, or PTYs.
    settings_surface: Option<(Entity<SettingsPane>, Subscription)>,
    settings_open: bool,
    /// Sidebar state sampled on entry. `None` means top-tab mode, where there
    /// is no left rail to fold and the setting must remain untouched.
    settings_restore_sidebar_collapsed: Option<bool>,
    settings_restore_side_panel_open: bool,
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
    /// 进行中的 pane 拖拽（把分屏里的一个 pane 拉出来成独立 tab）；
    /// 见 [`pane_header::PaneDrag`]。
    pane_drag: Option<pane_header::PaneDrag>,
    /// 其它窗口的标签悬停在本窗口终端区时的跨窗 dock 预览。
    cross_window_dock: Option<SplitNav>,
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
    /// 用户命令管理器贴在右侧覆盖显示，不占终端布局宽度，也不复用应用动作
    /// 命令面板的状态，避免两种“命令”语义互相污染。
    command_manager_open: bool,
    command_manager_input: Entity<InputState>,
    command_manager_selected: usize,
    command_manager_scroll: gpui::ScrollHandle,
    saved_commands: crate::saved_commands::SavedCommands,
    _command_manager_subscription: Subscription,
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
    /// `Some` 表示当前是统一 Quick Jump；值就是 provider scope。
    /// 命令面板与 Shell picker 必须保持 `None`，避免跨入口泄漏筛选状态。
    quick_jump_filter: Option<QuickJumpFilter>,
    /// 三种 palette 共用同一滚动位置，但每次打开/切 scope 都归零。显式 handle
    /// 让键盘选择能自动揭示当前行，也允许在 Windows 上始终画出可拖动 thumb。
    command_palette_scroll: gpui::ScrollHandle,
    _command_palette_subscription: Subscription,
    /// Shared old-shell drawer model. GPUI owns only presentation and polling;
    /// filesystem traversal, expansion state, ignore marking and throttling
    /// remain in `display::side_panel`.
    side_panel: crate::display::side_panel::SidePanel,
    side_panel_polling: bool,
    side_panel_anim_armed: bool,
    /// Files drawer search reuses the same real GPUI input as the Shell
    /// selector, including caret, selection, clipboard and IME behavior.
    file_tree_search_input: Entity<InputState>,
    _file_tree_search_subscription: Subscription,
    /// 文件树列表的滚动位置。抽屉走 `uniform_list` 虚拟化，滑块（组件库
    /// `Scrollbar`）读的是同一个 handle——这是抽屉里**唯一**的滚动模型，
    /// 旧壳那套行粒度 `SidePanel::scroll` 在 GPUI 侧恒为 0。
    file_tree_scroll: gpui::UniformListScrollHandle,
    /// 文件树右键：画在 workspace 根上，不进抽屉子孙树。见 `file_tree.rs`。
    file_tree_menu: Option<file_tree::FileTreeContextMenu>,
    /// 抽屉在 SSH pane 上的远端形态。与 `side_panel` 并存而不是替换它：
    /// 用户在远端 tab 和本地 tab 之间来回切时，两边的浏览位置都该留着。
    remote_browser: remote_files::RemoteBrowser,
    /// 远端列表的滚动位置。与本地树各用一个 handle：共用会让切换 pane 时
    /// 滚动条停在上一个列表的位置上，看起来像是列表内容对不上。
    remote_files_scroll: gpui::UniformListScrollHandle,
    /// 标签右键：同样画在根上。挂进标签行会让每一行都渲染同一份菜单，
    /// popover 阴影按标签数叠厚。见 `tab_menu.rs` 模块头。
    tab_menu: Option<tab_menu::TabContextMenu>,
    /// 终端/Markdown 选区右键：菜单必须在 workspace 根上唯一渲染；
    /// Send to Chat 对话框再通过 Root 的 dialog layer 接管焦点。
    selection_context_menu: Option<send_to_chat::SelectionContextMenu>,
    /// 复用旧壳随包分发的 AI 品牌图，不用近似字体图标替代。
    sidebar_logo_images: HashMap<(crate::display::AiLogo, bool), Arc<RenderImage>>,
    /// 品牌图缓存对应的整数物理像素边长；窗口跨 DPI 显示器时据此重建。
    sidebar_logo_target_px: u32,
    /// 跟随系统深浅：OS 外观切换的监听（旧壳 ThemeChanged 的对应物）。
    _appearance_sub: Subscription,
    /// spinner 在窗口失焦时冻结为静态状态；重新聚焦后由一次 render 恢复按需帧循环。
    spinner_window_active: bool,
    _spinner_activation_sub: Subscription,
    /// 本会话已注入的自定义键位 combo（gpui 绑定串）。键位表没有删除
    /// API，撤销只能靠后注的 NoAction 盖掉；这份清单就是撤销的依据。
    custom_keybinds_applied: Vec<String>,
    /// 侧栏「运行中」spinner 的相位（0..1，旧壳 `SPINNER_PERIOD` 800ms 一
    /// 圈）与上次帧时刻。侧栏和顶栏共用 GPUI 屏幕帧时钟。
    spinner_phase: f32,
    spinner_last: std::time::Instant,
    /// render 与 next-frame 回调之间的单槽门闩：防止其他 UI 更新重复挂帧。
    spinner_frame_pending: std::cell::Cell<bool>,
    /// 上一次 render 是否真的画出了运行态徽章；滚出视口后不应继续唤醒窗口。
    spinner_visible: std::cell::Cell<bool>,
    /// 系统关闭按钮可能连续送来多次 should-close；确认框在场时只保留一份。
    window_close_confirm_open: bool,
    /// `keep_session` 关窗后 HWND 已隐藏、PTY 仍在；托盘 / mux ATTACH 用来捞回。
    window_hidden: bool,
    /// 开窗时记下，mux `tab.new` 需要从 pump 拿到 `&mut Window`。
    window_handle: gpui::AnyWindowHandle,
    /// 进程内稳定窗口 id，供 Runtime API、MRU 路由和跨窗迁移精确寻址。
    runtime_window_id: u64,
    /// 快速终端复用工作区 UI，但不是普通会话窗口；关闭、初始尺寸和持久化
    /// 必须继续遵守旧壳的 session-exempt 合同。
    window_role: windowing::WindowRole,
    /// 与 RuntimeServer 共享同一状态中心，快照、wait、subscribe 与 agents.list
    /// 必须消费同一份 revision/state_change_seq 权威。
    runtime_hub: crate::runtime_api::RuntimeHub,
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
        ai_events: Option<std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>>,
        shell_events: Option<std::sync::mpsc::Receiver<crate::gpui_shell::GpuiShellEvent>>,
        runtime_window_id: u64,
        runtime_hub: crate::runtime_api::RuntimeHub,
        startup: windowing::WorkspaceStartup,
        window_role: windowing::WindowRole,
        cx: &mut Context<Self>,
    ) -> Self {
        // 启动相关设置只取样一次：本次开窗的恢复决策不能被恢复过程中的
        // 文件变化拆成互相矛盾的 restore/resume 状态。
        let runtime = nebula_settings::RuntimeSettings::load();
        let sidebar_width = runtime.sidebar_width;
        #[cfg(windows)]
        {
            let mut icon_scale = window.scale_factor();
            cx.observe_window_bounds(window, move |_, window, _| {
                if icon_scale != window.scale_factor() {
                    icon_scale = window.scale_factor();
                    crate::gpui_shell::set_native_window_icon(window);
                }
            })
            .detach();
        }
        let initial_grid = Self::prepare_initial_grid(
            window,
            cx,
            sidebar_width,
            window_role == windowing::WindowRole::Regular,
        );
        let this = cx.entity().downgrade();
        let appearance_sub = window.observe_window_appearance(move |_, cx| {
            if let Some(workspace) = this.upgrade() {
                workspace.update(cx, |workspace, cx| workspace.apply_runtime_settings(cx));
            }
        });
        let spinner_window_active = window.is_window_active();
        let spinner_activation_sub =
            cx.observe_window_activation(window, |workspace, window, cx| {
                let active = window.is_window_active();
                if workspace.spinner_window_active != active {
                    workspace.spinner_window_active = active;
                    workspace.spinner_last = std::time::Instant::now();
                    cx.notify();
                }
            });
        let command_palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索命令…"));
        let command_manager_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索已保存命令…"));
        let file_tree_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                workspace_ui_language().pick("搜索文件和文件夹…", "Search files and folders..."),
            )
        });
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
                        this.command_palette_scroll.scroll_to_item(0);
                        cx.notify();
                    },
                    InputEvent::PressEnter { .. } => {
                        this.run_selected_palette_action(window, cx);
                    },
                    _ => {},
                }
            },
        );
        let command_manager_subscription = cx.subscribe_in(
            &command_manager_input,
            window,
            |this: &mut Self,
             _: &Entity<InputState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<'_, Self>| {
                match event {
                    InputEvent::Change => {
                        this.command_manager_selected = 0;
                        this.command_manager_scroll.scroll_to_item(0);
                        cx.notify();
                    },
                    InputEvent::PressEnter { .. } => {
                        this.run_selected_saved_command(window, cx);
                    },
                    _ => {},
                }
            },
        );
        let file_tree_search_subscription =
            cx.subscribe_in(&file_tree_search_input, window, Self::on_file_tree_search_event);
        let sidebar_logo_target_px =
            (TAB_LABEL_ICON_SIZE * window.scale_factor()).round().max(1.0) as u32;
        let mut this = Self {
            tabs: Vec::new(),
            tab_meta: Vec::new(),
            tab_rename: None,
            // 首窗仍从 1 起，保持既有 runtime/测试身份；后续窗口用高 32 位
            // 分区，AI hook 只有 pane id 时也不会撞到另一窗口的同号 pane。
            next_pane_id: runtime_window_id
                .saturating_sub(1)
                .saturating_mul(1u64 << 32)
                .saturating_add(1),
            active: 0,
            settings_surface: None,
            settings_open: false,
            settings_restore_sidebar_collapsed: None,
            settings_restore_side_panel_open: false,
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
            pane_drag: None,
            cross_window_dock: None,
            split_drag: None,
            sidebar_resizing: false,
            split_bounds: Rc::new(RefCell::new(HashMap::new())),
            pane_bounds: Rc::new(RefCell::new(HashMap::new())),
            command_palette_open: false,
            command_palette_input,
            command_palette_selected: 0,
            command_manager_open: false,
            command_manager_input,
            command_manager_selected: 0,
            command_manager_scroll: gpui::ScrollHandle::new(),
            saved_commands: crate::saved_commands::SavedCommands::load().unwrap_or_default(),
            _command_manager_subscription: command_manager_subscription,
            git_commit_input,
            vcs_discard_confirm: None,
            palette_override: None,
            shell_picker_open: false,
            launcher_filter: crate::display::command_palette::LauncherFilter::All,
            quick_jump_filter: None,
            command_palette_scroll: gpui::ScrollHandle::new(),
            _command_palette_subscription: command_palette_subscription,
            side_panel: crate::display::side_panel::SidePanel::new(),
            side_panel_polling: false,
            side_panel_anim_armed: false,
            file_tree_search_input,
            _file_tree_search_subscription: file_tree_search_subscription,
            file_tree_scroll: gpui::UniformListScrollHandle::new(),
            file_tree_menu: None,
            remote_browser: remote_files::RemoteBrowser::default(),
            remote_files_scroll: gpui::UniformListScrollHandle::new(),
            tab_menu: None,
            selection_context_menu: None,
            sidebar_logo_images: sidebar_logo_images(sidebar_logo_target_px),
            sidebar_logo_target_px,
            _appearance_sub: appearance_sub,
            spinner_window_active,
            _spinner_activation_sub: spinner_activation_sub,
            custom_keybinds_applied: Vec::new(),
            spinner_phase: 0.0,
            spinner_last: std::time::Instant::now(),
            spinner_frame_pending: std::cell::Cell::new(false),
            spinner_visible: std::cell::Cell::new(false),
            window_close_confirm_open: false,
            window_hidden: false,
            window_handle: window.window_handle(),
            runtime_window_id,
            window_role,
            runtime_hub,
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
        match startup {
            windowing::WorkspaceStartup::RestoreOrDefault => {
                // 只有首窗恢复全局 session，避免每个新窗口重复回放同一批 PTY。
                if !runtime.restore_session
                    || !this.try_restore_session(runtime.resume_ai, window, cx)
                {
                    this.add_terminal(window, cx);
                }
            },
            windowing::WorkspaceStartup::NewTerminal { cwd } => {
                this.add_terminal_at(cwd, None, window, cx);
            },
            windowing::WorkspaceStartup::Empty => {},
        }
        if let Some(ai_events) = ai_events {
            Self::start_ai_hook_pump(ai_events, cx);
            // 全局热键是进程级单例，只在承载 ai-hook 泵的那扇初始窗口注册一次。
            Self::start_quick_terminal_hotkey(cx);
        }
        Self::start_agent_screen_watchdog(cx);
        if let Some(shell_events) = shell_events {
            Self::start_shell_event_pump(shell_events, cx);
        }
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
    fn prepare_initial_grid(
        window: &mut Window,
        cx: &mut App,
        sidebar_width: f32,
        fit_window_to_default_grid: bool,
    ) -> (u16, u16) {
        let (cell_w, line_h) = TerminalView::cell_metrics(window, cx);
        let (startup_cell_w, startup_line_h) = TerminalView::startup_cell_metrics(window, cx);
        // 标签栏位置只改变 chrome 内部布局，不能改变产品的默认外窗几何。
        // 顶栏模式仍保留与侧栏模式相同的横向预算，让两种模式启动时宽高一致。
        let chrome_w = sidebar_width + 16.0 + 24.0 + 2.0;
        let chrome_h = 34.0 + 16.0 + 16.0 + 2.0;
        let (w, h) = if fit_window_to_default_grid {
            let mut w = f32::from(TerminalView::DEFAULT_GRID_COLUMNS) * f32::from(startup_cell_w)
                + chrome_w;
            let mut h =
                f32::from(TerminalView::DEFAULT_GRID_LINES) * f32::from(startup_line_h) + chrome_h;
            if let Some(display) = cx.primary_display() {
                let bounds = display.bounds().size;
                w = w.min(f32::from(bounds.width) * 0.95);
                h = h.min(f32::from(bounds.height) * 0.95);
            }
            window.resize(size(px(w), px(h)));
            (w, h)
        } else {
            // 快速终端的 WindowOptions 已经给出目标显示器全宽和 40% 高度。
            // 再排队一次普通网格 resize 会与原生滑入竞争，首帧 DComp 表面只
            // 覆盖旧宽度，右侧因此变黑。
            let bounds = window.bounds().size;
            (f32::from(bounds.width), f32::from(bounds.height))
        };
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
        crate::shell_detect::shell_short_tag(&crate::platform::shell::default_shell_id()).into()
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
        if let Some(detected) = crate::shell_detect::detect_shells()
            .into_iter()
            .find(|shell| shell.id.eq_ignore_ascii_case(&shell_id))
        {
            let shell = detected.shell();
            return crate::session::LaunchSession::Shell {
                name: detected.name,
                program: shell.program().to_owned(),
                args: shell.args().to_vec(),
            };
        }
        crate::terminal_profiles::TerminalProfiles::load()
            .ok()
            .and_then(|store| {
                store.as_config_profiles().into_iter().find(|profile| {
                    profile.settings_id().is_some_and(|id| id.eq_ignore_ascii_case(&shell_id))
                })
            })
            .map(Self::profile_launch_session)
            .unwrap_or(crate::session::LaunchSession::Default)
    }

    fn profile_launch_session(
        profile: crate::config::ui_config::Profile,
    ) -> crate::session::LaunchSession {
        crate::session::LaunchSession::Profile {
            name: profile.name,
            command: profile.command,
            args: profile.args,
            cwd: profile.cwd.map(|path| path.to_string_lossy().into_owned()),
            shell_id: profile.shell_id,
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
            LaunchSession::Ssh { host } => {
                TerminalLaunch::Ssh { destination: host.clone(), cwd: None }
            },
        }
    }

    fn launch_shell_tag(launch: &crate::session::LaunchSession) -> Option<SharedString> {
        match launch {
            crate::session::LaunchSession::Default => Some(Self::default_shell_tag()),
            crate::session::LaunchSession::Shell { name, .. } => {
                Some(crate::shell_detect::shell_short_tag(name).into())
            },
            crate::session::LaunchSession::Ssh { .. } => Some("ssh".into()),
            crate::session::LaunchSession::Profile { name, shell_id, .. } => Some(
                crate::shell_detect::shell_short_tag(shell_id.as_deref().unwrap_or(name)).into(),
            ),
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
        crate::gpui_shell::apply_app_icon(runtime.app_icon, cx);
        self.sidebar_width = runtime.sidebar_width;
        self.tabs_position = runtime.tabs_position;
        self.sync_settings_layout();
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
    ) -> u64 {
        // 默认 shell 只在“创建新 Tab”的这一刻取样，并把实际 program/args
        // 一起冻结进 Tab launch。设置页随后改默认值只影响下一次创建；冷
        // 恢复也按本 Tab 的 launch 重建，不会把混合工作区抹成同一种 shell。
        let launch_session = Self::configured_local_launch(cx);
        self.add_terminal_with(launch_session, cwd, command, window, cx)
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
    ) -> u64 {
        if self.settings_open {
            self.leave_settings(window, cx);
        }
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
            broadcast: false,
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
        self.sync_side_panel_to_active(true, cx);
        cx.notify();
        focused
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
        self.add_ssh_terminal_at(destination, None, window, cx);
    }

    fn add_ssh_terminal_at(
        &mut self,
        destination: String,
        remote_cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open {
            self.leave_settings(window, cx);
        }
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
            cwd: remote_cwd,
        };
        let pane = self.new_pane(grid, launch, None, window, cx);
        let focused = pane.id;
        let tab = WorkspaceTab::Terminal {
            tree: SplitTree::leaf(pane.id),
            panes: vec![pane],
            focused,
            zoomed: false,
            broadcast: false,
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
        self.sync_side_panel_to_active(true, cx);
        cx.notify();
    }

    /// 在同一 tab、同一分屏位置替换失败的 SSH pane。先把新实体及订阅完整
    /// 建好，再原子替换树叶和 pane 所有权；旧实体的异步泵只会更新旧 Entity，
    /// 因而无法把迟到的 Failed/Ready 写进新连接。
    fn retry_ssh_pane(
        &mut self,
        tab_ix: usize,
        pane_id: u64,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(WorkspaceTab::Terminal { panes, .. }) = self.tabs.get(tab_ix) else { return };
        let Some(old) = panes.iter().find(|pane| pane.id == pane_id) else { return };
        let (grid, remote_cwd) = {
            let view = old.view.read(cx);
            if view.ssh_destination.as_deref() != Some(destination.as_str()) {
                return;
            }
            (
                (view.grid_cols() as u16, view.grid_rows() as u16),
                (!view.cwd.is_empty()).then(|| view.cwd.clone()),
            )
        };

        let launch = crate::gpui_shell::terminal::view::TerminalLaunch::Ssh {
            destination: destination.clone(),
            cwd: remote_cwd,
        };
        let replacement = self.new_pane(grid, launch, None, window, cx);
        let replacement_id = replacement.id;
        let old = {
            let Some(WorkspaceTab::Terminal { panes, tree, focused, .. }) =
                self.tabs.get_mut(tab_ix)
            else {
                replacement.view.read(cx).shutdown();
                return;
            };
            let Some(index) = panes.iter().position(|pane| pane.id == pane_id) else {
                replacement.view.read(cx).shutdown();
                return;
            };
            if !tree.replace_leaf(pane_id, replacement_id) {
                replacement.view.read(cx).shutdown();
                return;
            }
            if *focused == pane_id {
                *focused = replacement_id;
            }
            std::mem::replace(&mut panes[index], replacement)
        };

        self.runtime_hub.record_pane_closed(self.runtime_window_id, pane_id);
        self.pane_bounds.borrow_mut().remove(&pane_id);
        old.view.read(cx).shutdown();
        if tab_ix == self.active {
            self.focus_active(window, cx);
            self.sync_side_panel_to_active(true, cx);
        }
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
    ) -> Result<u64, crate::runtime_api::ApiError> {
        let active = self.active;
        let Some(WorkspaceTab::Terminal { panes, focused, .. }) = self.tabs.get(active) else {
            return Err(crate::runtime_api::ApiError::new(
                "invalid_state",
                "the active tab cannot be split",
            ));
        };
        let focused = *focused;
        let Some(anchor) = panes.iter().find(|pane| pane.id == focused) else {
            return Err(crate::runtime_api::ApiError::new(
                "action_failed",
                "the focused pane is missing from the active split tree",
            ));
        };
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
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed, .. }) =
            self.tabs.get_mut(active)
        else {
            // new_pane 之后 tab 结构不可能已变（同一同步调用栈），但防御住：
            // 树上挂不进去就立即回收，不留孤儿 PTY。
            pane.view.read(cx).shutdown();
            return Err(crate::runtime_api::ApiError::new(
                "action_failed",
                "the active terminal tab changed while creating a split",
            ));
        };
        if !tree.split_leaf(*focused, new_id, direction, 0.5) {
            pane.view.read(cx).shutdown();
            return Err(crate::runtime_api::ApiError::new(
                "action_failed",
                "the focused pane could not be attached to the split tree",
            ));
        }
        panes.push(pane);
        *focused = new_id;
        *zoomed = false;
        self.mark_structural_resize(active, cx);
        self.focus_active(window, cx);
        self.sync_side_panel_to_active(true, cx);
        cx.notify();
        Ok(new_id)
    }

    /// 结构性布局变化：让 tab 内每个 pane 的下一次网格观测直接下发到 Term 与
    /// ConPTY，不进尾沿去抖。
    ///
    /// 分屏创建/关闭、pane 替换、zoom 切换都属此列——几何只改一次，去抖没有
    /// 可合并的后续帧，而多等的每一毫秒都是 Term（已按新网格渲染）与 ConPTY
    /// （仍是旧几何）不一致的窗口期：这段时间到达的字节按旧宽度进网格，等提交
    /// 时本地 reflow 与 conhost rewrap 各折一套行数，提示符与回显就此错开几行。
    /// 旧壳 `window_context/split.rs` 的 `resize_active_layout()` 在同样的路径
    /// 上同步下发 grid + PTY，这里复刻同一条合同。
    fn mark_structural_resize(&mut self, tab_ix: usize, cx: &mut App) {
        let Some(WorkspaceTab::Terminal { panes, .. }) = self.tabs.get(tab_ix) else {
            return;
        };
        let views: Vec<_> = panes.iter().map(|pane| pane.view.clone()).collect();
        for view in views {
            view.update(cx, |view, _| view.mark_structural_resize());
        }
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
        if !matches!(outcome, RemoveOutcome::NotFound) {
            self.runtime_hub.record_pane_closed(self.runtime_window_id, pane_id);
        }
        match outcome {
            RemoveOutcome::NotFound => {},
            RemoveOutcome::WasRoot => self.close_tab(tab_ix, window, cx),
            RemoveOutcome::Collapsed(next_focus) => {
                if let Some(WorkspaceTab::Terminal { panes, focused, zoomed, broadcast, .. }) =
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
                    // 收敛到单 pane：广播没有语义了，留着开关状态只会骗人
                    // ——标题条此时也不再绘制，用户根本没有入口关掉它。
                    if panes.len() < 2 {
                        *broadcast = false;
                    }
                }
                self.remote_browser.forget(pane_id);
                self.pane_bounds.borrow_mut().remove(&pane_id);
                self.mark_structural_resize(tab_ix, cx);
                if tab_ix == self.active {
                    self.focus_active(window, cx);
                    self.sync_side_panel_to_active(true, cx);
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

    fn save_clean_window_session(&mut self, cx: &mut App) {
        windowing::save_current_window_session(
            self.runtime_window_id,
            self.snapshot_session(cx),
            session_persistence::SaveReason::WindowClose,
            cx,
        );
    }

    /// GPUI 的 should-close 回调必须同步返回：无繁忙进程时直接允许系统关闭；
    /// 有繁忙进程时先返回 false，再由对话框确认回调显式移除窗口。
    fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let persist_session = self.window_role == windowing::WindowRole::Regular;
        if persist_session && self.keep_session_on_close(window, cx) {
            return false;
        }
        let Some(process) = self.busy_process_in_window(cx) else {
            if persist_session {
                self.save_clean_window_session(cx);
            }
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
            confirm_dialog(
                dialog,
                window,
                "关闭窗口？",
                body.clone(),
                "关闭",
                "取消",
                ButtonVariant::Danger,
            )
            .on_ok(move |_, window, cx| {
                let _ = confirm_workspace.update(cx, |workspace, cx| {
                    if persist_session {
                        workspace.save_clean_window_session(cx);
                    }
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
            confirm_dialog(
                dialog,
                window,
                "关闭此分栏？",
                body.clone(),
                "关闭",
                "取消",
                ButtonVariant::Danger,
            )
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
            confirm_dialog(
                dialog,
                window,
                "关闭此标签页？",
                body.clone(),
                "关闭",
                "取消",
                ButtonVariant::Danger,
            )
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
            self.sync_side_panel_to_active(true, cx);
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
        let active = self.active;
        let toggled = match self.tabs.get_mut(active) {
            Some(WorkspaceTab::Terminal { panes, zoomed, .. }) if panes.len() > 1 => {
                *zoomed = !*zoomed;
                true
            },
            _ => false,
        };
        if toggled {
            // 缩放把一个 pane 的几何从半卡拉到整卡（其余 pane 反之），是一次性
            // 结构变化，同 split/close 走同步下发。
            self.mark_structural_resize(active, cx);
            cx.notify();
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
            if let Some((source, session_id)) = agent
                .as_ref()
                .and_then(|agent| Some((agent.source.clone(), agent.session_id.clone()?)))
            {
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
            WorkspaceTab::Terminal { panes, tree, focused, zoomed: false, broadcast: false },
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
    pub(crate) fn snapshot_session(&self, cx: &App) -> crate::session::Session {
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

    /// Open Settings as a window-level page while preserving the active tab.
    /// Side-tab mode folds its real left rail; top-tab mode has no such rail,
    /// so touching `sidebar_collapsed` there would only create hidden state.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            if let Some((view, _)) = self.settings_surface.as_ref() {
                let focus = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| window.focus(&focus, cx));
            }
            return;
        }

        if self.settings_surface.is_none() {
            let view = cx.new(|cx| SettingsPane::new(window, cx));
            let subscription = cx.subscribe_in(&view, window, Self::on_settings_event);
            self.settings_surface = Some((view, subscription));
        }

        self.settings_open = true;
        self.sync_settings_layout();

        self.settings_restore_side_panel_open = self.side_panel.open;
        if self.side_panel.open {
            self.side_panel.open = false;
            self.side_panel_anim_armed = true;
            self.file_tree_menu = None;
        }

        if let Some((view, _)) = self.settings_surface.as_ref() {
            let focus = view.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open {
            return;
        }
        self.leave_settings(window, cx);
        if self.tabs.is_empty() {
            windowing::close_empty_workspace_window(self.runtime_window_id, window, cx);
        }
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
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
        let is_markdown =
            path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
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
            |tab| matches!(tab, WorkspaceTab::Document { view, .. } if view.read(cx).path == path),
        ) {
            if let Some(WorkspaceTab::Document { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| {
                    view.reload(cx);
                    cx.notify();
                });
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::doc_tabs::DocTabView::new(path, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_document_event);
        self.insert_new_tab(WorkspaceTab::Document { view, _subscription: subscription });
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
            |tab| matches!(tab, WorkspaceTab::Code { view, .. } if view.read(cx).is_regular_path(&path)),
        ) {
            if let Some(WorkspaceTab::Code { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload(window, cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| crate::gpui_shell::code_tab::CodeTabView::new(path, window, cx));
        let subscription = cx.subscribe(&view, Self::on_code_tab_event);
        self.insert_new_tab(WorkspaceTab::Code { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 冲突文件仍属于代码 Tab，但以三栏合并形态打开；同一仓库、同一路径复用
    /// 已有 Tab，避免用户从冲突列表连续点击后堆出多个独立结果缓冲区。
    fn open_git_merge_tab(
        &mut self,
        relative_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = self.side_panel.git_location() else { return };
        if let Some(ix) = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Code { view, .. }
                if view.read(cx).matches_git_merge(&location, &relative_path))
        }) {
            if let Some(WorkspaceTab::Code { view, .. }) = self.tabs.get(ix) {
                view.clone().update(cx, |view, cx| view.reload_git_merge(window, cx));
            }
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| {
            crate::gpui_shell::code_tab::CodeTabView::new_git_merge(
                location,
                relative_path,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&view, Self::on_code_tab_event);
        self.insert_new_tab(WorkspaceTab::Code { view, _subscription: subscription });
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
            // OSC 7 cwd 与标题共用这条事件。只有当前聚焦 pane 能驱动共享文件树；
            // 后台 pane 的提示符更新不能把前台目录覆盖掉。
            TerminalViewEvent::TitleChanged => {
                let is_active_pane = self
                    .tabs
                    .get(self.active)
                    .and_then(WorkspaceTab::focused_view)
                    .is_some_and(|active| active.entity_id() == view.entity_id());
                if is_active_pane {
                    self.sync_side_panel_to_active(false, cx);
                }
                cx.notify();
            },
            TerminalViewEvent::Exited => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.runtime_hub.record_pane_exited(self.runtime_window_id, pane_id);
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
            TerminalViewEvent::RetrySsh(destination) => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.retry_ssh_pane(tab_ix, pane_id, destination.clone(), window, cx);
                }
            },
            TerminalViewEvent::FontSizeChanged => self.apply_runtime_settings(cx),
            // 任务栏是窗口级的，只反映**正被看着的那个 pane**：后台 tab 里的
            // 构建进度投到同一个按钮上只会互相覆盖，读数还不如没有。
            TerminalViewEvent::ProgressChanged(progress) => {
                if let Some((tab_ix, pane_id)) = self.locate_pane(view.entity_id())
                    && tab_ix == self.active
                    && matches!(
                        self.tabs.get(tab_ix),
                        Some(WorkspaceTab::Terminal { focused, .. }) if *focused == pane_id
                    )
                {
                    crate::taskbar::apply(windowing::native_hwnd(window).unwrap_or(0), *progress);
                }
                // 后台 pane 也要刷新自己的 tab badge；一次协议事件只触发一次
                // workspace render，只有 Running 状态会在 render 后续接共享时钟。
                cx.notify();
            },
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
            // 视图无条件上报用户输入，宿主在事件发生时检查广播开关并扇出。
            TerminalViewEvent::UserInput(input) => {
                if let Some((_, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.fan_out_broadcast(pane_id, input, cx);
                }
            },
            TerminalViewEvent::AiAttention(attention) => {
                if let Some((_, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.deliver_pane_notification(
                        pane_id,
                        crate::notify::Notification::AiTurn {
                            program: attention.source.clone(),
                            message: Some(attention.summary_for_pane(pane_id)),
                            attention: true,
                        },
                        window,
                        cx,
                    );
                }
            },
            TerminalViewEvent::Notification(notification) => {
                if let Some((_, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.deliver_pane_notification(pane_id, notification.clone(), window, cx);
                }
            },
            TerminalViewEvent::SelectionContextMenuRequested { position, text } => {
                if let Some((_, pane_id)) = self.locate_pane(view.entity_id()) {
                    self.open_terminal_selection_context_menu(
                        view.clone(),
                        pane_id,
                        *position,
                        text.clone(),
                        window,
                        cx,
                    );
                }
            },
        }
    }

    fn on_document_event(
        &mut self,
        _: &Entity<crate::gpui_shell::doc_tabs::DocTabView>,
        event: &DocTabViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DocTabViewEvent::SelectionContextMenuRequested { position, text } => {
                self.open_document_selection_context_menu(*position, text.clone(), window, cx);
            },
        }
    }

    fn on_code_tab_event(
        &mut self,
        _: Entity<crate::gpui_shell::code_tab::CodeTabView>,
        event: &CodeTabViewEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            CodeTabViewEvent::GitConflictResolved => {
                self.side_panel.request_refresh();
                cx.notify();
            },
        }
    }

    /// 热应用设置页变更，并把 SSH 连接请求转为新标签。
    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsPane>,
        event: &SettingsPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsPaneEvent::Close => self.close_settings(window, cx),
            SettingsPaneEvent::Changed => {
                self.apply_runtime_settings(cx);
                // 键位编辑器可能改了 keybind= 表：注入/撤销随之热更新。
                self.apply_custom_keybinds(cx);
            },
            SettingsPaneEvent::TerminalProfilesChanged => self.refresh_shell_if_open(window, cx),
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
                self.runtime_hub.record_pane_closed(self.runtime_window_id, pane.id);
                pane.view.read(cx).shutdown();
                self.remote_browser.forget(pane.id);
                bounds.remove(&pane.id);
            }
        }

        if self.tabs.is_empty() && !self.settings_open {
            windowing::close_empty_workspace_window(self.runtime_window_id, window, cx);
            return;
        }
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        windowing::save_current_window_session(
            self.runtime_window_id,
            self.snapshot_session(cx),
            session_persistence::SaveReason::TabsClosed,
            cx,
        );
        self.reveal_active_tab();
        self.focus_active(window, cx);
        self.sync_side_panel_to_active(true, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
        }
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            if let Some(meta) = self.tab_meta.get_mut(ix) {
                meta.has_bell = false;
            }
            self.reveal_active_tab();
            self.focus_active(window, cx);
            self.sync_side_panel_to_active(true, cx);
            cx.notify();
        }
    }

    /// ctrl+shift+w（对齐旧壳 CloseTab 语义）：tab 有分屏时关聚焦 pane，
    /// 单 pane 时关整个 tab；设置 tab 直接关 tab。
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
            return;
        }
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
        if self.settings_open {
            if let Some((view, _)) = self.settings_surface.as_ref() {
                let focus = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| window.focus(&focus, cx));
            }
            return;
        }
        let focus = match self.tabs.get(self.active) {
            Some(tab @ WorkspaceTab::Terminal { .. }) => match tab.focused_view() {
                Some(view) => view.read(cx).focus_handle(cx),
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
        window.defer(cx, move |window, cx| window.focus(&focus, cx));
    }

    /// 聚焦 tab 的**宿主可见** cwd。必须先识别 WSL，再读 `local_cwd()`：Windows
    /// 会把来宾 `/` 当成当前盘根目录，反过来的顺序会把 WSL `/` 显示成 `D:\`。
    /// WSL `/mnt/<盘>` 与可达 UNC 仍可供复制/Explorer 等宿主操作使用。
    ///
    /// Git 视图不受这里成败影响——它另有来宾直读路径，见 [`Self::active_wsl_cwd`]。
    fn active_local_cwd(&self, cx: &App) -> Option<std::path::PathBuf> {
        if let Some(located) = self.active_wsl_cwd(cx) {
            return crate::shell_detect::wsl_host_cwd(&located);
        }
        let view = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view)?;
        view.read(cx).local_cwd()
    }

    /// 聚焦 tab 所在的 WSL 发行版 + 来宾目录；不是 WSL、或发行版无从确定
    /// （裸 `wsl` 启动）时为 `None`。Git 视图拿它在来宾里直接跑 git，不经
    /// 任何 UNC 映射，所以宿主看不见 WSL 文件系统时依然有效。
    fn active_wsl_cwd(&self, cx: &App) -> Option<crate::shell_detect::WslCwd> {
        let view = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view)?;
        let raw = view.read(cx).cwd.clone();
        let Some(crate::session::LaunchSession::Shell { program, args, .. }) =
            self.meta(self.active).launch
        else {
            return None;
        };
        crate::shell_detect::wsl_cwd(&raw, &program, &args)
    }

    /// 抽屉这一帧该跟随的位置。WSL 先分流：只有 `/mnt/<盘>` 映射到宿主盘，
    /// `/`、`/home`、`/etc` 等始终交给来宾 `find`，不依赖不稳定的 UNC。
    ///
    /// 为什么要这个门：`/mnt/<盘>/…` 已经被 [`Self::active_local_cwd`] 落回宿
    /// 主盘，此时宿主 git 与来宾 git 读的是同一个工作树，而宿主 git 快得多
    /// （来宾路径每次快照都要起一个 `wsl.exe`，`/mnt` 上的 git 本身也慢）。
    /// 只有 `/home/…` 这类来宾自有路径宿主根本读不到，才值得走来宾。
    fn side_panel_follow(
        &self,
        cx: &App,
    ) -> (Option<std::path::PathBuf>, Option<crate::shell_detect::WslCwd>) {
        if let Some(wsl) = self.active_wsl_cwd(cx) {
            return match crate::shell_detect::wsl_mounted_host_cwd(&wsl) {
                Some(cwd) => (Some(cwd), None),
                None => (None, Some(wsl)),
            };
        }
        (self.active_local_cwd(cx), None)
    }

    /// 让窗口级共享文件树立即绑定当前 active tab / focused pane。切 pane/tab
    /// 时清掉树内手动浏览根；普通 OSC 7 轮询只在 cwd 真变化时由模型自动清掉。
    fn sync_side_panel_to_active(
        &mut self,
        reset_browse_root: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.side_panel.open {
            return false;
        }
        // 远端 pane 的位置不在本机文件系统里。不早退的话，`side_panel_follow`
        // 会拿远端路径去问宿主 `is_dir`（必然为假），于是本地树保留上一个有效
        // 根——用户在 SSH tab 上看到的是**上一个本地目录**的内容，而且没有任何
        // 提示。这正是"远端浏览器识别不到"的观感来源。
        if self.remote_browser.active() {
            return false;
        }
        let (cwd, wsl) = self.side_panel_follow(cx);
        let cleared = reset_browse_root && self.side_panel.clear_custom_root();
        self.side_panel.sync_at(cwd, wsl) || cleared
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

        self.sync_side_panel_to_active(true, cx);

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
                            if workspace.sync_side_panel_to_active(false, cx) {
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
                | PaletteAction::NewWindow
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
                        hint_style: WorkspacePaletteHintStyle::Shortcut,
                        search: item.search.to_owned(),
                        action: WorkspacePaletteAction::Shared(item.action.clone()),
                        icon: None,
                        icon_glyph: None,
                        icon_path: None,
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
                            hint_style: WorkspacePaletteHintStyle::Metadata,
                            search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
                            action: WorkspacePaletteAction::LaunchSshHost(host),
                            icon: None,
                            icon_glyph: Some(glyph),
                            icon_path: None,
                        }
                    },
                ),
            );
            rows
        });
        let mut rows: Vec<_> = rows
            .into_iter()
            .filter(|row| {
                if let Some(filter) = self.quick_jump_filter
                    && !filter.matches(&row.action)
                {
                    return false;
                }
                if self.shell_picker_open {
                    let keep = match self.launcher_filter {
                        crate::display::command_palette::LauncherFilter::All => true,
                        crate::display::command_palette::LauncherFilter::Ssh => {
                            matches!(row.action, WorkspacePaletteAction::LaunchSshHost(_))
                        },
                        crate::display::command_palette::LauncherFilter::Shell => {
                            matches!(
                                row.action,
                                WorkspacePaletteAction::LaunchShell(_)
                                    | WorkspacePaletteAction::LaunchProfile(_)
                            )
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

    fn reset_palette_query(
        &self,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        self.command_palette_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
    }

    /// 结果树里分组标题也是直接滚动子节点；键盘选中的是数据行，因此要把
    /// 数据索引换成真实子节点索引，才能让 `ScrollHandle` 精确揭示当前行。
    fn palette_scroll_node_index(rows: &[WorkspacePaletteRow], selected: usize) -> usize {
        let mut node_ix = 0;
        let mut previous_group: Option<&str> = None;
        for (row_ix, row) in rows.iter().enumerate() {
            if previous_group != Some(row.group.as_str()) {
                previous_group = Some(row.group.as_str());
                node_ix += 1;
            }
            if row_ix == selected {
                return node_ix;
            }
            node_ix += 1;
        }
        0
    }

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette_open {
            self.close_command_palette(window, cx);
            return;
        }
        self.command_manager_open = false;
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.palette_override = None;
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.quick_jump_filter = None;
        self.reset_palette_query(
            workspace_ui_language().pick("输入命令…", "Type a command..."),
            window,
            cx,
        );
        cx.notify();
    }

    fn dismiss_palette_state(&mut self) {
        self.command_palette_open = false;
        self.palette_override = None;
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.quick_jump_filter = None;
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
        let rows = self.filtered_palette_rows(cx);
        let len = rows.len();
        if len == 0 {
            self.command_palette_selected = 0;
        } else {
            self.command_palette_selected =
                (self.command_palette_selected as isize + delta).rem_euclid(len as isize) as usize;
            self.command_palette_scroll.scroll_to_item(Self::palette_scroll_node_index(
                &rows,
                self.command_palette_selected,
            ));
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
            WorkspacePaletteAction::FocusTab(tab) => {
                self.dismiss_palette_state();
                self.activate_tab(tab, window, cx);
                self.focus_active(window, cx);
                cx.notify();
            },
            WorkspacePaletteAction::FocusPane { tab, pane } => {
                self.dismiss_palette_state();
                self.activate_tab(tab, window, cx);
                self.focus_pane(tab, pane, window, cx);
                self.focus_active(window, cx);
                cx.notify();
            },
            WorkspacePaletteAction::OpenDirectory(path) => {
                self.dismiss_palette_state();
                self.add_terminal_at(Some(path), None, window, cx);
            },
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
            WorkspacePaletteAction::LaunchProfile(profile) => {
                self.launch_palette_profile(profile, window, cx);
            },
        }
    }

    fn open_quick_jump_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_manager_open = false;
        self.shell_picker_open = false;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.quick_jump_filter = Some(QuickJumpFilter::All);
        self.palette_override = Some(quick_jump::rows(self, cx));
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.reset_palette_query(
            QuickJumpFilter::All.placeholder(workspace_ui_language()),
            window,
            cx,
        );
        cx.notify();
    }

    /// 三点 / Ctrl+K：旧壳 `NewTabMenu` → `open_shell_menu` → `PaletteMode::Profiles`。
    fn open_shell_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_manager_open = false;
        let default_shell_id = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .and_then(|settings| settings.shell_id.clone())
            .unwrap_or_else(|| "powershell".to_owned());
        let language = workspace_ui_language();
        let rows = shell_palette_rows(
            crate::shell_detect::detect_shells(),
            crate::terminal_profiles::TerminalProfiles::load()
                .map(|store| store.as_config_profiles())
                .unwrap_or_default(),
            crate::gpui_shell::ssh_hosts::SshHostLists::load().merged(),
            &default_shell_id,
            language,
            window.scale_factor().max(0.5),
        );
        self.palette_override = Some(rows);
        self.shell_picker_open = true;
        self.launcher_filter = crate::display::command_palette::LauncherFilter::All;
        self.quick_jump_filter = None;
        self.command_palette_open = true;
        self.command_palette_selected = 0;
        self.reset_palette_query(
            WorkspacePaletteFilter::Launcher(crate::display::command_palette::LauncherFilter::All)
                .placeholder(language),
            window,
            cx,
        );
        cx.notify();
    }

    fn toggle_shell_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette_open && self.shell_picker_open {
            self.close_command_palette(window, cx);
            return;
        }
        self.open_shell_palette(window, cx);
    }

    fn set_workspace_palette_filter(
        &mut self,
        filter: WorkspacePaletteFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match filter {
            WorkspacePaletteFilter::Launcher(filter) => {
                if !self.shell_picker_open || self.launcher_filter == filter {
                    return;
                }
                self.launcher_filter = filter;
            },
            WorkspacePaletteFilter::QuickJump(filter) => {
                if self.quick_jump_filter.is_none() || self.quick_jump_filter == Some(filter) {
                    return;
                }
                self.quick_jump_filter = Some(filter);
            },
        }
        self.command_palette_selected = 0;
        self.command_palette_scroll.scroll_to_item(0);
        self.command_palette_input.update(cx, |input, cx| {
            input.set_placeholder(filter.placeholder(workspace_ui_language()), window, cx);
        });
        cx.notify();
    }

    fn launcher_chip_counts(
        &self,
    ) -> [(crate::display::command_palette::LauncherFilter, usize); 3] {
        use crate::display::command_palette::LauncherFilter;
        let rows = self.palette_override.as_deref().unwrap_or(&[]);
        let shell = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.action,
                    WorkspacePaletteAction::LaunchShell(_)
                        | WorkspacePaletteAction::LaunchProfile(_)
                )
            })
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

    fn quick_jump_chip_counts(&self) -> [(QuickJumpFilter, usize); 5] {
        let rows = self.palette_override.as_deref().unwrap_or(&[]);
        QuickJumpFilter::ALL.map(|filter| {
            let count = rows.iter().filter(|row| filter.matches(&row.action)).count();
            (filter, count)
        })
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

    fn launch_palette_profile(
        &mut self,
        profile: crate::config::ui_config::Profile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_palette_state();
        let launch = Self::profile_launch_session(profile);
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
            self.open_quick_jump_palette(window, cx);
            return;
        }
        self.dismiss_palette_state();
        match action {
            PaletteAction::NewTab => self.add_terminal(window, cx),
            PaletteAction::NewWindow => {
                cx.defer(|cx| {
                    if let Err(error) = windowing::open_new_window(cx, None) {
                        log::warn!("failed to open GPUI window: {error}");
                    }
                });
            },
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
                let _ = self.split_focused(SplitDirection::LeftRight, window, cx);
            },
            PaletteAction::SplitDown => {
                let _ = self.split_focused(SplitDirection::TopBottom, window, cx);
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
            WorkspaceTab::Document { view, .. } => view.read(cx).title.clone().into(),
            WorkspaceTab::Code { view, .. } => view.read(cx).title.clone().into(),
            tab @ WorkspaceTab::Terminal { .. } => {
                // 标签 = 聚焦 pane 的 cwd 末级目录名（旧壳 chrome_tab_label
                // 规则）。分屏计数**不拼在这里**：这份字符串还要喂给 runtime
                // API 的 tab label、跨窗拖拽标题和重命名预填，掺进 "⊞2" 会
                // 一路泄漏，而且长标题下会被 truncate_tab_label 截掉、被
                // custom_name 整条顶掉。计数改由 TabPresentation::pane_count
                // 单独画成胶囊，见 sidebar/top_tabs 的渲染。
                match tab.focused_view() {
                    Some(view) => view.read(cx).tab_label().into(),
                    None => SharedString::from("shell"),
                }
            },
        }
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
        let (cwd, wsl) = self.side_panel_follow(cx);
        self.side_panel.sync_at(cwd, wsl);
        cx.notify();
    }

    fn render_side_panel_slot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !self.side_panel_anim_armed && !self.side_panel.open {
            return div().into_any_element();
        }
        let open = self.side_panel.open;
        // 路由每帧都做，而且必须在挑渲染分支之前：聚焦 pane 可能刚从本地切到
        // 远端（或反过来），这一帧就该画对。
        let remote = self.side_panel.open && self.route_remote_browser(window, cx);
        let panel = match self.side_panel.view {
            // "文件"视图画谁由聚焦 pane 的身份决定，不是一个用户要自己选的
            // 页签——用户想看的永远是"当前这台机器上的文件"。
            crate::display::side_panel::PanelView::Files if remote => self.render_remote_files(cx),
            crate::display::side_panel::PanelView::Files => self.render_file_tree(cx),
            crate::display::side_panel::PanelView::Git => self.render_git_tree(cx),
        };
        div()
            .h_full()
            .flex()
            .justify_end()
            .flex_shrink_0()
            .overflow_hidden()
            // 槽位自己铺壳色：底部那 8px 间距与抽屉圆角的缺口露出来的必须是壳色，
            // 和终端卡四周的卡缝同一底。workspace 根是透明的（Acrylic 要透到 DWM），
            // 不铺这层的话缝里露的是系统背板——实测比壳色亮一档且靠窗边越亮
            // （44,46,53 → 62,64,71 vs 壳色 33,37,46），就是"底部颜色不对"。
            .bg(cx.theme().background)
            .child(
                // 抽屉整体从右缘推进来，而不是原地被擦出来。旧壳
                // （side_panel.rs:1718）把 x 插值成
                // `rest_x + (1-eased) * (w + margin)`——整块浮板在动；只动槽位宽度
                // 的话内容一动不动，只有裁剪窗口在变宽，那就是"擦除"的观感来源。
                // 槽位宽度仍然同步收放，正文（终端卡）才会跟着让位。
                //
                // 底部 8px 由槽位给（用户 08-26 裁定「文件树底部要留一段间距」，
                // 此前抽屉直插窗口底边）：写成抽屉自己的 margin 会和它的 `h_full`
                // 相加而溢出槽位、底部两角被 `overflow_hidden` 裁掉；写成父级
                // padding 则 `h_full` 按内容框解析，正好矮 8px。上边贴 chrome 下沿、
                // 右边贴窗口右缘不变，左边那条缝由终端卡的 `pr` 给。
                div()
                    .relative()
                    .h_full()
                    .flex_shrink_0()
                    .pb_2()
                    .child(panel)
                    .with_animation(
                        ("side-panel-push", open as usize),
                        Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                        move |band, t| {
                            let progress = if open { t } else { 1.0 - t };
                            band.left(px(SIDE_PANEL_SLOT_W * (1.0 - progress)))
                        },
                    ),
            )
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

    /// 复制标签页：另开一个**同身份**的 Tab，不克隆活动 PTY 也不克隆分屏树
    /// （旧壳 `window_context::duplicate_tab` 同合同——副本是新进程/新会话，
    /// 也避开 PTY 共享所有权）。
    ///
    /// 关键是继承 launch 身份。WSL / Git Bash / Nushell 这类 Tab 的
    /// program+args 记在 [`TabMeta::launch`]，而此前这里一律走
    /// `add_terminal_at`——那是"现取设置里的默认 shell"，于是复制一个
    /// WSL Tab 会得到 pwsh。`Default`（含旧快照没记身份的 `None`）仍按
    /// "复制这一刻"的默认 shell 解析，与旧壳 `spawn_tab_at` 一致；用户
    /// 元数据（自定义名字 / 色标）照旧壳一并带到副本上。
    fn duplicate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let Some(view) = tab.focused_view() else { return };
        let (ssh, cwd, remote_cwd, pane_id) = {
            let view = view.read(cx);
            (view.ssh_destination.clone(), view.local_cwd(), view.remote_cwd(), view.pane_id)
        };
        let meta = self.meta(ix);
        if let Some(destination) = ssh {
            let remote_cwd = remote_cwd.or_else(|| {
                self.remote_browser.path_for(pane_id, &destination).map(ToOwned::to_owned)
            });
            self.add_ssh_terminal_at(destination, remote_cwd, window, cx);
        } else {
            let launch = match meta.launch {
                None | Some(crate::session::LaunchSession::Default) => {
                    Self::configured_local_launch(cx)
                },
                Some(launch) => launch,
            };
            self.add_terminal_with(launch, cwd, None, window, cx);
        }
        // 两个 add_* 入口都已把副本设为 active，名字与色标在这里覆写。
        if let Some(target) = self.tab_meta.get_mut(self.active) {
            target.custom_name = meta.custom_name;
            target.color = meta.color;
        }
        cx.notify();
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
        let directory =
            crate::platform::dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
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

    /// Terminal tab 的内容区：单叶直渲、缩放态聚焦 pane 满卡，否则按
    /// 分屏树递归铺陈。
    fn render_terminal_tab(&self, tab_ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(WorkspaceTab::Terminal { tree, panes, focused, zoomed, broadcast }) =
            self.tabs.get(tab_ix)
        else {
            return div().into_any_element();
        };
        if *zoomed || tree.is_leaf() {
            let pane = panes.iter().find(|pane| pane.id == *focused).or_else(|| panes.first());
            let view = pane.map(|pane| pane.view.clone());
            let probe = pane.map(|pane| {
                let pane_id = pane.id;
                let store = self.pane_bounds.clone();
                // 普通单 Pane 也必须记录窗口坐标；dock 命中与旧壳一样依赖
                // 当前终端区域，不能只在进入分屏递归后才得到边界。
                gpui::canvas(
                    move |bounds, _, _| {
                        store.borrow_mut().insert(pane_id, bounds);
                    },
                    |_, _, _, _| (),
                )
                .absolute()
                .inset_0()
            });
            // 缩放态也要画标题条：否则进了独占就没有退出按钮，只剩快捷键。
            // 满卡时标题条两个上角都贴着卡片圆角。
            let header = pane.filter(|_| pane_header::header_visible(panes.len())).map(|pane| {
                let ordinal = pane_header::pane_order(tree)
                    .iter()
                    .position(|id| *id == pane.id)
                    .map_or(1, |at| at + 1);
                self.render_pane_header(
                    tab_ix,
                    pane.id,
                    ordinal,
                    &pane.view,
                    true,
                    *zoomed,
                    *broadcast,
                    pane_header::HeaderCorners { top_left: true, top_right: true },
                    cx,
                )
            });
            return v_flex()
                .size_full()
                .children(header)
                .child(div().flex_1().min_h_0().relative().children(probe).children(view))
                .into_any_element();
        }
        let mut path = Vec::new();
        let order = pane_header::pane_order(tree);
        self.render_split_node(
            tab_ix,
            tree,
            &mut path,
            panes,
            *focused,
            &order,
            *broadcast,
            pane_header::HeaderCorners { top_left: true, top_right: true },
            cx,
        )
    }

    /// 递归渲染分屏树。Split 节点 = flex 容器：first 占 `relative(ratio)`、
    /// 分隔条固定 `DIVIDER_GAP`、second 吃剩余（与 `nebula_split::layout`
    /// 的切割数学等价到 ±divider×ratio ≤ 2px；比例在提交时按真实视口吸附
    /// 整格，不累积漂移）。节点视口与 pane 矩形经 canvas prepaint 回写
    /// 帧记录，供拖拽换算与方向导航消费。
    /// `order` 是本 tab 的 pane 视觉次序（序号徽章的来源）：每层递归都传同一
    /// 份，不在叶子里重新遍历子树——那样每个叶子都会把自己算成 1。
    ///
    /// `corners` 标记这个子树是否还贴着卡片的上两角。GPUI 的 `overflow_hidden`
    /// 只按**矩形**裁剪，不跟圆角，所以带不透明底色的标题条会直接盖住卡片
    /// 圆角（用户 08-23 报的「上面一行越界」）。谁该收角只能由树位置决定，
    /// 在这里逐层往下传。
    #[allow(clippy::too_many_arguments)]
    fn render_split_node(
        &self,
        tab_ix: usize,
        node: &SplitTree<u64>,
        path: &mut Vec<bool>,
        panes: &[TerminalPane],
        focused: u64,
        order: &[u64],
        broadcast: bool,
        corners: pane_header::HeaderCorners,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            SplitTree::Leaf(id) => {
                let id = *id;
                let pane = panes.iter().find(|pane| pane.id == id);
                let view = pane.map(|pane| pane.view.clone());
                let store = self.pane_bounds.clone();
                let is_focused = id == focused;
                let dim = gpui::black().opacity(crate::display::NEBULA_UNFOCUSED_SPLIT_DIM);
                let veil = div().absolute().inset_0().bg(dim);
                let ordinal = order.iter().position(|other| *other == id).map_or(1, |at| at + 1);
                let header = pane.map(|pane| {
                    self.render_pane_header(
                        tab_ix, id, ordinal, &pane.view, is_focused, false, broadcast, corners, cx,
                    )
                });
                // 布局：标题条固定高 + 终端吃剩余。canvas 探针仍量**整个叶子**
                // ——方向导航与 dock 命中读的是 pane 矩形，不是终端矩形。
                v_flex()
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
                    .children(header)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .relative()
                            .children(view)
                            // 与旧壳一致：不用焦点描边，仅给非活动 pane 覆 30%
                            // 黑色 veil。veil 只盖终端区——压暗标题条会把四个
                            // pane 的标题一起糊成灰。
                            .when(!is_focused, |pane| pane.child(veil)),
                    )
                    .into_any_element()
            },
            SplitTree::Split { direction, ratio, dragging, first, second, .. } => {
                let direction = *direction;
                // Interactive drag uses a lightweight overlay. Keeping the
                // committed ratio here avoids rebuilding/reflowing both terminal
                // grids for every high-frequency pointer event.
                let show_ratio = *ratio;
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
                let first_el = self.render_split_node(
                    tab_ix,
                    first,
                    path,
                    panes,
                    focused,
                    order,
                    broadcast,
                    corners.first_child(direction),
                    cx,
                );
                path.pop();
                path.push(true);
                let second_el = self.render_split_node(
                    tab_ix,
                    second,
                    path,
                    panes,
                    focused,
                    order,
                    broadcast,
                    corners.second_child(direction),
                    cx,
                );
                path.pop();

                let drag_path = path.clone();
                let drag_start_ratio = *ratio;
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
                            this.split_drag = Some(SplitDrag {
                                tab: tab_ix,
                                path: drag_path.clone(),
                                direction,
                                preview_ratio: drag_start_ratio,
                                close_target: None,
                                last_notified: std::time::Instant::now(),
                            });
                            if let Some(WorkspaceTab::Terminal { tree, .. }) =
                                this.tabs.get_mut(tab_ix)
                                && let Some(SplitTree::Split { preview_ratio, dragging, .. }) =
                                    tree.node_mut(&drag_path)
                            {
                                *preview_ratio = None;
                                *dragging = true;
                            }
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

    /// 分隔条拖拽的指针移动：指针位置 → 原始比例 → 轻量预览线（常规带跟手、
    /// 关闭区钉边示意"松手即关"）。pane flex、终端网格与 PTY 都不追预览，
    /// 松手后才做一次结构提交。高频鼠标事件最多按 120Hz 通知绘制。
    fn update_split_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.split_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            let position = event.position;
            self.finish_split_drag(position, window, cx);
            return;
        }
        let raw = match self.split_drag_raw_ratio(event.position, window, cx) {
            Some(raw) => raw,
            None => return,
        };
        let preview_ratio = nebula_split::preview_ratio(raw);
        let close_target = nebula_split::drag_close_target(raw);
        let Some(drag) = self.split_drag.as_mut() else { return };
        let changed = (drag.preview_ratio - preview_ratio).abs() > f32::EPSILON
            || drag.close_target != close_target;
        if !changed {
            return;
        }
        let close_changed = drag.close_target != close_target;
        drag.preview_ratio = preview_ratio;
        drag.close_target = close_target;
        if close_changed || drag.last_notified.elapsed() >= Duration::from_millis(8) {
            drag.last_notified = std::time::Instant::now();
            cx.notify();
        }
    }

    /// Visual-only divider/close preview. It lives above the stable pane tree,
    /// so moving it does not invoke `TerminalElement::prepaint` with new bounds.
    fn split_drag_visual(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let drag = self.split_drag.as_ref()?;
        let viewport = self.split_bounds.borrow().get(&(drag.tab, drag.path.clone())).copied()?;
        let visual = split_drag_visual_geometry(
            drag.direction,
            to_split_rect(&viewport),
            drag.preview_ratio,
            drag.close_target,
        );
        let line = visual.divider;
        let close = visual.close_area.map(|area| {
            div()
                .absolute()
                .left(px(area.x))
                .top(px(area.y))
                .w(px(area.w))
                .h(px(area.h))
                .bg(cx.theme().warning.opacity(0.12))
        });
        Some(
            div()
                .absolute()
                .inset_0()
                .children(close)
                .child(
                    div()
                        .absolute()
                        .left(px(line.x))
                        .top(px(line.y))
                        .w(px(line.w))
                        .h(px(line.h))
                        .bg(cx.theme().primary.opacity(0.88)),
                )
                .into_any_element(),
        )
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
            // 繁忙 pane 会先弹确认框而不会立刻 remove_leaf；必须先清掉拖拽态，
            // 否则确认框背后会残留一条永久高亮的分隔线。
            if let Some(WorkspaceTab::Terminal { tree, .. }) = self.tabs.get_mut(drag.tab)
                && let Some(SplitTree::Split { preview_ratio, dragging, .. }) =
                    tree.node_mut(&drag.path)
            {
                *preview_ratio = None;
                *dragging = false;
            }
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

impl Render for NebulaWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.is_window_active() {
            windowing::mark_active(self.runtime_window_id, cx);
        }
        if !cx.has_active_drag() {
            self.cross_window_dock = None;
        }
        // 终端卡几何取一次，布局与壳色带共用同一个实例——两处各取一次也算
        // 「各写一份」，主题在这一帧中途换掉就会出现半旧半新的卡缝。
        let card_style = crate::gpui_shell::theme::PaneCardStyle::current(cx);
        let sidebar_logo_target_px =
            (TAB_LABEL_ICON_SIZE * window.scale_factor()).round().max(1.0) as u32;
        if sidebar_logo_target_px != self.sidebar_logo_target_px {
            // GPUI 窗口可跨不同 DPI 的显示器；原纹理只在整数物理像素尺寸
            // 变化时重建，普通 render 不重复解码 PNG。
            self.sidebar_logo_images = sidebar_logo_images(sidebar_logo_target_px);
            self.sidebar_logo_target_px = sidebar_logo_target_px;
        }
        let content: Option<gpui::AnyElement> = if self.settings_open {
            self.settings_surface
                .as_ref()
                .map(|(view, _)| gpui::IntoElement::into_any_element(view.clone()))
        } else {
            match self.tabs.get(self.active) {
                Some(WorkspaceTab::Terminal { .. }) => {
                    Some(self.render_terminal_tab(self.active, cx))
                },
                Some(WorkspaceTab::Settings { view, .. }) => {
                    Some(gpui::IntoElement::into_any_element(view.clone()))
                },
                Some(WorkspaceTab::Image { view }) => {
                    Some(gpui::IntoElement::into_any_element(view.clone()))
                },
                Some(WorkspaceTab::Document { view, .. }) => {
                    Some(gpui::IntoElement::into_any_element(view.clone()))
                },
                Some(WorkspaceTab::Code { view, .. }) => {
                    Some(gpui::IntoElement::into_any_element(view.clone()))
                },
                None => None,
            }
        };
        let files_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Files;
        let git_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Git;
        let settings_active = self.settings_open;
        let top_tabs = self.tabs_position == nebula_settings::TabsPositionName::Top;
        // dock 预览：被拖 tab 悬于终端区时高亮目标半区（松手即挂到那侧）。
        let dock_preview = self
            .tab_drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| drag.dock)
            .or(self.cross_window_dock)
            .and_then(|nav| {
                self.active_terminal_area().map(|area| match nav {
                    SplitNav::Left => (area.x, area.y, area.w * 0.5, area.h),
                    SplitNav::Right => (area.x + area.w * 0.5, area.y, area.w * 0.5, area.h),
                    SplitNav::Up => (area.x, area.y, area.w, area.h * 0.5),
                    SplitNav::Down => (area.x, area.y + area.h * 0.5, area.w, area.h * 0.5),
                })
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            // 旧壳每个像素只画一次：整窗先透明清屏，再分别画标题栏、侧栏、
            // 卡外壳与卡底。根节点不能再铺一张半透明底。
            .bg(gpui::transparent_black())
            .text_color(cx.theme().foreground)
            .can_drop({
                let runtime_window_id = self.runtime_window_id;
                move |value, _, _| {
                    value
                        .downcast_ref::<windowing::CrossWindowTabDrag>()
                        .is_some_and(|payload| {
                            payload.source_window_id() != runtime_window_id
                        })
                }
            })
            .on_drag_move::<windowing::CrossWindowTabDrag>(cx.listener(
                |this,
                 event: &gpui::DragMoveEvent<windowing::CrossWindowTabDrag>,
                 _window,
                 cx| {
                    let payload = event.drag(cx);
                    if payload.source_window_id() == this.runtime_window_id {
                        this.cross_window_dock = None;
                        return;
                    }
                    let position = event.event.position;
                    this.cross_window_dock = this
                        .dock_nav_at(f32::from(position.x), f32::from(position.y));
                    cx.notify();
                },
            ))
            .on_drop(cx.listener(
                |this, payload: &windowing::CrossWindowTabDrag, window, cx| {
                    let dock = this.cross_window_dock.take();
                    if this.accept_cross_window_tab(payload, dock, window, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.continue_pending_tab_drag(event, window, cx);
                // pane 拖拽的待命态同理：罩层只在激活后才存在，越阈值那一下
                // 的 move 必须由根节点喂进去。
                this.continue_pending_pane_drag(event, cx);
            }))
            // 旧壳在窗口级 mouse-up 无条件结束 tab drag。这里必须走 capture：
            // TerminalView 可能在 bubble phase 消费释放，导致 dock 永远不提交。
            .capture_any_mouse_up(cx.listener(|this, event: &gpui::MouseUpEvent, window, cx| {
                if event.button != MouseButton::Left {
                    return;
                }
                // pane 拖拽先结算：它和 tab 拖拽互斥（起手位置不同），但待命态
                // 必须在这里清掉，否则下一次点标题条会带着上一次的按点。
                let pane_dragged = this.release_pane_drag(window, cx);
                if this.release_tab_drag_at(event.position, window, cx) || pane_dragged {
                    // 真拖拽已经完成，不能再让源 tab 的 click 或终端选择收到释放。
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.add_terminal(window, cx);
            }))
            .on_action(cx.listener(|_, _: &NewWindow, _, cx| {
                cx.defer(|cx| {
                    if let Err(error) = windowing::open_new_window(cx, None) {
                        log::warn!("failed to open GPUI window: {error}");
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &CloseActiveTerminal, window, cx| {
                this.close_active(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                if this.settings_open {
                    return;
                }
                this.sidebar_collapsed = !this.sidebar_collapsed;
                this.sidebar_fold_armed = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.toggle_settings(window, cx);
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
                let _ = this.split_focused(SplitDirection::LeftRight, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                let _ = this.split_focused(SplitDirection::TopBottom, window, cx);
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
            .on_action(cx.listener(|this, _: &MoveTabLeft, window, cx| {
                this.move_active_tab(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabRight, window, cx| {
                this.move_active_tab(true, window, cx);
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
                if !this.copy_focused_terminal(window, cx) {
                    // Copy 是条件动作：无有效选区时继续派发原始 KeyDownEvent，
                    // 让 Ctrl+C 等自定义组合键按终端原义进入 PTY。
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &PasteClipboard, window, cx| {
                this.paste_focused_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFullscreen, window, _cx| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &OpenQuickJump, window, cx| {
                this.open_quick_jump_palette(window, cx);
            }))
            .child(
                // 用户显式配置的背景图画在 chrome 之下；系统 Mica/Aero/Acrylic
                // 位于整个 GPUI 内容层下方，由 DWM 合成，不能在这里读取壁纸仿画。
                // 卡外区域由这层负责，卡内切片由终端元素在卡底色之上重画。
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
                // 侧栏模式保留旧壳的侧栏开关与齿轮。组件默认 34px 标题栏会让
                // 32px 按钮几乎贴边，因此显式设为 48px、上下各留 8px；右侧
                // 窗口控制仍共享同一标题带。
                window_titlebar::settings_aware_title_bar(settings_active, cx)
                    .when(top_tabs, |bar| {
                        // 正文卡在顶部模式保留 8px 左缝；标题栏默认 12px，
                        // 这里覆写后首个 tab 才与卡内 powerline 严格同轴。
                        bar.pl(px(top_tabs::TOP_TAB_LEFT_INSET)).child(
                            self.render_top_title_bar(
                                files_active,
                                git_active,
                                settings_active,
                                window,
                                cx,
                            ),
                        )
                    })
                    .when(!top_tabs, |bar| {
                        bar.child(self.render_sidebar_title_bar(
                            files_active,
                            git_active,
                            settings_active,
                            cx,
                        ))
                    }),
            )
            .child(
                // 不用 h_flex：它默认 items_center，会把子项高度压成内容高度。
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .when(!top_tabs && !settings_active, |row| {
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
                                            .left(px(
                                                sidebar_resize_visual_offset(cx)
                                                    - SIDEBAR_RESIZE_HANDLE_WIDTH * 0.5,
                                            ))
                                            .w(px(SIDEBAR_RESIZE_HANDLE_WIDTH))
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
                        // 终端卡（一体化外壳）：唯一的结构分界。圆角、卡缝、投影、
                        // 竖线四项全部来自 `PaneCardStyle`（主题默认叠用户覆盖），
                        // 所以「浮起的圆角卡」和「铺满到窗口边 + 一条竖线」是同一
                        // 条渲染路径的两组取值，不是两套代码。无描边——融合靠壳色
                        // 包围圆角卡本身，不靠线框。
                        //
                        // 上边距一律为零：侧栏 / 终端卡 / 右侧抽屉三列的顶边都贴
                        // chrome 下沿（用户 08-26 裁定「pane 和左侧 tab 抬到和文件树
                        // 顶部一致」），这条写在 `PaneCardStyle::resolve` 里。
                        div()
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .child(
                                gpui::canvas(
                                    |_, _, _| (),
                                    |bounds, _, window, cx| {
                                        // 卡缝、圆角、竖线全部读同一份 style 真源：
                                        // 布局的 padding 与这里的壳色带必须逐边对上，
                                        // 两边各写一份字面量就是那圈白边的来源——壳色
                                        // 若按四边对称推算，卡的上两个圆角外侧会漏
                                        // 覆盖，浅色主题下直接露出一道顶部白缝。
                                        let card =
                                            crate::gpui_shell::theme::PaneCardStyle::current(cx);
                                        crate::gpui_shell::theme::paint_shell_around_card(
                                            bounds,
                                            card.margin,
                                            window,
                                            cx,
                                        );
                                    },
                                )
                                .absolute()
                                .inset_0(),
                            )
                            .pt(px(card_style.margin.top))
                            .pr(px(card_style.margin.right))
                            .pb(px(card_style.margin.bottom))
                            .pl(px(card_style.margin.left))
                            .child(
                            div()
                                .size_full()
                                .rounded(px(card_style.radius))
                                .bg(crate::gpui_shell::theme::card_content_bg(cx))
                                .when(card_style.shadow, |card| {
                                    // 投影落在有圆角的这一层，才会跟着卡的形状走。
                                    // 父容器没有 overflow_hidden，所以 blur 可以溢出
                                    // 到卡缝之外——那正是「卡浮在壳上」的观感来源。
                                    card.shadow(vec![crate::gpui_shell::theme::card_shadow(cx)])
                                })
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
                        )
                            .child(
                                // 竖线必须在卡内容之后覆盖绘制：内容底色带透明度，
                                // 若只盖住正文段，会让同一条线在标题栏下沿变色。
                                gpui::canvas(
                                    |_, _, _| (),
                                    |bounds, _, window, cx| {
                                        let card =
                                            crate::gpui_shell::theme::PaneCardStyle::current(cx);
                                        if let Some(divider_bounds) = pane_card_divider_bounds(
                                            bounds,
                                            card.divider,
                                            window.scale_factor(),
                                        ) {
                                            window.paint_quad(fill(
                                                divider_bounds,
                                                crate::gpui_shell::theme::card_divider_color(cx),
                                            ));
                                        }
                                    },
                                )
                                .absolute()
                                .inset_0(),
                            ),
                    )
                    .child(self.render_side_panel_slot(window, cx)),
            )
            .when_some(dock_preview, |root, (x, y, w, h)| {
                root.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(w))
                        .h(px(h))
                        .rounded(crate::gpui_shell::theme::card_radius(cx))
                        // 旧壳是低透明青色水洗，不是高饱和实线框；描边只
                        // 提示落区边界，不能盖过终端内容成为视觉主体。
                        .border_1()
                        .border_color(cx.theme().primary.opacity(0.28))
                        .bg(cx.theme().primary.opacity(0.08)),
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
            .children(self.pane_drag_overlay(cx))
            .children(self.split_drag_visual(cx))
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
                            // 分界跟着指针走：换算用的偏移必须与热区同源，
                            // 否则抓住线之后线会甩在指针后面。
                            let width = (f32::from(event.position.x)
                                - sidebar_resize_visual_offset(cx))
                                .clamp(
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
            .when(self.command_manager_open, |root| {
                root.child(self.render_command_manager(window, cx))
            })
            .when_some(self.render_file_tree_context_menu(), |root, menu| {
                root.child(menu)
            })
            .when_some(self.render_tab_context_menu(), |root, menu| root.child(menu))
            .when_some(self.render_selection_context_menu(), |root, menu| {
                root.child(menu)
            })
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
