//! The main event loop which performs I/O on the pseudoterminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::error;
use polling::{Event as PollingEvent, Events, PollMode, Poller};

use crate::event::{self, Event, EventListener, WindowSize};
use crate::grid::Dimensions as _;
use crate::index::{Column, Line};
use crate::osc_cwd::OscEvent;
use crate::sync::FairMutex;
use crate::term::Term;
use crate::{thread, tty};
use vte::ansi;

/// Max bytes to read from the PTY before forced terminal synchronization.
pub(crate) const READ_BUFFER_SIZE: usize = 0x10_0000;

/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

/// ConPTY 光标对账的静默期：最后一次 resize 提交后等这么久再探针，给
/// conhost 的内部整理（缓冲区塌缩/重锚）和 shell 的 resize 反应留时间。
const ALIGN_DELAY: Duration = Duration::from_millis(120);

/// Resize diagnostics are opt-in because PTY reads are a hot path. The trace
/// records the local grid state at the exact stream boundaries where bytes are
/// parsed or a resize is committed, so it can be aligned with
/// `NEBULA_PTY_RECORD=1` without changing terminal behavior.
fn resize_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NEBULA_RESIZE_TRACE").is_some())
}

fn trace_terminal_state<U: EventListener>(stage: &str, terminal: &Term<U>) {
    if !resize_trace_enabled() {
        return;
    }

    // PowerShell's configured prompt uses U+276F. Recording every visible
    // occurrence exposes stale prompt rows immediately after a reflow.
    let mut prompt_rows = Vec::new();
    for row in 0..terminal.screen_lines() {
        if (0..terminal.columns())
            .any(|column| terminal.grid()[Line(row as i32)][Column(column)].c == '❯')
        {
            prompt_rows.push(row);
        }
    }

    let cursor = terminal.grid().cursor.point;
    eprintln!(
        "[nebula:resize-trace] {stage} grid={}x{} cursor={}:{} wrap={} history={} display_offset={} prompts={prompt_rows:?}",
        terminal.columns(),
        terminal.screen_lines(),
        cursor.line.0,
        cursor.column.0,
        terminal.grid().cursor.input_needs_wrap,
        terminal.history_size(),
        terminal.grid().display_offset(),
    );
}

/// 向 conhost 要光标真值：临时 `AttachConsole` 到 ConPTY 子进程的控制台，
/// 读 `GetConsoleScreenBufferInfo`，换算成视口相对行（0 基）。
///
/// 进程同一时刻只能挂一个控制台，多 pane 并发对账用全局锁串行化；每次
/// 探针都 attach→读→detach，窗口只有微秒级。Nebula 主进程是 GUI 子系统
/// （自身无控制台），detach 后回到无控制台状态，不影响任何组件。失败
/// （子进程已退出、权限等）一律返回 None，对账静默放弃。
#[cfg(windows)]
fn conpty_cursor_viewport_row(pid: u32) -> Option<usize> {
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, FreeConsole, GetConsoleScreenBufferInfo,
    };

    static ATTACH_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ATTACH_LOCK.lock().ok()?;

    // SAFETY: 纯 Win32 控制台句柄操作；FreeConsole 对无控制台进程是无害
    // no-op，收尾的 FreeConsole 恢复 GUI 进程的无控制台状态。
    unsafe {
        FreeConsole();
        if AttachConsole(pid) == 0 {
            return None;
        }
        let mut conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let handle = CreateFileW(
            conout.as_mut_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            FreeConsole();
            return None;
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        let ok = GetConsoleScreenBufferInfo(handle, &mut info);
        CloseHandle(handle);
        FreeConsole();
        if ok == 0 {
            return None;
        }
        let row = i32::from(info.dwCursorPosition.Y) - i32::from(info.srWindow.Top);
        usize::try_from(row).ok()
    }
}

#[cfg(not(windows))]
fn conpty_cursor_viewport_row(_pid: u32) -> Option<usize> {
    None
}

/// 本地 PTY 与远端传输共用的有状态终端字节流处理器。
/// OSC 提取必须紧贴 VT 解析，避免两类会话产生不同的目录、命令和图片状态。
#[derive(Default)]
pub struct StreamProcessor {
    parser: ansi::Processor,
    cwd_sniffer: crate::osc_cwd::CwdSniffer,
    window_size: Option<WindowSize>,
    remote_hook_token: Option<String>,
}

impl StreamProcessor {
    pub fn resize(&mut self, window_size: WindowSize) {
        self.window_size = Some(window_size);
    }

    pub fn set_remote_hook_token(&mut self, token: String) {
        self.remote_hook_token = Some(token);
    }

