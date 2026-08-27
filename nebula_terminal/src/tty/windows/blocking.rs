//! Code for running a reader/writer on another thread while driving it through `polling`.

use std::io::prelude::*;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::{io, thread};

use piper::{Reader, Writer, pipe};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, PollMode, Poller};

use crate::thread::spawn_named;

struct Registration {
    interest: Mutex<Option<Interest>>,
    end: PipeEnd,
}

#[derive(Copy, Clone)]
enum PipeEnd {
    Reader,
    Writer,
}

struct Interest {
    /// The event to send about completion.
    event: Event,

    /// The poller to send the event to.
    poller: Arc<Poller>,

    /// The mode that we are in.
    mode: PollMode,
}

/// Poll a reader in another thread.
pub struct UnblockedReader<R> {
    /// The event to send about completion.
    interest: Arc<Registration>,

    /// The pipe that we are reading from.
    pipe: Reader,

    /// Is this the first time registering?
    first_register: bool,

    /// We logically own the reader, but we don't actually use it.
    _reader: PhantomData<R>,
}

impl<R: Read + Send + 'static> UnblockedReader<R> {
    /// Spawn a new unblocked reader.
    pub fn new(mut source: R, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (reader, mut writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Reader,
        });

        // Spawn the reader thread.
        spawn_named("nebula-tty-reader-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                // Read from the reader into the pipe.
                match writer.poll_fill(&mut context, &mut source) {
                    Poll::Ready(Ok(0)) => {
                        // Either the pipe is closed or the reader is at its EOF.
                        // In any case, we are done.
                        return;
                    },

                    Poll::Ready(Ok(_)) => {
                        // Keep reading.
                        continue;
                    },

                    Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                        // We were interrupted; continue.
                        continue;
                    },

                    Poll::Ready(Err(e)) => {
                        log::error!("error writing to pipe: {}", e);
                        return;
                    },

                    Poll::Pending => {
                        // We are now waiting on the other end to advance. Park the
                        // thread until they do.
                        thread::park();
                    },
                }
            }
        });

        Self { interest, pipe: reader, first_register: true, _reader: PhantomData }
    }

    /// Register interest in the reader.
    pub fn register(&mut self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = Some(Interest { event, poller: poller.clone(), mode });

        // Send the event to start off with if we have any data.
        if (!self.pipe.is_empty() && event.readable) || self.first_register {
            self.first_register = false;
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    /// Deregister interest in the reader.
    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }

    /// Try to read from the reader.
    pub fn try_read(&mut self, buf: &mut [u8]) -> usize {
        let waker = Waker::from(self.interest.clone());

        let read = match self.pipe.poll_drain_bytes(&mut Context::from_waker(&waker), buf) {
            Poll::Pending => 0,
            Poll::Ready(n) => n,
        };

        // piper 只在「管道为空」的那次读取里注册 read waker，一旦读到数据就
        // 立刻把它 take 掉，而写入侧的 `wake()` 对空 waker 是 no-op。于是调用
        // 方在管道仍有数据时停止读取（`pty_read` 到 `MAX_LOCKED_READ` 就会
        // 返回）时，会同时失去 waker 和待投递的 readable 事件 —— 对端随后一
        // 静默（AI CLI 答完就不再输出），这批字节便永久留在管道里：画面停在
        // 一个画到一半的残帧，滚动条却仍在底部，只有按键或 resize 能救回来
        // （前者触发 reregister 补投，后者走 resize 前的强制排空循环）。
        //
        // `PollMode::Level` 承诺的是「只要还有数据就保持可读」，这里补上
        // piper 不会替我们做的那次投递。
        //
        // 判据只能是「这次读到过数据」，不能再加「而且管道当下仍有货」：
        // `drain_inner` 是在**拷贝之前**就 `reader.take()` 摘掉 waker 的，因此
        // 「读到过数据」本身即意味着此刻管道处于无 waker 状态，与它当下空不空
        // 无关。加上 `!is_empty()` 就成了 TOCTOU——只堵住「摘 waker 的瞬间还有
        // 货」，漏掉「摘完之后才到的货」，而 AI CLI 的收尾字节（输入框、状态栏
        // 那几百字节）恰好总落在这个窗口里：先打完一屏 diff 把 `pty_read` 顶到
        // `MAX_LOCKED_READ` 提前 break、管道刚好排空，随后那批收尾字节的
        // `wake()` 就打在空 waker 上，于是屏幕上缺的正是输入框。
        //
        // 多投的代价是 O(1) 且自收敛：event_loop 因此多读一次，读到 0 时
        // `drain_inner` 会在开头重新注册 waker，状态回到「有 waker 等写入」。
        if read > 0 {
            waker.wake_by_ref();
        }

        read
    }

    /// Hand the receiving end to a detached thread that keeps consuming until
    /// the source hits EOF. Called on teardown, right before the pseudoconsole
    /// is closed: `ClosePseudoConsole` blocks until the console host has
    /// flushed its remaining output, and once the terminal stops draining, a
    /// full pipe parks the reader thread forever — the close call deadlocks
    /// and the process lingers after its window is gone.
    pub fn drain_detached(&mut self) {
        let (dead_reader, _dead_writer) = pipe(1);
        let mut real = std::mem::replace(&mut self.pipe, dead_reader);
        spawn_named("nebula-tty-drain-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);
            let mut sink = [0u8; 8192];
            loop {
                match real.poll_drain_bytes(&mut context, &mut sink) {
                    // Writer closed: the reader thread saw EOF — host is gone.
                    Poll::Ready(0) => return,
                    Poll::Ready(_) => continue,
                    Poll::Pending => thread::park(),
                }
            }
        });
    }
}

