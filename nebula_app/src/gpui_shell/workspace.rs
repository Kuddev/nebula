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
use gpui_component::menu::PopupMenuItem;
use nebula_split::{DIVIDER_GAP, HIT_SLOP, RemoveOutcome, SplitDirection, SplitNav, SplitTree};

gpui::actions!(
    nebula_workspace,
    [
        NewTerminal,
        CloseActiveTerminal,
        ToggleSidebar,
        OpenSettings,
        ToggleCommandPalette,
        CloseCommandPalette,
        CommandPaletteUp,
        CommandPaletteDown,
        ToggleFileTree,
        SplitRight,
        SplitDown,
        RenameActiveTab,
        ToggleZoom,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown
    ]
);

/// 工作区静态默认绑定的 combo 集（[`init`] 的镜像）。撤销已失效的自定义
/// 注入时要排除：gpui 的 NoAction 打在静态默认键上会误杀基础功能。
const STATIC_DEFAULT_COMBOS: &[&str] = &[
    "ctrl-shift-t",
    "ctrl-shift-w",
    "ctrl-shift-b",
    "ctrl-,",
    "ctrl-shift-p",
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
];

/// `keybind=` 自定义表（两壳共读）中 config::Action → GPUI 工作区动作的
/// 映射。映射不到的动作（prompt 跳转、复制粘贴、字号……）GPUI 壳尚未实
/// 装，编辑器仍可读写（旧壳消费），这里跳过不注入。
fn custom_workspace_binding(combo: &str, action: &crate::config::Action) -> Option<KeyBinding> {
    use crate::config::Action;
    let combo = gpui_binding_combo(combo);
    match action {
        Action::ToggleCommandPalette => {
            Some(KeyBinding::new(&combo, ToggleCommandPalette, None))
        },
        Action::CreateNewTab => Some(KeyBinding::new(&combo, NewTerminal, None)),
        Action::CloseTab => Some(KeyBinding::new(&combo, CloseActiveTerminal, None)),
        Action::ToggleFilesPanel => Some(KeyBinding::new(&combo, ToggleFileTree, None)),
        Action::SplitRight => Some(KeyBinding::new(&combo, SplitRight, None)),
        Action::SplitDown => Some(KeyBinding::new(&combo, SplitDown, None)),
        Action::ToggleZoom => Some(KeyBinding::new(&combo, ToggleZoom, None)),
        Action::FocusPaneLeft => Some(KeyBinding::new(&combo, FocusPaneLeft, None)),
        Action::FocusPaneRight => Some(KeyBinding::new(&combo, FocusPaneRight, None)),
        Action::FocusPaneUp => Some(KeyBinding::new(&combo, FocusPaneUp, None)),
        Action::FocusPaneDown => Some(KeyBinding::new(&combo, FocusPaneDown, None)),
        // `none` 禁用键：gpui 的 NoAction 绑定在最高优先级命中时吞掉按键，
        // 与旧壳 keybind=combo:none 的语义一致。
        Action::None => Some(KeyBinding::new(&combo, gpui::NoAction, None)),
        _ => None,
    }
}

/// 存储格式 combo（`ctrl+shift+t`）→ gpui 绑定串（`ctrl-shift-t`）。键名
/// 两套体系同构（小写命名键 + 单字符），只有 digitN 是旧壳 scancode 专用
/// 记法，折回数字字符。
fn gpui_binding_combo(combo: &str) -> String {
    combo.replace('+', "-").replace("digit", "")
}

/// 注册工作区快捷键；在 `gpui_component::init` 之后调用一次。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-w", CloseActiveTerminal, None),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, None),
        KeyBinding::new("ctrl-shift-f", ToggleFileTree, None),
        KeyBinding::new("escape", CloseCommandPalette, None),
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
        decode_sidebar_logo(
            include_bytes!("../../../extra/logo/ai_claude.png"),
            None,
            target_size,
        )
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
    if let Some(image) =
        decode_sidebar_logo(
            include_bytes!("../../../extra/logo/ai_grok_light.png"),
            None,
            target_size,
        )
    {
        images.insert((AiLogo::Grok, true), image);
    }
    if let Some(image) =
        decode_sidebar_logo(
            include_bytes!("../../../extra/logo/ai_grok_dark.png"),
            None,
            target_size,
        )
    {
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
/// `gap_1`(4px) 同源）；受约束拖拽按此步距换算让位槽位。
const TAB_ROW_H: f32 = 34.0;
const TAB_ROW_PITCH: f32 = TAB_ROW_H + 8.0;
const SIDE_PANEL_SLOT_W: f32 = 328.0;

/// 侧栏 tab 行的关闭按钮边长。旧壳 `chrome_tab_layout` 取
/// `max(row_h * 0.58, 16)`；`Button::xsmall()` 的 `size_5` 恰好落在这个数上，
/// 所以两个壳的 × 命中区同尺寸。垂直位置**必须**由 flex 居中给出：曾用
/// `absolute().top(px(3.0))` 硬写，34px 行里整枚按钮偏上 4px（用户报的
/// "删除按钮偏移了"）。
const TAB_CLOSE_SIZE: f32 = 20.0;

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

/// 侧栏 tab 的**受约束**拖拽（旧壳 `TabDrag` 语义，非自由 DnD）：
/// 被拖的整行骑在指针的 Y 位移上、钳制在列表范围内，只沿列表轴滑动；
/// 路径上的行向反方向让出一个槽位；释放时按让位结果提交换位。
/// 横向语义（旧壳 `dock_nav_at`）：指针进入终端区域后按四分区三角判定
/// dock 侧，释放时把被拖 tab 的整棵分屏树挂进活动 tab（50/50）。
struct TabDrag {
    /// 被拖 tab 的存储下标（拖拽期间存储顺序不变，只有视觉位移）。
    source: usize,
    /// 按下点的窗口坐标；位移 = 当前指针 − 它（X 只参与激活阈值）。
    press_x: f32,
    press_y: f32,
    /// 当前视觉位移（逻辑 px，已按列表首尾钳制）。
    offset_y: f32,
    /// 过阈值才算真拖拽；未过阈值的按-放仍是点击（选中交给 click）。
    active: bool,
    /// 指针悬于终端区域时的 dock 侧（最近边三角分区）；侧栏内为 None。
    dock: Option<SplitNav>,
}

#[derive(Clone)]
enum WorkspacePaletteAction {
    Shared(crate::display::command_palette::PaletteAction),
    RunAiSession {
        command: String,
        cwd: Option<std::path::PathBuf>,
    },
    /// 启动器混排的 SSH 主机行（数据源 = 共享主机列表权威）。
    LaunchSshHost(String),
}

#[derive(Clone)]
struct WorkspacePaletteRow {
    group_order: usize,
    group: String,
    label: String,
    hint: String,
    search: String,
    action: WorkspacePaletteAction,
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

fn open_in_file_manager(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
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
            });
        }
    }
    rows
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
}