    pub fn next_sync_timeout(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    pub fn sync_bytes_count(&self) -> usize {
        self.parser.sync_bytes_count()
    }

    pub fn stop_sync<U: EventListener>(&mut self, terminal: &mut Term<U>) {
        self.parser.stop_sync(terminal);
    }

    pub fn feed<U: EventListener>(
        &mut self,
        terminal: &mut Term<U>,
        event_proxy: &U,
        bytes: &[u8],
    ) {
        let osc_events = self.cwd_sniffer.feed(bytes);
        let mut latest_cwd = None;
        let mut advanced = 0;
        for (offset, event) in osc_events {
            match event {
                OscEvent::Cwd(cwd) => latest_cwd = Some(cwd),
                OscEvent::CommandStart => event_proxy.send_event(Event::CommandStart),
                OscEvent::CommandDone { exit_code } => {
                    event_proxy.send_event(Event::CommandDone { exit_code })
                },
                OscEvent::UserVar { name, value } => {
                    event_proxy.send_event(Event::UserVar { name, value })
                },
                OscEvent::Notify(text) => event_proxy.send_event(Event::Notify(text)),
                OscEvent::RemoteHook { token, envelope } => {
                    if self.remote_hook_token.as_deref() == Some(token.as_str()) {
                        event_proxy.send_event(Event::AiHookEnvelope(envelope));
                    }
                },
                OscEvent::PromptMark => {
                    self.parser.advance(terminal, &bytes[advanced..offset]);
                    advanced = offset;
                    terminal.nebula_add_prompt_mark();
                },
                OscEvent::InlineImage { png, width, height } => {
                    self.parser.advance(terminal, &bytes[advanced..offset]);
                    advanced = offset;
                    let (cell_w, cell_h) = self.window_size.map_or((9.0, 20.0), |ws| {
                        (f32::from(ws.cell_width), f32::from(ws.cell_height))
                    });
                    let max_w = terminal.columns() as f32 * cell_w;
                    let scale = (max_w / width as f32).min(1.0);
                    let disp_w = width as f32 * scale;
                    let disp_h = height as f32 * scale;
                    let rows = (disp_h / cell_h).ceil().max(1.0) as usize;
                    let abs_line = terminal.nebula_cursor_abs_line();
                    for _ in 0..=rows {
                        self.parser.advance(terminal, b"\r\n");
                    }
                    event_proxy.send_event(Event::InlineImage {
                        png: std::sync::Arc::new(png),
                        abs_line,
                        width: disp_w,
                        height: disp_h,
                    });
                },
            }
        }
        self.parser.advance(terminal, &bytes[advanced..]);
        if let Some(cwd) = latest_cwd {
            event_proxy.send_event(Event::CwdReport(cwd));
        }
    }
}

/// Messages that may be sent to the `EventLoop`.
#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    /// Indicates that the `EventLoop` should shut down, as Nebula is shutting down.
    Shutdown,

    /// Instruction to resize the PTY.
    Resize(WindowSize),
}

/// The main event loop.
///
/// Handles all the PTY I/O and runs the PTY parser which updates terminal
/// state.
pub struct EventLoop<T: tty::EventedPty, U: EventListener> {
    poll: Arc<Poller>,
    pty: T,
    rx: PeekableReceiver<Msg>,
    tx: Sender<Msg>,
    terminal: Arc<FairMutex<Term<U>>>,
    event_proxy: U,
    drain_on_exit: bool,
    ref_test: bool,
}

