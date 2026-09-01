//! 目录枚举与过滤索引：本地深走 + WSL guest 枚举。
//!
//! 从 `side_panel.rs` 拆出（2026-08-31）。

use super::*;

pub(crate) fn wsl_root_key(located: &crate::shell_detect::WslCwd) -> PathBuf {
    PathBuf::from(normalize_wsl_guest_path(&located.guest))
}

pub(crate) fn normalize_wsl_guest_path(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    if normalized == "/" {
        return normalized;
    }
    normalized.trim_end_matches('/').to_owned()
}

pub(crate) fn wsl_guest_parent(path: &str) -> Option<String> {
    let normalized = normalize_wsl_guest_path(path);
    if normalized == "/" {
        return None;
    }
    let parent = normalized
        .rsplit_once('/')
        .map_or("/", |(parent, _)| if parent.is_empty() { "/" } else { parent });
    Some(parent.to_owned())
}

pub(crate) fn wsl_guest_join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

/// `find` 输出为重复的 `type NUL value NUL`。只接受完整记录；来宾进程被
/// 中断时尾部半条记录会被自然丢弃，不会生成指向错误位置的行。
pub(crate) fn parse_wsl_find_pairs(output: &[u8]) -> Vec<(u8, String)> {
    let fields: Vec<&[u8]> = output
        .split_inclusive(|byte| *byte == 0)
        .filter_map(|field| field.strip_suffix(&[0]))
        .collect();

    fields
        .chunks_exact(2)
        .filter_map(|pair| {
            let kind = pair[0].first().copied()?;
            Some((kind, String::from_utf8_lossy(pair[1]).into_owned()))
        })
        .collect()
}

/// `Command::output` 没有超时；WSL 桥一旦卡住会永久占住 SidePanel 的唯一工人。
/// 两根 pipe 各自排水，避免输出填满后子进程与父进程互相等待。
pub(crate) fn command_output_with_timeout(
    mut command: std::process::Command,
    timeout: Option<Duration>,
) -> std::io::Result<std::process::Output> {
    let Some(timeout) = timeout else { return command.output() };
    use std::io::Read as _;
    use std::process::Stdio;

    // stdin 显式关掉：这些子进程（`wsl.exe -d … -- find`、`git`）都不读 stdin，
    // 而我们用 `CREATE_NO_WINDOW` 启动它们——不设置 stdin 会把父进程的控制台
    // 句柄继承给一个没有控制台的子进程。这是卫生问题，不是任何已知 bug 的根因。
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "missing stdout pipe")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "missing stderr pipe")
    })?;
    let read_pipe = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        })
    };
    let stdout_reader = read_pipe(Box::new(stdout));
    let stderr_reader = read_pipe(Box::new(stderr));
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("command exceeded {} seconds", timeout.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    let join = |reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>| {
        reader.join().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Other, "command pipe reader panicked")
        })?
    };
    Ok(std::process::Output { status, stdout: join(stdout_reader)?, stderr: join(stderr_reader)? })
}

/// 跑一条来宾 `find`，返回 `(stdout, 退出码是否为 0)`。
///
/// 非零退出**不丢 stdout**：多起点的 `find` 只要有一个起点不存在就整体非零，
/// 而其余起点的记录照样已经写出来了。调用方按自己的语义决定这算失败还是降级。
pub(crate) fn run_wsl_find_lenient(
    distro: &str,
    args: impl IntoIterator<Item = OsString>,
) -> Option<(Vec<u8>, bool)> {
    let mut command = std::process::Command::new("wsl.exe");
    command.args(["-d", distro, "--", "find"]).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = match command_output_with_timeout(command, Some(WSL_COMMAND_TIMEOUT)) {
        Ok(output) => output,
        Err(error) => {
            log::warn!("WSL file tree find failed in {distro}: {error}");
            return None;
        },
    };
    let exit_ok = output.status.success();
    if !exit_ok {
        let reason = String::from_utf8_lossy(&output.stderr);
        log::debug!("WSL file tree find exited non-zero in {distro}: {}", reason.trim());
    }
    Some((output.stdout, exit_ok))
}

/// 退出码非零一律当失败的严格版；单起点调用（搜索索引）用它。
pub(crate) fn run_wsl_find(
    distro: &str,
    args: impl IntoIterator<Item = OsString>,
) -> Option<Vec<u8>> {
    let (stdout, exit_ok) = run_wsl_find_lenient(distro, args)?;
    exit_ok.then_some(stdout)
}

