//! 终端视图：持有会话、处理输入与 IME、驱动重绘。

mod broadcast;
mod runtime;

pub use broadcast::TerminalInput;

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    Font, FontFeatures, FontStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, ScrollWheelEvent, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, TextRun, UTF16Selection, Window, div, point, px,
};
use gpui_component::WindowExt as _;
use nebula_settings::CellWidthModeName;
use nebula_terminal::event::{Event as TermEvent, Notify as _, OnResize as _, WindowSize};
use nebula_terminal::event_loop::Msg;
use nebula_terminal::grid::{Dimensions as _, Scroll};
use nebula_terminal::index::{Column, Line, Point as TermPoint, Side};
use nebula_terminal::render::{CellMetrics, TerminalViewport, ViewportTracker};
use nebula_terminal::selection::{Selection, SelectionType};
use nebula_terminal::term::TermMode;
use nebula_terminal::vte::ansi::CursorStyle;

use std::sync::Arc;

use super::colors::Palette;
use super::element::{TerminalElement, completion_popup_layout};
use super::keymap;
use super::mouse_protocol;
use super::session::{self, TerminalSession};
use super::suggest;
use super::{KEY_CONTEXT, TerminalBackTab, TerminalTab};
use crate::gpui_shell::config::Settings;
use crate::gpui_shell::prelude::{
    ActiveTheme as _, Colorize as _, DialogButtonProps, center_confirm_dialog,
};
use crate::{config::UiConfig, font_install::REQUIRED_FONT_FAMILY};
use futures::StreamExt as _;

/// 等宽字体描述。GPUI 的 Windows 后端收到空 feature 列表会在
/// `apply_font_features` 里提前返回，Maple 的 contextual ligature 因而不会
/// 生效；显式给出 calt=1 会让该后端一并注册 liga/clig/calt。
fn mono_font(family: &str, weight: FontWeight, style: FontStyle) -> Font {
    Font {
        weight,
        style,
        features: FontFeatures(Arc::new(vec![("calt".to_owned(), 1)])),
        ..crate::font_install::gpui_font_with_fallbacks(family)
    }
}

/// 粘贴确认阈值：一次粘进来达到这个行数，先弹模态问一句。
///
/// 旧壳的判据是「非 bracketed 且含任何换行」（`event.rs` 的 `paste`）。那条
/// 规则对裸 shell 是对的——换行落地即执行——但 codex/vim/PSReadLine 这类自己
/// 接管换行的应用也跟着挨弹窗，#35 因此给 bracketed 模式开了豁免；豁免之后
/// 往 PSReadLine 里糊三十行反而一声不响。这里换成体量判据：值得再看一眼的
/// 是「一次糊进来几十行」，与对端是否 bracketing 无关；两三行仍旧直接放行。
const PASTE_CONFIRM_LINES: usize = 20;

/// Overlay 滚动条的拇指宽度、最小高度与命中放宽量（逻辑 px；旧壳
/// `scrollbar_geometry` 的 4/24/8 设备 px 在同一 DPI 语义下等值）。4px 的细条
/// 不好抓，所以命中带比可见拇指左右各宽 `SLOP`。
const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
const SCROLLBAR_SLOP: f32 = 8.0;

/// 拖选越过网格上/下边界后的自动回滚：tick 间隔与「每远离 20px 加一行」的
/// 速度分档取自旧壳 `SELECTION_SCROLLING_INTERVAL` / `SELECTION_SCROLLING_STEP`
/// （旧壳按设备像素存常量再乘 DPI；GPUI 的 `Pixels` 本就是逻辑像素，这里直接
/// 是逻辑口径）。
const SELECTION_SCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(15);
const SELECTION_SCROLL_STEP: f32 = 20.0;

/// 一次自动回滚该滚几行：指针在网格上方为正（往回滚历史）、下方为负，网格
/// 内为 0。贴边即 1 行，之后每远离 `SELECTION_SCROLL_STEP` 像素加一档。
///
/// `max_lines` 是每 tick 的上限。旧壳不需要它——winit 不捕获指针，`mouse_y`
/// 永远落在窗口内；GPUI 在 Windows 上按下即 `SetCapture`，指针甩到屏幕外时
/// y 可以是很大的负数，不封顶就会「一抖到底」。
fn selection_scroll_lines(y: f32, top: f32, bottom: f32, max_lines: i32) -> i32 {
    let step = SELECTION_SCROLL_STEP.max(1.0);
    let max = max_lines.max(1);
    if y < top {
        (1 + ((top - y) / step) as i32).min(max)
    } else if y >= bottom {
        -((1 + ((y - bottom) / step) as i32).min(max))
    } else {
        0
    }
}

/// 粘贴体量按「落到终端算几行」数，不用 `str::lines`。
///
/// `str::lines` 只认 `\n`，纯 `\r` 的剪贴板内容（老式 Mac 换行、部分 Windows
/// 程序导出的选区）会被算成一行，可它进了 PTY 一样是回车。这里 `\r\n` 算一次
/// 换行，末尾那个换行不额外多算一行。
fn paste_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let breaks = text.replace("\r\n", "\n").matches(['\n', '\r']).count();
    if text.ends_with('\n') || text.ends_with('\r') { breaks } else { breaks + 1 }
}

/// 当前 UI 语言。`RuntimeSettings` 每次读盘，所以只在用户动作（复制提示、
/// 粘贴确认）时取，不进渲染热路径。
fn ui_language() -> crate::display::UiLanguage {
    match nebula_settings::RuntimeSettings::load().language {
        nebula_settings::LanguagePref::System => crate::display::LanguagePreference::System,
        nebula_settings::LanguagePref::ZhCn => crate::display::LanguagePreference::ZhCn,
        nebula_settings::LanguagePref::EnUs => crate::display::LanguagePreference::EnUs,
    }
    .resolved()
}

/// 终端视图对宿主（Panel/Workspace）暴露的状态变化。
pub enum TerminalViewEvent {
    /// OSC 标题变化，宿主应刷新 Tab 标题。
    TitleChanged,
    /// 会话结束（子进程退出或 PTY 故障），只发一次。
    Exited,
    /// 用户在本视图内按下鼠标：分屏宿主据此更新聚焦 pane（键盘焦点已由
    /// 视图自己 `window.focus` 拿走，这里只同步宿主的 pane 级焦点记账）。
    FocusRequested,
    /// 连接卡片的取消/关闭（旧壳 TabRequest::Close 的对应物）。
    RequestClose,
    /// 失败后在当前分屏叶原位重建同一 SSH 目标。
    RetrySsh(String),
    /// Ctrl+滚轮改了终端字号：宿主应对所有 pane 热应用并写盘。
    FontSizeChanged,
    /// BEL（`^G`）。后台 tab 记铃点；本 tab 的闪烁/声音由视图自己处理。
    Bell,
    /// 用户语义输入。宿主可按 tab 的广播状态扇出；接收 pane 必须重新编码，
    /// 不能复用发送方已经受终端 mode 影响的字节。
    UserInput(TerminalInput),
}

/// 会话种类：本地 shell 或 SSH 直连（russh，共享旧壳业务层）。
///
/// `shell` 是 Tab 创建/恢复时已经裁定好的启动命令。不能只传 cwd 再在
/// `TerminalView::new` 里读取“此刻的默认 Shell”，否则恢复混合的 PowerShell /
/// WSL 工作区时，每个 Tab 都会被重新解释成同一个当前默认值。
pub enum TerminalLaunch {
    Local {
        cwd: Option<std::path::PathBuf>,
        shell: Option<nebula_terminal::tty::Shell>,
        /// 只用于欢迎屏选择 PowerShell/Bash 形态，不参与 PTY 启动。
        shell_name: Option<String>,
    },
    Ssh {
        destination: String,
    },
}

pub(crate) fn last_path_component(path: &str) -> Option<String> {
    let name = path
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())?;
    Some(name.to_owned())
}

/// 侧边栏只消费稳定的枚举态，不直接窥探终端/SSH 的内部错误字段。
/// `Done`/`Attention` 是旧壳 `AgentStatus::Done`/`Blocked` 的呈现位：
/// 回合结束≠会话结束，转圈必须停，但要留下「有结果没看」的痕迹。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarActivity {
    Idle,
    Running,
    /// Agent 回合完成，等待下一条指令（旧壳蓝点语义）。
    Done,
    /// Agent 停在授权/提问上，需要用户表态（旧壳手掌语义）。
    Attention,
    Failed,
}

pub struct TerminalView {
    pub pane_id: u64,
    pub session: Option<TerminalSession>,
    pub focus_handle: FocusHandle,
    /// 公式覆盖层（探测/持久化状态复用旧壳 `terminal_math`，每 pane 一份）。
    pub math: super::math_overlay::MathOverlay,
    pub font: Font,
    pub font_bold: Font,
    pub font_italic: Font,
    pub font_bold_italic: Font,
    pub font_size: Pixels,
    cell_width_mode: nebula_settings::CellWidthModeName,
    /// Cell offsets use physical pixels, matching the legacy crossfont
    /// contract. They are applied after GPUI has shaped the actual face.
    font_offset_x: f32,
    font_offset_y: f32,
    pub palette: Arc<Palette>,
    pub marked_text: Option<String>,
    pub ime_bounds: Bounds<Pixels>,
    pub title: String,
    /// shell 集成 `NEBULA|cwd|branch|program` 标题上报的工作目录；tab 标签
    /// 的第一优先来源（与旧壳 `nebula_state.cwd` 同源同义）。
    pub cwd: String,
    /// 同上标题协议的 git 分支字段（当前仅存备用）。
    pub branch: String,
    /// 标题协议第 4 字段：远端 `nebula ssh` 上报的运行中程序，本地为空。
    pub running_program: Option<String>,
    /// OSC 133;C 起、133;D 止：命令正在执行。自带 PowerShell prompt 靠
    /// `NEBULA|` 标题报 `running_program`，装了 OSC133 集成的 bash/zsh/nu 只
    /// 发这一对语义标记——两条路都要能点亮侧栏的「运行中」。
    command_running: bool,
    /// 进程树给出的反证：`command_running` 为真、但 shell 树下只剩交互式
    /// shell 与 console plumbing 时置位，转圈据此熄灭（issue #42）。判据与
    /// 节流都在 `runtime::reconcile_shell_activity`，裁定理由见那里。
    command_running_disproved: bool,
    /// `command_running` 置位的时刻，进程树探测的节流窗口从这里起算。
    command_started: Option<std::time::Instant>,
    /// 上次跑进程树探测的时刻，用于节流。
    last_process_probe: Option<std::time::Instant>,
    active_run: Option<crate::runtime_api::RuntimePaneRun>,
    last_run: Option<crate::runtime_api::RuntimeRunOutcome>,
    /// Hook 事件驱动的 agent 回合状态（旧壳 `nebula_state.agent_status` 的
    /// 边沿触发版）。`Unknown` = 本 pane 没有 agent 参与，spinner 完全由
    /// `command_running`/`running_program` 决定。
    agent_status: crate::ai_agents::AgentStatus,
    /// 状态证据来源与命中的屏幕规则。Runtime API 直接投影这两项，外部
    /// Agent 可以区分 hook 权威边沿、屏幕补偿和仅进程识别。
    agent_status_source: crate::ai_agents::AgentStatusSource,
    agent_status_rule: Option<String>,
    /// 本次前台 agent 会话是否收到过 hook（旧壳同名字段同语义）：屏幕
    /// 检测的空闲提示符不得降级 hook 报出的 Done/Blocked 精确终态。
    agent_hook_seen: bool,
    /// 本 pane 的 agent 在当前回合有过活动（hook 派活、屏幕判 working、或
    /// Runtime 提交）。屏幕回到空闲提示符时据此区分「干完了、你还没看」
    /// （Done，蓝点）与「从没开工」（Idle，只显示 shell 标签）——消费点在
    /// `runtime::refresh_agent_screen_state`。
    agent_turn_active: bool,
    /// 屏幕检测连续看到空闲提示符的拍数；Working 连续两拍空闲才降级
    /// （单拍可能是重绘间隙），非 idle 检测与任何 hook 边沿都清零。
    idle_screen_streak: u8,
    /// Runtime 派活后，旧输入框仍可能连续命中 `prompt_idle`。在看到本回合
    /// 的 working/blocked 屏幕或权威 hook 前，不允许它伪造完成边沿。
    agent_runtime_submit_pending: bool,
    pending_runtime_submit: Option<crate::display::state::RuntimeSubmitBarrier>,
    /// OSC 9 程序通知，攒到 render（那里才有 `Window` 判聚焦）再呈现。
    pending_notify: Vec<String>,
    /// SSH 直连目的地（`user@host[:port]`）；本地会话为 None。
    pub ssh_destination: Option<String>,
    /// SSH 连接阶段（业务层上报）：Ready 前画连接横幅，Failed 驻留错误。
    ssh_stage: Option<crate::ssh_session::SshStage>,
    /// SSH 连接卡片状态（旧壳 `display::ssh_connect::SshConnectState` 的
    /// 直接复用：阶段日志、粒子相位、进度插值、350ms 显示门槛都在里面）。
    pub(super) ssh_connect: Option<crate::display::ssh_connect::SshConnectState>,
    /// 上一次动画帧时刻（卡片 step 的 delta 来源）。
    ssh_connect_last_step: std::time::Instant,
    /// Hook-reported identity for exact resume/fork. Process/title inference is
    /// never accepted here because a wrong id would continue the wrong chat.
    pub ai_session: Option<crate::display::AiSessionIdentity>,
    error: Option<String>,
    exited: Option<String>,
    /// 滚动条拖拽中：按下时记下的「指针在拇指内的 y 偏移」，拖动全程据此
    /// 反算 `display_offset`，拇指不会在按下那一刻跳到指针中心。
    scrollbar_drag: Option<f32>,
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    cols: usize,
    rows: usize,
    /// Last geometry committed to both the terminal grid and ConPTY. During
    /// an interactive resize `cols`/`rows` are the live visual viewport while
    /// this remains at the last coalesced boundary.
    window_size: WindowSize,
    /// 启动稳定闸：开窗 resize 是异步落地的，首帧可能还是旧尺寸。闸门
    /// 关着时不向 Term/ConPTY 下发布局，直到布局命中 spawn 网格（零下发
    /// 收口）或宽限期超时（小屏收拢等真实差异，放行一次性纠正）。没有
    /// 这道闸，ConPTY 会在 shell 首屏输出中途被 目标→旧→目标 来回重排
    /// （shrink 重排具破坏性，参见 WT Terminal::UserResize 注释），
    /// PSReadLine 的坐标缓存随之错位——表现为打字回显落到陈旧行。
    grid_synced: bool,
    spawn_at: std::time::Instant,
    viewports: ViewportTracker,
    /// Final visual viewport waiting for the trailing-edge resize commit.
    pending_resize: Option<TerminalViewport>,
    /// Invalidates older settle timers when a newer layout observation wins.
    resize_epoch: u64,
    scroll_px: f32,
    selecting: bool,
    /// 拖选越界后的自动回滚 tick 世代：停止或重开一轮就 +1，让在途的旧
    /// 定时器链自行失效（同 `cursor_blink_epoch` 的模式）。
    selection_scroll_epoch: u64,
    /// 是否已有一条自动回滚定时器链在跑——每次 move 都开一条会叠出 N 倍速。
    selection_scroll_active: bool,
    /// OSC 8 / 正则 URL：虚线下划线、悬停预览、Ctrl+点击打开。
    pub(super) hint_config: Arc<UiConfig>,
    pub(super) link_hover: Option<super::osc_links::LinkHover>,
    pending_link_open: bool,
    /// 选中即复制（旧壳 `copy_on_select`）；关闭时复制交给右键路径。
    copy_on_select: bool,
    /// 鼠标模式下最后上报的单元格：move 事件按"进入新单元格"去重。
    last_report_point: Option<TermPoint>,
    /// GPUI 没有旧壳 scheduler 的 `BlinkCursor` 事件，视图自己只维护可见相位；
    /// 光标是否允许闪烁仍由共享 `Term::cursor_style()` 裁定。
    cursor_visible: bool,
    cursor_blink_epoch: u64,
    /// 最近一次由设置页下发的默认样式。只在它真正变化时清理 shell 的
    /// DECSCUSR/DEC mode 12 覆盖，避免无关设置变更打断 vim 等程序光标。
    default_cursor_style: CursorStyle,
    /// 补全运行时状态（行镜像/ghost 余量/弹窗候选），引擎与旧壳共享：
    /// 计算在 `display::suggest_engine`，数据源在 `terminal::suggest` 的
    /// 进程级单例。cwd 由 NEBULA| 标题协议同步进来。
    pub(super) suggest: crate::display::NebulaPaneState,
    /// `suggest` 取样时的可视光标位置。旧壳在一次 Term 锁内同时读取提示行
    /// 与光标；GPUI 的 render/paint 分两次取锁，因此用此锚点拒绝跨世代组合
    /// （典型是退格回显夹在两次取锁之间造成 ghost 左右跳）。
    pub(super) suggest_anchor: Option<(usize, usize)>,
    ghost_enabled: bool,
    accept: crate::display::AcceptKey,
    completion_style: crate::display::CompletionStyle,
    /// BEL 后暂停侧栏转圈，直到用户再往 PTY 打字（旧壳 `awaiting_input`）。
    awaiting_input: bool,
    /// BEL 视觉闪烁：整 pane 盖一层前景 12%，约 150ms。
    pub(super) bell_flash: bool,
    bell_flash_epoch: u64,
    /// 失焦时再递系统 toast / 驻留横幅（render 里才有 Window）。
    pending_bell_notify: Option<(String, String)>,
}

