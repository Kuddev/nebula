//! 并发传输引擎：把一个文件的字节搬过网络，尽量填满带宽时延积。
//!
//! # 为什么不能直接用流式读写
//!
//! 上游 `SftpSession::open` 给出的文件句柄实现了 `AsyncRead`/`AsyncWrite`，
//! 但两个方向的在途能力天差地别：
//!
//! - **写**方向内部维护一个应答队列，同时可以有 `max_concurrent_writes` 个
//!   WRITE 在途。配好参数后 `write_all` 自然就是流水线的，不需要我们做事。
//! - **读**方向每个句柄只持有*一个*在途 READ：下一个请求要等上一个的数据
//!   回来才发得出去。所以无论怎么调参数，单句柄顺序读的吞吐上限都是
//!   `分块 / 往返时延`——跨洲链路上这个数字只有几 MB/s，和带宽无关。
//!
//! 拿不到底层请求队列（上游把它藏在私有字段里），所以下载的并发靠**同一个
//! 远端文件开多个句柄、每个句柄负责一段连续区间**来实现。每个句柄内部依旧
//! 是顺序读，但 N 个句柄意味着 N 个 READ 同时在途，效果与请求级流水线等价，
//! 而且不依赖上游的任何内部细节。
//!
//! # 跨平台
//!
//! 分段方案的另一个好处是本地落盘不需要按偏移写：每个工作者把自己的本地
//! 文件句柄 seek 到段起点后顺序写，用的是各平台都有的 `AsyncSeek` +
//! `AsyncWrite`。整个引擎里没有一处平台分支。

use std::future::{Future, poll_fn};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::limits::{MIN_SEGMENTED_DOWNLOAD, TRANSFER_CHUNK, plan_segments};

type TransferError = Box<dyn std::error::Error + Send + Sync>;
type TransferResult<T> = Result<T, TransferError>;

/// 传输过程中引擎需要向外汇报和向外询问的东西。
///
/// 抽成 trait 而不是直接依赖某个 UI 类型：引擎跑在网络 runtime 上，它不该
/// 知道自己的进度最终画在哪儿，也不该知道"取消"这个意图是从哪个按钮来的。
pub(crate) trait TransferObserver: Send + Sync {
    /// 已确认落地的字节数增量。
    fn advance(&self, bytes: u64);

    /// 是否应该立刻停下。引擎在每个分块边界询问，所以取消的响应粒度是一块。
    fn cancelled(&self) -> bool;
}

/// 每个分块边界检查一次取消。
fn guard(observer: &dyn TransferObserver) -> TransferResult<()> {
    if observer.cancelled() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "操作已取消").into())
    } else {
        Ok(())
    }
}

/// 并发驱动一批 future 直到全部结束，按原顺序收集各自结果。
///
/// 为什么手写而不是引入 future 组合库：这个模块在所有构建配置下都要编译，
/// 而组合库在本包里是可选依赖。这里需要的语义也只有一条——"全都发出去，
/// 全都等到底，每个的结果都要"，而且必须是**全都等到底**：某一段失败时，
/// 其他段已经确认落盘的字节数是算续传偏移的依据，提前返回就把它丢了。
///
/// 唤醒是保守的（任一子任务就绪会让所有未完成的子任务各被 poll 一次）。
/// 段数是并发窗口量级的几十个，每次多余的 poll 只是一次状态检查，和一次
/// 网络往返比可以忽略。
async fn join_all<F: Future>(tasks: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<Option<Pin<Box<F>>>> =
        tasks.into_iter().map(|task| Some(Box::pin(task))).collect();
    let mut done: Vec<Option<F::Output>> = pending.iter().map(|_| None).collect();
    poll_fn(move |cx| {
        let mut all_ready = true;
        for (slot, result) in pending.iter_mut().zip(done.iter_mut()) {
            let Some(task) = slot.as_mut() else { continue };
            match task.as_mut().poll(cx) {
                Poll::Ready(value) => {
                    *result = Some(value);
                    *slot = None;
                },
                Poll::Pending => all_ready = false,
            }
        }
        if !all_ready {
            return Poll::Pending;
        }
        Poll::Ready(
            done.iter_mut()
                .map(|slot| slot.take().expect("all_ready 为真时每个槽位都已落值"))
                .collect(),
        )
    })
    .await
}