impl<T, U> EventLoop<T, U>
where
    T: tty::EventedPty + event::OnResize + Send + 'static,
    U: EventListener + Send + 'static,
{
    /// Create a new event loop.
    pub fn new(
        terminal: Arc<FairMutex<Term<U>>>,
        event_proxy: U,
        pty: T,
        drain_on_exit: bool,
        ref_test: bool,
    ) -> io::Result<EventLoop<T, U>> {
        let (tx, rx) = mpsc::channel();
        let poll = Poller::new()?.into();
        Ok(EventLoop {
            poll,
            pty,
            tx,
            rx: PeekableReceiver::new(rx),
            terminal,
            event_proxy,
            drain_on_exit,
            ref_test,
        })
    }

    pub fn channel(&self) -> EventLoopSender {
        EventLoopSender { sender: self.tx.clone(), poller: self.poll.clone() }
    }

    /// Drain the channel.
    ///
    /// Returns `Err(())` when a shutdown message was received; otherwise the
    /// last resize in this channel batch is returned for an ordered commit.
    fn drain_recv_channel(&mut self, state: &mut State) -> Result<Option<WindowSize>, ()> {
        // Resize storms (live window drags) queue faster than
        // ResizePseudoConsole drains them — the console host performs a full
        // viewport reflow per call. Within one drain only the newest size
        // matters: intermediate sizes carry no information (the next message
        // supersedes them), and the final size is never dropped because it is
        // always the last one seen. The slower the host reflows, the more
        // sizes pile up per drain and the harder the coalescing works.
        let mut resize = None;
        while let Some(msg) = self.rx.recv() {
            match msg {
                Msg::Input(input) => state.write_list.push_back(input),
                Msg::Resize(window_size) => resize = Some(window_size),
                Msg::Shutdown => return Err(()),
            }
        }

        Ok(resize)
    }

    #[inline]
    fn pty_read<X>(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        mut writer: Option<&mut X>,
    ) -> io::Result<usize>
    where
        X: Write,
    {
        let mut unprocessed = 0;
        let mut processed = 0;

        // Reserve the next terminal lock for PTY reading.
        let _terminal_lease = Some(self.terminal.lease());
        let mut terminal = None;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => break,
                Ok(got) => {
                    // Startup profiling: the process-wide first PTY output ≈
                    // the console host finished its bring-up handshake and
                    // the shell started talking.
                    {
                        use std::sync::atomic::{AtomicBool, Ordering};
                        static FIRST_BYTES: AtomicBool = AtomicBool::new(false);
                        if !FIRST_BYTES.swap(true, Ordering::Relaxed) {
                            crate::pty_trace("first conout bytes");
                        }
                    }
                    unprocessed += got;
                },
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            break;
                        }
                    },
                    _ => return Err(err),
                },
            }

            // Attempt to lock the terminal.
            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.terminal.try_lock_unfair() {
                    // Force block if we are at the buffer size limit.
                    None if unprocessed >= READ_BUFFER_SIZE => self.terminal.lock_unfair(),
                    None => continue,
                    Some(terminal) => terminal,
                }),
            };

            // Write a copy of the bytes to the ref test file.
            if let Some(writer) = &mut writer {
                writer.write_all(&buf[..unprocessed]).unwrap();
            }

            state.stream.feed(&mut **terminal, &self.event_proxy, &buf[..unprocessed]);
            trace_terminal_state("after-pty-read", &terminal);

            processed += unprocessed;
            unprocessed = 0;

            // Assure we're not blocking the terminal too long unnecessarily.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        // Queue terminal redraw unless all processed bytes were synchronized.
        if state.stream.sync_bytes_count() < processed && processed > 0 {
            self.event_proxy.send_event(Event::Wakeup);
        }

        Ok(processed)
    }

    #[inline]
    fn pty_write(&mut self, state: &mut State) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));
                        break 'write_many;
                    },
                    Ok(n) => {
                        current.advance(n);
                        if current.finished() {
                            state.goto_next();
                            break 'write_one;
                        }
                    },
                    Err(err) => {
                        state.set_current(Some(current));
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    },
                }
            }
        }

        Ok(())
    }

    pub fn spawn(mut self) -> JoinHandle<(Self, State)> {
        thread::spawn_named("PTY reader", move || {
            let mut state = State::default();
            let mut buf = [0u8; READ_BUFFER_SIZE];

            let poll_opts = PollMode::Level;
            let mut interest = PollingEvent::readable(0);

            // Register TTY through EventedRW interface.
            if let Err(err) = unsafe { self.pty.register(&self.poll, interest, poll_opts) } {
                error!("Event loop registration error: {err}");
                return (self, state);
            }

            let mut events = Events::with_capacity(NonZeroUsize::new(1024).unwrap());

            let mut pipe = if self.ref_test {
                Some(File::create("./nebula.recording").expect("create nebula recording"))
            } else {
                None
            };

            // Reason the transport died without a child exit, reported after
            // the loop: without it the app never learns the session is gone
            // and the tab turns into a zombie (unresponsive input, no notice).
            let mut failure: Option<String> = None;

            'event_loop: loop {
                // Wakeup the event loop when a synchronized update timeout or
                // the pending ConPTY align deadline was reached.
                let deadline = match (state.stream.next_sync_timeout(), state.align_at) {
                    (Some(sync), Some(align)) => Some(sync.min(align)),
                    (sync, align) => sync.or(align),
                };
                let timeout = deadline.map(|at| at.saturating_duration_since(Instant::now()));

                events.clear();
                if let Err(err) = self.poll.wait(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            error!("Event loop polling error: {err}");
                            failure = Some(format!("event loop polling error: {err}"));
                            break 'event_loop;
                        },
                    }
                }

                // ConPTY 光标对账到点：向 conhost 要真值并把本地网格滚到同
                // 一坐标系（机理见 `Term::conpty_realign`）。
                if state.align_at.is_some_and(|at| Instant::now() >= at) {
                    state.align_at = None;
                    if let Some(target) = self.pty.child_pid().and_then(conpty_cursor_viewport_row) {
                        let mut terminal = self.terminal.lock();
                        trace_terminal_state("before-align", &terminal);
                        terminal.conpty_realign(target);
                        trace_terminal_state("after-align", &terminal);
                        drop(terminal);
                        self.event_proxy.send_event(Event::Wakeup);
                    }
                }

                // Handle synchronized update timeout. The align deadline can
                // wake the loop early with empty events — only a genuinely
                // expired sync window may force-stop the synchronized update.
                if events.is_empty() && self.rx.peek().is_none() {
                    if state.stream.next_sync_timeout().is_some_and(|st| Instant::now() >= st) {
                        state.stream.stop_sync(&mut *self.terminal.lock());
                        self.event_proxy.send_event(Event::Wakeup);
                    }
                    continue;
                }

                // Handle channel events, if there are any.
                let pending_resize = match self.drain_recv_channel(&mut state) {
                    Ok(resize) => resize,
                    Err(()) => break,
                };

                if let Some(window_size) = pending_resize {
                    // A resize is a stream boundary, not a UI-side property.
                    // Drain everything already readable while the grid still
                    // has the old geometry; otherwise old-width absolute CUP
                    // output can be parsed into a new-width grid. This is the
                    // Drain readable bytes against the old grid before the
                    // size change lands, so CUP sequences keep their geometry.
                    loop {
                        match self.pty_read(&mut state, &mut buf, pipe.as_mut()) {
                            Ok(processed) if processed >= MAX_LOCKED_READ => continue,
                            Ok(_) => break,
                            Err(err) => {
                                error!("PTY read before resize failed: {err}");
                                failure = Some(format!("PTY read before resize failed: {err}"));
                                break 'event_loop;
                            },
                        }
                    }

                    // Match ConPTY's order: reflow the client model
                    // first, then ask ConPTY to resize. ResizePseudoConsole is
                    // synchronous; repaint bytes it produces are consumed by
                    // the normal readable-event path below against this grid.
                    {
                        let mut terminal = self.terminal.lock();
                        trace_terminal_state("before-resize", &terminal);
                        terminal.resize(window_size);
                        trace_terminal_state("after-resize", &terminal);
                    }
                    self.pty.on_resize(window_size);
                    state.stream.resize(window_size);
                    // resize 尘埃落定后做一次 ConPTY 光标对账；新 resize 顺
                    // 延死线，风暴天然合并成一次探针。
                    state.align_at = Some(Instant::now() + ALIGN_DELAY);
                    self.event_proxy.send_event(Event::Wakeup);
                }

                for event in events.iter() {
                    match event.key {
                        tty::PTY_CHILD_EVENT_TOKEN => {
                            if let Some(tty::ChildEvent::Exited(status)) =
                                self.pty.next_child_event()
                            {
                                if let Some(status) = status {
                                    self.event_proxy.send_event(Event::ChildExit(status));
                                }
                                if self.drain_on_exit {
                                    let _ = self.pty_read(&mut state, &mut buf, pipe.as_mut());
                                }
                                self.terminal.lock().exit();
                                self.event_proxy.send_event(Event::Wakeup);
                                break 'event_loop;
                            }
                        },

                        tty::PTY_READ_WRITE_TOKEN => {
                            if event.is_interrupt() {
                                // Don't try to do I/O on a dead PTY.
                                continue;
                            }

                            if event.readable {
                                if let Err(err) = self.pty_read(&mut state, &mut buf, pipe.as_mut())
                                {
                                    // On Linux, a `read` on the master side of a PTY can fail
                                    // with `EIO` if the client side hangs up.  In that case,
                                    // just loop back round for the inevitable `Exited` event.
                                    // This sucks, but checking the process is either racy or
                                    // blocking.
                                    #[cfg(target_os = "linux")]
                                    if err.raw_os_error() == Some(libc::EIO) {
                                        continue;
                                    }

                                    error!("Error reading from PTY in event loop: {err}");
                                    failure = Some(format!("PTY read failed: {err}"));
                                    break 'event_loop;
                                }
                            }

                            if event.writable {
                                if let Err(err) = self.pty_write(&mut state) {
                                    error!("Error writing to PTY in event loop: {err}");
                                    failure = Some(format!("PTY write failed: {err}"));
                                    break 'event_loop;
                                }
                            }
                        },
                        _ => (),
                    }
                }

                // Register write interest if necessary.
                let needs_write = state.needs_write();
                if needs_write != interest.writable {
                    interest.writable = needs_write;

                    // Re-register with new interest.
                    self.pty.reregister(&self.poll, interest, poll_opts).unwrap();
                }
            }

            // Announce a transport death exactly like a child exit so the
            // app's existing teardown runs (`exit()` triggers `Event::Exit`).
            // The child-exit and shutdown paths leave `failure` unset.
            if let Some(reason) = failure {
                self.event_proxy.send_event(Event::PtyFailure(reason));
                self.terminal.lock().exit();
                self.event_proxy.send_event(Event::Wakeup);
            }

            // The evented instances are not dropped here so deregister them explicitly.
            let _ = self.pty.deregister(&self.poll);

            (self, state)
        })
    }
}

