//! 远端文件传输的协议参数：分块大小、在途请求窗口、请求超时。
//!
//! 这些数字决定吞吐，所以判据写在这里而不是散落在调用点。核心权衡只有一条：
//! **单请求越大越省往返，但越容易踩服务器实现的坑；在途请求越多越能填满
//! 带宽时延积，且不改变单请求大小。** 所以我们把分块钉死在保守值，靠加大
//! 窗口拿吞吐——这是成熟客户端的共同选择。
//!
//! 不要为了"看起来更快"调大 [`TRANSFER_CHUNK`]：超过 32 KiB 的 READ/WRITE
//! 在部分服务端实现上会被静默截断，产出的文件长度对得上而内容是错的，
//! 而且错误只在传大文件时出现。窗口翻倍是安全的，分块翻倍不是。

/// 单个 READ/WRITE 请求携带的数据长度。
///
/// 32 KiB 是 SFTP 生态的事实底线：协议本身不规定上限，服务端各自实现，
/// 而所有主流客户端都收敛到这个值。比它大的收益是线性的（少一半往返），
/// 代价是非线性的（个别服务端静默出错）。
pub(crate) const TRANSFER_CHUNK: usize = 32 * 1024;

/// `max_packet_len` 需要覆盖 [`TRANSFER_CHUNK`] 再加协议头。
///
/// 上游按 `max_packet_len - overhead` 反算单请求可携带的数据长度，WRITE 的
/// overhead 是 21 字节固定头加文件句柄长度，READ 是 9 字节。句柄长度由服务端
/// 决定（常见 4~8 字节），留 256 字节余量后即使句柄异常长也只是把一个请求
/// 拆成两个，不会出错。
const PACKET_HEADROOM: usize = 256;

/// 传给上游的单包上限。
pub(crate) const MAX_PACKET_LEN: u32 = (TRANSFER_CHUNK + PACKET_HEADROOM) as u32;

/// 上传方向允许同时在途的 WRITE 请求数。
///
/// 32 KiB × 64 ≈ 2 MiB 在途。带宽时延积的意义：跨洲链路 200ms 往返、
/// 100 Mbps 带宽下,填满管道需要约 2.5 MiB 在途——窗口不够时吞吐由往返
/// 次数而不是带宽决定，串行传输在这种链路上只能跑到理论值的百分之几。
pub(crate) const UPLOAD_WINDOW: usize = 64;

/// 下载方向的并发读窗口。
///
/// 与上传对称。上游的 `AsyncRead` 实现每个文件句柄只维持一个在途 READ，
/// 所以这个窗口靠"同一文件开多个句柄、每句柄负责一段连续区间"来达成，
/// 见 [`super::transfer`]。
pub(crate) const DOWNLOAD_WINDOW: usize = 64;

/// 单个请求的响应超时（秒）。
///
/// 上游默认 10 秒。慢链路上一次 32 KiB 往返超过 10 秒是可能的（尤其经跳板
/// 或移动网络），误判超时会把一次能完成的传输打断成失败重试。放宽到 30 秒
/// 仍能在服务端真的没响应时收敛，而不是无限等待。
pub(crate) const REQUEST_TIMEOUT_SECS: u64 = 30;

/// 值得启用分段并发下载的最小文件尺寸。
///
/// 小于这个值时分段的收益（少数次往返）抵不过成本（每段一次 OPEN 往返
/// 加一个本地文件句柄），所以小文件走单句柄顺序读。判据取两个分块：
/// 一个文件至少要能切成两段才谈得上并发。
pub(crate) const MIN_SEGMENTED_DOWNLOAD: u64 = (TRANSFER_CHUNK * 2) as u64;

/// 目录传输时同时处理的文件数。
///
/// 与单文件内的请求窗口是两个不同的东西，必须分开命名：这个数字控制
/// "同时有几个文件在传"，窗口控制"一个文件内有几个请求在途"。混为一谈会
/// 让并发度变成两者相乘，把服务端的 `MaxSessions` 和句柄表顶穿。
///
/// 取 4 而不是更大：目录传输里每个文件都要 OPEN/CLOSE，并发过高时服务端的
/// 句柄表和我们的分段窗口会互相挤占。
pub(crate) const DIRECTORY_FILE_CONCURRENCY: usize = 4;

/// 遍历远端目录树时并发发出的列目录请求数。
///
/// SFTP 没有递归 LIST，统计一棵宽目录的总大小只能一层层问。串行问的话，
/// 光是"算出总共多少字节"就要花掉和传输相当的时间，进度条要等很久才动。
/// 保持适度：列目录与文件 I/O 共用同一条控制通道。
pub(crate) const DIRECTORY_LISTING_CONCURRENCY: usize = 4;

/// 一个文件在目录传输里能用的分段窗口。
///
/// **不变量：在途请求总数不随并发文件数增长。** 单文件传输独占整个窗口；
/// 目录传输把窗口按并发文件数摊开，`文件并发 × 单文件窗口` 始终不超过
/// [`DOWNLOAD_WINDOW`]。
///
/// 为什么要守这条：两个并发度相乘会静默突破服务端的句柄和会话上限，而症状
/// 是"传大目录时偶尔失败"——最难查的那类问题。把总量钉死，加大文件并发就
/// 只会改变请求的分布，不会改变总压力。
pub(crate) fn window_per_file(concurrent_files: usize) -> usize {
    (DOWNLOAD_WINDOW / concurrent_files.max(1)).max(1)
}