/// 以固定并发度跑完一批任务；任一任务失败即整体失败。
///
/// 实现是 N 个工作者从一个共享游标上领活儿，而不是把输入切成 N 份——切份法
/// 会让一个慢文件把它那份后面的活儿全堵住，而领活儿法里其他工作者会继续
/// 消化队列。
///
/// 有任务失败后，其余工作者在**领下一件活儿之前**就会停手：继续传注定要被
/// 丢弃的文件既浪费带宽，也会让错误信息滞后于用户看到的进度。已经在传的
/// 那件不会被中途掐断（取消是观察者的职责，粒度在分块边界）。
///
/// 闭包收到的引用寿命绑在 `items` 上而不是每次调用上：任务 future 要把这个
/// 引用带进 `await`，如果寿命只到"这一次调用返回"，future 里的借用就活不过
/// 创建它的那一刻。
pub(crate) async fn run_bounded<'a, T, Fut>(
    items: &'a [T],
    concurrency: usize,
    task: impl Fn(&'a T) -> Fut,
) -> TransferResult<()>
where
    Fut: Future<Output = TransferResult<()>>,
{
    if items.is_empty() {
        return Ok(());
    }
    let cursor = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let workers = concurrency.max(1).min(items.len());
    let outcomes = join_all(
        (0..workers)
            .map(|_| async {
                loop {
                    if failed.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let index = cursor.fetch_add(1, Ordering::AcqRel);
                    let Some(item) = items.get(index) else { return Ok(()) };
                    if let Err(error) = task(item).await {
                        failed.store(true, Ordering::Release);
                        return Err(error);
                    }
                }
            })
            .collect(),
    )
    .await;
    // 只报第一个错误。多个工作者同时失败时，后面的通常是同一个根因
    // （断线、权限）的回声，全都摊给用户反而看不出发生了什么。
    outcomes.into_iter().find_map(Result::err).map_or(Ok(()), Err)
}

/// [`run_bounded`] 的收集版：每个任务产出一个值，按输入顺序还给调用方。
///
/// 顺序必须是输入顺序而不是完成顺序：调用方拿它和输入 `zip` 起来配对，
/// 乱序会把 A 的结果挂到 B 头上。任一任务失败即整体失败。
pub(crate) async fn map_bounded<'a, T, R, Fut>(
    items: &'a [T],
    concurrency: usize,
    task: impl Fn(&'a T) -> Fut,
) -> TransferResult<Vec<R>>
where
    Fut: Future<Output = TransferResult<R>>,
{
    if items.is_empty() {
        return Ok(Vec::new());
    }
    // 批量很小（并发度量级），逐个 poll 的开销可以忽略，所以直接一次性发出去
    // 而不再套一层领活儿队列。真正需要限流的是"同时有几个请求在网络上"，
    // 这里的批已经由调用方按并发度切好了。
    let mut collected = Vec::with_capacity(items.len());
    for batch in items.chunks(concurrency.max(1)) {
        let outcomes = join_all(batch.iter().map(|item| task(item)).collect()).await;
        for outcome in outcomes {
            collected.push(outcome?);
        }
    }
    Ok(collected)
}

/// 一段连续区间的完成情况，用于拼出"最高连续完成偏移"。
///
/// 并发分段下各段完成的先后顺序是乱的。**累计字节数不能当断点续传的偏移
/// 量**：传了 5 MiB 不代表前 5 MiB 都在盘上，可能是第 1 段和第 10 段各传了
/// 一半。所以每段单独记自己确认到哪儿，续传偏移只取从文件头开始不间断的
/// 那一截。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentProgress {
    offset: u64,
    length: u64,
    done: u64,
}

/// 从各段进度算出可以安全续传的偏移。
///
/// 判据：从零开始沿段边界往前走，只要某段没有完整完成就停在那里。返回值
/// 之前的每一个字节都已确认写入本地。
fn contiguous_offset(segments: &[SegmentProgress]) -> u64 {
    let mut reached = 0u64;
    for segment in segments {
        if segment.offset != reached {
            break;
        }
        reached += segment.done;
        if segment.done != segment.length {
            break;
        }
    }
    reached
}

/// 一次分段下载的结果。
pub(crate) struct DownloadOutcome {
    /// 本次实际写入本地的字节数。
    pub(crate) written: u64,
    /// 从文件头算起已确认连续落盘的字节数。全部成功时等于文件长度；
    /// 失败时这是可以安全续传的偏移。
    pub(crate) contiguous: u64,
}