/// `find -printf` 的格式串：类型 + NUL + 全路径 + NUL。**反斜杠必须写两遍**。
///
/// 2026-08-21 实测：`wsl.exe -d <发行版> -- <命令>` 在把参数转发给来宾时会吞掉
/// 一层反斜杠。同一条 find、同一个目录，三种写法的输出对照：
///
/// | 传入 | NUL 个数 | 输出开头 |
/// |---|---|---|
/// | `%y\0%f\0`   | **0**  | `l0lib0d0opt0…` |
/// | `%y\\0%f\\0` | 54     | `l\0lib\0d\0opt\0…` |
/// | `sh -c` 包一层 | 54   | `l\0lib\0d\0opt\0…` |
///
/// 也就是说单反斜杠版本让 find 收到的是 `%y0%f0`，输出用字面字符 `'0'` 分隔、
/// 一个 NUL 都没有，[`parse_wsl_find_pairs`] 因此永远配不出记录、返回空列表——
/// UI 再把空列表显示成"此目录为空"。这就是 WSL 文件树空白的根因。
///
/// 用双反斜杠而不是 `sh -c` 包装：后者要为含空格/引号的来宾路径再做一层 shell
/// 引用，而这里只需要把转义层数补对。
pub(crate) const WSL_FIND_PATH_FORMAT: &str = r"%y\\0%p\\0";

/// 一趟 `find` 的结果，按父目录分桶。
pub(crate) struct WslDirListing {
    /// 来宾父目录 → 该层条目 `(is_dir, 名字, 来宾全路径)`，已按"目录在前、
    /// 名字不分大小写升序"排好并按 [`MAX_PER_DIR`] 截断。
    pub(crate) by_dir: HashMap<String, Vec<(bool, String, String)>>,
    /// `find` 自身的退出码是否为 0。多起点时一个起点不存在就会让它非零，
    /// 所以这只是降级信号，判断"树是否可用"要看根桶在不在。
    pub(crate) exit_ok: bool,
}

/// 这一趟要枚举的来宾目录：根，加上 `expanded` 里位于根之下的每一个。
///
/// 祖先链是否也展开无所谓——多列一个目录在同一条命令里几乎免费，漏列一个却会
/// 让那层显示成空目录。排序后返回：`expanded` 是 `HashSet`，遍历顺序不定，而
/// 截断必须是确定的。根一定排在首位（它是所有后代的真前缀且更短），所以截断
/// 永远不会把根本身丢掉。
pub(crate) fn wsl_dirs_to_list(root: &str, expanded: &HashSet<PathBuf>) -> Vec<String> {
    /// 命令行长度是有上限的（Windows 约 32 KiB）。来宾路径平均几十字节，256 个
    /// 起点离上限还很远，而真展开到 256 个目录本身已经超出 [`MAX_ROWS`] 的显示
    /// 能力了。
    const MAX_ROOTS: usize = 256;

    let mut dirs = vec![root.to_owned()];
    for path in expanded {
        let Some(text) = path.to_str() else { continue };
        let guest = normalize_wsl_guest_path(text);
        if !wsl_path_is_descendant(root, &guest) {
            continue;
        }
        dirs.push(guest);
    }
    dirs.sort_unstable();
    dirs.dedup();
    dirs.truncate(MAX_ROOTS);
    dirs
}

/// `path` 是否严格位于 `root` 之下。两侧都已 [`normalize_wsl_guest_path`]。
pub(crate) fn wsl_path_is_descendant(root: &str, path: &str) -> bool {
    if root == "/" {
        return path.starts_with('/') && path.len() > 1;
    }
    path.len() > root.len()
        && path.starts_with(root)
        && path.as_bytes().get(root.len()) == Some(&b'/')
}

/// 一条 `find` 枚举全部给定目录。`None` = 这次来宾枚举**根本没跑成**
/// （wsl.exe 起不来 / 超时），与"目录真的是空的"必须区分：前者要让用户看到
/// 可重试的提示，后者才是"此目录为空"。
///
/// # 为什么是一条而不是每层一条
///
/// 旧实现在递归里每展开一层就 fork 一次 `wsl.exe`。实测冷启动 7.5 秒、热约
/// 300 ms，而快照工人每次重建整棵树都要把已展开的每一层重跑一遍——展开 5 层
/// 就是 5 次串行往返。这就是"WSL 打开路径非常卡"的根因。`find` 原生接受多个
/// 起点，而要枚举哪些目录**不需要先枚举才能知道**（`expanded` 集合已经持有），
/// 所以一次调用就够。
///
/// 用 `%p`（全路径）而非 `%f`：多起点的输出是混在一起的，只有全路径才能按父
/// 目录分桶还原成树。
pub(crate) fn wsl_read_dirs(distro: &str, dirs: &[String]) -> Option<WslDirListing> {
    if dirs.is_empty() {
        return None;
    }
    let mut args: Vec<OsString> = dirs.iter().map(OsString::from).collect();
    args.extend(
        ["-mindepth", "1", "-maxdepth", "1", "-printf", WSL_FIND_PATH_FORMAT]
            .into_iter()
            .map(OsString::from),
    );
    let (output, exit_ok) = run_wsl_find_lenient(distro, args)?;
    Some(WslDirListing { by_dir: bucket_wsl_find_output(&output), exit_ok })
}