/// 侧栏行内重命名的活动状态（旧壳 `nebula_tab_rename` 同形态：被编辑的那
/// 一行原地变输入框，而不是弹一个对话框）。Enter 提交、Esc 取消、失焦提交；
/// 提交空串 = 恢复自动标签名。
struct TabRename {
    ix: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
}

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
    /// 运行时持久化的侧栏逻辑宽；布局、初始窗口和折叠动画必须同源。
    sidebar_width: f32,
    /// 首次手动切换后才启用折叠动画：启动帧保持静止落位（旧壳同感，
    /// spring 构造时直接初始化在端点上）。
    sidebar_fold_armed: bool,
    /// 同理，tab 列表的折叠动画也只在首次手动切换后启用。
    tabs_fold_armed: bool,
    /// 开窗时反推的目标网格（含小屏收拢）；首个终端按它 spawn。
    initial_grid: (u16, u16),
    /// 进行中的 tab 拖拽（含未过阈值的待命态）；见 [`TabDrag`]。
    tab_drag: Option<TabDrag>,
    /// 进行中的分隔条拖拽；预览比例写进树节点，松手提交（吸附整格）。
    split_drag: Option<SplitDrag>,
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
    ai_session_palette: Option<Vec<WorkspacePaletteRow>>,
    _command_palette_subscription: Subscription,
    /// Shared old-shell drawer model. GPUI owns only presentation and polling;
    /// filesystem traversal, expansion state, ignore marking and throttling
    /// remain in `display::side_panel`.
    side_panel: crate::display::side_panel::SidePanel,
    side_panel_polling: bool,
    side_panel_anim_armed: bool,
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
}

impl NebulaWorkspace {
    /// 所有新建标签共用同一个插入口，避免本地/SSH/设置各自解释配置。
    fn insert_new_tab(&mut self, tab: WorkspaceTab) {
        let position = nebula_settings::RuntimeSettings::load().new_tab_position;
        let at = new_tab_insert_index(position, self.active, self.tabs.len());
        self.insert_tab_at(at, tab, TabMeta::default());
        self.active = at;
    }

    /// `tabs` + `tab_meta` 的唯一插入口（见 [`TabMeta`] 的同下标合同）。
    fn insert_tab_at(&mut self, at: usize, tab: WorkspaceTab, meta: TabMeta) {
        let at = at.min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.tab_meta.insert(at, meta);
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
        Some((tab, meta))
    }

    /// 当前标签的元数据（下标越界时给一份默认值，读取点因此不必各自判空）。
    fn meta(&self, ix: usize) -> TabMeta {
        self.tab_meta.get(ix).cloned().unwrap_or_default()
    }

    pub fn new(
        window: &mut Window,
        ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar_width = nebula_settings::RuntimeSettings::load().sidebar_width;
        let initial_grid = Self::size_window_to_default_grid(window, cx, sidebar_width);
        let this = cx.entity().downgrade();
        let appearance_sub = window.observe_window_appearance(move |_, cx| {
            if let Some(workspace) = this.upgrade() {
                workspace.update(cx, |workspace, cx| workspace.apply_runtime_settings(cx));
            }
        });
        let command_palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索命令…"));
        let git_commit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("提交信息…"));
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
            sidebar_width,
            sidebar_fold_armed: false,
            tabs_fold_armed: false,
            initial_grid,
            tab_drag: None,
            split_drag: None,
            split_bounds: Rc::new(RefCell::new(HashMap::new())),
            pane_bounds: Rc::new(RefCell::new(HashMap::new())),
            command_palette_open: false,
            command_palette_input,
            command_palette_selected: 0,
            git_commit_input,
            ai_session_palette: None,
            _command_palette_subscription: command_palette_subscription,
            side_panel: crate::display::side_panel::SidePanel::new(),
            side_panel_polling: false,
            side_panel_anim_armed: false,
            sidebar_logo_images: sidebar_logo_images(sidebar_logo_target_px),
            sidebar_logo_target_px,
            _appearance_sub: appearance_sub,
            custom_keybinds_applied: Vec::new(),
            spinner_phase: 0.0,
            spinner_last: std::time::Instant::now(),
            last_saved_session: None,
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
        // 会话恢复（共享 v4 schema + 崩溃断路器）：恢复不出任何 tab 才落
        // 到出厂的单终端。1 Hz 自动保存随后启动，首笔成功写盘自然把
        // `boot_attempts` 清零（快照恒以 0 构造，旧壳同义）。
        if !this.try_restore_session(window, cx) {
            this.add_terminal(window, cx);
        }
        Self::start_ai_hook_pump(ai_events, cx);
        this.start_session_autosave(cx);
        this.apply_custom_keybinds(cx);
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

    /// 配置的默认 shell 的口语短标；未配置时引擎默认拉起 PowerShell。
    fn default_shell_tag() -> SharedString {
        let runtime = nebula_settings::RuntimeSettings::load();
        crate::shell_detect::shell_short_tag(runtime.shell.as_deref().unwrap_or("powershell"))
            .into()
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
            LaunchSession::Default => {
                TerminalLaunch::Local { cwd, shell: None, shell_name: None }
            },
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
                TerminalLaunch::Ssh { destination: host.clone() }
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
        self.sidebar_width = nebula_settings::RuntimeSettings::load().sidebar_width;
        cx.notify();
    }

