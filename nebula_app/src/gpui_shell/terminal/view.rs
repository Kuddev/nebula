//! 终端视图：持有会话、处理输入与 IME、驱动重绘。

mod broadcast;
mod pointer;
mod runtime;

pub use broadcast::TerminalInput;
pub use runtime::InputOrigin;

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    Font, FontFeatures, FontStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, ScrollWheelEvent, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, TextRun, UTF16Selection, Window, div, point, px,
};
use gpui_component::{Sizable as _, WindowExt as _, checkbox::Checkbox};
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
use crate::gpui_shell::prelude::{ActiveTheme as _, ButtonVariant, Colorize as _, confirm_dialog};
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

/// 「刚成功」的对勾停留多久再沉降为圆点。看门狗按 1Hz 复核，所以实际停留是
/// 这个值到 +1s；取 1.2s 是为了让它至少跨过一个复核点，不会一闪就没。
pub(super) const COMPLETION_FLASH: std::time::Duration = std::time::Duration::from_millis(1200);

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

fn cursor_blink_allowed(
    terminal_blinking: bool,
    vi_mode: bool,
    show_cursor: bool,
    ime_preedit: bool,
    window_active: bool,
    pane_focused: bool,
) -> bool {
    terminal_blinking && (vi_mode || show_cursor) && !ime_preedit && window_active && pane_focused
}

