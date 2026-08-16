//! PTY 会话接线：`Term` + `EventLoop` + ConPTY，全部来自 `nebula_terminal`。
//!
//! 与 `nebula_app::window_context::create_pane` 相同的模式，只是事件出口换成
//! futures channel，让 GPUI 前台任务可以 `await` 事件。SSH 会话复用同一
//! `TerminalSession` 形状：传输层换成 `ssh_session::spawn_session`（russh
//! 直连，与旧壳同一条业务路径），事件与输入协议不变。

use std::sync::Arc;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nebula_terminal::Term;
use nebula_terminal::event::{Event, EventListener, WindowSize};
use nebula_terminal::event_loop::{EventLoop, Notifier};
use nebula_terminal::grid::Dimensions;
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::Config;
use nebula_terminal::tty;
use nebula_terminal::tty::EventedPty as _;

/// 把 PTY/SSH 线程发出的终端事件转投到 GPUI 前台的异步通道。本地与 SSH
/// 会话共用同一形状：`stages` 只有 SSH 业务层会写（连接阶段横幅的数据
/// 源），本地会话的这条通道保持沉默——统一类型让 `Term<EventProxy>` 在
/// 视图层只有一种，SSH 不需要平行的第二套会话结构。
#[derive(Clone)]
pub struct EventProxy {
    events: UnboundedSender<Event>,
    stages: UnboundedSender<crate::ssh_session::SshStage>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.events.unbounded_send(event);
    }
}

impl crate::ssh_session::SshEventHost for EventProxy {
    fn ssh_stage(&self, stage: crate::ssh_session::SshStage) {
        let _ = self.stages.unbounded_send(stage);
    }
}

/// 初始网格尺寸；`Term::new`/`Term::resize` 只消费行列数。
#[derive(Clone, Copy)]
pub struct GridSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct TerminalSession {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    /// PTY 直系 shell PID；关闭确认沿用旧壳 `busy_child(shell_pid)` 判据。
    /// SSH 没有本地 shell 进程，固定为 0。
    pub shell_pid: u32,
}

/// 一次会话 spawn 的完整出口：会话句柄 + 终端事件流 + SSH 阶段流
/// （本地会话的阶段流永远安静，接收端可以直接丢弃）。
pub type SpawnedSession =
    (TerminalSession, UnboundedReceiver<Event>, UnboundedReceiver<crate::ssh_session::SshStage>);

/// 启动一个本地 shell 会话。尺寸随后由首帧 prepaint 按真实布局重设；
/// `term_config` 携带运行时设置（默认光标形状/闪烁等）。
///
/// `shell`：设置里选定的默认 shell（`shell_detect::resolve_id` +
/// `DetectedShell::shell()`，含各家集成注入）；`None` 走引擎默认
/// （PowerShell + PTY 层集成）——语义与旧壳 `default_shell_launch` 一致。
pub fn spawn(
    window_size: WindowSize,
    term_config: Config,
    shell: Option<tty::Shell>,
    pane_id: u64,
    cwd: Option<std::path::PathBuf>,
) -> std::io::Result<SpawnedSession> {
    let (tx, rx) = unbounded();
    let (stage_tx, stage_rx) = unbounded();
    let proxy = EventProxy { events: tx, stages: stage_tx };

    let grid = GridSize {
        columns: window_size.num_cols as usize,
        screen_lines: window_size.num_lines as usize,
    };
    let term = Arc::new(FairMutex::new(Term::new(term_config, &grid, proxy.clone())));

    let mut options = tty::Options::default();
    options.shell = shell;
    options.working_directory = cwd;
    options.env.insert(crate::ai_hook::PANE_ENV.to_owned(), pane_id.to_string());
    tty::setup_env();
    let pty = tty::new(&options, window_size, 0)?;
    let shell_pid = pty.child_pid().unwrap_or(0);

    // NEBULA_PTY_RECORD=1 复用 ref_test 的 conout 录制（./nebula.recording），
    // 用于 resize 锚定问题的字节级取证。
    let record = std::env::var_os("NEBULA_PTY_RECORD").is_some();
    let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, options.drain_on_exit, record)?;
    let notifier = Notifier(event_loop.channel());
    let _io_thread = event_loop.spawn();

    Ok((TerminalSession { term, notifier, shell_pid }, rx, stage_rx))
}

/// 启动一个 SSH 直连会话（russh，与旧壳 `create_ssh_pane` 同一业务层）：
/// 地址解析/认证/代理/跳板全部在共享 Runtime 内完成；连接失败的原因由
/// 业务层写进 grid，阶段流交给视图画连接横幅。
pub fn spawn_ssh(
    destination: String,
    window_size: WindowSize,
    term_config: Config,
) -> std::io::Result<SpawnedSession> {
    let (tx, rx) = unbounded();
    let (stage_tx, stage_rx) = unbounded();
    let proxy = EventProxy { events: tx, stages: stage_tx };

    let grid = GridSize {
        columns: window_size.num_cols as usize,
        screen_lines: window_size.num_lines as usize,
    };
    let term = Arc::new(FairMutex::new(Term::new(term_config, &grid, proxy.clone())));
    let sender =
        crate::ssh_session::spawn_session(destination, window_size, Arc::clone(&term), proxy)?;

    Ok((TerminalSession { term, notifier: Notifier(sender), shell_pid: 0 }, rx, stage_rx))
}
