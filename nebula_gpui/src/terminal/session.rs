//! PTY 会话接线：`Term` + `EventLoop` + ConPTY，全部来自 `nebula_terminal`。
//!
//! 与 `nebula_app::window_context::create_pane` 相同的模式，只是事件出口换成
//! futures channel，让 GPUI 前台任务可以 `await` 事件。

use std::sync::Arc;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nebula_terminal::Term;
use nebula_terminal::event::{Event, EventListener, WindowSize};
use nebula_terminal::event_loop::{EventLoop, Notifier};
use nebula_terminal::grid::Dimensions;
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::Config;
use nebula_terminal::tty;

/// 把 PTY 线程发出的终端事件转投到 GPUI 前台的异步通道。
#[derive(Clone)]
pub struct EventProxy(UnboundedSender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
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
}

/// 启动一个本地 shell 会话。尺寸随后由首帧 prepaint 按真实布局重设。
pub fn spawn(
    window_size: WindowSize,
) -> std::io::Result<(TerminalSession, UnboundedReceiver<Event>)> {
    let (tx, rx) = unbounded();
    let proxy = EventProxy(tx);

    let grid = GridSize {
        columns: window_size.num_cols as usize,
        screen_lines: window_size.num_lines as usize,
    };
    let term = Arc::new(FairMutex::new(Term::new(Config::default(), &grid, proxy.clone())));

    let options = tty::Options::default();
    tty::setup_env();
    let pty = tty::new(&options, window_size, 0)?;

    let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, options.drain_on_exit, false)?;
    let notifier = Notifier(event_loop.channel());
    let _io_thread = event_loop.spawn();

    Ok((TerminalSession { term, notifier }, rx))
}