impl<R: Read + Send + 'static> Read for UnblockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.try_read(buf))
    }
}

/// Poll a writer in another thread.
pub struct UnblockedWriter<W> {
    /// The interest to send about completion.
    interest: Arc<Registration>,

    /// The pipe that we are writing to.
    pipe: Writer,

    /// We logically own the writer, but we don't actually use it.
    _reader: PhantomData<W>,
}

impl<W: Write + Send + 'static> UnblockedWriter<W> {
    /// Spawn a new unblocked writer.
    pub fn new(mut sink: W, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (mut reader, writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Writer,
        });

        // Spawn the writer thread.
        spawn_named("nebula-tty-writer-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                // Write from the pipe into the writer.
                match reader.poll_drain(&mut context, &mut sink) {
                    Poll::Ready(Ok(0)) => {
                        // Either the pipe is closed or the writer is full.
                        // In any case, we are done.
                        return;
                    },

                    Poll::Ready(Ok(_)) => {
                        // Keep writing.
                        continue;
                    },

                    Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                        // We were interrupted; continue.
                        continue;
                    },

                    Poll::Ready(Err(e)) => {
                        log::error!("error writing to pipe: {}", e);
                        return;
                    },

                    Poll::Pending => {
                        // We are now waiting on the other end to advance. Park the
                        // thread until they do.
                        thread::park();
                    },
                }
            }
        });

        Self { interest, pipe: writer, _reader: PhantomData }
    }

    /// Register interest in the writer.
    pub fn register(&self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = Some(Interest { event, poller: poller.clone(), mode });

        // Send the event to start off with if we have room for data.
        if !self.pipe.is_full() && event.writable {
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    /// Deregister interest in the writer.
    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }

    /// Try to write to the writer.
    pub fn try_write(&mut self, buf: &[u8]) -> usize {
        let waker = Waker::from(self.interest.clone());

        match self.pipe.poll_fill_bytes(&mut Context::from_waker(&waker), buf) {
            Poll::Pending => 0,
            Poll::Ready(n) => n,
        }
    }
}