/// Helper type which tracks how much of a buffer has been written.
struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

pub struct Notifier(pub EventLoopSender);

impl event::Notify for Notifier {
    fn notify<B>(&self, bytes: B)
    where
        B: Into<Cow<'static, [u8]>>,
    {
        let bytes = bytes.into();
        // Terminal hangs if we send 0 bytes through.
        if bytes.is_empty() {
            return;
        }

        let _ = self.0.send(Msg::Input(bytes));
    }
}

impl event::OnResize for Notifier {
    fn on_resize(&mut self, window_size: WindowSize) {
        let _ = self.0.send(Msg::Resize(window_size));
    }
}

#[derive(Debug)]
pub enum EventLoopSendError {
    /// Error polling the event loop.
    Io(io::Error),

    /// Error sending a message to the event loop.
    Send(mpsc::SendError<Msg>),
}

impl Display for EventLoopSendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            EventLoopSendError::Io(err) => err.fmt(f),
            EventLoopSendError::Send(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for EventLoopSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EventLoopSendError::Io(err) => err.source(),
            EventLoopSendError::Send(err) => err.source(),
        }
    }
}

#[derive(Clone)]
pub struct EventLoopSender {
    sender: Sender<Msg>,
    poller: Arc<Poller>,
}

impl EventLoopSender {
    /// 为非 PTY 传输创建消息通道，继续复用应用既有的输入、缩放和关闭协议。
    pub fn standalone() -> io::Result<(Self, Receiver<Msg>)> {
        let (sender, receiver) = mpsc::channel();
        Ok((Self { sender, poller: Arc::new(Poller::new()?) }, receiver))
    }