/// 上传一个本地文件到远端路径。
///
/// 写方向的并发由会话参数（`max_concurrent_writes`）承担，这里只要把分块
/// 喂给它并在块边界检查取消。缓冲区固定一个分块：更大的缓冲不会提高在途
/// 请求数，只会让取消的响应变钝。
pub(crate) async fn upload_stream(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    observer: &dyn TransferObserver,
) -> TransferResult<()> {
    let mut source = tokio::fs::File::open(local).await?;
    let mut target = sftp
        .open_with_flags(
            remote.to_owned(),
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await?;
    let mut buffer = vec![0u8; TRANSFER_CHUNK];
    loop {
        guard(observer)?;
        let count = source.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        target.write_all(&buffer[..count]).await?;
        observer.advance(count as u64);
    }
    // `shutdown` 先把在途的 WRITE 应答排空再关句柄。少了这一步，最后一个
    // 窗口的写可能还没被服务端确认，句柄就关了。
    target.shutdown().await?;
    Ok(())
}

/// 下载一个远端文件到本地路径，按需分段并发。
///
/// `total` 是远端报告的文件长度，只用来切分段计划：真正写入多少字节由每段
/// 实际读到的数据决定，所以远端长度不准（稀疏文件、传输中被追写）时结果
/// 仍然是"我们确实读到的那些字节"，不会凭长度补零。
///
/// `window` 是这个文件可用的在途请求数。由调用方给而不是在这里取常量：
/// 单文件传输独占整个窗口，目录传输要把窗口摊给并发的多个文件，判据在
/// [`super::limits::window_per_file`]。
pub(crate) async fn download_segmented(
    sftp: &SftpSession,
    remote: &str,
    total: u64,
    local: &Path,
    window: usize,
    observer: &dyn TransferObserver,
) -> TransferResult<DownloadOutcome> {
    // 先把本地文件建出来，段工作者才能各自打开它并 seek 到自己的起点。
    // 零字节文件到这里就结束——建出空文件即是正确结果。
    let file = tokio::fs::File::create(local).await?;
    if total == 0 {
        file.sync_all().await?;
        return Ok(DownloadOutcome { written: 0, contiguous: 0 });
    }
    drop(file);

    let window = if total < MIN_SEGMENTED_DOWNLOAD { 1 } else { window.max(1) };
    let plan = plan_segments(total, window);

    // 顺序路径单独走：只有一段时开并发脚手架纯属浪费，而绝大多数文件
    // （配置、脚本、源码）都落在这一支。
    if plan.len() <= 1 {
        let written = download_range(sftp, remote, 0, total, local, observer).await?;
        return Ok(DownloadOutcome { written, contiguous: written });
    }

    let tasks: Vec<_> = plan
        .iter()
        .map(|&(offset, length)| async move {
            download_range(sftp, remote, offset, length, local, observer).await
        })
        .collect();
    let results = join_all(tasks).await;

    let mut segments = Vec::with_capacity(results.len());
    let mut written = 0u64;
    let mut failure = None;
    for (&(offset, length), result) in plan.iter().zip(results) {
        match result {
            Ok(done) => {
                written += done;
                segments.push(SegmentProgress { offset, length, done });
            },
            Err(error) => {
                // 失败的段贡献 0 字节：它写了多少无从确认，按最保守的方式
                // 记账，续传偏移就不会落在一段没写全的数据后面。
                segments.push(SegmentProgress { offset, length, done: 0 });
                failure = failure.or(Some(error));
            },
        }
    }

    let contiguous = contiguous_offset(&segments);
    match failure {
        Some(error) => Err(error),
        None => Ok(DownloadOutcome { written, contiguous }),
    }
}

/// 下载 `[offset, offset + length)` 这一段到本地同偏移处。
///
/// 远端句柄和本地句柄都是这一段私有的，所以段之间没有共享可变状态，
/// 不需要任何锁。
async fn download_range(
    sftp: &SftpSession,
    remote: &str,
    offset: u64,
    length: u64,
    local: &Path,
    observer: &dyn TransferObserver,
) -> TransferResult<u64> {
    let mut source = sftp.open(remote.to_owned()).await?;
    let mut target = tokio::fs::OpenOptions::new().write(true).open(local).await?;
    if offset > 0 {
        source.seek(io::SeekFrom::Start(offset)).await?;
        target.seek(io::SeekFrom::Start(offset)).await?;
    }

    let mut buffer = vec![0u8; TRANSFER_CHUNK];
    let mut remaining = length;
    let mut written = 0u64;
    while remaining > 0 {
        guard(observer)?;
        // 只读到段边界为止：段末尾那次读必须缩短，否则会读进下一段的地盘，
        // 两个工作者写同一片区域。
        let want = usize::try_from(remaining).unwrap_or(TRANSFER_CHUNK).min(TRANSFER_CHUNK);
        let count = source.read(&mut buffer[..want]).await?;
        if count == 0 {
            // 远端报的长度比实际内容长（稀疏或被截短）。不是错误，这一段
            // 就到这里。
            break;
        }
        target.write_all(&buffer[..count]).await?;
        observer.advance(count as u64);
        written += count as u64;
        remaining -= count as u64;
    }
    target.flush().await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(offset: u64, length: u64, done: u64) -> SegmentProgress {
        SegmentProgress { offset, length, done }
    }

    #[test]
    fn contiguous_offset_stops_at_the_first_incomplete_segment() {
        // 第 1 段满、第 2 段半满：能续传的只到第 2 段已完成的位置。
        let segments = [progress(0, 100, 100), progress(100, 100, 40), progress(200, 100, 100)];
        assert_eq!(contiguous_offset(&segments), 140);
    }

    #[test]
    fn a_later_finished_segment_never_inflates_the_resume_offset() {
        // 这是"累计字节数当断点"会写坏文件的场景：总共传了 200 字节，但前
        // 100 字节是空的，从 200 续传会在文件里留下一个空洞。
        let segments = [progress(0, 100, 0), progress(100, 100, 100), progress(200, 100, 100)];
        assert_eq!(contiguous_offset(&segments), 0);
    }

    #[test]
    fn all_segments_complete_yields_the_whole_file() {
        let segments = [progress(0, 100, 100), progress(100, 100, 100)];
        assert_eq!(contiguous_offset(&segments), 200);
    }

    #[test]
    fn a_gap_in_the_segment_plan_is_not_crossed() {
        // 段计划本该连续；万一不连续（计划有 bug 或被人为拼接），也绝不能
        // 跨过空洞把后面的进度算进续传偏移。
        let segments = [progress(0, 100, 100), progress(150, 100, 100)];
        assert_eq!(contiguous_offset(&segments), 100);
    }

    #[test]
    fn no_progress_at_all_resumes_from_the_beginning() {
        assert_eq!(contiguous_offset(&[]), 0);
        assert_eq!(contiguous_offset(&[progress(0, 100, 0)]), 0);
    }

    #[test]
    fn join_all_keeps_input_order_and_waits_for_every_task() {
        // 顺序必须按输入而不是完成先后：段进度是按 plan 的下标对齐的，
        // 乱序会把 A 段的字节数记到 B 段头上。
        let outputs =
            block_on(join_all(vec![yield_then(3, 1), yield_then(0, 2), yield_then(1, 3)]));
        assert_eq!(outputs, vec![1, 2, 3]);
    }

    #[test]
    fn run_bounded_visits_every_item_exactly_once() {
        let items: Vec<u32> = (0..50).collect();
        let seen = std::sync::Mutex::new(Vec::new());
        let result = block_on(run_bounded(&items, 4, |item| {
            let seen = &seen;
            async move {
                tokio::task::yield_now().await;
                seen.lock().expect("锁未中毒").push(*item);
                Ok(())
            }
        }));
        assert!(result.is_ok());
        let mut visited = seen.into_inner().expect("锁未中毒");
        visited.sort_unstable();
        assert_eq!(visited, items);
    }

    #[test]
    fn run_bounded_stops_handing_out_work_after_a_failure() {
        // 失败后其余工作者不该继续领活儿：注定要丢弃的传输白占带宽，
        // 而且会让错误信息滞后于用户看到的进度。
        let items: Vec<u32> = (0..200).collect();
        let started = AtomicUsize::new(0);
        let result = block_on(run_bounded(&items, 4, |item| {
            let started = &started;
            async move {
                started.fetch_add(1, Ordering::AcqRel);
                tokio::task::yield_now().await;
                if *item == 0 {
                    return Err(io::Error::other("第一件就失败").into());
                }
                Ok(())
            }
        }));
        assert!(result.is_err());
        // 并发度是 4，所以失败被观测到之前最多有 4 件在手上。放宽到 8 以容纳
        // 调度抖动，但绝不该接近 200。
        assert!(started.load(Ordering::Acquire) <= 8, "失败后仍在领新活儿");
    }

    #[test]
    fn run_bounded_on_an_empty_batch_is_a_no_op() {
        let items: Vec<u32> = Vec::new();
        let result = block_on(run_bounded(&items, 4, |_| async { Ok(()) }));
        assert!(result.is_ok());
    }

    /// 让出 `rounds` 次再返回 `value` 的 future，用来制造乱序完成。
    async fn yield_then(rounds: usize, value: u32) -> u32 {
        for _ in 0..rounds {
            tokio::task::yield_now().await;
        }
        value
    }

    /// 单线程跑完一个 future，不为几个顺序断言拉起多线程 runtime。
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("构建单线程 runtime")
            .block_on(future)
    }
}