impl<W: Write + Send + 'static> Write for UnblockedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.try_write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        // Nothing to flush.
        Ok(())
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

impl Wake for Registration {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut interest_lock = self.interest.lock().unwrap();
        if let Some(interest) = interest_lock.as_ref() {
            // Send the event to the poller.
            let send_event = match self.end {
                PipeEnd::Reader => interest.event.readable,
                PipeEnd::Writer => interest.event.writable,
            };

            if send_event {
                interest.poller.post(CompletionPacket::new(interest.event)).ok();

                // Clear the event if we're in oneshot mode.
                if matches!(interest.mode, PollMode::Oneshot | PollMode::EdgeOneshot) {
                    *interest_lock = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use polling::Events;

    use super::*;

    /// 「给一批就静默」的源：`recv` 阻塞住读线程，精确复刻 AI CLI 打完一轮
    /// 就不再输出的时序。源不静默的话总有下一次写入替我们投递事件，缺唤醒
    /// 也就显不出来——静默是这个 bug 的必要条件。
    struct ScriptedSource {
        rx: mpsc::Receiver<Vec<u8>>,
        pending: Vec<u8>,
    }

    impl Read for ScriptedSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pending.is_empty() {
                match self.rx.recv() {
                    Ok(chunk) => self.pending = chunk,
                    // 发送端关闭 = EOF，读线程正常收尾。
                    Err(_) => return Ok(0),
                }
            }
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            Ok(n)
        }
    }

    fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        cond()
    }

    fn readable_arrived(poller: &Poller, key: usize) -> bool {
        let mut events = Events::new();
        poller.wait(&mut events, Some(Duration::from_secs(2))).unwrap();
        events.iter().any(|event| event.key == key && event.readable)
    }

    /// 把管道读空之后仍必须补投一次 readable。
    ///
    /// 回归防线：判据一旦退回 `read > 0 && !self.pipe.is_empty()`，这一步就再没有
    /// 事件可等——管道已空、waker 已被 `drain_inner` 在拷贝前摘掉，源再静默下去
    /// 便没有任何一方会投递，字节永久滞留（症状：画面停在半帧，缺 CLI 输入框，
    /// 只有按键或 resize 能救回来）。
    #[test]
    fn reposts_readable_after_draining_the_pipe() {
        const KEY: usize = 7;

        let (tx, rx) = mpsc::channel();
        let poller = Arc::new(Poller::new().unwrap());
        let event = Event::readable(KEY);
        let mut reader = UnblockedReader::new(ScriptedSource { rx, pending: Vec::new() }, 4096);

        // 首次 register 自带一次无条件投递；先消化掉，后面等到的事件就只可能
        // 来自 try_read 的补投。
        reader.register(&poller, event, PollMode::Level);
        assert!(readable_arrived(&poller, KEY), "首次 register 应当自投一次");

        tx.send(b"hello".to_vec()).unwrap();
        assert!(
            wait_until(Duration::from_secs(2), || !reader.pipe.is_empty()),
            "读线程没有把数据搬进管道"
        );

        let mut buf = [0u8; 64];
        assert_eq!(reader.try_read(&mut buf), 5);
        assert!(reader.pipe.is_empty(), "这一读须把管道读空，否则测不到 TOCTOU 的那一半");

        assert!(
            readable_arrived(&poller, KEY),
            "读空管道后没有补投 readable：源一静默，这批字节就永久滞留"
        );

        // 补投是自收敛的：再读一次返回 0，piper 借这次空读重新注册 waker，
        // 后续写入照样能唤醒——多投的那一次不会把唤醒链弄坏。
        assert_eq!(reader.try_read(&mut buf), 0);
        tx.send(b"world".to_vec()).unwrap();
        assert!(readable_arrived(&poller, KEY), "重新注册的 waker 失效，后续输出会卡住");
        assert_eq!(reader.try_read(&mut buf), 5);
    }
}