/// 把一趟多起点 `find` 的扁平输出按父目录分桶还原成树的各层。
///
/// 每桶按"目录在前、名字不分大小写升序"排好并按 [`MAX_PER_DIR`] 截断——截断
/// 必须在排序**之后**，否则采样到的是文件系统顺序（见
/// `per_directory_cap_keeps_the_ordered_head`）。
pub(crate) fn bucket_wsl_find_output(
    output: &[u8],
) -> HashMap<String, Vec<(bool, String, String)>> {
    let mut by_dir: HashMap<String, Vec<(bool, String, String)>> = HashMap::new();
    for (kind, guest_path) in parse_wsl_find_pairs(output) {
        let Some((parent, name)) = guest_path.rsplit_once('/') else { continue };
        if name.is_empty() || name == ".git" {
            continue;
        }
        // 根下的条目（`/home`）切出来的父是空串，归一化回 `/`。
        let parent = if parent.is_empty() { "/" } else { parent };
        by_dir.entry(parent.to_owned()).or_default().push((
            kind == b'd',
            name.to_owned(),
            guest_path.clone(),
        ));
    }
    for entries in by_dir.values_mut() {
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        entries.truncate(MAX_PER_DIR);
    }
    by_dir
}

/// 列**一个**来宾目录，给补齐用（文件树走多起点的 [`wsl_read_dirs`]）。
///
/// 返回 `(是否目录, 名字)`。`None` = 这次枚举失败，与"目录是空的"必须区分：
/// 前者下次还该重试，后者不必（见 [`crate::remote_dirs::finish_fetch`]）。
///
/// **阻塞**：一次 `wsl.exe` 子进程往返，冷启动可达数秒。只能在后台线程上调。
pub fn wsl_list_one_dir(distro: &str, guest_dir: &str) -> Option<Vec<(bool, String)>> {
    let dir = normalize_wsl_guest_path(guest_dir);
    let args = [dir.as_str(), "-mindepth", "1", "-maxdepth", "1", "-printf", WSL_FIND_PATH_FORMAT]
        .into_iter()
        .map(OsString::from);
    let (output, exit_ok) = run_wsl_find_lenient(distro, args)?;
    if !exit_ok {
        // 单起点的非零退出就是"这个目录读不到"（不存在 / 无权限），没有
        // 多起点那种"其余起点仍然有效"的余地。
        return None;
    }
    Some(
        bucket_wsl_find_output(&output)
            .remove(&dir)
            .unwrap_or_default()
            .into_iter()
            .map(|(is_dir, name, _)| (is_dir, name))
            .collect(),
    )
}

pub(crate) fn build_wsl_search_index(
    located: &crate::shell_detect::WslCwd,
    index: &mut Vec<FileRow>,
    budget: &mut usize,
) {
    let root = normalize_wsl_guest_path(&located.guest);
    let mut args: Vec<OsString> =
        [root.as_str(), "-mindepth", "1", "-maxdepth", "8", "(", "-type", "d", "(", "-name", ".*"]
            .into_iter()
            .map(OsString::from)
            .collect();
    for skipped in SEARCH_SKIP_DIRS.iter().filter(|name| !name.starts_with('.')) {
        args.extend([OsString::from("-o"), OsString::from("-name"), OsString::from(skipped)]);
    }
    args.extend(
        [")", "-prune", ")", "-o", "-printf", WSL_FIND_PATH_FORMAT].into_iter().map(OsString::from),
    );
    let Some(output) = run_wsl_find(&located.distro, args) else { return };
    for (kind, guest_path) in parse_wsl_find_pairs(&output) {
        if *budget == 0 || index.len() >= SEARCH_INDEX_CAP {
            break;
        }
        *budget -= 1;
        let name = guest_path.rsplit('/').next().unwrap_or(&guest_path).to_owned();
        index.push(FileRow {
            path: PathBuf::from(&guest_path),
            guest_path: Some(guest_path),
            name,
            depth: 0,
            is_dir: kind == b'd',
            expanded: false,
            is_parent: false,
            ignored: false,
        });
    }
}