fn restart_cursor_blink_phase(cursor_visible: &mut bool, cursor_blink_epoch: &mut u64) -> u64 {
    *cursor_visible = true;
    *cursor_blink_epoch = cursor_blink_epoch.wrapping_add(1);
    *cursor_blink_epoch
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

/// 只在粘贴可能被当前 shell 立即执行时确认。
///
/// Bracketed paste 与全屏 TUI 都会把内容作为应用输入处理；继续按行数拦截只会
/// 给右键粘贴平添一层。裸 shell 则对换行、提权命令和控制字符保持保守，这与
/// 与常见终端的 Paste Protection 风险模型一致。
fn paste_needs_confirmation(text: &str, mode: TermMode) -> bool {
    if mode.intersects(TermMode::BRACKETED_PASTE | TermMode::ALT_SCREEN) {
        return false;
    }

    let has_line_break = text.contains(['\n', '\r']);
    let has_unsafe_control = text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'));
    let starts_privileged_command = text
        .split(['\n', '\r'])
        .any(|line| matches!(line.split_ascii_whitespace().next(), Some("sudo" | "su")));

    has_line_break || has_unsafe_control || starts_privileged_command
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
    /// Provider 明确上报的 permission/awaiting-input 上下文。宿主拥有 Window，
    /// 由它负责驻留提示、后台 Tab 标记和系统通知；raw context 不进入文案。
    AiAttention(crate::ai_hook::AttentionContext),
    /// 非应用鼠标模式下右键命中真实终端选区。菜单由 workspace 根持有，
    /// 避免每个 pane 都渲染一份带叠加阴影的 PopupMenu。
    SelectionContextMenuRequested { position: Point<Pixels>, text: String },
    /// 程序上报的任务进度（OSC 9;4）变了。宿主把 pane 级状态投到 tab badge，
    /// 并且只把当前聚焦 pane 投到窗口级任务栏。
    ProgressChanged(crate::taskbar::TaskProgress),
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
        /// Remote cwd inherited by duplicate-tab. `None` starts in the
        /// account's ordinary login directory.
        cwd: Option<String>,
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
///
/// 成员分两类，决定它在什么时候该消失（呈现规则在
/// `workspace::sidebar::tab_presentation`，两种 tab 布局共用那一处）：
///
/// - **事件**（`Done` / `Completed`）——「刚发生了一件你不在场的事」。你到场就
///   没有信息量了，所以当前 tab 一律不显示。
/// - **状态**（`Running` / `Paused` / `WaitingInput` / `Attention` /
///   `CommandFailed` / `Failed`）——「此刻仍是这个样子」。你看不看它都还成立，
///   所以永远显示。
///   `CommandFailed` 归到这一类：命令失败是需要你处理的结果，不是「未读输出」，
///   看一眼不等于处理完了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarActivity {
    Idle,
    Running,
    /// 程序通过 OSC 9;4 报告暂停。它仍是进行中状态，但不应继续驱动 spinner。
    Paused,
    /// Agent 回合完成，等待下一条指令（旧壳蓝点语义）。唯一的「事件」态。
    Done,
    /// 刚刚完成（`COMPLETION_FLASH` 之内）：闪一个对勾再沉降为 [`Self::Done`]
    /// 的圆点。与 `Done` 同属「事件」，当前 tab 一样不显示。
    Completed,
    /// 上一条命令以非 0 退出（OSC 133;D）。与 [`Self::Failed`] 不是一件事：这条
    /// 说「最近那条命令失败了」，下一条命令起跑即清；`Failed` 说「这个 pane 本身
    /// 坏了」（进程没了、SSH 连不上），不会自己好。
    CommandFailed,
    /// 命令停在交互提示上等人打字（`[y/n]`、口令、Press ENTER）。
    ///
    /// 与 `Attention` 拆开而不是合并：两者都画开掌（要求用户做的事是同一件），
    /// 差别走颜色轴——`Attention` 是 agent 被授权卡住、整个回合就此阻塞，取
    /// warning；停在 `[y/n]` 是常态，取 primary，不抢警示色。
    WaitingInput,
    /// Agent 停在授权/提问上，需要用户表态（旧壳手掌语义，现在形状也回到手掌）。
    Attention,
    Failed,
}

pub struct TerminalView {
    pub(super) answers: crate::assistant_answer::AnswerInbox,
    pub(super) answer_reader: Option<gpui::Entity<super::answer_reader::AnswerReader>>,
    preserve_agent_math_source: bool,
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
    /// 把**应用写死的**颜色按当前主题矫正（最低对比度 + 旧主题表面重映射）。
    ///
    /// 与旧壳 `Display::terminal_color_resolver` 是同一个类型、同一份缓存语义：
    /// grid 里存的始终是 PTY 给的原色，只有**绘制结果**进缓存，所以换主题时连
    /// 已经躺在 scrollback 里的历史输出也会跟着重算——这是修历史输出唯一的手段。
    /// 每个 pane 一份：缓存键含底色，不同 pane 的底色可以不同。
    pub color_resolver: crate::display::terminal_color::TerminalColorResolver,
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
    /// 这个 pane 的主 agent 进程 pid（第一个报到的那个）。只有它能写 pane 的
    /// 会话身份；嵌套 `claude -p` 子代理有自己的 pid，它那个短命 session id
    /// 不能顶掉真正活着的会话。回到提示符（133;D）或 SessionEnd 时清空。
    primary_agent_pid: Option<u32>,
    /// 程序上报的任务进度（OSC 9;4）。存在 pane 上、由宿主投到任务栏：一个
    /// 窗口只有一个任务栏按钮，谁被看着只有宿主知道。
    pub progress: crate::taskbar::TaskProgress,
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
    /// OSC 1337 图片的串行后台解码队列和有界像素缓存。图片不进入字符网格，
    /// 只用事件携带的绝对行锚定到对应的 scrollback 位置。
    pub(super) inline_images: super::inline_image::InlineImageStore,
    /// SSH 直连目的地（`user@host[:port]`）；本地会话为 None。
    pub ssh_destination: Option<String>,
    /// 创建本地 PTY 时冻结的受控环境，供独立 `pane.exec` child 复用。
    pub(crate) exec_context: Option<crate::runtime_exec::PaneExecContext>,
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
    /// 子进程已被告知的几何。`cols`/`rows`
    /// 是屏幕上正在渲染的视口，交互式拖拽期间它每帧跟手、本地网格随之 reflow，
    /// 而这里停在最后一次真正下发给 ConPTY 的边界上。判断"要不要通知子进程"
    /// 只能拿这个字段比，拿 `cols`/`rows` 比会把每一帧都当成新几何。
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
    /// 只等 PTY 那一半——网格已在观测当帧 reflow 过。
    pending_resize: Option<TerminalViewport>,
    /// 下一次网格变化是"结构性"的，子进程立即知道、不进尾沿去抖。
    ///
    /// 旧壳 `resize_active_layout()`（split.rs 的 Full variant）在分屏创建/
    /// 关闭、tab 新建这类一次性结构变化上同步下发 grid + PTY，只有交互式拖拽
    /// 才走"先只改 grid、PTY 等 settle"的两段式；结构性变化则立即同步两边。
    /// GPUI 壳原先把两者混成一条尾沿去抖路径，于是分屏后有 150ms
    /// 窗口期：子进程仍按旧宽度输出，那些字节按旧几何进网格，等提交时本地
    /// reflow 与 conhost rewrap 各折一套行数——提示符与回显从此错开几行（旧壳
    /// 同样的分屏动作没有这个窗口期，所以不复现）。
    structural_resize: bool,
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
    /// OS 窗口前台状态与 pane 内焦点是两层独立条件。缓存它们是因为 blink
    /// timer 回调没有 `Window`，状态变化由 GPUI observer 立即重启相位。
    cursor_window_active: bool,
    cursor_pane_focused: bool,
    _cursor_blink_subscriptions: [gpui::Subscription; 3],
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
    /// 上一条命令的退出码非 0（OSC 133;D 报的）。下一条命令起跑即作废
    /// （`mark_command_running`）——徽章说的是「最近那条失败了」，不是「这个
    /// pane 坏了」，后者是 `SidebarActivity::Failed`。
    last_command_failed: bool,
    /// 进入 `Finished` 的时刻。`COMPLETION_FLASH` 之内画对勾，之后沉降为圆点
    /// 两段式反馈先确认「刚成功」，再退化成「有结果没看」。
    completed_at: Option<std::time::Instant>,
    /// 上一次看门狗观察到的任务状态，用来认出「刚刚进入完成」这个边沿。
    last_task_state: Option<crate::runtime_api::RuntimeTaskState>,
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
    /// 与 legacy `config::cursor::Cursor::default().blink_interval` 保持一致。
    const CURSOR_BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

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
            TerminalLaunch::Ssh { destination, cwd } => {
                cwd.clone().unwrap_or_else(|| destination.clone())
            },
        };
        let (ssh_destination, initial_title, intro_shell_name, suggest_env, exec_context, spawned) =
            match launch {
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
                    let options = session::local_options(effective, pane_id, cwd);
                    let exec_context =
                        crate::runtime_exec::PaneExecContext::from_pty_options(&options);
                    (
                        None,
                        String::from("shell"),
                        shell_name,
                        suggest_env,
                        Some(exec_context),
                        session::spawn(initial, term_config, options),
                    )
                },
                TerminalLaunch::Ssh { destination, cwd } => (
                    Some(destination.clone()),
                    destination.clone(),
                    None,
                    crate::display::SuggestEnv::Ssh { destination: destination.clone() },
                    None,
                    session::spawn_ssh(destination, cwd, initial, term_config),
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
                    // Unix 没有 PowerShell 版欢迎脚本：一律走 fastfetch 回退链。
                    let intro_shell = if !cfg!(windows) || id.contains("wsl") || id.contains("bash")
                    {
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

        let focus_handle = cx.focus_handle();
        let cursor_window_active = window.is_window_active();
        let cursor_pane_focused = focus_handle.is_focused(window);
        let cursor_blink_subscriptions = [
            cx.observe_window_activation(window, |view, window, cx| {
                let active = window.is_window_active();
                if view.cursor_window_active != active {
                    view.cursor_window_active = active;
                    view.restart_cursor_blink(cx);
                }
            }),
            cx.on_focus_in(&focus_handle, window, |view, _window, cx| {
                if !view.cursor_pane_focused {
                    view.cursor_pane_focused = true;
                    view.restart_cursor_blink(cx);
                }
            }),
            cx.on_focus_out(&focus_handle, window, |view, _event, _window, cx| {
                if view.cursor_pane_focused {
                    view.cursor_pane_focused = false;
                    view.restart_cursor_blink(cx);
                }
            }),
        ];

        let mut view = Self {
            pane_id,
            session,
            focus_handle,
            math: super::math_overlay::MathOverlay::default(),
            answers: crate::assistant_answer::AnswerInbox::default(),
            answer_reader: None,
            preserve_agent_math_source: false,
            font: mono_font(&families[0], FontWeight::NORMAL, FontStyle::Normal),
            font_bold: mono_font(&families[1], FontWeight::BOLD, FontStyle::Normal),
            font_italic: mono_font(&families[2], FontWeight::NORMAL, FontStyle::Italic),
            font_bold_italic: mono_font(&families[3], FontWeight::BOLD, FontStyle::Italic),
            font_size,
            cell_width_mode,
            font_offset_x,
            font_offset_y,
            palette,
            color_resolver: Default::default(),
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
            primary_agent_pid: None,
            progress: crate::taskbar::TaskProgress::None,
            agent_turn_active: false,
            idle_screen_streak: 0,
            agent_runtime_submit_pending: false,
            pending_runtime_submit: None,
            pending_notify: Vec::new(),
            inline_images: super::inline_image::InlineImageStore::default(),
            ssh_destination,
            exec_context,
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
            structural_resize: false,
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
            cursor_window_active,
            cursor_pane_focused,
            _cursor_blink_subscriptions: cursor_blink_subscriptions,
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
            last_command_failed: false,
            completed_at: None,
            last_task_state: None,
            bell_flash: false,
            bell_flash_epoch: 0,
            pending_bell_notify: None,
        };
        // 出生即把亮暗种进 Term：`Term::color_scheme_dark` 的默认值是「暗」，
        // 浅色主题下启动的 pane 如果不种，第一个 DECSET 2031 的订阅方会拿到
        // 一个错的初值，而且在用户下一次改主题之前都纠不回来。
        if let Some(session) = &view.session {
            session.term.lock().set_color_scheme(view.palette.is_dark());
        }
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
            // OSC 9;4：程序自报任务进度。只在真的变了的时候上报——任务栏那条
            // 路要过 COM，`cargo build` 每秒发好几次进度更新，逐条投过去纯属
            // 浪费。
            TermEvent::Progress { state, value } => {
                let progress = crate::taskbar::TaskProgress::from_osc(state, value);
                if progress != self.progress {
                    self.progress = progress;
                    cx.emit(TerminalViewEvent::ProgressChanged(progress));
                }
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
            TermEvent::InlineImage { data, abs_line, width, height } => {
                let cell_height = f32::from(self.window_size.cell_height.max(1));
                let row_span = (height / cell_height).ceil().max(1.0) as usize;
                match self.inline_images.enqueue(data, abs_line, width, height, row_span) {
                    Ok(()) => self.drive_inline_image_decode(cx),
                    Err(error) => log::warn!("terminal image dropped: {error}"),
                }
            },
            TermEvent::CommandStart => {
                self.answers.begin_command();
                // 程序身份来自 Enter 时捕获的完整命令行；直接启动和
                // npx/node/uvx 等包装启动都会归一成 Agent slug。它是 WSL
                // 看不见来宾进程时的主通道，hook 信封仍是更准的覆盖层。
                let identity =
                    crate::ai_agents::AgentKind::parse_command(&self.suggest.last_committed)
                        .map(|agent| agent.slug().to_owned())
                        .or_else(|| crate::display::extract_program(&self.suggest.last_committed));
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
                // 首个词就是一个交互式 shell（`cmd`、`wsl`、裸 `bash`）：133;C
                // 是真的，但这条「命令」其实是一个新提示符，133;D 永远不会来
                // ——那个 shell 接管了终端，而我们的集成不在它里面。立刻按「已被
                // 进程树反证」处理，转圈不必等 3 秒节流窗口。
                //
                // 这不是把状态钉死：后续对账双向纠正，真在那个 shell 里跑起活儿
                // 会多出一个子进程，进程树看得见，状态会被拉回运行中。
                if crate::process_tree::is_interactive_shell_command(&self.suggest.last_committed) {
                    self.command_running_disproved = true;
                }
                if let Some(run) = &mut self.active_run
                    && run.phase == crate::runtime_api::RuntimeRunPhase::Submitted
                {
                    run.phase = crate::runtime_api::RuntimeRunPhase::Started;
                }
                cx.notify();
            },
            // 退出码接到「上一条命令失败」的未读徽章上（⚠ 三角）；`Some(0)` 与
            // `None`（没带退出码的 133;D）都算过关。记在下面那道 barrier 之后：
            // 属于上一轮/初始化的那个边沿不能改写徽章。
            TermEvent::CommandDone { exit_code } => {
                // 新 PTY 初始化提示符也可能先发一个 CommandDone。Runtime
                // 文本还在等待回显 barrier 时，这个边沿属于上一轮/初始化，
                // 不能清掉尚未发送的 Enter 或把新请求提前投影成 idle。
                if self.pending_runtime_submit.is_some() {
                    return;
                }
                self.last_command_failed = exit_code.is_some_and(|code| code != 0);
                // 旧壳同款收尾：CLI 退回提示符后，它不再是这个 pane 的前台
                // 事实——hook 稍后若仍在跑会重新点亮（handle_ai_hook 覆写）。
                if self.clear_foreground_agent_state() {
                    cx.emit(TerminalViewEvent::TitleChanged);
                }
                if let Some(run) = self.active_run.take() {
                    self.last_run =
                        Some(crate::runtime_api::RuntimeRunOutcome::command_done(run, exit_code));
                }
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
            // 仍未接线：OSC 1337 UserVar（AI 查询拦截）。
            _ => {},
        }
    }

    fn drive_inline_image_decode(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.inline_images.start_next() else { return };
        let decode = cx
            .background_executor()
            .spawn(async move { super::inline_image::decode(pending) });
        cx.spawn(async move |this, cx| {
            let result = decode.await;
            let _ = this.update(cx, |view, cx| {
                if let Err(error) = view.inline_images.finish(result) {
                    log::warn!("terminal image dropped: {error}");
                }
                view.drive_inline_image_decode(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// `Exited` 只对宿主发一次；重复的退出信号（ChildExit 之后必然跟 Exit）只更新文案。
    fn mark_exited(&mut self, message: String, cx: &mut Context<Self>) {
        self.pending_runtime_submit = None;
        self.suggest.pending_command_prompt = None;
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
            let mode = term.mode();
            cursor_blink_allowed(
                term.cursor_style().blinking,
                mode.contains(TermMode::VI),
                mode.contains(TermMode::SHOW_CURSOR),
                self.marked_text.is_some(),
                self.cursor_window_active,
                self.cursor_pane_focused,
            )
        })
    }

    fn restart_cursor_blink(&mut self, cx: &mut Context<Self>) {
        let epoch =
            restart_cursor_blink_phase(&mut self.cursor_visible, &mut self.cursor_blink_epoch);
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
        // 「有人在等你」的兜底与响铃的**提示方式**无关：`BellMode::None` 关掉的
        // 是声音和闪屏，不该把徽章一起关掉，所以这一段排在那道闸之前。
        //
        // 兜底只在没有权威判定时成立（见 `AgentStatus::is_decided`）：CC /
        // codex 回合结束也会响铃，让响铃压过 hook / 屏幕规则报出的 `Done`，
        // 屏幕上就会把「完成」显示成「在问你」。
        if self.running_program.is_some() && !self.agent_status.is_decided() {
            self.awaiting_input = true;
            cx.notify();
        }
        let mode = nebula_settings::RuntimeSettings::load().bell;
        if mode == nebula_settings::BellModeName::None {
            return;
        }
        let audible_ok = if mode.audible() { Self::ring_audible() } else { false };
        if mode.visual() || (mode.audible() && !audible_ok) {
            self.flash_bell(cx);
        }
        let program = self.running_program.clone();
        self.pending_bell_notify = Some(match program {
            Some(name) => (name, "任务完成，等待输入".to_owned()),
            None => (crate::brand::NAME.to_owned(), "终端响铃".to_owned()),
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

    /// 宿主宣告：下一次网格变化来自结构性布局改动，直接提交、不去抖。
    /// 见 `structural_resize` 字段注释。
    pub fn mark_structural_resize(&mut self) {
        self.structural_resize = true;
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

        // 本地网格立刻跟手，只有子进程那一半去抖（旧壳也通过
        // `resize_active_layout_grids` 采用同一策略）。两半的代价完全不对称：客户端
        // reflow 便宜且可逆，而每一次 `ResizePseudoConsole` 都让 conhost 重排
        // 自己的缓冲区，那些重排累积出的光标行漂移事后无从察觉。让网格落后于
        // 渲染就只能靠"视觉裁剪"预览未提交的几何，而裁剪只能裁行、无法重排列
        // ——宽度一变预览就是错的，且 Term 与屏幕不一致的每一毫秒里到达的字节
        // 都会按旧宽度进网格。
        if change.grid_changed {
            self.resize_grid_only(viewport);
        }

        // 结构性变化（分屏创建/关闭、zoom、面板开合）不去抖：它只来一次，没有
        // 后续帧可以合并，多等的每一毫秒都是子进程按旧几何输出的窗口期。旧壳在
        // 这些路径上走 `resize_active_layout()` 同步下发，这里复刻同一条合同。
        if std::mem::take(&mut self.structural_resize) {
            self.pending_resize = None;
            self.resize_epoch = self.resize_epoch.wrapping_add(1);
            self.commit_viewport(viewport);
            return;
        }

        // ConPTY 的直通式 conhost 在 resize 时零输出，指望终端侧 reflow 与
        // 它内部 buffer rewrap 一致；两者的换行语义存在路径依赖差异，每多
        // 一次中间宽度的 ResizePseudoConsole 就多攒一分光标行漂移（字节取
        // 证：13 次提交后 PSReadLine 的 CUP 行比真实提示行高 7 行）。旧壳
        // (winit) 的模态拖拽天然只在松手后送达一次 resize，从不累积。这里
        // 复刻该合同：子进程那一半纯尾沿去抖——只进 pending，视口静默
        // RESIZE_SETTLE_DELAY 后一次性下发。净零手势（挤压后拖回原宽）最终
        // 提交同尺寸 no-op，rewrap 次数为零；网格已在上面逐帧跟手，所以去抖
        // 的代价只落在"子进程晚知道几十毫秒"，屏幕上看不出来。
        self.pending_resize = Some(viewport);
        self.schedule_settled_resize(cx);
    }

    /// 只让本地网格 reflow 到 `viewport`，子进程留在旧几何上。
    ///
    /// 走 `Msg::ResizeGrid` 而不是直接锁 `Term`：event_loop 的 resize 分支会先
    /// 把旧几何下已可读的字节全部消化掉，绝对 CUP 序列因此不会被解析进新宽度
    /// 的网格。UI 线程自己上锁 resize 就绕过了这道流边界保护。
    fn resize_grid_only(&mut self, viewport: TerminalViewport) {
        let Some(session) = &self.session else { return };
        let mut notifier = nebula_terminal::event_loop::Notifier(session.notifier.0.clone());
        notifier.on_resize_grid(viewport.window_size());
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
                    crate::gpui_shell::try_write_stderr(format_args!(
                        "[nebula:resize-trace] settle-timer gate={gate}"
                    ));
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
            crate::gpui_shell::try_write_stderr(format_args!(
                "[nebula:gpui] failed to persist font size: {err}"
            ));
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
        // 底色换了就把矫正缓存作废，并记下「旧底色 → 新底色」这一跳：应用当初
        // 按旧主题底色画的连续表面（面板、状态栏）要跟着搬过去，否则浅色主题上
        // 会留一整块旧的深色板。旧壳 `apply_nebula_theme` 同一时机做同一件事。
        let previous_background = self.palette.background;
        if previous_background != palette.background {
            self.color_resolver.theme_changed(
                super::element::rgb_from_rgba(previous_background),
                super::element::rgb_from_rgba(palette.background),
            );
        }
        self.palette = palette;
        self.copy_on_select = copy_on_select;
        self.default_cursor_style = default_cursor_style;
        if let Some(session) = &self.session {
            let mut term = session.term.lock();
            term.set_default_cursor_style(default_cursor_style);
            // 主题/底色变了要告诉订阅了 DECSET 2031 的子进程（`CSI ? 997;N n`）。
            // 没有这一步，已经跑着的 codex/cc/nvim 只知道它启动那一刻用 OSC 11
            // 问到的背景色：深色主题切成浅色之后，它继续用为深底挑的配色画在白
            // 底上。`set_color_scheme` 自己做变化检测，这里无条件调。
            term.set_color_scheme(self.palette.is_dark());
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

    /// Absolute remote cwd reported by OSC 7/title integration. Unlike
    /// [`Self::local_cwd`], this deliberately does not consult the host
    /// filesystem; a POSIX path belongs to the SSH endpoint.
    pub fn remote_cwd(&self) -> Option<String> {
        self.ssh_destination.as_ref()?;
        let path = self.cwd.trim();
        (path.starts_with('/') && !path.chars().any(char::is_control)).then(|| path.to_owned())
    }

    /// 这个 pane 的远端身份，仅当会话**已经就绪**时才给。
    ///
    /// 两个条件都必须成立：既要是 SSH pane，又要已经握手认证完成。只看
    /// `ssh_destination` 的话，连接卡片还在转圈时远端浏览器就会去开 SFTP
    /// 通道，撞上一个还没建立的传输——用户看到的是文件面板先报一个错，然后
    /// 终端才连上。
    pub fn ready_ssh_destination(&self) -> Option<&str> {
        let destination = self.ssh_destination.as_deref()?;
        matches!(self.ssh_stage, Some(crate::ssh_session::SshStage::Ready)).then_some(destination)
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

    /// 读剪贴板并粘贴。只有裸 shell 可能立即执行内容时才弹确认；支持
    /// bracketed paste 的应用和全屏 TUI 一路直通，不给右键粘贴平添一层。
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let lines = paste_line_count(&text);
        if nebula_settings::RuntimeSettings::load().multiline_paste_confirm
            && paste_needs_confirmation(&text, self.term_mode())
        {
            self.confirm_paste(text, lines, window, cx);
            return;
        }
        self.paste_now(&text, cx);
    }

    /// 风险粘贴的阻断式确认——提示三层里的「模态」：有待办动作、必须先决策。
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
        let never_ask_again = Arc::new(std::sync::atomic::AtomicBool::new(false));
        window.open_dialog(cx, move |dialog, window, _cx| {
            let text = text.clone();
            let view = view.clone();
            let checked = never_ask_again.load(std::sync::atomic::Ordering::Relaxed);
            let checkbox_state = never_ask_again.clone();
            let persist_state = never_ask_again.clone();
            confirm_dialog(
                dialog,
                window,
                title.clone(),
                body.clone(),
                ok_text.clone(),
                cancel_text.clone(),
                ButtonVariant::Primary,
            )
            .child(
                Checkbox::new("nebula-paste-never-ask")
                    .label(language.pick("不再询问", "Don't ask again"))
                    .checked(checked)
                    .small()
                    .on_click(move |checked, window, _| {
                        checkbox_state.store(*checked, std::sync::atomic::Ordering::Relaxed);
                        window.refresh();
                    }),
            )
            .on_ok(move |_, _window, cx| {
                if persist_state.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = nebula_settings::persist_keys(&[(
                        "multiline_paste_confirm",
                        "0".to_owned(),
                    )]);
                }
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
            crate::display::nebula_input_text(&mut self.suggest, &normalized);
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
    /// 行镜像。读法与旧壳 `nebula_commit_line` 的 Windows 契约一致：无法证明
    /// 是提示符的 REPL 行或中线编辑读不到就宁缺毋滥——键击重构的
    /// line_buf 在光标移动/Tab 补全后就是拼接垃圾，不能进历史。Agent 已在
    /// 前台时保留最初 shell 提示符，内部交互的 Enter 不得覆盖退出证据。
    fn commit_line(&mut self, cx: &mut Context<Self>) {
        let agent_already_active =
            self.running_program.as_deref().and_then(crate::ai_agents::AgentKind::parse).is_some();
        if !agent_already_active {
            self.suggest.pending_command_prompt = None;
        }
        #[cfg(windows)]
        if !agent_already_active && let Some(session) = &self.session {
            let term = session.term.lock();
            if !term.mode().intersects(TermMode::ALT_SCREEN | TermMode::VI) {
                let cursor = term.grid().cursor.point;
                match crate::display::nebula_prompt_line_from_raw_grid(
                    &term,
                    cursor,
                    &self.suggest.line_buf,
                    &self.suggest.suggest_env,
                ) {
                    Some(line) => {
                        self.suggest.screen_line = line.input;
                        self.suggest.pending_command_prompt = Some(line.prompt);
                    },
                    None => {
                        self.suggest.screen_line.clear();
                        self.suggest.pending_command_prompt = None;
                    },
                }
            } else {
                self.suggest.screen_line.clear();
                self.suggest.pending_command_prompt = None;
            }
        }
        suggest::commit_line(&mut self.suggest);
        if let Some(agent) =
            crate::ai_agents::AgentKind::parse_command(&self.suggest.last_committed)
        {
            self.running_program = Some(agent.slug().to_owned());
            self.agent_status = crate::ai_agents::AgentStatus::Working;
            self.agent_status_source = crate::ai_agents::AgentStatusSource::Process;
            self.agent_status_rule = None;
            self.agent_hook_seen = false;
            self.agent_turn_active = true;
            self.idle_screen_streak = 0;
            self.command_started = Some(std::time::Instant::now());
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
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

    fn track_encoded_key(&mut self, ks: &gpui::Keystroke, mode: &TermMode, cx: &mut Context<Self>) {
        if self.marked_text.is_some() || mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        let mods = &ks.modifiers;
        let plain_mods = !mods.control && !mods.alt && !mods.platform;
        match ks.key.as_str() {
            "enter" => {
                // 与 keymap 的 Shift+Enter 分流一致；传统 VT 仍发送 CR 并提交。
                let is_negotiated_multiline = mods.shift
                    && !mods.control
                    && !mods.alt
                    && (mode.intersects(
                        TermMode::DISAMBIGUATE_ESC_CODES
                            | TermMode::REPORT_ALL_KEYS_AS_ESC
                            | TermMode::REPORT_EVENT_TYPES,
                    ) || (cfg!(windows)
                        && mode.contains(TermMode::WIN32_INPUT_MODE)
                        && !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)));
                if !is_negotiated_multiline {
                    self.commit_line(cx);
                }
            },
            "backspace" if mods.control && !mods.alt && !mods.platform => {
                crate::display::nebula_input_delete_word(&mut self.suggest);
            },
            "backspace" if plain_mods => {
                crate::display::nebula_input_backspace(&mut self.suggest);
            },
            key => {
                let is_modifier = matches!(
                    key,
                    "shift" | "control" | "alt" | "platform" | "function" | "capslock"
                );
                // key_char 非空 = 平台判定它产生文本（随后从 IME 管道到达）。
                let produces_text = ks.key_char.as_deref().is_some_and(|text| !text.is_empty());
                if !is_modifier && !produces_text {
                    crate::display::nebula_clear_line(&mut self.suggest);
                }
            },
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.exited.is_some() || self.answer_reader.is_some() {
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
                    // 候选自动出现时保持未选中，让 Up/Down 继续交给 shell
                    // 历史；Tab 或鼠标先建立选中态后，方向键才导航列表。
                    key @ ("tab" | "down" | "up")
                        if key == "tab" || self.suggest.completion_selected.is_some() =>
                    {
                        suggest::popup_move(&mut self.suggest, if key == "up" { -1 } else { 1 });
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
                    crate::display::nebula_input_char(&mut self.suggest, c);
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
        self.track_encoded_key(ks, &mode, cx);

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
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.answer_reader.as_ref().map_or_else(|| self.focus_handle.clone(), |reader| reader.read(cx).focus_handle.clone())
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
        if self.marked_text.take().is_some() {
            self.restart_cursor_blink(cx);
        } else {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.answer_reader.is_some() {
            return;
        }
        let had_marked_text = self.marked_text.take().is_some();
        if !text.is_empty() && self.exited.is_none() {
            // 行镜像吃 IME 管道的字符（含中文提交与普通击键文本）。
            if !self.term_mode().contains(TermMode::ALT_SCREEN) {
                crate::display::nebula_input_text(&mut self.suggest, text);
            }
            self.write_user_text(text.to_owned(), false, text.as_bytes().to_vec(), cx);
        } else if had_marked_text {
            self.restart_cursor_blink(cx);
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
        if self.answer_reader.is_some() {
            return;
        }
        let marked_text = if new_text.is_empty() { None } else { Some(new_text.to_string()) };
        if self.marked_text != marked_text {
            self.marked_text = marked_text;
            self.restart_cursor_blink(cx);
        } else {
            cx.notify();
        }
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
        if let Some(reader) = &self.answer_reader {
            return div().size_full().child(reader.clone()).into_any_element();
        }
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
        if let Some(answer) = self.answers.latest.clone() {
            let provider = if answer.provider == "claude" { "Claude Code" } else { "Codex" };
            return div().size_full().relative()
                .child(root)
                .child(crate::gpui_shell::prelude::h_flex().absolute().top_0().right_2().px_2().gap_2().items_center()
                    .bg(cx.theme().background)
                    .child(div().text_xs().child(format!("{provider} · 回答")))
                    .child(crate::gpui_shell::prelude::Button::new("answer-open")
                        .label("阅读").small()
                        .on_click(cx.listener(|view, _, window, cx| view.open_answer(window, cx)))))
                .into_any_element();
        }
        root.into_any_element()
    }
}

impl TerminalView {
    fn open_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.answers.latest.clone() else { return };
        let reader = cx.new(|cx| super::answer_reader::AnswerReader::new(snapshot, cx));
        if self.agent_status == crate::ai_agents::AgentStatus::Blocked {
            reader.update(cx, |reader, cx| reader.needs_attention(cx));
        }
        cx.subscribe_in(&reader, window, |view, _, _: &super::answer_reader::ReaderEvent, window, cx| {
            view.answer_reader = None;
            window.focus(&view.focus_handle, cx);
            cx.notify();
        }).detach();
        let focus = reader.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        self.answer_reader = Some(reader);
        cx.notify();
    }

    pub(super) fn uses_source_reader_math(&mut self) -> bool {
        self.preserve_agent_math_source |= self.running_program.as_deref().is_some_and(|program| {
            matches!(program, "claude" | "codex")
        }) || self.ai_session.as_ref().is_some_and(|session| {
            matches!(session.source.as_str(), "claude" | "codex")
        });
        self.preserve_agent_math_source
    }
}

#[cfg(test)]
mod tests;