/// 上游 SFTP 会话的构造参数。
pub(crate) fn session_config() -> russh_sftp::client::Config {
    russh_sftp::client::Config {
        max_packet_len: MAX_PACKET_LEN,
        max_concurrent_writes: UPLOAD_WINDOW,
        request_timeout_secs: REQUEST_TIMEOUT_SECS,
    }
}

/// 把文件按并发窗口切成连续区间。
///
/// 返回每段的 `(起始偏移, 长度)`。段数不超过 `window`，每段都是分块的整数倍
/// （最后一段除外），这样每个工作者内部的顺序读都对齐在请求边界上，不会出现
/// 跨段的半个请求。
///
/// `total == 0` 返回空：零字节文件不需要读，调用方只要建出空文件。
pub(crate) fn plan_segments(total: u64, window: usize) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    let window = window.max(1) as u64;
    let chunk = TRANSFER_CHUNK as u64;
    // 总块数向上取整；段数不超过块数，否则会排出长度为 0 的空段。
    let blocks = total.div_ceil(chunk);
    let segments = window.min(blocks);
    // 每段的块数向上取整，保证所有段加起来覆盖整个文件。这也意味着实际段数
    // 可能少于 `segments`（例如 3 块切 2 段时每段 2 块，第二段只剩 1 块）。
    let blocks_per_segment = blocks.div_ceil(segments);
    let span = blocks_per_segment * chunk;

    let mut plan = Vec::new();
    let mut offset = 0u64;
    while offset < total {
        let length = span.min(total - offset);
        plan.push((offset, length));
        offset += length;
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_fits_inside_one_packet_with_room_for_protocol_overhead() {
        // 单请求必须能完整装下一个分块，否则每块都被拆成两个请求，
        // 往返数翻倍而窗口利用率减半。
        const WRITE_OVERHEAD: u32 = 21;
        const MAX_HANDLE_LEN: u32 = 64;
        assert!(MAX_PACKET_LEN - WRITE_OVERHEAD - MAX_HANDLE_LEN >= TRANSFER_CHUNK as u32);
    }

    #[test]
    fn segments_cover_the_whole_file_without_gaps_or_overlap() {
        for total in [1u64, 4096, 32_768, 32_769, 1_048_576, 10_000_003] {
            let plan = plan_segments(total, DOWNLOAD_WINDOW);
            let mut expected_offset = 0u64;
            for (offset, length) in &plan {
                assert_eq!(*offset, expected_offset, "total={total} 段起点不连续");
                assert!(*length > 0, "total={total} 出现空段");
                expected_offset += length;
            }
            assert_eq!(expected_offset, total, "total={total} 段长之和不等于文件长度");
        }
    }

    #[test]
    fn segment_count_never_exceeds_the_window() {
        for total in [1u64, 100_000_000, u64::from(u32::MAX)] {
            assert!(plan_segments(total, DOWNLOAD_WINDOW).len() <= DOWNLOAD_WINDOW);
        }
    }

    #[test]
    fn every_segment_but_the_last_starts_on_a_request_boundary() {
        // 段起点必须是分块的整数倍：工作者内部按分块顺序读，段起点不对齐
        // 会让每段的第一个请求变成不足一块的短读。
        let plan = plan_segments(10_000_003, DOWNLOAD_WINDOW);
        for (offset, _) in &plan {
            assert_eq!(offset % TRANSFER_CHUNK as u64, 0);
        }
    }

    #[test]
    fn a_zero_length_file_needs_no_reads() {
        assert!(plan_segments(0, DOWNLOAD_WINDOW).is_empty());
    }

    #[test]
    fn small_files_stay_on_a_single_segment() {
        // 一个分块以内的文件切不出第二段，避免为了并发白开一次 OPEN。
        assert_eq!(plan_segments(TRANSFER_CHUNK as u64, DOWNLOAD_WINDOW).len(), 1);
    }

    #[test]
    fn total_requests_in_flight_never_grow_with_file_concurrency() {
        // 这是最关键的一条不变量：两个并发度相乘会静默突破服务端上限。
        for files in 1..=DOWNLOAD_WINDOW {
            let total = files * window_per_file(files);
            assert!(
                total <= DOWNLOAD_WINDOW,
                "{files} 个并发文件 × 每文件 {} 窗口 = {total}，超出总预算 {DOWNLOAD_WINDOW}",
                window_per_file(files)
            );
        }
    }

    #[test]
    fn every_file_keeps_at_least_one_request_in_flight() {
        // 摊薄到零就等于不传了。文件并发再高，每个文件也要留一个请求位。
        for files in [1, DOWNLOAD_WINDOW, DOWNLOAD_WINDOW * 4, usize::MAX] {
            assert!(window_per_file(files) >= 1);
        }
        // 并发度为 0 是调用方的 bug，但不能因此除零崩掉。
        assert_eq!(window_per_file(0), DOWNLOAD_WINDOW);
    }

    #[test]
    fn the_directory_defaults_stay_inside_the_budget() {
        assert!(
            DIRECTORY_FILE_CONCURRENCY * window_per_file(DIRECTORY_FILE_CONCURRENCY)
                <= DOWNLOAD_WINDOW
        );
        assert!(DIRECTORY_LISTING_CONCURRENCY >= 1);
    }
}