    pub fn send(&self, msg: Msg) -> Result<(), EventLoopSendError> {
        self.sender.send(msg).map_err(EventLoopSendError::Send)?;
        self.poller.notify().map_err(EventLoopSendError::Io)
    }

    /// A sender wired to nothing: the receiving side is dropped on the spot,
    /// so every `send` errors out and is discarded by the caller's `let _ =`.
    /// For panes with no PTY behind them (e.g. document-viewer tabs), whose
    /// input must be swallowed instead of reaching some other pane's shell.
    pub fn sink() -> Self {
        let (sender, _) = mpsc::channel();
        Self { sender, poller: Arc::new(Poller::new().expect("create sink poller")) }
    }
}

/// All of the mutable state needed to run the event loop.
///
/// Contains list of items to write, current write state, etc. Anything that
/// would otherwise be mutated on the `EventLoop` goes here.
#[derive(Default)]
pub struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    stream: StreamProcessor,
    /// ConPTY 光标对账的死线：resize 提交后 `ALIGN_DELAY` 触发，见
    /// `conpty_cursor_viewport_row`。
    align_at: Option<Instant>,
}

impl State {
    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing { source: c, written: 0 }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

struct PeekableReceiver<T> {
    rx: Receiver<T>,
    peeked: Option<T>,
}

impl<T> PeekableReceiver<T> {
    fn new(rx: Receiver<T>) -> Self {
        Self { rx, peeked: None }
    }

    fn peek(&mut self) -> Option<&T> {
        if self.peeked.is_none() {
            self.peeked = self.rx.try_recv().ok();
        }

        self.peeked.as_ref()
    }

    fn recv(&mut self) -> Option<T> {
        if self.peeked.is_some() {
            self.peeked.take()
        } else {
            match self.rx.try_recv() {
                Err(TryRecvError::Disconnected) => panic!("event loop channel closed"),
                res => res.ok(),
            }
        }
    }
}