impl TerminalView {
    /// 默认画布（旧壳 `display` 的 `Dimensions { columns: 116, lines: 30 }`
    /// 同源）：窗口按基准字号定形，spawn 网格再按当前缩放字号反推。
    pub const DEFAULT_GRID_COLUMNS: u16 = 116;
    pub const DEFAULT_GRID_LINES: u16 = 30;

    fn effective_cell_width(
        raw_width: f32,
        mode: nebula_settings::CellWidthModeName,
        scale: f32,
        offset_x: f32,
    ) -> Pixels {
        // 旧壳的 crossfont 度量与取整都发生在设备像素域。若先在 GPUI
        // 逻辑像素域取整，150% DPI 下每列会多出半个物理像素，116 列会
        // 把启动窗口横向撑大几十像素。
        let scale = scale.max(0.5);
        let device_width = raw_width * scale + offset_x;
        let device_width = match mode {
            nebula_settings::CellWidthModeName::Compact => device_width.floor(),
            nebula_settings::CellWidthModeName::Relaxed => device_width.round(),
        };
        px(device_width.max(1.0) / scale)
    }

    pub(super) fn cell_width_for_advance(&self, raw_width: f32, scale: f32) -> Pixels {
        Self::effective_cell_width(raw_width, self.cell_width_mode, scale, self.font_offset_x)
    }

    fn effective_line_height(natural_height: f32, offset_y: f32, scale: f32) -> Pixels {
        let scale = scale.max(0.5);
        // GPUI's shaped line exposes the platform's ascent+descent, which on
        // DirectWrite includes the same font line gap used by crossfont. The
        // old shell then floors natural height + offset in device pixels.
        px(((natural_height * scale + offset_y).floor().max(1.0)) / scale)
    }

    pub(super) fn line_height_for_metrics(&self, natural_height: f32, scale: f32) -> Pixels {
        Self::effective_line_height(natural_height, self.font_offset_y, scale)
    }

    /// 启动稳定闸的宽限期：等待开窗 resize 落地到布局的最长时间。超时
    /// 即认定差异是真实的（如小屏收拢），按当前视口放行纠正。
    const STARTUP_GRID_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

    /// 尾沿去抖窗口：视口静默这么久才向 Term/ConPTY 提交一次 resize。
    /// 见 `set_layout` 内的合同注释（conhost rewrap 漂移取证）。
    const RESIZE_SETTLE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
    const CURSOR_BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