    fn add_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_terminal_at(None, None, window, cx);
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
            TabMeta {
                shell_tag,
                launch: Some(launch_session),
                ..TabMeta::default()
            },
        );
        self.active = at;
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
                launch: Some(crate::session::LaunchSession::Ssh {
                    host: destination,
                }),
                ..TabMeta::default()
            },
        );
        self.active = at;
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
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let workspace = workspace.clone();
            dialog
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

    fn request_close_tab(
        &mut self,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(process) = self.busy_process_in_tab(tab_ix, None, cx) else {
            self.close_tab(tab_ix, window, cx);
            return;
        };
        let body: SharedString = format!("{process} 仍在运行，关闭会中止它。").into();
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let workspace = workspace.clone();
            dialog
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
    fn try_restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
            if self.restore_tab(tab, window, cx) {
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::session::{LaunchSession, LayoutSession};

        let layout = tab.layout.clone().unwrap_or(LayoutSession::Pane {
            cwd: tab.cwd.clone(),
            agent: None,
        });
        // v1-v3 / 早期 GPUI 快照没有 launch，按共享 schema 回退 Default；
        // v4 的 Shell/Profile/Ssh 必须原样用于首 Pane，不能再次读取当前默认。
        let saved_launch = tab.launch.clone().unwrap_or(LaunchSession::Default);
        let grid = self.initial_grid;
        let mut panes: Vec<TerminalPane> = Vec::new();
        for (index, leaf) in layout.leaves().into_iter().enumerate() {
            let LayoutSession::Pane { cwd, agent } = leaf else { continue };
            let launch = if index == 0 {
                Self::terminal_launch_from_session(
                    &saved_launch,
                    crate::session::valid_dir(cwd),
                )
            } else {
                // 共享 v4 与旧壳只把 Tab 的 launch 赋给首 Pane；其它叶子没有
                // 独立启动身份，保持既有 Default 恢复语义。
                crate::gpui_shell::terminal::view::TerminalLaunch::Local {
                    cwd: crate::session::valid_dir(cwd),
                    shell: None,
                    shell_name: None,
                }
            };
            let command = agent.as_ref().and_then(|agent| agent.resume_command());
            panes.push(self.new_pane(grid, launch, command, window, cx));
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
                            .filter(|program| {
                                crate::ai_agents::AgentKind::parse(program).is_some()
                            })
                            .map(|program| AgentSession {
                                source: program.to_owned(),
                                session_id: None,
                            })
                    });
                (view.cwd.clone(), agent)
            };
            let layout =
                crate::gpui_shell::session_restore::layout_from_tree(tree, &leaf_data);
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
            let active_pane =
                tree.leaves().iter().position(|id| id == focused).unwrap_or(0);
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

    fn fork_ai_session(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.tabs.get(ix).and_then(WorkspaceTab::focused_view) else { return };
        let (command, cwd) = {
            let view = view.read(cx);
            let Some(command) = view.ai_fork_command() else { return };
            (command, view.local_cwd())
        };
        self.activate_tab(ix, window, cx);
        self.add_terminal_at(cwd, Some(command), window, cx);
    }

    /// 设置页是单例 tab（旧壳同形态）：已开则激活，未开则新建。
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(WorkspaceTab::is_settings) {
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| SettingsPane::new(window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_settings_event);
        self.insert_new_tab(WorkspaceTab::Settings { view, _subscription: subscription });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 文件路由（旧壳 `input/chrome.rs` 双击合同）：图片 → 图片 tab；
    /// 可读文本 → 文档 tab；其余交系统处理器。
    fn open_document_path(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::display::image_viewer::viewable_file(&path) {
            self.open_image_tab(path, window, cx);
        } else if crate::display::markdown_view::viewable_file(&path) {
            self.open_doc_tab(path, window, cx);
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
        if let Some(ix) = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Image { view } if view.read(cx).path == path)
        }) {
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
        if let Some(ix) = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Document { view } if view.read(cx).path == path)
        }) {
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
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    /// 拖拽中的落点槽位：源下标 + 位移换算的整槽数（越过半格即换位，
    /// 与旧壳 update_tab_drag 的中点交换一致）。
    fn drag_slot(drag: &TabDrag, len: usize) -> usize {
        let slots = (drag.offset_y / TAB_ROW_PITCH).round() as isize;
        (drag.source as isize + slots).clamp(0, len.saturating_sub(1) as isize) as usize
    }

    /// 指针移动喂给拖拽状态机：过阈值（任一轴）激活、Y 位移按列表首尾
    /// 钳制、终端区内实时计算 dock 侧。松键事件在窗外丢失时
    /// （`pressed_button` 已空）按当前位置收尾，不让让位状态卡在半途。
    fn update_tab_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish_tab_drag(window, cx);
            return;
        }
        let len = self.tabs.len();
        // 在可变借用 drag 之前算好 dock（旧壳同注：compute before the
        // mutable borrow）。dock 仅当源与目标都是可参与的 Terminal tab。
        let source = self.tab_drag.as_ref().map(|drag| drag.source);
        let dock = source.filter(|&source| self.dock_allowed(source)).and_then(|_| {
            self.dock_nav_at(f32::from(event.position.x), f32::from(event.position.y))
        });
        let drag = self.tab_drag.as_mut().expect("checked above");
        let dx = f32::from(event.position.x) - drag.press_x;
        let dy = f32::from(event.position.y) - drag.press_y;
        if !drag.active && (dy.abs() >= TAB_DRAG_THRESHOLD || dx.abs() >= TAB_DRAG_THRESHOLD) {
            drag.active = true;
        }
        if drag.active {
            let up = -(drag.source as f32) * TAB_ROW_PITCH;
            let down = (len.saturating_sub(1) as f32 - drag.source as f32) * TAB_ROW_PITCH;
            drag.offset_y = dy.clamp(up, down.max(up));
            drag.dock = dock;
            cx.notify();
        }
    }

    /// 释放：dock 侧优先（挂整树进活动 tab），其余按让位结果提交换位；
    /// 未激活的只清状态（点击语义由行的 on_click 负责）。
    fn finish_tab_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.take() else { return };
        if drag.active {
            if let Some(nav) = drag.dock {
                self.dock_tab_into_active(drag.source, nav, window, cx);
            } else {
                let target = Self::drag_slot(&drag, self.tabs.len());
                self.move_tab(drag.source, target, window, cx);
            }
        }
        cx.notify();
    }

    /// 被拖 tab 能否 dock 进活动 tab：双方都是 Terminal 且不是同一个。
    /// 设置/文档/图片 tab 既不能被 dock 也不能接受 dock（旧壳同约束）。
    fn dock_allowed(&self, source: usize) -> bool {
        source != self.active
            && self.tabs.get(source).is_some_and(WorkspaceTab::is_terminal)
            && self.tabs.get(self.active).is_some_and(WorkspaceTab::is_terminal)
    }

    /// 活动 tab 终端区域（全部 pane 上一帧矩形的并集，窗口坐标）。
    fn active_terminal_area(&self) -> Option<nebula_split::Rect> {
        let Some(WorkspaceTab::Terminal { panes, .. }) = self.tabs.get(self.active) else {
            return None;
        };
        let bounds = self.pane_bounds.borrow();
        let mut acc: Option<(f32, f32, f32, f32)> = None;
        for pane in panes {
            let Some(b) = bounds.get(&pane.id) else { continue };
            let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (x1, y1) = (x0 + f32::from(b.size.width), y0 + f32::from(b.size.height));
            acc = Some(match acc {
                Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
                None => (x0, y0, x1, y1),
            });
        }
        let (x0, y0, x1, y1) = acc?;
        (x1 > x0 && y1 > y0).then(|| nebula_split::Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// 指针在终端区域内的 dock 侧，区域外 None。区域沿对角线四分：最近边
    /// 获胜，得到自然的三角 dock 区（旧壳 `dock_nav_at` 逐字对照）。
    fn dock_nav_at(&self, x: f32, y: f32) -> Option<SplitNav> {
        let area = self.active_terminal_area()?;
        if !area.contains(x, y) {
            return None;
        }
        let nx = (x - area.x) / area.w;
        let ny = (y - area.y) / area.h;
        let (dl, dr, dt, db) = (nx, 1.0 - nx, ny, 1.0 - ny);
        let min = dl.min(dr).min(dt).min(db);
        Some(if min == dl {
            SplitNav::Left
        } else if min == dr {
            SplitNav::Right
        } else if min == dt {
            SplitNav::Up
        } else {
            SplitNav::Down
        })
    }

    /// 把 tab `source` 的整棵分屏树挂进活动 tab（旧壳 `dock_tab_into_active`
    /// 同合同）：活动布局变成 50/50 分割、被 dock 的树在 `nav` 侧，源 tab
    /// 从侧栏消失，焦点跟随被 dock 的 pane。纯树手术——pane 实体连同订阅
    /// 一起搬家，PTY 不动；新几何由下一帧 prepaint 的 resize 合同收敛。
    fn dock_tab_into_active(
        &mut self,
        source: usize,
        nav: SplitNav,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dock_allowed(source) {
            return;
        }
        let Some((
            WorkspaceTab::Terminal {
                panes: src_panes, tree: src_tree, focused: src_focused, ..
            },
            _meta,
        )) = self.remove_tab_at(source)
        else {
            unreachable!("dock_allowed 已保证 source 是 Terminal");
        };
        if source < self.active {
            self.active -= 1;
        }
        let Some(WorkspaceTab::Terminal { panes, tree, focused, zoomed }) =
            self.tabs.get_mut(self.active)
        else {
            unreachable!("dock_allowed 已保证 active 是 Terminal");
        };
        let old = std::mem::replace(tree, SplitTree::leaf(src_focused));
        *tree = dock_tree(old, src_tree, nav);
        panes.extend(src_panes);
        // Focus follows the pane that accepted the dock operation.
        *focused = src_focused;
        // A zoomed pane would hide the fresh split; drop the zoom.
        *zoomed = false;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 拖拽排序（旧壳 `end_tab_drag` 的侧栏 reorder 语义）：被拖 tab 落到
    /// 目标下标，其余顺移；激活位跟着自己的 tab 走，不因排序漂移。
    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let Some((tab, meta)) = self.remove_tab_at(from) else { return };
        self.insert_tab_at(to, tab, meta);
        self.active = if self.active == from {
            to
        } else {
            // 先补移除造成的左移，再补插入造成的右移。
            let mut ix = self.active;
            if ix > from {
                ix -= 1;
            }
            if ix >= to {
                ix += 1;
            }
            ix
        };
        self.focus_active(window, cx);
        cx.notify();
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
            // 图片/文档查看 tab 没有键盘焦点语义（滚轮/拖拽直达元素）。
            Some(WorkspaceTab::Image { .. } | WorkspaceTab::Document { .. }) | None => return,
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
        let language = match nebula_settings::RuntimeSettings::load().language {
            nebula_settings::LanguagePref::System => crate::display::LanguagePreference::System,
            nebula_settings::LanguagePref::ZhCn => crate::display::LanguagePreference::ZhCn,
            nebula_settings::LanguagePref::EnUs => crate::display::LanguagePreference::EnUs,
        }
        .resolved();
        let rows = self.ai_session_palette.clone().unwrap_or_else(|| {
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
                    }
                })
                .collect();
            // 启动器混排（旧壳 ⌘K 裁定）：SSH 主机与命令同列，置顶/隐藏
            // 次序由共享 merge 权威裁定。
            rows.extend(
                crate::gpui_shell::ssh_hosts::SshHostLists::load().merged().into_iter().map(
                    |host| WorkspacePaletteRow {
                        group_order: usize::MAX,
                        group: language.pick("SSH 主机", "SSH HOSTS").to_owned(),
                        label: host.clone(),
                        hint: "SSH".to_owned(),
                        search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
                        action: WorkspacePaletteAction::LaunchSshHost(host),
                    },
                ),
            );
            rows
        });
        let mut rows: Vec<_> = rows
            .into_iter()
            .filter(|row| {
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
        self.ai_session_palette = None;
        self.command_palette_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.command_palette_open {
            return;
        }
        self.command_palette_open = false;
        self.ai_session_palette = None;
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
                self.command_palette_open = false;
                self.ai_session_palette = None;
                self.add_terminal_at(cwd, Some(command), window, cx);
            },
            WorkspacePaletteAction::LaunchSshHost(host) => {
                self.command_palette_open = false;
                self.ai_session_palette = None;
                self.add_ssh_terminal(host, window, cx);
            },
        }
    }

    fn open_ai_session_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_session_palette = Some(ai_session_palette_rows(crate::ai_sessions::scan(30)));
        self.command_palette_selected = 0;
        self.command_palette_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
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
        self.command_palette_open = false;
        self.ai_session_palette = None;
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
                let _ = nebula_settings::persist_keys(&[
                    ("theme", theme.prompt_name().to_owned()),
                    ("follow_system_theme", "0".to_owned()),
                ]);
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
                    .items_center()
                    .rounded_md()
                    .when(selected, |row| row.bg(selected_bg))
                    .hover(|row| row.bg(hover_bg))
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
                                this.command_palette_open = false;
                                this.ai_session_palette = None;
                                this.add_terminal_at(cwd, Some(command), window, cx);
                            },
                            WorkspacePaletteAction::LaunchSshHost(host) => {
                                this.command_palette_open = false;
                                this.ai_session_palette = None;
                                this.add_ssh_terminal(host, window, cx);
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
            .bg(theme.overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_command_palette(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this: &mut Self, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        this.move_command_palette_selection(-1, cx);
                        cx.stop_propagation();
                    },
                    "down" => {
                        this.move_command_palette_selection(1, cx);
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
                    .child(v_flex().max_h(px(430.0)).overflow_y_scrollbar().gap_1().children(rows)),
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

    fn render_file_tree(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        let selected_path = self.side_panel.selected.clone();
        let scroll = self.side_panel.scroll;
        let root = self
            .side_panel
            .root()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "等待终端上报工作目录…".to_owned());
        let rows: Vec<_> = self.side_panel.file_rows().iter().skip(scroll).cloned().collect();

        let row_elements = rows.into_iter().enumerate().map(|(visible_ix, row)| {
            let path = row.path.clone();
            let open_path = path.clone();
            let is_dir = row.is_dir;
            let is_parent = row.is_parent;
            let selected = selected_path.as_ref() == Some(&path);
            let fg = if row.ignored { muted } else { theme.foreground };
            let _chevron = if row.is_dir && !row.is_parent {
                if row.expanded { "⌄" } else { "›" }
            } else {
                ""
            };
            let file_glyph =
                (!row.is_dir).then(|| crate::display::side_panel::file_type_icon(&row.name));
            let legacy_chevron = row
                .is_dir
                .then(|| {
                    (!row.is_parent).then(|| crate::display::side_panel::chevron_icon(row.expanded))
                })
                .flatten();

            h_flex()
                .id(SharedString::from(format!("file-tree-row-{visible_ix}")))
                .h(px(30.0))
                .w_full()
                .items_center()
                .pr_2()
                .pl(px(8.0 + row.depth as f32 * 16.0))
                .gap_1()
                .rounded_md()
                .text_color(fg)
                .when(selected, |item| item.bg(selected_bg))
                .hover(|item| item.bg(hover))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_shrink_0()
                        .font_family(mono_family.clone())
                        .text_sm()
                        .text_color(muted)
                        .child(legacy_chevron.unwrap_or("")),
                )
                .when(is_dir, |item| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(mono_family.clone())
                            .text_sm()
                            .text_color(if row.ignored { muted } else { theme.foreground })
                            .child(crate::display::side_panel::folder_icon(row.expanded)),
                    )
                })
                .when_some(file_glyph, |item, glyph| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(mono_family.clone())
                            .text_sm()
                            .child(glyph),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(row.name),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_dir {
                        if this.side_panel.click_row(visible_ix) {
                            cx.notify();
                        }
                    } else {
                        this.side_panel.selected = Some(path.clone());
                        cx.notify();
                    }
                }))
                .when(!is_dir && !is_parent, |item| {
                    item.on_double_click(cx.listener(move |this, _, window, cx| {
                        // 与旧壳 chrome 同合同：应用内能读的（图片/可读文本）
                        // 开查看 tab，其余交系统处理器。
                        this.open_document_path(open_path.clone(), window, cx);
                    }))
                })
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
            .shadow_lg()
            .occlude()
            .child(view_switch)
            .child(
                h_flex()
                    .h(px(30.0))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(root),
                    )
                    .child(
                        Button::new("file-tree-refresh")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("刷新目录树")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.request_refresh();
                                let cwd = this.active_local_cwd(cx);
                                this.side_panel.sync(cwd);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("file-tree-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("关闭目录树 (Ctrl+Shift+F)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_file_tree(cx);
                            })),
                    ),
            )
            .when_some(self.side_panel.root_notice(), |panel, notice| {
                panel.child(div().text_xs().text_color(theme.warning).child(notice.to_owned()))
            })
            .child(
                v_flex().flex_1().min_h_0().gap_1().overflow_y_scrollbar().children(row_elements),
            )
            .into_any_element()
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
            // SVN 没有暂存区：单区「修改」；Git 保持未暂存/已暂存两区。
            let sections: Vec<(&str, &Vec<(char, String)>)> = match info.vcs {
                VcsKind::Git => vec![("未暂存", &info.unstaged), ("已暂存", &info.staged)],
                VcsKind::Svn => vec![("修改", &info.unstaged)],
            };
            for (section, entries) in sections {
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
                for (index, (status, relative_path)) in entries.iter().enumerate() {
                    let path = root
                        .as_ref()
                        .map(|root| root.join(relative_path))
                        .unwrap_or_else(|| std::path::PathBuf::from(relative_path));
                    let selected_row = selected.as_ref() == Some(&path);
                    let status_color = match status {
                        'A' | '?' => theme.success,
                        'D' => theme.danger,
                        _ => theme.warning,
                    };
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!(
                                "git-tree-row-{section}-{index}-{relative_path}"
                            )))
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
                                    .w(px(18.0))
                                    .font_family(mono_family.clone())
                                    .text_sm()
                                    .text_color(status_color)
                                    .child(status.to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(relative_path.clone()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.side_panel.selected = Some(path.clone());
                                cx.notify();
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
            .shadow_lg()
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
            .child(v_flex().flex_1().min_h_0().gap_1().overflow_y_scrollbar().children(rows))
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

    /// 进入行内重命名：输入框预填当前显示名（自定义名优先，否则自动标签），
    /// 焦点直接落进去。已在编辑别的行时先提交前一行。
    fn begin_rename(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.commit_rename(window, cx);
        let current = self
            .meta(ix)
            .custom_name
            .unwrap_or_else(|| self.tab_title(ix, cx).to_string());
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                    // 点走 = 提交（旧壳空串语义：清空即恢复自动名），不静默丢弃。
                    InputEvent::Blur => this.commit_rename(window, cx),
                    _ => {},
                }
            },
        );
        input.read(cx).focus_handle(cx).focus(window);
        self.tab_rename = Some(TabRename { ix, input, _subscription: subscription });
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.tab_rename.take() else { return };
        let name = rename.input.read(cx).value().trim().to_owned();
        if let Some(meta) = self.tab_meta.get_mut(rename.ix) {
            meta.custom_name = (!name.is_empty()).then_some(name);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_rename.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    /// 标签色标：同色再点一次即取消（旧壳菜单里选中的那枚色块 = 当前色）。
    fn set_tab_color(&mut self, ix: usize, color: Option<Rgb>, cx: &mut Context<Self>) {
        if let Some(meta) = self.tab_meta.get_mut(ix) {
            meta.color = color;
            cx.notify();
        }
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
        let export = crate::session::Session::new(0, vec![tab]);
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
                    // 失败即时可重试（换个目录再来一次），不占消息栏的
                    // "待办动作"层级——分三层的判据是有没有待办，不是严重度。
                    crate::display::ToastKind::Warning,
                    format!("工作区导出失败：{error}"),
                ),
            });
        })
        .detach();
    }

    /// 标签行右键与三点按钮共用一份命令表，避免两个入口继续漂移。
    /// 条目、分组与键帽对齐旧壳 `display::context_menu` 的 Tab 目标。
    fn tab_popup_menu(
        mut menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        terminal: bool,
        ai_fork: bool,
        color: Option<Rgb>,
    ) -> PopupMenu {
        if ai_fork {
            let target = workspace.clone();
            menu = menu.item(PopupMenuItem::new("分叉 AI 会话").icon(IconName::Copy).on_click(
                move |_, window, cx| {
                    if let Some(workspace) = target.upgrade() {
                        workspace.update(cx, |workspace, cx| {
                            workspace.fork_ai_session(ix, window, cx);
                        });
                    }
                },
            ));
        }
        if terminal {
            let duplicate = workspace.clone();
            let export = workspace.clone();
            let split_right = workspace.clone();
            let split_down = workspace.clone();
            menu = menu
                .item(PopupMenuItem::new("复制标签页").icon(IconName::Copy).on_click(
                    move |_, window, cx| {
                        if let Some(workspace) = duplicate.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.duplicate_tab(ix, window, cx);
                            });
                        }
                    },
                ))
                .item(PopupMenuItem::new("导出为工作区…").icon(IconName::Inbox).on_click(
                    move |_, window, cx| {
                        if let Some(workspace) = export.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.export_tab(ix, window, cx);
                            });
                        }
                    },
                ))
                .separator()
                // `action` 只用来渲染键帽：handler 存在时组件不会 dispatch
                // 它（见 PopupMenu::confirm），所以命令仍然作用在 `ix` 上，
                // 而不是"活动标签"——右键别的标签也不会打错对象。
                .item(
                    PopupMenuItem::new("左右分屏")
                        .icon(IconName::PanelRight)
                        .action(Box::new(SplitRight))
                        .on_click(move |_, window, cx| {
                            if let Some(workspace) = split_right.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.activate_tab(ix, window, cx);
                                    workspace
                                        .split_focused(SplitDirection::LeftRight, window, cx);
                                });
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("上下分屏")
                        .icon(IconName::PanelBottom)
                        .action(Box::new(SplitDown))
                        .on_click(move |_, window, cx| {
                            if let Some(workspace) = split_down.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.activate_tab(ix, window, cx);
                                    workspace
                                        .split_focused(SplitDirection::TopBottom, window, cx);
                                });
                            }
                        }),
                );
        }
        let rename = workspace.clone();
        let close = workspace.clone();
        menu = menu
            .separator()
            .item(
                PopupMenuItem::new("重命名")
                    .icon(IconName::ALargeSmall)
                    .action(Box::new(RenameActiveTab))
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = rename.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.begin_rename(ix, window, cx);
                            });
                        }
                    }),
            )
            .item(
                PopupMenuItem::new("关闭")
                    .icon(IconName::Close)
                    .action(Box::new(CloseActiveTerminal))
                    .on_click(move |_, window, cx| {
                        if let Some(workspace) = close.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.request_close_tab(ix, window, cx);
                            });
                        }
                    }),
            );
        Self::tab_color_items(menu, workspace, ix, color)
    }

    /// 标签颜色行（旧壳菜单尾部的色板）：首槽 `A` = 无色，其后是 7 枚品牌
    /// 色。当前色带一圈选中环，再点一次同色即取消。
    ///
    /// 整行是一个 `ElementItem`：色块自己吃 mouse_down 落色，外层的 click
    /// 没有 handler，只负责收起菜单（见 `PopupMenu::confirm`）。
    fn tab_color_items(
        menu: PopupMenu,
        workspace: gpui::WeakEntity<Self>,
        ix: usize,
        current: Option<Rgb>,
    ) -> PopupMenu {
        menu.separator().item(PopupMenuItem::label("标签颜色")).item(PopupMenuItem::element(
            move |_, cx| {
                let swatches = std::iter::once(None)
                    .chain(crate::display::context_menu::TAB_COLORS.into_iter().map(Some));
                let mut row = h_flex().gap_1().py_1();
                for (slot, color) in swatches.enumerate() {
                    let selected = color == current;
                    let target = workspace.clone();
                    // 无色槽用主题 accent 打底并压一个 "A"，与旧壳一致：
                    // 它是"自动"，不是第八种颜色。
                    let fill = color
                        .map(|color| gpui::Rgba {
                            r: color.r as f32 / 255.0,
                            g: color.g as f32 / 255.0,
                            b: color.b as f32 / 255.0,
                            a: 1.0,
                        })
                        .unwrap_or_else(|| cx.theme().primary.into());
                    row = row.child(
                        div()
                            .id(("tab-color", slot))
                            .size(px(20.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(fill)
                            .cursor_pointer()
                            .when(selected, |swatch| {
                                swatch.border_2().border_color(cx.theme().foreground)
                            })
                            .when(color.is_none(), |swatch| {
                                swatch
                                    .text_size(px(11.0))
                                    .text_color(cx.theme().primary_foreground)
                                    .child("A")
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_, _, cx| {
                                    if let Some(workspace) = target.upgrade() {
                                        workspace.update(cx, |workspace, cx| {
                                            let next = if selected { None } else { color };
                                            workspace.set_tab_color(ix, next, cx);
                                        });
                                    }
                                },
                            ),
                    );
                }
                row
            },
        ))
    }

    fn sidebar_popup_menu(menu: PopupMenu, workspace: gpui::WeakEntity<Self>) -> PopupMenu {
        let new_tab = workspace.clone();
        let settings = workspace.clone();
        let files = workspace.clone();
        let git = workspace;
        menu.item(PopupMenuItem::new("新建终端").icon(IconName::Plus).on_click(
            move |_, window, cx| {
                if let Some(workspace) = new_tab.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        workspace.add_terminal(window, cx);
                    });
                }
            },
        ))
        .item(PopupMenuItem::new("设置").icon(IconName::Settings).on_click(move |_, window, cx| {
            if let Some(workspace) = settings.upgrade() {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_settings(window, cx);
                });
            }
        }))
        .separator()
        .item(PopupMenuItem::new("文件树").icon(IconName::PanelRightOpen).on_click(
            move |_, _, cx| {
                if let Some(workspace) = files.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        workspace.toggle_file_tree(cx);
                    });
                }
            },
        ))
        .item(PopupMenuItem::new("Git 状态").icon(IconName::GitHub).on_click(
            move |_, _, cx| {
                if let Some(workspace) = git.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        workspace.toggle_git_tree(cx);
                    });
                }
            },
        ))
    }

    /// 侧栏等宽标签的 cell 宽：与终端元素同一套度量法（塑形一个 "M" 取
    /// advance），列数换算与省略号都建立在它上面。字体缺失时回落 0.6em，
    /// 只影响截断位置、不会画错。
    fn sidebar_cell_width(
        &self,
        window: &mut Window,
        family: &SharedString,
        size_px: f32,
    ) -> f32 {
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
                let scale = window.scale_factor().max(0.5);
                let snap = |value: f32| (value * scale).round() / scale;
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
                    let x1 = x0 + stroke;
                    let y1 = y0 + stroke;
                    let snapped_x0 = snap(x0);
                    let snapped_y0 = snap(y0);
                    let snapped_w = (snap(x1) - snapped_x0).max(1.0 / scale);
                    let snapped_h = (snap(y1) - snapped_y0).max(1.0 / scale);
                    window.paint_quad(
                        gpui::fill(
                            Bounds::new(
                                gpui::point(px(snapped_x0), px(snapped_y0)),
                                size(px(snapped_w), px(snapped_h)),
                            ),
                            c,
                        )
                        .corner_radii(px(snapped_w.min(snapped_h) * 0.5)),
                    );
                }
            },
        )
        .size(px(11.0))
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
        // 标签字号跟着终端字号走（旧壳 chrome 只有这一套度量），所以设置页
        // 调字号、Ctrl+滚轮缩放之后侧栏一起变，不再固定 14px。
        let label_px = settings.map(|settings| settings.font_size_px).unwrap_or(15.0);
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
            .filter(|d| d.active)
            .map(|d| (d.source, Self::drag_slot(d, self.tabs.len()), d.offset_y));

        // 本次渲染里是否有「运行中」行（spinner 帧循环的开关）。
        let items_running = std::cell::Cell::new(false);
        let items = (0..self.tabs.len()).map(|ix| {
            let active = ix == self.active;
            let title = self.tab_title(ix, cx);
            let is_settings = self.tabs[ix].is_settings();
            let is_terminal = self.tabs[ix].is_terminal();
            let (program, activity, ai_fork) = self.tabs[ix]
                .focused_view()
                .map(|entity| {
                    let view = entity.read(cx);
                    let program = view
                        .running_program
                        .clone()
                        .or_else(|| {
                            view.ai_session.as_ref().map(|identity| identity.source.clone())
                        })
                        .or_else(|| view.ssh_destination.as_ref().map(|_| "ssh".to_owned()));
                    (program, view.sidebar_activity(), view.ai_fork_command().is_some())
                })
                .unwrap_or((None, SidebarActivity::Idle, false));
            // GPUI 自己持有同一组嵌入 PNG，不应被旧 OpenGL 渲染器的 `png`
            // feature 门控挡住；映射仍复用旧壳的 program 归一化规则。
            let logo = program.as_deref().and_then(crate::display::ai_logo_for_program);
            let logo_image =
                logo.and_then(|logo| self.sidebar_logo_images.get(&(logo, dark)).cloned());
            let program_glyph = program
                .as_deref()
                .filter(|_| logo_image.is_none())
                .map(crate::display::program_icon)
                .or_else(|| match &self.tabs[ix] {
                    // 文档/图片 tab 的行首图标与文件树同一套 codicon（旧壳
                    // custom_name 前缀的对应物；字形靠 mono 字体渲染）。
                    WorkspaceTab::Document { .. } => Some("\u{eb1d}"),
                    WorkspaceTab::Image { view } => Some(
                        crate::display::side_panel::file_type_icon(&view.read(cx).title),
                    ),
                    _ => None,
                });
            let workspace = cx.entity().downgrade();
            let context_workspace = workspace.clone();
            let hover_group: SharedString = format!("sidebar-tab-hover-{ix}").into();
            let shell_tag = (is_terminal && activity == SidebarActivity::Idle)
                .then(|| self.meta(ix).shell_tag)
                .flatten()
                .filter(|tag| !tag.is_empty());
            // 可用列数 = （侧栏宽 − 外层 p_2 − 行内 px_2 − 行内 gap − 状态槽
            // − 行首图标槽）÷ cell 宽。省略号由旧壳同一份 `truncate_tab_label`
            // 追加，两壳的裁切位置因此一致。
            let has_icon = is_settings || logo_image.is_some() || program_glyph.is_some();
            let label_avail = self.sidebar_width
                - 16.0
                - 16.0
                - TAB_STATUS_SLOT_W
                - 8.0
                - if has_icon { TAB_LABEL_ICON_W + 8.0 } else { 0.0 };
            let label_cols = (label_avail / cell_w).floor().max(1.0) as usize;
            let title: SharedString = crate::display::truncate_tab_label(&title, label_cols).into();
            // 用户明确设置过的标签色：行左侧一条竖光条（旧壳 strip，位置与
            // 尺寸同源：左内缩 4、上下各留 7、宽 2.5）。默认标签不占这层
            // 视觉层级。
            let tab_color = self.meta(ix).color;
            let strip = tab_color.map(|color| gpui::Rgba {
                r: color.r as f32 / 255.0,
                g: color.g as f32 / 255.0,
                b: color.b as f32 / 255.0,
                a: 1.0,
            });
            let renaming = self
                .tab_rename
                .as_ref()
                .filter(|rename| rename.ix == ix)
                .map(|rename| rename.input.clone());
            let status_color = if active { active_fg } else { muted };
            let resting_status: Option<gpui::AnyElement> = match activity {
                SidebarActivity::Running => {
                    items_running.set(true);
                    let (track, head) =
                        crate::gpui_shell::theme::sidebar_spinner_colors(cx, active);
                    Some(Self::spinner(self.spinner_phase, track, head).into_any_element())
                },
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
                .gap_2()
                .px_2()
                .h(px(TAB_ROW_H))
                .items_center()
                // 旧壳 pill 圆角 = UI_CORNER_RADIUS_LOGICAL(8)，rounded_md(6)
                // 偏小一圈，选中水洗的轮廓形状会不一样。
                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
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
                            offset_y: 0.0,
                            active: false,
                            dock: None,
                        });
                    }),
                )
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
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.cancel_rename(window, cx);
                            }
                        }))
                        .child(Input::new(&input).xsmall())
                        .into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono_family.clone())
                        .text_size(px(label_px))
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
                .context_menu(move |menu, _window, _| {
                    Self::tab_popup_menu(
                        menu.external_link_icon(false),
                        context_workspace.clone(),
                        ix,
                        is_terminal,
                        ai_fork,
                        tab_color,
                    )
                });
            if dragged {
                // 骑指针 + 提到最上层画（deferred 只延后绘制、不动布局），
                // 阴影给"拿起来"的抬升感。
                gpui::deferred(row.top(px(drag.map(|(_, _, off)| off).unwrap_or(0.0))).shadow_md())
                    .into_any_element()
            } else if shift != 0.0 {
                // 让位滑动：进位方向 ease-out 滑入（旧壳是双向弹簧；回位
                // 这里先直落，违和再补逐帧插值）。
                row.with_animation(
                    ("tab-make-way", ix),
                    Animation::new(Duration::from_millis(120)).with_easing(ease_out_quint()),
                    move |row, t| row.top(px(shift * t)),
                )
                .into_any_element()
            } else {
                row.into_any_element()
            }
        });

        let header_group: SharedString = "sidebar-tabs-header-hover".into();
        let header_workspace = cx.entity().downgrade();
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
                        this.tabs_section_collapsed = !this.tabs_section_collapsed;
                        this.tabs_fold_armed = true;
                        cx.notify();
                    }))
                    .child(
                        // 箭头、TABS 和计数仍作为一个排版段；折叠命中已经
                        // 提升到外层整行。右侧 +/⋯ 自己停止冒泡。
                        h_flex()
                            .h_full()
                            .items_center()
                            .pr_1()
                            .child(
                                // 箭头与标题同一条 mono 文本 run（旧壳 codicon
                                // 字位 \u{eab4}/\u{eab6} + BOLD），不用细线 SVG——
                                // 实心字形才和旧壳一样有存在感。
                                div()
                                    .w(px(tabs_disclosure_slot_w))
                                    .flex_shrink_0()
                                    .font_family(mono_family.clone())
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(muted)
                                    .child(if self.tabs_section_collapsed {
                                        "\u{eab6}"
                                    } else {
                                        "\u{eab4}"
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::BOLD)
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
                                    .text_color(faint)
                                    .child(count),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("sidebar-new-tab")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .invisible()
                            .group_hover(header_group, |button| button.visible())
                            .tooltip("新建终端 (Ctrl+Shift+T)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.add_terminal(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("sidebar-tabs-menu-stop")
                            // DropdownMenu 自己管理 click；外壳截住冒泡，避免
                            // 打开菜单时顺手折叠 TABS。
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                Button::new("sidebar-tabs-menu")
                                    .icon(IconName::EllipsisVertical)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("标签与面板")
                                    .dropdown_menu(move |menu, _, _| {
                                        Self::sidebar_popup_menu(menu, header_workspace.clone())
                                    }),
                            ),
                    ),
            )
            .child(self.render_tabs_section(items));
        // spinner 帧循环（旧壳 motion frame 的对应物）：有任何行在
        // 「运行中」时推进相位；notify → 下一次 render 再续帧。
        if items_running.get() {
            cx.on_next_frame(window, |this, _, cx| {
                let now = std::time::Instant::now();
                let dt = now - this.spinner_last;
                this.spinner_last = now;
                this.spinner_phase =
                    (this.spinner_phase + dt.as_secs_f32() / 0.8).rem_euclid(1.0);
                cx.notify();
            });
        }
        sidebar
    }

    /// Tab 列表的折叠槽位：`max_h` 在 0..内容高之间走与侧栏折叠同一条 240ms
    /// ease-out 曲线，`overflow_hidden` 负责动画期间的裁剪，观感是卷帘而不是
    /// 瞬间闪现。首次手动切换后才启用——启动帧必须静止落位。
    ///
    /// 内容高算得出来（`TAB_ROW_H` + `gap_2` 都是常量），不用等布局回写；插
    /// `max_h` 而不是 `h` 是为了保住列表原本的 `flex_1`：tab 多到超出可用空间
    /// 时上限不生效，压缩行为与折叠前一致。
    fn render_tabs_section<I>(&self, items: I) -> gpui::AnyElement
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        let collapsed = self.tabs_section_collapsed;
        let list = v_flex().flex_1().gap_2().children(items);
        if !self.tabs_fold_armed {
            return if collapsed { div().into_any_element() } else { list.into_any_element() };
        }
        let rows = self.tabs.len().max(1) as f32;
        let content_h = rows * TAB_ROW_H + (rows - 1.0) * (TAB_ROW_PITCH - TAB_ROW_H);
        let (from, to) = if collapsed { (content_h, 0.0) } else { (0.0, content_h) };
        div()
            .flex_1()
            .overflow_hidden()
            .child(list)
            .with_animation(
                ("tabs-fold", collapsed as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| slot.max_h(px(from + (to - from) * t)),
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
                // 描边恒占 1px（聚焦换色而非出现/消失），pane 网格几何不随
                // 焦点切换抖动。
                let border = if is_focused {
                    cx.theme().primary.opacity(0.45)
                } else {
                    gpui::transparent_black()
                };
                div()
                    .size_full()
                    .relative()
                    .min_w_0()
                    .min_h_0()
                    .border_1()
                    .border_color(border)
                    .rounded(crate::gpui_shell::theme::card_radius())
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
                let bar_color = if dragging { cx.theme().primary } else { cx.theme().border };
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
            None => None,
        };
        let files_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Files;
        let git_active = self.side_panel.open
            && self.side_panel.view == crate::display::side_panel::PanelView::Git;
        let settings_active = matches!(self.tabs.get(self.active), Some(WorkspaceTab::Settings { .. }));
        // dock 预览：被拖 tab 悬于终端区时高亮目标半区（松手即挂到那侧）。
        let dock_preview = self
            .tab_drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| drag.dock)
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
                TitleBar::new()
                    .child(
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
                                    .tooltip("Git 状态")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_git_tree(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                // 不用 h_flex：它默认 items_center，会把子项高度压成内容高度。
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar_slot(window, cx))
                    .child(
                        // 终端卡（一体化外壳）：唯一的结构分界。圆角与旧壳卡
                        // 同源（UI_SHELL_RADIUS_LOGICAL=14），无描边——融合靠
                        // 壳色包围圆角卡本身，不靠线框。p_2 = 旧壳 8px 卡缝。
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
                            .p_2()
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
mod tests {
    use super::*;

    #[test]
    fn ai_hook_routing_never_falls_back_for_a_stale_exact_pane() {
        let pane_ids = [7u64, 11];
        assert_eq!(ai_hook_target_pane(&pane_ids, Some(11), Some(7)), Some(11));
        assert_eq!(ai_hook_target_pane(&pane_ids, Some(99), Some(7)), None);
        assert_eq!(ai_hook_target_pane(&pane_ids, None, Some(7)), Some(7));
        assert_eq!(ai_hook_target_pane(&pane_ids, None, None), None);
    }

    #[test]
    fn new_tab_position_uses_the_shared_runtime_semantics() {
        use nebula_settings::NewTabPositionName::{AfterCurrent, End};

        assert_eq!(new_tab_insert_index(AfterCurrent, 1, 4), 2);
        assert_eq!(new_tab_insert_index(End, 1, 4), 4);
        assert_eq!(new_tab_insert_index(AfterCurrent, 9, 2), 2);
        assert_eq!(new_tab_insert_index(AfterCurrent, 0, 0), 0);
    }

    /// 分屏生命周期合同的树侧不变式：split 后叶集扩张、关到最后一个叶
    /// 判 WasRoot（宿主关整 tab）、中途摘叶塌缩并移交焦点。
    #[test]
    fn split_tree_lifecycle_matches_pane_contract() {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, 2, SplitDirection::LeftRight, 0.5));
        assert!(tree.split_leaf(2, 3, SplitDirection::TopBottom, 0.5));
        assert_eq!(tree.leaves(), vec![1, 2, 3]);

        // 摘中间叶：父节点塌缩，焦点移交幸存子树首叶。
        assert_eq!(tree.remove_leaf(2), RemoveOutcome::Collapsed(3));
        assert_eq!(tree.leaves(), vec![1, 3]);
        // 摘到只剩一个：WasRoot=宿主应关整个 tab，树不再可用。
        assert_eq!(tree.remove_leaf(1), RemoveOutcome::Collapsed(3));
        assert_eq!(tree.remove_leaf(3), RemoveOutcome::WasRoot);
        // 不存在的叶子不产生副作用。
        let mut single = SplitTree::leaf(9u64);
        assert_eq!(single.remove_leaf(4), RemoveOutcome::NotFound);
    }

    #[test]
    fn dock_tree_places_the_source_tree_on_the_nav_side() {
        let docked = dock_tree(SplitTree::leaf(1u64), SplitTree::leaf(9u64), SplitNav::Left);
        assert_eq!(docked.leaves(), vec![9, 1], "Left：source 树进 first 槽");
        match &docked {
            SplitTree::Split { direction: SplitDirection::LeftRight, ratio, .. } => {
                assert!((ratio - 0.5).abs() < f32::EPSILON, "dock 恒为 50/50 根分割");
            },
            _ => panic!("dock 应产生根级左右分割"),
        }

        // 多叶源树整树挂入，叶序保持源树内部顺序。
        let mut source = SplitTree::leaf(7u64);
        assert!(source.split_leaf(7, 8, SplitDirection::TopBottom, 0.5));
        let docked = dock_tree(SplitTree::leaf(1u64), source, SplitNav::Down);
        assert_eq!(docked.leaves(), vec![1, 7, 8], "Down：source 树进 second 槽");
        match &docked {
            SplitTree::Split { direction: SplitDirection::TopBottom, .. } => {},
            _ => panic!("Down 应产生上下分割"),
        }
    }

    #[test]
    fn cwd_palette_actions_are_available_only_for_local_terminal_tabs() {
        use crate::display::command_palette::PaletteAction;

        assert!(!NebulaWorkspace::palette_action_available(&PaletteAction::CopyCwd, false,));
        assert!(!NebulaWorkspace::palette_action_available(&PaletteAction::RevealCwd, false,));
        assert!(NebulaWorkspace::palette_action_available(&PaletteAction::CopyCwd, true,));
        assert!(NebulaWorkspace::palette_action_available(&PaletteAction::RevealCwd, true,));
        assert!(NebulaWorkspace::palette_action_available(&PaletteAction::NewTab, false));
    }

    #[test]
    fn ai_session_palette_exposes_verified_resume_and_fork_commands() {
        let sessions = vec![
            crate::ai_sessions::AiSession::test_session(
                crate::ai_agents::AgentKind::Claude,
                "claude-42",
                "Fix resize",
            ),
            crate::ai_sessions::AiSession::test_session(
                crate::ai_agents::AgentKind::Aider,
                "aider-42",
                "Unsupported",
            ),
        ];
        let rows = ai_session_palette_rows(sessions);
        assert_eq!(rows.len(), 2, "Claude supports resume and fork; Aider supports neither");
        assert!(matches!(
            &rows[0].action,
            WorkspacePaletteAction::RunAiSession { command, .. }
                if command == "claude --resume claude-42"
        ));
        assert!(matches!(
            &rows[1].action,
            WorkspacePaletteAction::RunAiSession { command, .. }
                if command == "claude --resume claude-42 --fork-session"
        ));
    }
}