    fn measure_cell_metrics(
        window: &Window,
        family: &str,
        font_size: Pixels,
        mode: nebula_settings::CellWidthModeName,
        offset_x: f32,
        offset_y: f32,
    ) -> (Pixels, Pixels) {
        let font = mono_font(&family, FontWeight::NORMAL, FontStyle::Normal);
        let sample = window.text_system().shape_line(
            SharedString::new_static("M"),
            font_size,
            &[TextRun {
                len: 1,
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        (
            Self::effective_cell_width(
                sample.width.as_f32(),
                mode,
                window.scale_factor(),
                offset_x,
            ),
            Self::effective_line_height(
                sample.ascent.as_f32() + sample.descent.as_f32(),
                offset_y,
                window.scale_factor(),
            ),
        )
    }

    /// 首帧前的当前单元格度量（与 element prepaint 同一公式：shape "M" 的
    /// advance / ascent / descent + 配置 offset）。让 spawn 网格与首帧布局一致，避免启动
    /// 即触发一次 ConPTY resize（DA 探询与回显竞态的温床）。
    pub fn cell_metrics(window: &Window, cx: &App) -> (Pixels, Pixels) {
        let (family, font_size, mode, offset_x, offset_y) = match cx.try_global::<Settings>() {
            Some(settings) => (
                settings.font_family.as_str(),
                settings.font_size_px,
                settings.cell_width_mode,
                settings.font_offset_x,
                settings.font_offset_y,
            ),
            None => (REQUIRED_FONT_FAMILY, 15.0, CellWidthModeName::Compact, 0.0, 0.0),
        };
        Self::measure_cell_metrics(window, family, px(font_size), mode, offset_x, offset_y)
    }

    /// 旧壳窗口定形使用配置基准字号，不使用持久化缩放；缩放后的字号只
    /// 影响最终能容纳的行列数。否则放大一级就会把 116 列全部加到窗宽上。
    pub fn startup_cell_metrics(window: &Window, cx: &App) -> (Pixels, Pixels) {
        let (family, font_size, mode, offset_x, offset_y) = match cx.try_global::<Settings>() {
            Some(settings) => (
                settings.font_family.as_str(),
                settings.base_font_size_px,
                settings.cell_width_mode,
                settings.font_offset_x,
                settings.font_offset_y,
            ),
            None => (REQUIRED_FONT_FAMILY, 15.0, CellWidthModeName::Compact, 0.0, 0.0),
        };
        Self::measure_cell_metrics(window, family, px(font_size), mode, offset_x, offset_y)
    }

    /// `spawn_grid`：PTY 出生网格（宿主已把窗口定形到该几何）。首帧布局
    /// 与之相同则零下发；见 `set_layout` 的启动稳定闸。
    pub fn new(
        pane_id: u64,
        spawn_grid: (u16, u16),
        launch: TerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 字体、调色板与终端启动配置来自用户配置（nebula.toml +
        // nebula_settings.txt，bootstrap 时装载为全局 Settings）。
        let (
            families,
            font_size,
            cell_width_mode,
            font_offset_x,
            font_offset_y,
            palette,
            term_config,
            copy_on_select,
            shell,
        ) = match cx.try_global::<Settings>() {
            Some(settings) => (
                [
                    settings.font_family.clone(),
                    settings.font_bold_family.clone(),
                    settings.font_italic_family.clone(),
                    settings.font_bold_italic_family.clone(),
                ],
                px(settings.font_size_px),
                settings.cell_width_mode,
                settings.font_offset_x,
                settings.font_offset_y,
                Arc::new(settings.palette.clone()),
                settings.term_config(),
                settings.copy_on_select,
                // 设置选定的默认 shell（旧壳 default_shell_launch 同径：
                // resolve 失败或 PTY 集成 id 落回引擎默认）。WSL id 在
                // 这里换上 bash 集成注入，cwd/git 分支经 NEBULA| 标题回流。
                settings
                    .shell_id
                    .as_deref()
                    .and_then(crate::shell_detect::resolve_id)
                    .map(|detected| detected.shell()),
            ),
            None => (
                std::array::from_fn(|_| REQUIRED_FONT_FAMILY.to_owned()),
                px(15.0),
                CellWidthModeName::Compact,
                0.0,
                0.0,
                Arc::new(Palette::default()),
                nebula_terminal::term::Config::default(),
                // 旧壳的出厂默认即开。
                true,
                None,
            ),
        };
        let default_cursor_style = term_config.default_cursor_style;
        let (cell_w, line_h) = Self::cell_metrics(window, cx);
        // 像素口径与 viewport 上报一致（设备 px），避免首帧一次像素级差异。
        let scale = window.scale_factor();
        let initial = WindowSize {
            num_lines: spawn_grid.1.max(2),
            num_cols: spawn_grid.0.max(2),
            cell_width: (cell_w.as_f32() * scale).round().max(1.0) as u16,
            cell_height: (line_h.as_f32() * scale).round().max(1.0) as u16,
        };
        let initial_cwd = match &launch {
            TerminalLaunch::Local { cwd, .. } => {
                cwd.as_ref().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default()
            },
            TerminalLaunch::Ssh { destination } => destination.clone(),
        };
        let (ssh_destination, initial_title, intro_shell_name, suggest_env, spawned) = match launch
        {
            TerminalLaunch::Local { cwd, shell: launch_shell, shell_name } => {
                // 显式 launch（会话恢复/创建时冻结）优先；只有旧会话没有
                // 身份时才回退当前设置。这正是共享 v4 的 Default 语义。
                let effective = launch_shell.or(shell);
                // 补齐要知道这个 pane 面对**哪台机器**：`wsl.exe -d <发行版>`
                // 启动的 tab，文件系统和命令集都在来宾里，本进程的 `std::fs`
                // 和 PATH 描述的是另一台机器。
                let suggest_env = effective
                    .as_ref()
                    .and_then(|shell| {
                        crate::shell_detect::wsl_launch_distro(shell.program(), shell.args())
                    })
                    .map_or(crate::display::SuggestEnv::Local, |distro| {
                        crate::display::SuggestEnv::Wsl { distro: distro.to_owned() }
                    });
                (
                    None,
                    String::from("shell"),
                    shell_name,
                    suggest_env,
                    session::spawn(initial, term_config, effective, pane_id, cwd),
                )
            },
            TerminalLaunch::Ssh { destination } => (
                Some(destination.clone()),
                destination.clone(),
                None,
                crate::display::SuggestEnv::Ssh { destination: destination.clone() },
                session::spawn_ssh(destination, initial, term_config),
            ),
        };
        let is_ssh = ssh_destination.is_some();
        let (session, error) = match spawned {
            Ok((session, mut rx, mut stage_rx)) => {
                // 新会话欢迎屏（设置 fetch=1，旧壳 fastfetch 同一入口）：
                // 命令先进 conhost 输入队列，shell 出提示符即执行。宽度按
                // 出生网格裁定双列/堆叠版式；bash/WSL id 走 fastfetch 回退
                // 链，其余按 PowerShell 智能双列脚本。SSH 会话不注入本地
                // 欢迎屏（远端 shell 有自己的首屏）。
                let runtime = nebula_settings::RuntimeSettings::load();
                if runtime.fetch && !is_ssh {
                    use nebula_terminal::event::Notify as _;
                    let id = intro_shell_name
                        .as_deref()
                        .or(runtime.shell.as_deref())
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let intro_shell = if id.contains("wsl") || id.contains("bash") {
                        crate::display::NebulaShell::Bash
                    } else {
                        crate::display::NebulaShell::PowerShell
                    };
                    let notifier =
                        nebula_terminal::event_loop::Notifier(session.notifier.0.clone());
                    notifier.notify(
                        crate::window_context::welcome::nebula_fastfetch_intro_command_for(
                            usize::from(spawn_grid.0),
                            intro_shell,
                        ),
                    );
                }
                cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        // 合并同一批到达的事件，避免每个 Wakeup 都独立触发一帧。
                        let mut batch = vec![event];
                        while batch.len() < 128 {
                            match rx.try_recv() {
                                Ok(event) => batch.push(event),
                                _ => break,
                            }
                        }
                        let done = batch.iter().any(|e| matches!(e, TermEvent::Exit));
                        if this
                            .update(cx, |view: &mut Self, cx| {
                                for event in batch {
                                    view.process_event(event, cx);
                                }
                            })
                            .is_err()
                            || done
                        {
                            break;
                        }
                    }
                })
                .detach();
                if is_ssh {
                    // SSH 连接阶段泵：横幅数据源（与旧壳连接卡片同一
                    // 上报流，350ms 门槛之类的视觉策略交给渲染端）。
                    cx.spawn(async move |this, cx| {
                        while let Some(stage) = stage_rx.next().await {
                            if this
                                .update(cx, |view: &mut Self, cx| {
                                    view.ssh_stage = Some(stage.clone());
                                    // 连接卡片状态机（旧壳 ssh_connect_stage
                                    // 同款合同）：Ready 移除；Resolve 开新卡
                                    // 片；其余阶段推进。中途断线不会让用
                                    // 了半天的终端凭空复活一张卡片。
                                    match stage {
                                        crate::ssh_session::SshStage::Ready => {
                                            view.ssh_connect = None;
                                        },
                                        other => {
                                            match &mut view.ssh_connect {
                                                Some(state) => state.set_stage(other),
                                                None => {
                                                    if matches!(
                                                        other,
                                                        crate::ssh_session::SshStage::Resolve
                                                    ) {
                                                        if let Some(dest) =
                                                            view.ssh_destination.clone()
                                                        {
                                                            view.ssh_connect = Some(
                                                                crate::display::ssh_connect::SshConnectState::new(dest),
                                                            );
                                                            view.ssh_connect_last_step =
                                                                std::time::Instant::now();
                                                        }
                                                    }
                                                },
                                            }
                                        },
                                    }
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                (Some(session), None)
            },
            Err(err) => {
                let what = if is_ssh { "SSH 会话启动失败" } else { "PTY 启动失败" };
                (None, Some(format!("{what}: {err}")))
            },
        };

        let (ghost_enabled, accept, completion_style) = match cx.try_global::<Settings>() {
            Some(settings) => (settings.ghost, settings.accept, settings.completion_style),
            None => (true, Default::default(), Default::default()),
        };

        let mut view = Self {
            pane_id,
            session,
            focus_handle: cx.focus_handle(),
            math: super::math_overlay::MathOverlay::default(),
            font: mono_font(&families[0], FontWeight::NORMAL, FontStyle::Normal),
            font_bold: mono_font(&families[1], FontWeight::BOLD, FontStyle::Normal),
            font_italic: mono_font(&families[2], FontWeight::NORMAL, FontStyle::Italic),
            font_bold_italic: mono_font(&families[3], FontWeight::BOLD, FontStyle::Italic),
            font_size,
            cell_width_mode,
            font_offset_x,
            font_offset_y,
            palette,
            marked_text: None,
            ime_bounds: Bounds::default(),
            title: initial_title,
            cwd: initial_cwd,
            branch: String::new(),
            running_program: None,
            command_running: false,
            command_running_disproved: false,
            command_started: None,
            last_process_probe: None,
            active_run: None,
            last_run: None,
            agent_status: crate::ai_agents::AgentStatus::Unknown,
            agent_status_source: crate::ai_agents::AgentStatusSource::Unknown,
            agent_status_rule: None,
            agent_hook_seen: false,
            agent_turn_active: false,
            idle_screen_streak: 0,
            agent_runtime_submit_pending: false,
            pending_runtime_submit: None,
            pending_notify: Vec::new(),
            ssh_destination,
            ssh_stage: None,
            ssh_connect: None,
            ssh_connect_last_step: std::time::Instant::now(),
            ai_session: None,
            error,
            exited: None,
            scrollbar_drag: None,
            origin: point(px(0.0), px(0.0)),
            cell_width: cell_w,
            line_height: line_h,
            cols: initial.num_cols as usize,
            rows: initial.num_lines as usize,
            window_size: initial,
            grid_synced: false,
            spawn_at: std::time::Instant::now(),
            viewports: ViewportTracker::default(),
            pending_resize: None,
            resize_epoch: 0,
            scroll_px: 0.0,
            selecting: false,
            selection_scroll_epoch: 0,
            selection_scroll_active: false,
            hint_config: super::osc_links::hint_config(),
            link_hover: None,
            pending_link_open: false,
            copy_on_select,
            last_report_point: None,
            cursor_visible: true,
            cursor_blink_epoch: 0,
            default_cursor_style,
            suggest: {
                // `NebulaPaneState` 有几个 display 模块私有的字段，函数式更新
                // 语法（`..Default::default()`）在本模块用不了；先取默认值，再
                // 写这里唯一要定制的公开字段。
                let mut state = crate::display::NebulaPaneState::default();
                state.suggest_env = suggest_env;
                state
            },
            suggest_anchor: None,
            ghost_enabled,
            accept,
            completion_style,
            awaiting_input: false,
            bell_flash: false,
            bell_flash_epoch: 0,
            pending_bell_notify: None,
        };
        view.restart_cursor_blink(cx);
        view
    }

    fn process_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {
                self.flush_pending_runtime_submit(cx);
                cx.notify();
            },
            TermEvent::MouseCursorDirty => {
                cx.notify();
            },
            TermEvent::CursorBlinkingChange => self.restart_cursor_blink(cx),
            TermEvent::Title(title) => {
                // `NEBULA|cwd|branch|program` 标题协议（与旧壳 event.rs 的
                // 解析同构）：shell 集成把 cwd/分支塞在标题里喂玻璃 powerline
                // 与 tab 标签，而不是真拿来当窗口标题。
                if let Some(rest) = title.strip_prefix("NEBULA|") {
                    let mut parts = rest.splitn(3, '|');
                    self.cwd = parts.next().unwrap_or("").trim().to_owned();
                    self.branch = parts.next().unwrap_or("").trim().to_owned();
                    self.running_program =
                        parts.next().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty());
                    // 补全引擎的 cwd 与目录 frecency 同步吃 shell 的权威上报
                    // （旧壳 nebula_record_directory 同径）。
                    if self.suggest.cwd != self.cwd {
                        self.suggest.cwd = self.cwd.clone();
                        super::suggest::record_directory(&self.cwd);
                    }
                    // 协议串不是窗口标题，更不是 tab 名。
                } else {
                    self.title = title;
                }
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            },
            TermEvent::ResetTitle => {
                self.title = String::from("shell");
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            },
            TermEvent::PtyWrite(text) => self.write_bytes(text.into_bytes()),
            TermEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            },
            TermEvent::ClipboardLoad(_, formatter) => {
                let text =
                    cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                self.write_bytes(formatter(&text).into_bytes());
            },
            TermEvent::ColorRequest(index, formatter) => {
                let reply = self.session.as_ref().map(|session| {
                    let term = session.term.lock();
                    self.palette.query_reply(index, term.colors())
                });
                if let Some(rgb) = reply {
                    self.write_bytes(formatter(rgb).into_bytes());
                }
            },
            TermEvent::TextAreaSizeRequest(formatter) => {
                self.write_bytes(formatter(self.window_size).into_bytes());
            },
            TermEvent::ChildExit(code) => {
                self.mark_exited(format!("进程已退出（{code:?}）"), cx);
            },
            TermEvent::PtyFailure(reason) => {
                self.mark_exited(format!("PTY 故障：{reason}"), cx);
            },
            TermEvent::Exit => {
                self.mark_exited(String::from("会话已结束"), cx);
            },
            TermEvent::CwdReport(cwd) => {
                // 标准 OSC 7 / 9;9 的目录上报。只动 cwd，`NEBULA|` 标题带来的
                // branch/program 保持不变——两条通道并存（旧壳 event.rs:3142
                // 同合同）。少了这一条，只有自带 PowerShell prompt 的 tab 能被
                // 目录树/git 跟随，bash/zsh/nu 一律跟不动。
                let cwd = cwd.trim().to_owned();
                if !cwd.is_empty() && self.cwd != cwd {
                    self.cwd = cwd;
                    self.suggest.cwd = self.cwd.clone();
                    super::suggest::record_directory(&self.cwd);
                    // GPUI 没有旧壳每次 chrome 刷新都 `sync_chrome_tabs()` 的
                    // 循环；侧栏标题画在 workspace 里，cwd 变了必须发
                    // TitleChanged，`on_terminal_event` 才会 `cx.notify()` 重绘。
                    cx.emit(TerminalViewEvent::TitleChanged);
                    cx.notify();
                }
            },
            TermEvent::CommandStart => {
                // 程序身份（旧壳 event.rs CommandStart 分支原样）：从 Enter
                // 时捕获的命令行提取首 token，AgentKind 认得的归一成 slug。
                // 这是侧栏 AI 图标对「直接敲 codex/grok 而没装 hook」的
                // 会话也点亮的那条路——hook 信封只是更准的覆盖层。
                let identity =
                    crate::display::extract_program(&self.suggest.last_committed).map(|program| {
                        crate::ai_agents::AgentKind::parse(&program)
                            .map(|agent| agent.slug().to_owned())
                            .unwrap_or(program)
                    });
                if identity != self.running_program {
                    self.running_program = identity;
                    cx.emit(TerminalViewEvent::TitleChanged);
                }
                if self
                    .running_program
                    .as_deref()
                    .and_then(crate::ai_agents::AgentKind::parse)
                    .is_some()
                    && !self.agent_hook_seen
                {
                    self.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
                    self.agent_status_rule = None;
                }
                self.mark_command_running();
                if let Some(run) = &mut self.active_run
                    && run.phase == crate::runtime_api::RuntimeRunPhase::Submitted
                {
                    run.phase = crate::runtime_api::RuntimeRunPhase::Started;
                }
                cx.notify();
            },
            // 退出码先只用来结束「运行中」；失败命令的未读标记要等
            // `SidebarActivity` 扩出未读态再接，现在记下来也没有呈现位置。
            TermEvent::CommandDone { exit_code } => {
                // 新 PTY 初始化提示符也可能先发一个 CommandDone。Runtime
                // 文本还在等待回显 barrier 时，这个边沿属于上一轮/初始化，
                // 不能清掉尚未发送的 Enter 或把新请求提前投影成 idle。
                if self.pending_runtime_submit.is_some() {
                    return;
                }
                // 旧壳同款收尾：CLI 退回提示符后，它不再是这个 pane 的前台
                // 事实——hook 稍后若仍在跑会重新点亮（handle_ai_hook 覆写）。
                if self.running_program.take().is_some() || self.ai_session.take().is_some() {
                    cx.emit(TerminalViewEvent::TitleChanged);
                }
                self.command_running = false;
                self.command_running_disproved = false;
                self.command_started = None;
                self.last_process_probe = None;
                if let Some(run) = self.active_run.take() {
                    self.last_run =
                        Some(crate::runtime_api::RuntimeRunOutcome::command_done(run, exit_code));
                }
                self.agent_status = crate::ai_agents::AgentStatus::Unknown;
                self.agent_status_source = crate::ai_agents::AgentStatusSource::Unknown;
                self.agent_status_rule = None;
                self.agent_hook_seen = false;
                self.agent_turn_active = false;
                self.idle_screen_streak = 0;
                self.agent_runtime_submit_pending = false;
                self.pending_runtime_submit = None;
                cx.notify();
            },
            TermEvent::Notify(body) => {
                // 程序自己发的 OSC 9 通知：旧壳只在窗口没聚焦时才递出去
                // （event.rs:3291），聚焦时用户本来就在看这个 pane。这里攒着，
                // render 拿到 `Window` 才判聚焦。
                let body = body.trim().to_owned();
                if !body.is_empty() {
                    self.pending_notify.push(body);
                    cx.notify();
                }
            },
            TermEvent::AiHookEnvelope(envelope) => {
                // SSH pane 里的 agent 靠私有 OSC 把 hook 信封带回本地（本地
                // agent 走 workspace 的 ai_events 通道）。信封在 event_loop 里
                // 已核过通道令牌，这里解析出来喂进同一个应用路径。
                //
                // hook 自报客户端名（claude/codex），是**程序身份的权威来源**
                // ——比 OSC 133 的命令行嗅探准，后者漏掉包装启动和没有 shell
                // 集成的会话。少了这一条，远端 agent 的侧栏图标和会话身份
                // （fork/恢复要用）全都缺席。
                if let Some(hook) = crate::ai_hook::parse_remote_envelope(&envelope, None) {
                    self.handle_ai_hook(&hook, cx);
                }
            },
            TermEvent::Bell => self.on_bell(cx),
            // 仍未接线：内联图片（要图片管线把 abs_line 锚到网格）、OSC 1337
            // UserVar（AI 查询拦截）。
            _ => {},
        }
    }

    /// `Exited` 只对宿主发一次；重复的退出信号（ChildExit 之后必然跟 Exit）只更新文案。
    fn mark_exited(&mut self, message: String, cx: &mut Context<Self>) {
        self.pending_runtime_submit = None;
        if self.exited.is_none() {
            self.exited = Some(message);
            cx.emit(TerminalViewEvent::Exited);
        }
        cx.notify();
    }

    /// 让 EventLoop 退出并回收 ConPTY/子进程。幂等：重复调用只会得到发送失败。
    pub fn shutdown(&self) {
        if let Some(session) = &self.session {
            let _ = session.notifier.0.send(Msg::Shutdown);
        }
    }

    /// 旧壳 `WindowContext::busy_process_in` 的单 Pane 形态：进程树只负责
    /// 判断 shell 下面是否仍有子进程；展示名称优先采用终端协议识别出的
    /// 程序名，避免把 Claude Code 一律显示成承载它的 `node.exe`。
    pub fn busy_process(&self) -> Option<String> {
        let shell_pid = self.session.as_ref()?.shell_pid;
        if shell_pid == 0 {
            return None;
        }
        let executable = crate::process_tree::busy_child(shell_pid)?;
        Some(
            self.running_program
                .clone()
                .unwrap_or_else(|| crate::process_tree::display_name(&executable)),
        )
    }

    fn write_bytes(&self, bytes: Vec<u8>) {
        if let Some(session) = &self.session {
            session.notifier.notify(bytes);
        }
    }

    /// 输入后回到底部并请求重绘。
    fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.awaiting_input = false;
        if let Some(session) = &self.session {
            {
                let mut term = session.term.lock();
                term.scroll_display(Scroll::Bottom);
                term.selection = None;
            }
            session.notifier.notify(bytes);
        }
        self.restart_cursor_blink(cx);
        cx.notify();
    }

    fn cursor_should_blink(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            let term = session.term.lock();
            term.cursor_style().blinking && term.mode().contains(TermMode::SHOW_CURSOR)
        })
    }

    fn restart_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.cursor_blink_epoch = self.cursor_blink_epoch.wrapping_add(1);
        let epoch = self.cursor_blink_epoch;
        self.schedule_cursor_blink_tick(epoch, cx);
        cx.notify();
    }

    fn schedule_cursor_blink_tick(&self, epoch: u64, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Self::CURSOR_BLINK_INTERVAL).await;
            let _ = this.update(cx, |view, cx| {
                if view.cursor_blink_epoch != epoch {
                    return;
                }
                if !view.cursor_should_blink() {
                    if !view.cursor_visible {
                        view.cursor_visible = true;
                        cx.notify();
                    }
                    return;
                }
                view.cursor_visible = !view.cursor_visible;
                view.schedule_cursor_blink_tick(epoch, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    fn on_bell(&mut self, cx: &mut Context<Self>) {
        let mode = nebula_settings::RuntimeSettings::load().bell;
        if mode == nebula_settings::BellModeName::None {
            return;
        }
        let audible_ok = if mode.audible() { Self::ring_audible() } else { false };
        if mode.visual() || (mode.audible() && !audible_ok) {
            self.flash_bell(cx);
        }
        if self.running_program.is_some() {
            self.awaiting_input = true;
        }
        let program = self.running_program.clone();
        self.pending_bell_notify = Some(match program {
            Some(name) => (name, "任务完成，等待输入".to_owned()),
            None => ("Nebula".to_owned(), "终端响铃".to_owned()),
        });
        cx.emit(TerminalViewEvent::Bell);
        cx.notify();
    }

    fn ring_audible() -> bool {
        crate::platform::beep();
        cfg!(windows)
    }

    fn flash_bell(&mut self, cx: &mut Context<Self>) {
        self.bell_flash = true;
        self.bell_flash_epoch = self.bell_flash_epoch.wrapping_add(1);
        let epoch = self.bell_flash_epoch;
        cx.notify();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(std::time::Duration::from_millis(150)).await;
            let _ = this.update(cx, |view, cx| {
                if view.bell_flash_epoch == epoch {
                    view.bell_flash = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 元素 prepaint 回写布局：内容矩形与度量交给渲染合同裁定网格。
    /// 网格变化时同步 Term 与 ConPTY；行列不变但像素口径变化也上报 PTY
    /// （应用可能关心像素度量）；稳态帧 observe 返回 None，零额外开销。
    pub fn set_layout(
        &mut self,
        origin: Point<Pixels>,
        cell_width: Pixels,
        line_height: Pixels,
        content: Size<Pixels>,
        scale: f32,
        cx: &mut Context<Self>,
    ) {
        self.origin = origin;
        self.cell_width = cell_width;
        self.line_height = line_height;
        let metrics = CellMetrics {
            cell_width: cell_width.as_f32(),
            cell_height: line_height.as_f32(),
            scale,
        };
        let change =
            self.viewports.observe(content.width.as_f32(), content.height.as_f32(), &metrics);

        // 启动稳定闸：开窗 resize 异步落地，首帧可能还是开窗前的旧尺寸。
        // 命中 spawn 网格前不向 Term/ConPTY 下发（PTY 出生即目标几何，
        // 零下发收口）；宽限期后仍未命中（小屏收拢等真实差异）则放行，
        // 一次性按当前视口纠正。observe 会把过渡帧并进 current，因此
        // 释放判定不依赖本帧是否有增量。
        if !self.grid_synced {
            let Some(viewport) =
                change.map(|c| c.viewport).or_else(|| self.viewports.current().copied())
            else {
                return;
            };
            let landed = (viewport.cols, viewport.rows)
                == (self.window_size.num_cols, self.window_size.num_lines);
            if !landed && self.spawn_at.elapsed() < Self::STARTUP_GRID_GRACE {
                return;
            }
            self.grid_synced = true;
            self.cols = viewport.cols as usize;
            self.rows = viewport.rows as usize;
            if !landed {
                self.commit_viewport(viewport);
            } else {
                self.window_size = viewport.window_size();
            }
            return;
        }

        let Some(change) = change else {
            return;
        };
        let viewport = change.viewport;
        self.cols = viewport.cols as usize;
        self.rows = viewport.rows as usize;

        // ConPTY 的直通式 conhost 在 resize 时零输出，指望终端侧 reflow 与
        // 它内部 buffer rewrap 一致；两者的换行语义存在路径依赖差异，每多
        // 一次中间宽度的 ResizePseudoConsole 就多攒一分光标行漂移（字节取
        // 证：13 次提交后 PSReadLine 的 CUP 行比真实提示行高 7 行）。旧壳
        // (winit) 的模态拖拽天然只在松手后送达一次 resize，从不累积。这里
        // 复刻该合同：纯尾沿去抖——任何网格/像素变化都只进 pending，视口
        // 静默 RESIZE_SETTLE_DELAY 后一次性提交。净零手势（挤压后拖回原宽）
        // 最终提交同尺寸 no-op，rewrap 次数为零。
        self.pending_resize = Some(viewport);
        self.schedule_settled_resize(cx);
    }

    /// Commit one viewport in grid-before-PTY order. Output produced after
    /// `ResizePseudoConsole` therefore always parses against the same geometry
    /// history ConPTY used to generate its absolute cursor coordinates.
    fn commit_viewport(&mut self, viewport: TerminalViewport) {
        let next = viewport.window_size();
        let grid_changed = (self.window_size.num_cols, self.window_size.num_lines)
            != (next.num_cols, next.num_lines);
        let pixel_changed = (self.window_size.cell_width, self.window_size.cell_height)
            != (next.cell_width, next.cell_height);
        if !grid_changed && !pixel_changed {
            return;
        }

        if let Some(session) = &self.session {
            let mut notifier = nebula_terminal::event_loop::Notifier(session.notifier.0.clone());
            notifier.on_resize(next);
        }
        self.window_size = next;
    }

    /// 一次拖拽手势（窗口边框/分屏把手）是否仍在进行。conhost 的 buffer
    /// rewrap 与本地 reflow 的换行语义存在路径依赖差异，每一次中间几何的
    /// `ResizePseudoConsole` 都会累积光标行漂移（字节取证：一次拖拽 14 次
    /// 提交后 PSReadLine 的 CUP 行比真实提示行高 7 行，且 conhost 全程零
    /// 重绘字节，漂移无法事后察觉）。旧壳 (winit) 的模态拖拽天然只在松手
    /// 后送达一次 resize，从不出这个问题；GPUI 在模态循环内持续派发布局，
    /// 时间去抖（150ms settle）与布局批次同周期，挡不住中间提交。因此按
    /// 手势门控：左键仍按住就不提交，settle 定时器自我续期到松手为止。
    #[cfg(windows)]
    fn drag_gesture_active() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        // SAFETY: GetAsyncKeyState 只读全局按键状态，无副作用。
        (unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000) != 0
    }

    #[cfg(not(windows))]
    fn drag_gesture_active() -> bool {
        false
    }

    fn schedule_settled_resize(&mut self, cx: &mut Context<Self>) {
        self.resize_epoch = self.resize_epoch.wrapping_add(1);
        let epoch = self.resize_epoch;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Self::RESIZE_SETTLE_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                if view.resize_epoch != epoch {
                    return;
                }
                let gate = Self::drag_gesture_active();
                if std::env::var_os("NEBULA_RESIZE_TRACE").is_some() {
                    eprintln!("[nebula:resize-trace] settle-timer gate={gate}");
                }
                if gate {
                    // 手势未松开：净零手势（挤压后拖回原宽）最终提交同尺寸
                    // no-op，ConPTY 一次 rewrap 都不做。
                    view.schedule_settled_resize(cx);
                    return;
                }
                let Some(viewport) = view.pending_resize.take() else { return };
                view.commit_viewport(viewport);
                cx.notify();
            });
        })
        .detach();
    }

    /// 旧壳 `change_font_size`：一步 1 逻辑 px，钳 4–64。写盘后通知宿主
    /// 给所有 pane 热应用，chrome 仍锚定 toml 字号。
    fn zoom_font_size(&mut self, step: f32, cx: &mut Context<Self>) {
        let current =
            cx.try_global::<Settings>().map(|settings| settings.font_size_px).unwrap_or(15.0);
        let next = (current.round() + step).clamp(4.0, 64.0);
        if (next - current).abs() < f32::EPSILON {
            return;
        }
        if let Err(err) = nebula_settings::persist_keys(&[("font_size", format!("{next:.2}"))]) {
            eprintln!("[nebula:gpui] failed to persist font size: {err}");
            return;
        }
        let theme = crate::gpui_shell::theme::effective_theme_name(cx);
        cx.set_global(Settings::load(theme));
        self.apply_settings(cx);
        cx.emit(TerminalViewEvent::FontSizeChanged);
        cx.notify();
    }

    /// 热应用运行时设置（设置页改动后由宿主调用）。默认光标样式只更新
    /// `Term` 的 fallback；程序通过 DECSCUSR 设置的临时样式仍保持权威。
    pub fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = cx.try_global::<Settings>() else { return };
        let families = [
            settings.font_family.clone(),
            settings.font_bold_family.clone(),
            settings.font_italic_family.clone(),
            settings.font_bold_italic_family.clone(),
        ];
        let font_size = px(settings.font_size_px);
        let palette = Arc::new(settings.palette.clone());
        let copy_on_select = settings.copy_on_select;
        let default_cursor_style = settings.term_config().default_cursor_style;
        let cursor_style_changed = self.default_cursor_style != default_cursor_style;
        self.ghost_enabled = settings.ghost;
        self.accept = settings.accept;
        self.completion_style = settings.completion_style;
        // 样式/开关热切换即作废当前提示：缓存键留着会挡住新样式的首次重算。
        self.suggest.clear_completion_hints();

        self.font = mono_font(&families[0], FontWeight::NORMAL, FontStyle::Normal);
        self.font_bold = mono_font(&families[1], FontWeight::BOLD, FontStyle::Normal);
        self.font_italic = mono_font(&families[2], FontWeight::NORMAL, FontStyle::Italic);
        self.font_bold_italic = mono_font(&families[3], FontWeight::BOLD, FontStyle::Italic);
        self.font_size = font_size;
        self.cell_width_mode = settings.cell_width_mode;
        self.font_offset_x = settings.font_offset_x;
        self.font_offset_y = settings.font_offset_y;
        self.palette = palette;
        self.copy_on_select = copy_on_select;
        self.default_cursor_style = default_cursor_style;
        if let Some(session) = &self.session {
            let mut term = session.term.lock();
            term.set_default_cursor_style(default_cursor_style);
            if cursor_style_changed {
                // 与旧壳 apply_default_cursor_style 同合同：用户显式改光标后，
                // 立即解除 shell 启动阶段钉住的旧样式，让新默认当场可见。
                term.reset_cursor_style_override();
            }
        }
        self.restart_cursor_blink(cx);
        cx.notify();
    }

    /// 侧栏 tab 标签：旧壳 `chrome_tab_label` 只认**路径末级名**。
    /// OSC 标题（脚本名、`NEBULA|…` 整串）只属于窗口标题，绝不能当标签，
    /// 否则跑脚本时侧栏会变成 `foo.ps1`，cwd 上报失败时还会拼出
    /// `.tmp-stay-launch.tmp-stay-launch` 这种重复段。
    pub fn tab_label(&self) -> String {
        last_path_component(&self.cwd)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|path| last_path_component(&path.to_string_lossy()))
            })
            .unwrap_or_else(|| ".".to_owned())
    }

    pub fn grid_rows(&self) -> usize {
        self.rows
    }

    pub fn grid_cols(&self) -> usize {
        self.cols
    }

    /// Local cwd reported by shell integration, for host chrome services
    /// (file tree/directory picker). Remote POSIX paths naturally fail
    /// `is_dir` at the host boundary and are left to the SFTP adapter.
    pub fn local_cwd(&self) -> Option<std::path::PathBuf> {
        let path = std::path::PathBuf::from(self.cwd.trim());
        path.is_dir().then_some(path)
    }

    fn term_mode(&self) -> TermMode {
        self.session.as_ref().map(|s| *s.term.lock().mode()).unwrap_or_default()
    }

    /// 复制选区并返回是否实际写入剪贴板。`notify` 表示显式复制
    /// （Ctrl+Shift+C、右键或自定义 Copy）：成功后清除选区并弹确认 toast；
    /// copy_on_select 刻意静默且保留选区，避免鼠标抬手后立刻失去视觉反馈。
    pub fn copy_selection(
        &mut self,
        notify: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = &self.session else { return false };
        let text = session.term.lock().selection_to_string();
        let Some(text) = text.filter(|text| !text.is_empty()) else { return false };

        let lines = text.lines().count().max(1);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if notify {
            // 原始按键可能紧接着再次到来；先清除选区，下一次 Copy 才能按
            // “未处理”传播回终端，而不是重复复制并再次弹 toast。
            session.term.lock().selection = None;
            let message = match ui_language() {
                crate::display::UiLanguage::EnUs => {
                    format!("Copied {lines} lines to clipboard")
                },
                _ => format!("已复制 {lines} 行到剪贴板"),
            };
            crate::gpui_shell::toast::toast(window, cx, crate::display::ToastKind::Info, message);
            cx.notify();
        }
        true
    }

    /// 读剪贴板并粘贴。体量达到 [`PASTE_CONFIRM_LINES`] 时先弹确认模态，确认
    /// 后才走 [`Self::paste_now`]；小段粘贴一路直通，不打断手感。
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let lines = paste_line_count(&text);
        if lines >= PASTE_CONFIRM_LINES {
            self.confirm_paste(text, lines, window, cx);
            return;
        }
        self.paste_now(&text, cx);
    }

    /// 大批量粘贴的阻断式确认——提示三层里的「模态」：有待办动作、必须先决策。
    ///
    /// 粘贴内容随模态一起快照，确认时不再回读剪贴板：弹窗期间用户完全可能又
    /// 复制了别的东西，回读会把「已经看过并确认的那一份」换成没人看过的内容。
    fn confirm_paste(
        &mut self,
        text: String,
        lines: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = ui_language();
        // bracketed 的对端（codex/vim/PSReadLine）整块收下、自己决定怎么处理；
        // 裸 shell 是换行落地即执行。风险不同，话就得说得不一样。
        let runs_line_by_line = !self.term_mode().contains(TermMode::BRACKETED_PASTE);
        let title: SharedString = match language {
            crate::display::UiLanguage::EnUs => format!("Paste {lines} lines?"),
            _ => format!("粘贴 {lines} 行文本？"),
        }
        .into();
        let body: SharedString = if runs_line_by_line {
            language.pick(
                "shell 会把这些内容逐行执行。请确认来源可信。",
                "The shell will run these lines one by one. Make sure you trust the source.",
            )
        } else {
            language.pick(
                "内容会作为一整块交给当前程序，不会逐行执行；但行数不少，请确认来源可信。",
                "The app receives this as a single paste and will not run it line by line, but \
                 it is a lot of text — make sure you trust the source.",
            )
        }
        .into();
        let ok_text: SharedString = language.pick("粘贴", "Paste").into();
        let cancel_text: SharedString = language.pick("取消", "Cancel").into();

        // builder 每帧重跑、`on_ok` 也是 `Fn`：文本与视图句柄都得可克隆共享。
        // 视图用弱引用——模态活在窗口 `Root` 里，强引用会让「弹窗期间关掉这个
        // pane」的 view 释放不掉。
        let text = Arc::new(text);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let text = text.clone();
            let view = view.clone();
            center_confirm_dialog(dialog, window)
                .title(title.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok_text.clone())
                        .cancel_text(cancel_text.clone())
                        .show_cancel(true),
                )
                .child(body.clone())
                .on_ok(move |_, _window, cx| {
                    let _ = view.update(cx, |this, cx| this.paste_now(&text, cx));
                    true
                })
        });
    }

    fn paste_now(&mut self, text: &str, cx: &mut Context<Self>) {
        self.paste_now_impl(text, true, cx);
    }

    fn paste_now_impl(&mut self, text: &str, emit: bool, cx: &mut Context<Self>) {
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        // 行镜像吃粘贴的字面文本；多行/控制字符由引擎侧作废（与旧壳
        // `nebula_input_text` 的防注入契约一致）。
        if !self.term_mode().contains(TermMode::ALT_SCREEN) {
            crate::display::Display::nebula_input_text(&mut self.suggest, &normalized);
        }
        let bytes = if self.term_mode().contains(TermMode::BRACKETED_PASTE) {
            let mut b = b"\x1b[200~".to_vec();
            // 过滤粘贴内容里的收尾哨兵，防止注入截断。
            b.extend_from_slice(normalized.replace("\x1b[201~", "").as_bytes());
            b.extend_from_slice(b"\x1b[201~");
            b
        } else {
            normalized.into_bytes()
        };
        if emit {
            self.write_user_text(text.to_owned(), true, bytes, cx);
        } else {
            self.write_input(bytes, cx);
        }
    }

    /// Enter 提交：从 grid 读回显真值（screen truth）记入共享历史，然后清
    /// 行镜像。读法与旧壳 `nebula_commit_line` 的 Windows 契约一致：无提示
    /// 箭头（cmd/ssh/REPL）或中线编辑读不到就宁缺毋滥——键击重构的
    /// line_buf 在光标移动/Tab 补全后就是拼接垃圾，不能进历史。
    fn commit_line(&mut self) {
        #[cfg(windows)]
        if let Some(session) = &self.session {
            let term = session.term.lock();
            if !term.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI) {
                let cursor = term.grid().cursor.point;
                match crate::display::Display::nebula_input_from_raw_grid(&term, cursor) {
                    Some(line) => self.suggest.screen_line = line,
                    None => self.suggest.screen_line.clear(),
                }
            } else {
                self.suggest.screen_line.clear();
            }
        }
        suggest::commit_line(&mut self.suggest);
    }

    /// 用元素在网格快照同一次 `Term` 锁内取得的提示行重算 ghost/弹窗。
    /// 这与旧壳 `draw_pane` 的锁序一致，避免退格回显夹在 render/paint 两次
    /// 取锁之间时拼成“旧提示 + 新光标”的跳动帧。
    pub(super) fn refresh_suggestion_from_snapshot(
        &mut self,
        line: Option<String>,
        anchor: Option<(usize, usize)>,
    ) {
        #[cfg(windows)]
        {
            if self.exited.is_some() || !self.ghost_enabled {
                self.suggest_anchor = None;
                self.suggest.clear_completion_hints();
                return;
            }
            if self.session.is_none() {
                self.suggest_anchor = None;
                self.suggest.clear_completion_hints();
                return;
            }
            match line {
                Some(line) => {
                    self.suggest_anchor = anchor;
                    self.suggest.screen_line = line.clone();
                    suggest::update(
                        &mut self.suggest,
                        Some(line),
                        self.ghost_enabled,
                        self.completion_style,
                    );
                },
                None => {
                    self.suggest_anchor = None;
                    self.suggest.screen_line.clear();
                    self.suggest.clear_completion_hints();
                },
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (line, anchor);
        }
    }

    /// 补齐登记了一个还没缓存的来宾 / 远端目录时，去后台拉一次。
    ///
    /// 补齐本身跑在按键路径上，绝不能做 IO——一次 `wsl.exe -- find` 冷启动实测
    /// 可达 7.5 秒，一次 SFTP 是完整的网络往返。所以它只把目录登记在
    /// `pending_remote_dir`，真正的往返在这里发生：结果进 [`crate::remote_dirs`]
    /// 的进程级缓存，代际一变，下一次重算就有候选了。
    ///
    /// 用户的体感是"第一次 Tab 没反应，之后都有"——而不是"每次 Tab 卡住整个
    /// 窗口"。
    pub(super) fn drive_pending_remote_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.suggest.pending_remote_dir.take() else { return };
        let env = self.suggest.suggest_env.clone();
        // 连按 Tab 不该排出一串子进程 / 往返。
        if !crate::remote_dirs::begin_fetch(&env, &dir) {
            return;
        }
        match env.clone() {
            crate::display::SuggestEnv::Wsl { distro } => {
                cx.spawn(async move |this, cx| {
                    let target = dir.clone();
                    // 子进程往返是阻塞的，必须落在后台线程池上。
                    let entries = cx
                        .background_spawn(
                            async move { crate::remote_dirs::fetch_wsl(&distro, &target) },
                        )
                        .await;
                    crate::remote_dirs::finish_fetch(&env, &dir, entries);
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            },
            crate::display::SuggestEnv::Ssh { destination } => {
                // SSH 的 async 只能跑在项目自己的 tokio runtime 上（连接池和
                // 认证策略都在那儿），而这里要等的是 GPUI 的任务——用一条
                // oneshot 把两个 executor 接起来。
                let Ok(runtime) = crate::ssh_session::runtime() else { return };
                let (tx, rx) = tokio::sync::oneshot::channel();
                let target = dir.clone();
                runtime.spawn(async move {
                    let listed =
                        crate::ssh_sftp::list_dir_for_completion(&destination, &target).await;
                    let _ = tx.send(listed);
                });
                cx.spawn(async move |this, cx| {
                    let entries = rx.await.ok().flatten().map(|entries| {
                        entries
                            .into_iter()
                            .map(|(is_dir, name)| crate::remote_dirs::RemoteEntry { name, is_dir })
                            .collect()
                    });
                    crate::remote_dirs::finish_fetch(&env, &dir, entries);
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            },
            // 本机 pane 的补齐直接读 `std::fs`，走不到这条路。
            crate::display::SuggestEnv::Local => {},
        }
    }

    fn track_encoded_key(&mut self, ks: &gpui::Keystroke, mode: &TermMode) {
        if self.marked_text.is_some() || mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        let mods = &ks.modifiers;
        let plain_mods = !mods.control && !mods.alt && !mods.platform;
        match ks.key.as_str() {
            "enter" => self.commit_line(),
            "backspace" if mods.control && !mods.alt && !mods.platform => {
                crate::display::Display::nebula_input_delete_word(&mut self.suggest);
            },
            "backspace" if plain_mods => {
                crate::display::Display::nebula_input_backspace(&mut self.suggest);
            },
            key => {
                let is_modifier = matches!(
                    key,
                    "shift" | "control" | "alt" | "platform" | "function" | "capslock"
                );
                // key_char 非空 = 平台判定它产生文本（随后从 IME 管道到达）。
                let produces_text = ks.key_char.as_deref().is_some_and(|text| !text.is_empty());
                if !is_modifier && !produces_text {
                    crate::display::Display::nebula_clear_line(&mut self.suggest);
                }
            },
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.exited.is_some() {
            return;
        }
        // 旧壳 `keyboard.rs`：IME 组合中不编码、不拦截。GPUI Windows 在
        // `stop_propagation` 后会跳过 `TranslateMessage`，组合中若把按键
        // 吃掉，候选窗和退格都会坏。
        if self.marked_text.is_some() {
            return;
        }
        let ks = &event.keystroke;
        let mods = &ks.modifiers;

        // 终端惯例快捷键优先于编码器。
        if mods.control && mods.shift {
            match ks.key.as_str() {
                "c" => {
                    self.copy_selection(true, window, cx);
                    cx.stop_propagation();
                    return;
                },
                "v" => {
                    self.paste(window, cx);
                    cx.stop_propagation();
                    return;
                },
                // 应用级快捷键（新建/关闭 Tab、折叠侧栏、分屏、缩放、标签
                // 位置移动）：不编码、不拦截，让按键冒泡到 workspace 的 action
                // 绑定。少写一个键位，症状就是"快捷键没反应"外加一串 CSI
                // 序列被打进 PTY——本文件下面的回滚翻页分支只吃**不带 ctrl**
                // 的 shift+pageup，所以 ctrl+shift 这一组必须在这里放行。
                "t" | "w" | "b" | "p" | "f" | "d" | "s" | "g" | "o" | "enter" | "pageup"
                | "pagedown" => return,
                _ => {},
            }
        }
        // 分屏焦点导航（ctrl+alt+方向）同样冒泡给 workspace，不编码成
        // CSI 1;7 序列（旧壳 FocusPane* 绑定的对应物）。
        if mods.control && mods.alt && matches!(ks.key.as_str(), "left" | "right" | "up" | "down") {
            return;
        }
        // 工作区切 tab（ctrl+tab / ctrl+shift+tab）：不编码进 PTY。
        if mods.control && !mods.alt && ks.key.as_str() == "tab" {
            return;
        }

        let mode = self.term_mode();

        // ---- 补全接管（对齐旧壳 keyboard.rs 的弹窗/ghost 协议）----
        // IME 组合中不截流；备用屏（TUI）没有提示可接；修饰组合全部放行，
        // 列表关闭后按键立即恢复原义（上下键回到 shell 历史）。
        let plain = !mods.control
            && !mods.alt
            && !mods.platform
            && !mods.function
            && !mods.shift
            && self.marked_text.is_none()
            && !mode.contains(TermMode::ALT_SCREEN);
        if plain {
            if suggest::popup_active(&self.suggest) {
                match ks.key.as_str() {
                    "down" | "up" | "tab" => {
                        suggest::popup_move(
                            &mut self.suggest,
                            if ks.key.as_str() == "up" { -1 } else { 1 },
                        );
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    },
                    "escape" => {
                        if suggest::popup_dismiss(&mut self.suggest) {
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                    },
                    "enter" => {
                        if self.accept_completion_popup(cx) {
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                    },
                    "right" if suggest::accepts(self.accept, "right") => {
                        if self.accept_completion_popup(cx) {
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                    },
                    _ => {},
                }
            } else if !self.suggest.suggestion.is_empty()
                && matches!(ks.key.as_str(), "tab" | "right")
                && suggest::accepts(self.accept, ks.key.as_str())
            {
                // ghost：接受键把余量如同击键般写入，shell 自己回显；Tab 在
                // 无提示时穿透给 shell 自己的补全（encode 兜底）。
                let ghost = std::mem::take(&mut self.suggest.suggestion);
                for c in ghost.chars() {
                    crate::display::Display::nebula_input_char(&mut self.suggest, c);
                }
                self.write_user_text(ghost.clone(), false, ghost.into_bytes(), cx);
                cx.stop_propagation();
                return;
            }
        }

        // 回滚快捷键（对齐旧壳默认绑定，仅主屏）：Shift+PageUp/PageDown
        // 翻页、Shift+Home/End 到顶/到底；备用屏下不拦截，交给编码器
        // 把带修饰的键序发给应用（less 自己处理 Shift+PageUp）。
        if mods.shift && !mods.control && !mods.alt && !mode.contains(TermMode::ALT_SCREEN) {
            let scroll = match ks.key.as_str() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                if let Some(session) = &self.session {
                    session.term.lock().scroll_display(scroll);
                }
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }

        // ---- 提示行镜像（旧壳 prompt-line tracker 同构）----
        // 可打印字符走 IME 管道、在 replace_text_in_range 镜像；这里只看
        // 编码器处理的键。凡是让 shell 编辑器偏离"直排追加"假设的键（方向、
        // Tab、Esc、Home、Ctrl+C……）一律作废本行镜像与提示，宁缺毋滥。
        self.track_encoded_key(ks, &mode);

        if let Some(bytes) = keymap::encode(ks, &mode) {
            self.write_user_key(ks.clone(), bytes, cx);
            cx.stop_propagation();
        }
    }

    /// 截住组件 Root 的 Tab 焦点遍历后回灌既有终端按键路径；有补齐时只接受补齐，
    /// 否则仍由原编码器发送 Tab，避免产生第二套按键语义。
    fn dispatch_terminal_tab(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut modifiers = gpui::Modifiers::default();
        modifiers.shift = shift;
        self.on_key_down(
            &KeyDownEvent {
                keystroke: gpui::Keystroke { modifiers, key: "tab".to_owned(), key_char: None },
                is_held: false,
                prefer_character_input: false,
            },
            window,
            cx,
        );
    }

    fn on_terminal_tab(&mut self, _: &TerminalTab, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch_terminal_tab(false, window, cx);
    }

    fn on_terminal_back_tab(
        &mut self,
        _: &TerminalBackTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_terminal_tab(true, window, cx);
    }

    /// 终端 overlay 滚动条的拇指矩形——渲染与命中测试共用的唯一真值源（旧壳
    /// `scrollbar_geometry` 同合同）。`None` = 不该出现：贴着底部时自动隐藏，
    /// 没有回滚历史时也不画。
    pub(super) fn scrollbar_thumb(
        &self,
        display_offset: usize,
        history: usize,
    ) -> Option<Bounds<Pixels>> {
        let screen = self.rows;
        let total = history + screen;
        if display_offset == 0 || screen == 0 || total <= screen {
            return None;
        }
        let track_top = self.origin.y.as_f32();
        let track_h = self.line_height.as_f32() * screen as f32;
        if track_h <= 1.0 {
            return None;
        }
        let min_thumb = SCROLLBAR_MIN_THUMB.min(track_h);
        let thumb_h = (track_h * screen as f32 / total as f32).clamp(min_thumb, track_h);
        // 视口顶端之上还剩多少行历史：0 = 拉到最顶，history = 贴着底部。
        let above = (history - display_offset) as f32;
        let max_y = (track_h - thumb_h).max(0.0);
        let thumb_y = track_top + (track_h * above / total as f32).clamp(0.0, max_y);
        // 浮在网格右缘（overlay 风格：不占列宽、不画轨道）。
        let grid_right = self.origin.x.as_f32() + self.cell_width.as_f32() * self.cols as f32;
        Some(Bounds::new(
            point(px(grid_right - SCROLLBAR_W), px(thumb_y)),
            gpui::size(px(SCROLLBAR_W), px(thumb_h)),
        ))
    }

    pub(super) fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    /// 回滚历史行数：拇指高度与拖拽反算的分母来源。
    pub(super) fn history_size(&self) -> usize {
        self.session.as_ref().map(|s| s.term.lock().history_size()).unwrap_or(0)
    }

    /// 命中滚动条则返回「指针在拇指内的 y 偏移」；落在轨道上（拇指上下方）
    /// 返回半个拇指高＝拇指跳到指针处居中（旧壳 `scrollbar_grab` 同合同）。
    fn scrollbar_grab(
        &self,
        position: Point<Pixels>,
        display_offset: usize,
        history: usize,
    ) -> Option<f32> {
        let thumb = self.scrollbar_thumb(display_offset, history)?;
        let x = position.x.as_f32();
        let thumb_x = thumb.origin.x.as_f32();
        if x < thumb_x - SCROLLBAR_SLOP || x > thumb_x + SCROLLBAR_W + SCROLLBAR_SLOP {
            return None;
        }
        let track_top = self.origin.y.as_f32();
        let track_h = self.line_height.as_f32() * self.rows as f32;
        let y = position.y.as_f32();
        if y < track_top || y > track_top + track_h {
            return None;
        }
        let thumb_top = thumb.origin.y.as_f32();
        let thumb_h = thumb.size.height.as_f32();
        if y >= thumb_top && y <= thumb_top + thumb_h {
            Some(y - thumb_top)
        } else {
            Some(thumb_h / 2.0)
        }
    }

    /// 把拖动中的指针 y 反算回 `display_offset`——`scrollbar_thumb` 那套定位
    /// 数学的逆运算（旧壳 `scrollbar_target_offset` 同合同）。
    fn scrollbar_target_offset(&self, y: f32, grab: f32, history: usize) -> usize {
        if history == 0 {
            return 0;
        }
        let total = (history + self.rows) as f32;
        let track_top = self.origin.y.as_f32();
        let track_h = (self.line_height.as_f32() * self.rows as f32).max(1.0);
        let above =
            ((y - grab - track_top) / track_h * total).round().clamp(0.0, history as f32) as usize;
        history - above
    }

    /// 一次取锁读出滚动条几何要的两个量：当前回滚位置与历史行数。
    fn scroll_state(&self) -> (usize, usize) {
        self.session
            .as_ref()
            .map(|s| {
                let term = s.term.lock();
                (term.grid().display_offset(), term.history_size())
            })
            .unwrap_or((0, 0))
    }

    /// 把回滚位置落到 `target`（滚动条拖拽的落点提交）。
    fn scroll_to_offset(&self, target: usize, current: usize) {
        if target == current {
            return;
        }
        if let Some(session) = &self.session {
            session.term.lock().scroll_display(Scroll::Delta(target as i32 - current as i32));
        }
    }

    /// 像素坐标 → 网格坐标（含滚动偏移）与半格判定。
    fn grid_point(&self, position: Point<Pixels>) -> (TermPoint, Side) {
        let rel_x = (position.x - self.origin.x).as_f32() / self.cell_width.as_f32().max(1.0);
        let rel_y = (position.y - self.origin.y).as_f32() / self.line_height.as_f32().max(1.0);
        let col = (rel_x.floor().max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (rel_y.floor().max(0.0) as usize).min(self.rows.saturating_sub(1));
        let side = if rel_x.fract() > 0.5 { Side::Right } else { Side::Left };
        let point = self.session.as_ref().map_or_else(
            || TermPoint::new(Line(row as i32), Column(col)),
            |session| {
                session
                    .term
                    .lock()
                    .visual_viewport_to_point(self.rows, TermPoint::new(row, Column(col)))
            },
        );
        (point, side)
    }

    fn selection_is_empty(&self) -> bool {
        self.session.as_ref().is_none_or(|session| {
            session.term.lock().selection.as_ref().is_none_or(|selection| selection.is_empty())
        })
    }

    fn update_link_hover(
        &mut self,
        position: Point<Pixels>,
        mods: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let next = if self.selecting {
            None
        } else {
            self.session.as_ref().and_then(|session| {
                let (point, _) = self.grid_point(position);
                let term = session.term.lock();
                super::osc_links::highlighted_at(&term, &self.hint_config, point, mods).and_then(
                    |hint| super::osc_links::hover_from_hint(&term, hint, self.rows, self.cols),
                )
            })
        };
        let changed = match (&self.link_hover, &next) {
            (None, None) => false,
            (Some(prev), Some(new)) => {
                prev.preview != new.preview
                    || prev.anchor_row != new.anchor_row
                    || prev.anchor_col != new.anchor_col
                    || prev.hint != new.hint
            },
            _ => true,
        };
        if changed {
            self.link_hover = next;
            cx.notify();
        }
    }

    fn clear_link_hover(&mut self, cx: &mut Context<Self>) {
        if self.link_hover.take().is_some() {
            cx.notify();
        }
    }

    fn try_open_hovered_link(&self, cx: &Context<Self>) {
        let Some(hover) = self.link_hover.as_ref() else { return };
        let Some(session) = self.session.as_ref() else { return };
        let term = session.term.lock();
        super::osc_links::open_hint(&hover.hint, &term, cx);
    }

    /// 应用是否接管了鼠标（vim/htop 等）。Shift 按住时强制旁路——这是
    /// 终端的通用逃生门：应用吃鼠标时用户仍能选择/复制。
    fn mouse_mode_active(&self, mods: &gpui::Modifiers) -> bool {
        !mods.shift && self.term_mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 把一次鼠标事件按当前协议（SGR/normal/UTF-8）编码上报给应用。
    fn send_mouse_report(
        &mut self,
        position: Point<Pixels>,
        button: u8,
        pressed: bool,
        mods: &gpui::Modifiers,
    ) {
        let (point, _) = self.grid_point(position);
        self.last_report_point = Some(point);
        let mode = self.term_mode();
        let mods =
            mouse_protocol::ReportMods { shift: mods.shift, alt: mods.alt, control: mods.control };
        if let Some(bytes) = mouse_protocol::report(&mode, point, button, pressed, mods) {
            self.write_bytes(bytes);
        }
    }

    /// 像素命中与绘制共用同一份候选几何，截短列表或上翻时也不会点错行。
    fn completion_popup_hit(&self, position: Point<Pixels>) -> Option<usize> {
        if !suggest::popup_active(&self.suggest) {
            return None;
        }
        let (cursor_row, cursor_col) = self.suggest_anchor?;
        let popup = completion_popup_layout(
            &self.suggest.completion_items,
            self.suggest.completion_selected,
            cursor_row,
            cursor_col,
            self.rows,
            self.cols,
        )?;
        let left = self.origin.x + self.cell_width * popup.start_col as f32;
        let top = self.origin.y + self.line_height * popup.start_line as f32;
        let right = left + self.cell_width * popup.width as f32;
        let bottom = top + self.line_height * popup.rows as f32;
        if position.x < left || position.x >= right || position.y < top || position.y >= bottom {
            return None;
        }
        let row = ((position.y - top).as_f32() / self.line_height.as_f32().max(1.0)) as usize;
        let index = popup.offset + row;
        (index < self.suggest.completion_items.len()).then_some(index)
    }

    /// 右侧窄条不覆盖候选字符列；点击轨道按指针比例跳到完整候选集中的位置。
    fn completion_popup_scrollbar_target(&self, position: Point<Pixels>) -> Option<usize> {
        if !suggest::popup_active(&self.suggest) {
            return None;
        }
        let (cursor_row, cursor_col) = self.suggest_anchor?;
        let popup = completion_popup_layout(
            &self.suggest.completion_items,
            self.suggest.completion_selected,
            cursor_row,
            cursor_col,
            self.rows,
            self.cols,
        )?;
        let len = self.suggest.completion_items.len();
        if len <= popup.rows {
            return None;
        }
        let content_right =
            self.origin.x + self.cell_width * (popup.start_col + popup.width) as f32;
        let top = self.origin.y + self.line_height * popup.start_line as f32;
        let height = self.line_height * popup.rows as f32;
        if position.x < content_right
            || position.x >= content_right + px(5.0)
            || position.y < top
            || position.y >= top + height
        {
            return None;
        }
        let progress = ((position.y - top).as_f32() / height.as_f32().max(1.0)).clamp(0.0, 1.0);
        Some((progress * (len - 1) as f32).round() as usize)
    }

    /// 接受当前候选时只写入补全余量。即使余量为空也算已处理，Enter 不能
    /// 继续透传成一次命令执行。
    fn accept_completion_popup(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(insert) = suggest::popup_take(&mut self.suggest) else { return false };
        let before = if self.suggest.screen_line.is_empty() {
            self.suggest.line_buf.clone()
        } else {
            self.suggest.screen_line.clone()
        };
        for c in insert.chars() {
            crate::display::Display::nebula_input_char(&mut self.suggest, c);
        }
        self.suggest.completion_suppressed_line = Some(format!("{before}{insert}"));
        if !insert.is_empty() {
            self.write_user_text(insert.clone(), false, insert.into_bytes(), cx);
        }
        true
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if self.session.is_none() {
            return;
        }
        if event.button == MouseButton::Left
            && let Some(index) = self.completion_popup_scrollbar_target(event.position)
        {
            self.suggest.completion_selected = Some(index);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Left
            && let Some(index) = self.completion_popup_hit(event.position)
        {
            self.suggest.completion_selected = Some(index);
            if self.accept_completion_popup(cx) {
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }
        // 面板外第一次点击只负责关闭浮层，避免同一击又在终端里开选区或触发
        // TUI 鼠标协议；缓存当前行，行不变时浮层不会下一帧原地复活。
        if suggest::popup_dismiss(&mut self.suggest) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // 滚动条是壳的控件，命中优先于选区和鼠标上报——否则在开了鼠标追踪的
        // TUI 里（codex/vim）根本抓不住条。贴底时拇指为 None，正常操作零影响。
        let (display_offset, history) = self.scroll_state();
        if let Some(grab) = self.scrollbar_grab(event.position, display_offset, history) {
            self.scrollbar_drag = Some(grab);
            let target = self.scrollbar_target_offset(event.position.y.as_f32(), grab, history);
            self.scroll_to_offset(target, display_offset);
            cx.notify();
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_LEFT,
                true,
                &event.modifiers,
            );
            return;
        }
        if event.modifiers.control && event.click_count == 1 {
            let (point, _) = self.grid_point(event.position);
            let hit = self.session.as_ref().is_some_and(|session| {
                let term = session.term.lock();
                super::osc_links::highlighted_at(&term, &self.hint_config, point, &event.modifiers)
                    .is_some()
            });
            if hit {
                if let Some(session) = &self.session {
                    session.term.lock().selection = None;
                }
                self.selecting = false;
                self.pending_link_open = true;
                self.update_link_hover(event.position, &event.modifiers, cx);
                return;
            }
        }
        let (point, side) = self.grid_point(event.position);
        let ty = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        if let Some(session) = &self.session {
            let mut term = session.term.lock();
            // Shift+单击扩展既有选区到点击处（原生文本框行为，对齐旧壳）；
            // 其余情况都开新选区。
            let extend = event.modifiers.shift
                && event.click_count == 1
                && term.selection.as_ref().is_some_and(|s| !s.is_empty());
            match term.selection.as_mut() {
                Some(selection) if extend => selection.update(point, side),
                _ => term.selection = Some(Selection::new(ty, point, side)),
            }
        }
        self.selecting = true;
        // 自动回滚在这里就上弦，而不是等第一次「拖出网格」的移动事件：指针
        // 一旦离开元素 hitbox 就不再有 move 送进来，一甩到顶的拖法最后一个
        // 元素内事件往往还在网格中间，等 move 判定＝永远等不到（用户报的
        // 「甩到顶不动，往下再往上才滚」）。tick 自己看指针位置，网格内是
        // 一次比较就返回的空转。
        self.start_selection_scroll(window, cx);
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 候选行 hover 与键盘高亮共用选中索引；这样鼠标所见即 Enter 所收。
        // 移出面板时保留最后选择，继续按 Tab 不会突然跳回第一项。
        if event.pressed_button.is_none()
            && let Some(index) = self.completion_popup_hit(event.position)
        {
            if self.suggest.completion_selected != Some(index) {
                self.suggest.completion_selected = Some(index);
                cx.notify();
            }
            return;
        }
        // 拖条中：指针的整段轨迹都归滚动条，不进选区也不上报。
        if let Some(grab) = self.scrollbar_drag {
            if event.pressed_button != Some(MouseButton::Left) {
                self.scrollbar_drag = None;
                return;
            }
            let (display_offset, history) = self.scroll_state();
            let target = self.scrollbar_target_offset(event.position.y.as_f32(), grab, history);
            self.scroll_to_offset(target, display_offset);
            cx.notify();
            return;
        }
        if self.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (point, side) = self.grid_point(event.position);
            if let Some(session) = &self.session {
                if let Some(selection) = session.term.lock().selection.as_mut() {
                    selection.update(point, side);
                }
            }
            // 越界回滚不在这里判：链在按下时就起了（见 `start_selection_scroll`），
            // 指针是否出了网格由 tick 自己看——一甩到顶的拖法最后一个元素内
            // 事件往往还在网格中间，靠 move 判定就永远起不来。
            cx.notify();
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.clear_link_hover(cx);
            // 鼠标模式的移动上报：拖动 = 按钮码+32（需 DRAG 或 MOTION 任一），
            // 无按键纯移动 = 35（仅 MOTION）；同一单元格内的移动不重报。
            let mode = self.term_mode();
            if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                return;
            }
            let button = match event.pressed_button {
                Some(MouseButton::Left) => {
                    mouse_protocol::BUTTON_LEFT + mouse_protocol::DRAG_OFFSET
                },
                Some(MouseButton::Middle) => {
                    mouse_protocol::BUTTON_MIDDLE + mouse_protocol::DRAG_OFFSET
                },
                Some(MouseButton::Right) => {
                    mouse_protocol::BUTTON_RIGHT + mouse_protocol::DRAG_OFFSET
                },
                _ => {
                    if !mode.contains(TermMode::MOUSE_MOTION) {
                        return;
                    }
                    mouse_protocol::MOTION_ONLY
                },
            };
            let (point, _) = self.grid_point(event.position);
            if self.last_report_point == Some(point) {
                return;
            }
            self.send_mouse_report(event.position, button, true, &event.modifiers);
            return;
        }
        self.update_link_hover(event.position, &event.modifiers, cx);
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_selection_scroll();
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
        if !self.selecting && self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_LEFT,
                false,
                &event.modifiers,
            );
            return;
        }
        self.selecting = false;
        let open_link = (self.pending_link_open || event.modifiers.control)
            && self.selection_is_empty()
            && self.link_hover.is_some();
        if open_link {
            self.try_open_hovered_link(cx);
        } else if self.copy_on_select {
            self.copy_selection(false, window, cx);
        }
        self.pending_link_open = false;
        cx.notify();
    }

    /// 指针在终端之外松手（拖到 tab 栏、别的 pane，或按下后甩出窗口——
    /// Windows 平台按下即 `SetCapture`，事件仍会送达）。GPUI 的 `on_mouse_up`
    /// 只在 hitbox 命中时派发，没有这条兜底，`selecting` 会一直停在按下态：
    /// 自动回滚停不下来，下一次移动还会继续改选区。
    fn on_mouse_up_out(
        &mut self,
        _event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_selection_scroll();
        let dragging_scrollbar = self.scrollbar_drag.take().is_some();
        if !self.selecting {
            if dragging_scrollbar {
                cx.notify();
            }
            return;
        }
        self.selecting = false;
        self.pending_link_open = false;
        // 指针不在终端上，链接打开不该发生；只补选中即复制这一条收尾。
        if self.copy_on_select {
            self.copy_selection(false, window, cx);
        }
        cx.notify();
    }

    /// 网格的上下边界（窗口坐标），自动回滚的触发线。旧壳用的是
    /// `padding_y` 与文本区底边，这里同义：卡内 8px 呼吸边距之内就已经
    /// 算「出了网格」。
    fn grid_vertical_bounds(&self) -> (f32, f32) {
        let top = self.origin.y.as_f32();
        (top, top + self.line_height.as_f32() * self.rows as f32)
    }

    fn selection_scroll_lines_at(&self, y: f32) -> i32 {
        let (top, bottom) = self.grid_vertical_bounds();
        selection_scroll_lines(y, top, bottom, self.rows as i32)
    }

    /// 选区拖拽全程常驻的自动回滚链：按下即起，松手才停。
    ///
    /// 不用「移动越界才起链」是因为那条判据依赖 move 事件送得到——指针离开
    /// 元素 hitbox 后 GPUI 就不再派发，一甩到顶的拖法最后一个元素内事件通常
    /// 还在网格中间，链于是永远起不来。常驻链把「是否越界」挪进 tick，指针
    /// 在网格内时它只是一次比较加返回，不重绘也不取 Term 锁。
    fn start_selection_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.selection_scroll_active {
            return;
        }
        self.selection_scroll_active = true;
        self.selection_scroll_epoch = self.selection_scroll_epoch.wrapping_add(1);
        let epoch = self.selection_scroll_epoch;
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(SELECTION_SCROLL_INTERVAL).await;
                let keep = this.update_in(cx, |view, window, cx| {
                    view.selection_scroll_tick(epoch, window, cx)
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn stop_selection_scroll(&mut self) {
        if !self.selection_scroll_active {
            return;
        }
        self.selection_scroll_active = false;
        self.selection_scroll_epoch = self.selection_scroll_epoch.wrapping_add(1);
    }

    /// 一次自动回滚 tick：越界就滚 N 行，并把选区末端跟到指针**此刻**所在
    /// 的格；没越界就什么都不做，等下一次 tick。
    ///
    /// 位置每 tick 从 `window.mouse_position()` 现取而不是沿用最后一次 move：
    /// 指针停在元素外（tab 栏、下方的 pane）时根本不会再有 move 事件，沿用
    /// 旧值就等于速度被钉死在贴边那一档，旧壳「越拖越快」的手感会丢。
    ///
    /// 返回 `false` 表示这条定时器链该结束（松手、换会话，或被新链顶掉）。
    fn selection_scroll_tick(
        &mut self,
        epoch: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if epoch != self.selection_scroll_epoch || !self.selection_scroll_active {
            return false;
        }
        if !self.selecting || self.session.is_none() {
            self.stop_selection_scroll();
            return false;
        }
        // 按下但还没拖出一个格：旧壳同样要求选区非空才滚，否则在卡片边距上
        // 按一下不动就会开始翻历史。
        if self.selection_is_empty() {
            return true;
        }
        let position = window.mouse_position();
        let lines = self.selection_scroll_lines_at(position.y.as_f32());
        if lines == 0 {
            return true;
        }
        if let Some(session) = &self.session {
            session.term.lock().scroll_display(Scroll::Delta(lines));
        }
        // 滚动换了视口到绝对行的映射，选区末端必须按滚动后的网格重算——
        // 否则视口滑过去了，选区还钉在原来那几行。两段各自取一次锁：
        // `grid_point` 内部还要读 Term，握着锁进去会自锁。
        let (point, side) = self.grid_point(position);
        if let Some(session) = &self.session
            && let Some(selection) = session.term.lock().selection.as_mut()
        {
            selection.update(point, side);
        }
        cx.notify();
        true
    }

    /// 右键（旧壳 Windows 惯例）：有选区 → 复制并清除；无选区 → 粘贴。
    /// 应用接管鼠标时上报给应用。
    fn on_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if suggest::popup_dismiss(&mut self.suggest) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.session.is_none() {
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_RIGHT,
                true,
                &event.modifiers,
            );
            return;
        }
        let has_selection = self
            .session
            .as_ref()
            .is_some_and(|s| s.term.lock().selection.as_ref().is_some_and(|sel| !sel.is_empty()));
        if has_selection {
            self.copy_selection(true, window, cx);
            if let Some(session) = &self.session {
                session.term.lock().selection = None;
            }
            cx.notify();
        } else {
            self.paste(window, cx);
        }
    }

    fn on_right_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_RIGHT,
                false,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    /// 中键只在鼠标模式下有意义（上报给应用）；旧壳在 Windows 上没有
    /// 中键粘贴路径，这里保持一致。
    fn on_middle_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if suggest::popup_dismiss(&mut self.suggest) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_MIDDLE,
                true,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    fn on_middle_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_MIDDLE,
                false,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_y = event.delta.pixel_delta(self.line_height).y.as_f32();
        // 旧壳 `mouse_wheel_input`：Ctrl+滚轮先于一切滚动消费者，一步 1
        // 逻辑像素，钳在 4–64。设置页步进会写盘；这里同样写 `font_size=`，
        // 让下次启动跟上（旧壳只在其它设置落盘时顺便带走当前字号）。
        if event.modifiers.control && !event.modifiers.alt && delta_y != 0.0 {
            let step = if delta_y > 0.0 { 1.0 } else { -1.0 };
            self.zoom_font_size(step, cx);
            cx.stop_propagation();
            return;
        }
        if suggest::popup_active(&self.suggest)
            && (self.completion_popup_hit(event.position).is_some()
                || self.completion_popup_scrollbar_target(event.position).is_some())
        {
            if delta_y != 0.0 {
                // 系统会把一格滚轮编码成“滚动 N 行”（Windows 常见为 3），
                // 终端回滚区需要尊重这个倍率，候选选择器则不应该：一次滚轮
                // 手势只跨一个 item，否则 8 行视口会瞬间跳过半屏。
                suggest::popup_move(&mut self.suggest, if delta_y > 0.0 { -1 } else { 1 });
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        self.scroll_px += delta_y;
        let lines = (self.scroll_px / self.line_height.as_f32().max(1.0)).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * self.line_height.as_f32();
        let mode = self.term_mode();
        // 应用接管鼠标时滚轮也归应用（htop 列表滚动）；Shift 旁路回本地回滚。
        if !event.modifiers.shift && mode.intersects(TermMode::MOUSE_MODE) {
            let code =
                if lines > 0 { mouse_protocol::WHEEL_UP } else { mouse_protocol::WHEEL_DOWN };
            for _ in 0..lines.unsigned_abs() {
                self.send_mouse_report(event.position, code, true, &event.modifiers);
            }
            cx.notify();
            return;
        }
        let Some(session) = &self.session else { return };
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // 备用屏（less/vim）：滚轮翻译成方向键。
            let seq: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                if lines > 0 { b"\x1bOA" } else { b"\x1bOB" }
            } else if lines > 0 {
                b"\x1b[A"
            } else {
                b"\x1b[B"
            };
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs() {
                bytes.extend_from_slice(seq);
            }
            session.notifier.notify(bytes);
        } else {
            session.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }

    fn marked_utf16_len(&self) -> usize {
        self.marked_text.as_deref().map(|t| t.encode_utf16().count()).unwrap_or(0)
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // 兜底清理：无论视图以何种路径销毁（关 Tab、关窗口、退出应用），
        // 都保证 PTY 线程和子进程被回收。
        self.shutdown();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // 终端没有文本框语义的选区；返回折叠选区让 IME 把组合文本插在光标处。
        let len = self.marked_utf16_len();
        Some(UTF16Selection { range: len..len, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked_text.as_ref().map(|_| 0..self.marked_utf16_len())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() && self.exited.is_none() {
            // 行镜像吃 IME 管道的字符（含中文提交与普通击键文本）。
            if !self.term_mode().contains(TermMode::ALT_SCREEN) {
                crate::display::Display::nebula_input_text(&mut self.suggest, text);
            }
            self.write_user_text(text.to_owned(), false, text.as_bytes().to_vec(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = if new_text.is_empty() { None } else { Some(new_text.to_string()) };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.ime_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // OSC 9 程序通知：旧壳只在窗口没聚焦时递出去，聚焦时用户本来就在看这
        // 个 pane。这里是最早能同时拿到 `Window` 和 view 可变引用的地方。用驻留
        // 式 banner 而不是 toast——人不在看的时候弹一条 5 秒自动消失的提示等于
        // 没弹。推送 defer 到本轮 effect 之后，不在 render 中途改 `Root`。
        if !self.pending_notify.is_empty() {
            let notes = std::mem::take(&mut self.pending_notify);
            if !window.is_window_active() {
                window.defer(cx, move |window, cx| {
                    for body in notes {
                        crate::gpui_shell::toast::banner(
                            window,
                            cx,
                            crate::display::ToastKind::Info,
                            body,
                        );
                    }
                });
            }
        }
        if let Some((title, body)) = self.pending_bell_notify.take() {
            if !window.is_window_active() {
                #[cfg(windows)]
                crate::notify::toast(&title, &body);
                window.defer(cx, move |window, cx| {
                    crate::gpui_shell::toast::banner(
                        window,
                        cx,
                        crate::display::ToastKind::Info,
                        format!("{title} · {body}"),
                    );
                });
            }
        }
        // 文件树拖到终端时的落点高亮：强调色压到很低的不透明度，与 hover 的
        // 中性灰区分开——用户要能一眼看出"松手会落在这里"。
        let drop_highlight = cx.theme().accent.opacity(0.18);
        let mut root = div()
            .id("nebula-terminal")
            .key_context(KEY_CONTEXT)
            .size_full()
            // SSH 连接卡片使用 absolute + inset_0；本 pane 必须成为它的
            // containing block，否则坐标会逃到工作区/窗口根，轨道和粒子
            // 就会出现在终端卡片之外甚至窗口底部。
            .relative()
            .overflow_hidden()
            // 不画自己的背景：卡容器统一负责"卡底色（带窗口透明度）→
            // 壁纸"两层，这里再铺一层不透明 bg 会把它们全部盖死。圆角也
            // 只属于整个终端卡外壳；分屏 pane 自己带圆角会露出四个独立卡片。
            // 卡内呼吸边距，对齐旧壳网格 reserve 的换算值：上下 8（chrome
            // 64/底 16 各减 8px 卡缝），左右 12（CONTENT_PAD_X 20 − 卡缝 8）。
            // 网格因此不贴圆角；padding 区点击由 grid_point 的钳制兜底。
            .py(px(8.0))
            .px(px(12.0))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_terminal_tab))
            .on_action(cx.listener(Self::on_terminal_back_tab))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // 拖出终端范围松手的兜底（含拖到窗口外）：见 `on_mouse_up_out`。
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_right_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_middle_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            // 文件树拖进来 = 把路径粘进 shell（旧壳 `FileDrag` 同一合同：只
            // 粘贴，绝不代按 Enter）。`drag_over` 给一层落点高亮，否则用户拖
            // 到一半不知道松手会不会生效。
            .drag_over::<crate::gpui_shell::file_drop::FileTreeDrag>(move |style, _, _, _| {
                style.bg(drop_highlight)
            })
            .on_drop(cx.listener(
                |this,
                 drag: &crate::gpui_shell::file_drop::FileTreeDrag,
                 window: &mut Window,
                 cx| {
                    let Some(bytes) =
                        crate::display::side_panel::drop_text_for_path(&drag.path_text)
                    else {
                        // 含控制字符的路径写进 PTY 等于替用户执行命令。
                        return;
                    };
                    this.write_bytes(bytes);
                    // 粘完把焦点交回终端：用户接着就要敲命令，还要先点一下
                    // 才能输入的话，这个手势就白省了。
                    window.focus(&this.focus_handle, cx);
                    cx.notify();
                },
            ))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.clear_link_hover(cx);
                }
            }));

        if let Some(error) = &self.error {
            root = root.child(div().p_4().text_color(gpui::red()).child(error.clone()));
        } else {
            root = root.child(TerminalElement::new(cx.entity()));
            // SSH 连接卡片（旧壳 `display::ssh_connect` 的 GPUI 形态）：
            // 状态机/文案/常量直接复用，卡片遮罩盖住空 grid。350ms 显示
            // 门槛由 `visible()` 决定；动画帧驱动粒子与进度插值。
            if let Some(state) = &self.ssh_connect {
                if state.visible() {
                    root = root.child(super::ssh_connect_overlay::overlay(state, cx));
                    if !state.failed() {
                        // 每帧 step + 重绘（旧壳吃共享 motion frame，GPUI 用
                        // 下一帧回调自驱）：notify 触发下一次 render，render
                        // 再续下一帧——环由渲染侧闭合，无需递归闭包。
                        cx.on_next_frame(window, |this, _, cx| {
                            let now = std::time::Instant::now();
                            let dt = now - this.ssh_connect_last_step;
                            this.ssh_connect_last_step = now;
                            if let Some(state) = &mut this.ssh_connect {
                                state.step(dt);
                            }
                            cx.notify();
                        });
                    }
                }
            }
            if let Some(exited) = &self.exited {
                root = root.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(gpui::opaque_grey(0.2, 0.9))
                        .text_sm()
                        .child(exited.clone()),
                );
            }
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use gpui::{FontStyle, FontWeight};

    use super::{
        PASTE_CONFIRM_LINES, TerminalView, mono_font, paste_line_count, selection_scroll_lines,
    };

    /// 拖选自动回滚的判据（旧壳 `update_selection_scrolling` 的数值合同）：
    /// 网格内不滚，越过上边界往回滚历史、越过下边界往前滚，贴边 1 行、每远离
    /// 20px 加一档，并且封顶在一屏——指针甩出窗口不该一抖到底。
    #[test]
    fn selection_scroll_speed_scales_with_distance_past_the_grid() {
        let (top, bottom, rows) = (100.0, 500.0, 30);
        assert_eq!(selection_scroll_lines(300.0, top, bottom, rows), 0, "网格内不滚");
        assert_eq!(selection_scroll_lines(top, top, bottom, rows), 0, "上边界本身仍在网格内");
        assert_eq!(selection_scroll_lines(top - 1.0, top, bottom, rows), 1, "刚越界＝最慢一档");
        assert_eq!(selection_scroll_lines(top - 20.0, top, bottom, rows), 2);
        assert_eq!(selection_scroll_lines(top - 40.0, top, bottom, rows), 3);
        assert_eq!(selection_scroll_lines(bottom, top, bottom, rows), -1, "底边像素属于下方");
        assert_eq!(selection_scroll_lines(bottom + 20.0, top, bottom, rows), -2);
        assert_eq!(
            selection_scroll_lines(-10_000.0, top, bottom, rows),
            rows,
            "甩到屏幕外也每 tick 最多一屏"
        );
        assert_eq!(selection_scroll_lines(10_000.0, top, bottom, rows), -rows);
    }

    /// 行数判据数的是「落到终端算几行」，不是 `str::lines`：CRLF 一次换行、
    /// 裸 CR 也是换行（`str::lines` 会把它整段算成 1 行）、末尾那个换行不多
    /// 算一行。20 行确认弹窗的触发全靠这个函数，所以它就是那条合同。
    #[test]
    fn paste_line_count_counts_terminal_rows_not_str_lines() {
        assert_eq!(paste_line_count(""), 0);
        assert_eq!(paste_line_count("one"), 1);
        assert_eq!(paste_line_count("one\n"), 1, "末尾换行不额外多算一行");
        assert_eq!(paste_line_count("a\r\nb"), 2, "CRLF 只算一次换行");
        assert_eq!(paste_line_count("a\rb\rc"), 3, "裸 CR 也是换行");
        assert_eq!(paste_line_count("a\r\nb\r\n"), 2);
    }

    /// 阈值边界：19 行直通、20 行拦下问一句。
    #[test]
    fn paste_confirmation_triggers_at_the_threshold() {
        let rows = |n: usize| "x\n".repeat(n);
        assert!(paste_line_count(&rows(PASTE_CONFIRM_LINES - 1)) < PASTE_CONFIRM_LINES);
        assert!(paste_line_count(&rows(PASTE_CONFIRM_LINES)) >= PASTE_CONFIRM_LINES);
    }

    #[test]
    fn terminal_font_explicitly_enables_maple_ligatures() {
        let font = mono_font(
            crate::font_install::REQUIRED_FONT_FAMILY,
            FontWeight::NORMAL,
            FontStyle::Normal,
        );
        assert_eq!(font.features.tag_value_list(), &[("calt".to_owned(), 1)]);
    }

    #[test]
    fn cell_width_mode_matches_the_legacy_grid_rounding_contract() {
        assert_eq!(
            f32::from(TerminalView::effective_cell_width(
                10.8,
                nebula_settings::CellWidthModeName::Compact,
                1.0,
                0.0,
            )),
            10.0
        );
        assert_eq!(
            f32::from(TerminalView::effective_cell_width(
                10.8,
                nebula_settings::CellWidthModeName::Relaxed,
                1.0,
                0.0,
            )),
            11.0
        );

        let compact = f32::from(TerminalView::effective_cell_width(
            9.2,
            nebula_settings::CellWidthModeName::Compact,
            1.5,
            0.0,
        ));
        assert!((compact - 13.0 / 1.5).abs() < f32::EPSILON);

        let relaxed = f32::from(TerminalView::effective_cell_width(
            9.2,
            nebula_settings::CellWidthModeName::Relaxed,
            1.5,
            0.0,
        ));
        assert!((relaxed - 14.0 / 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn line_height_uses_shaped_metrics_and_device_pixel_offset() {
        // Maple's hhea metrics are 1.32em. At 150% DPI, the legacy 4px
        // Windows offset yields floor(15 * 1.32 * 1.5 + 4) = 33 device px.
        let height = TerminalView::effective_line_height(15.0 * 1.32, 4.0, 1.5);
        assert!((f32::from(height) - 22.0).abs() < f32::EPSILON);
    }
}
