//! VCS 数据层：Git/SVN 状态读取与写操作命令构造。
//!
//! 从 `side_panel.rs` 拆出（2026-08-31）。渲染在 [`super::render`]，
//! 目录枚举在 [`super::enumerate`]，面板状态机留在 [`super`]。

use super::*;

pub(crate) fn git_pull_args() -> Vec<String> {
    vec!["pull".into(), "--ff-only".into()]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SvnMutation {
    Add(Vec<PathBuf>),
    Commit(String),
    Update,
    Revert(PathBuf),
    Resolve(PathBuf),
    Cleanup,
}

impl SvnMutation {
    pub(crate) fn cli_args(&self) -> Vec<OsString> {
        let args: Vec<OsString> = match self {
            Self::Add(paths) => {
                let mut args = vec!["add".into(), "--parents".into(), "--".into()];
                args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
                args
            },
            Self::Commit(message) => vec![
                "commit".into(),
                "--non-interactive".into(),
                "-m".into(),
                message.as_str().into(),
            ],
            Self::Update => vec!["update".into(), "--non-interactive".into()],
            Self::Revert(path) => vec![
                "revert".into(),
                "--depth".into(),
                "infinity".into(),
                "--".into(),
                path.as_os_str().to_owned(),
            ],
            // `working` 保留用户解决后的文件内容，只把冲突状态标记为已处理。
            Self::Resolve(path) => vec![
                "resolve".into(),
                "--accept".into(),
                "working".into(),
                "--".into(),
                path.as_os_str().to_owned(),
            ],
            Self::Cleanup => vec!["cleanup".into()],
        };
        args
    }

    pub(crate) fn tortoise_args(&self, working_dir: &Path) -> Vec<OsString> {
        let command = |name: &str| OsString::from(format!("/command:{name}"));
        let path_arg = |path: &Path| OsString::from(format!("/path:{}", path.display()));
        match self {
            Self::Add(paths) => vec![
                command("add"),
                OsString::from(format!(
                    "/path:{}",
                    paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join("*")
                )),
            ],
            Self::Commit(message) => vec![
                command("commit"),
                path_arg(working_dir),
                OsString::from(format!("/logmsg:{message}")),
            ],
            Self::Update => vec![command("update"), path_arg(working_dir)],
            Self::Revert(path) => vec![command("revert"), path_arg(path)],
            Self::Resolve(path) => vec![command("resolve"), path_arg(path)],
            // TortoiseProc 的 cleanup 对话框需要额外 `/cleanup` 才勾选基础清理。
            Self::Cleanup => vec![command("cleanup"), path_arg(working_dir), "/cleanup".into()],
        }
    }
}

/// 一个 TortoiseProc 对话框调用。
///
/// TortoiseSVN 的自动化接口形状极其规整——绝大多数操作就是
/// `TortoiseProc.exe /command:<名字> /path:<路径>`，少数几个要么收版本库 URL
/// 而不是文件系统路径（repobrowser / checkout），要么需要一两个固定开关
/// （blame 的修订区间、update 的 `/rev`）。所以这里不是每个操作一个变体，
/// 而是把这三类形状各做一个：加一个新操作等于加一行调用，不用动这个类型。
///
/// 命令名是 TortoiseSVN 的外部契约（见其 automation 文档），拼错不会编译
/// 失败、只会让 TortoiseProc 静默什么都不做，所以名字集中在 [`SidePanel`]
/// 的各个 `svn_*` 方法里一处一个，不散落。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SvnVisual {
    /// 作用在工作副本路径上：`/command:<command> /path:<path> <extra…>`。
    WorkingCopy { command: &'static str, path: PathBuf, extra: &'static [&'static str] },
    /// 作用在版本库上。`url_key` 是 `/path:` 或 `/url:`——repobrowser 认前者
    /// （值仍是 URL），checkout 认后者，两个不能互换。
    Repository { command: &'static str, root: PathBuf, url_key: &'static str },
}

impl SvnVisual {
    pub(crate) fn tortoise_args(&self) -> Vec<OsString> {
        match self {
            Self::WorkingCopy { command, path, extra } => {
                let mut args = vec![
                    OsString::from(format!("/command:{command}")),
                    OsString::from(format!("/path:{}", path.display())),
                ];
                args.extend(extra.iter().map(OsString::from));
                args
            },
            Self::Repository { command, root, url_key } => vec![
                OsString::from(format!("/command:{command}")),
                OsString::from(format!("{url_key}{}", local_repository_url(root))),
            ],
        }
    }
}

/// 工作副本对话框的构造快捷方式。
fn working_copy_dialog(command: &'static str, path: PathBuf) -> SvnVisual {
    SvnVisual::WorkingCopy { command, path, extra: &[] }
}

pub(crate) fn local_repository_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    if encoded.starts_with("//") {
        format!("file:{encoded}")
    } else if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

pub(crate) fn find_path_command(program: &str) -> Option<PathBuf> {
    let requested = PathBuf::from(program);
    if requested.components().count() > 1 {
        return requested.is_file().then_some(requested);
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let direct = directory.join(program);
        if direct.is_file() {
            return Some(direct);
        }
        #[cfg(windows)]
        {
            let executable = directory.join(format!("{program}.exe"));
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

#[cfg(windows)]
pub(crate) fn find_tortoise_proc() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    // 自定义安装盘不会出现在 ProgramFiles；注册表的 ProcPath 才是权威位置。
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for key_name in [r"SOFTWARE\TortoiseSVN", r"SOFTWARE\WOW6432Node\TortoiseSVN"] {
            let candidate = RegKey::predef(hive)
                .open_subkey(key_name)
                .and_then(|key| key.get_value::<String, _>("ProcPath"))
                .ok()
                .map(PathBuf::from);
            if candidate.as_ref().is_some_and(|path| path.is_file()) {
                return candidate;
            }
        }
    }
    if let Some(path) = find_path_command("TortoiseProc") {
        return Some(path);
    }
    for root in
        ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(|name| std::env::var_os(name))
    {
        for relative in [r"TortoiseSVN\bin\TortoiseProc.exe", r"SVN\bin\TortoiseProc.exe"] {
            let candidate = PathBuf::from(&root).join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
pub(crate) fn find_tortoise_proc() -> Option<PathBuf> {
    None
}

/// `svn.exe` 的位置。查一次，之后免费。
///
/// 为什么值得缓存：[`find_path_command`] 会遍历整个 `PATH`——开发机上 80 项
/// 起步，每项还要两次 `is_file`（带 `.exe` 和不带）——而这个函数原先是**每次
/// 点按钮**都在 UI 线程上重跑的，一次点击要走两遍（先找 CLI 再找 Tortoise）。
/// 可执行文件的位置在进程生命周期内不会变，所以缓存没有失效面；真装上/卸掉
/// 客户端的用户重启一次终端即可。
pub(crate) fn svn_cli() -> Option<&'static Path> {
    static SLOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| find_path_command("svn")).as_deref()
}

/// `TortoiseProc.exe` 的位置。同上，另加省掉四次注册表查询。
pub(crate) fn tortoise_proc() -> Option<&'static Path> {
    static SLOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SLOT.get_or_init(find_tortoise_proc).as_deref()
}

/// 这台机器上能不能做 SVN 的可视化操作（日志、锁定、属性…）。按钮层用它
/// 决定是画按钮还是画一条"需要 TortoiseSVN"的提示，而不是让用户点了才知道。
pub(crate) fn tortoise_available() -> bool {
    tortoise_proc().is_some()
}

pub(crate) fn svn_relative_target(root: &Path, relative: &str) -> Option<PathBuf> {
    use std::path::Component;

    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(component, Component::Prefix(_) | Component::RootDir | Component::ParentDir)
        })
    {
        return None;
    }
    Some(root.join(relative))
}

impl SidePanel {
    // ---- git mutations (add / commit / pull / push) ----

    /// Whether a git mutation is in flight (buttons gray out).
    pub fn op_running(&self) -> bool {
        self.op_running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Last mutation's error, if any (cleared by the next successful op).
    pub fn op_error(&self) -> Option<String> {
        let e = self.op_error.lock().ok()?;
        (!e.is_empty()).then(|| e.clone())
    }

    fn set_op_error(&mut self, message: impl Into<String>) {
        if let Ok(mut error) = self.op_error.lock() {
            *error = message.into();
        }
    }

    /// Run `<program> <args>` on a worker thread; UI stays live (a push can
    /// take seconds over the network). Completion flips `op_done`, which the
    /// next drawn frame folds into a refresh.
    fn spawn_vcs_at(&mut self, program: PathBuf, args: Vec<OsString>, root: PathBuf) {
        use std::sync::atomic::Ordering;
        if self.op_running.swap(true, Ordering::Relaxed) {
            return; // one at a time
        }
        let running = self.op_running.clone();
        let done = self.op_done.clone();
        let error = self.op_error.clone();
        if let Ok(mut message) = error.lock() {
            message.clear();
        }
        let display_name = program.display().to_string();
        let spawn_result =
            std::thread::Builder::new().name("nebula-vcs-op".into()).spawn(move || {
                let mut cmd = std::process::Command::new(&program);
                cmd.args(&args).current_dir(&root);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                let msg = match cmd.output() {
                    Ok(out) if out.status.success() => String::new(),
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        // First meaningful line is enough for a status strip.
                        err.lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or(&format!("{display_name} 失败"))
                            .to_string()
                    },
                    Err(e) => format!("{display_name}: {e}"),
                };
                if let Ok(mut slot) = error.lock() {
                    *slot = msg;
                }
                running.store(false, Ordering::Relaxed);
                done.store(true, Ordering::Relaxed);
            });
        if let Err(spawn_error) = spawn_result {
            self.op_running.store(false, Ordering::Relaxed);
            self.set_op_error(format!("无法启动版本控制任务: {spawn_error}"));
        }
    }

    fn spawn_vcs(&mut self, program: impl Into<PathBuf>, args: Vec<OsString>) {
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        self.spawn_vcs_at(program.into(), args, root);
    }

    fn spawn_git(&mut self, args: Vec<String>) {
        if let Some(located) = self.followed_wsl.clone() {
            let mut guest_args: Vec<OsString> = [
                "-d",
                located.distro.as_str(),
                "--",
                "git",
                "-C",
                located.guest.as_str(),
                "--no-optional-locks",
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            guest_args.extend(args.into_iter().map(OsString::from));
            let host_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            self.spawn_vcs_at(PathBuf::from("wsl.exe"), guest_args, host_cwd);
        } else {
            self.spawn_vcs("git", args.into_iter().map(OsString::from).collect());
        }
    }

    fn spawn_svn_mutation(&mut self, operation: SvnMutation) {
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        if let Some(svn) = svn_cli() {
            self.spawn_vcs_at(svn.to_path_buf(), operation.cli_args(), root);
        } else if let Some(tortoise) = tortoise_proc() {
            self.spawn_vcs_at(tortoise.to_path_buf(), operation.tortoise_args(&root), root);
        } else {
            self.set_op_error("未找到 svn.exe 或 TortoiseSVN，无法执行 SVN 操作");
        }
    }

    fn launch_svn_visual(&mut self, visual: SvnVisual) -> bool {
        let Some(program) = tortoise_proc() else {
            self.set_op_error("此操作需要 TortoiseSVN（未找到 TortoiseProc.exe）");
            return false;
        };
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        let mut command = std::process::Command::new(program);
        command.args(visual.tortoise_args()).current_dir(root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW 不会隐藏 Tortoise GUI。
        }
        match command.spawn() {
            Ok(_) => {
                self.set_op_error(String::new());
                true
            },
            Err(error) => {
                self.set_op_error(format!("无法启动 {}: {error}", program.display()));
                false
            },
        }
    }

    /// 在当前 SVN 工作副本里开一个 TortoiseProc 对话框。
    ///
    /// 统一的守卫：必须是工作副本（服务端版本库上这些命令全无意义）。不检查
    /// `op_running`——这些对话框是 TortoiseSVN 自己的进程，和面板里排队的写
    /// 操作互不阻塞，禁掉只会让用户在等一个 commit 时连日志都看不了。
    fn launch_working_copy_dialog(&mut self, command: &'static str, relative: &str) -> bool {
        if self.vcs() != Some(VcsKind::Svn) {
            return false;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        let Some(path) = svn_relative_target(&root, relative) else { return false };
        self.launch_svn_visual(working_copy_dialog(command, path))
    }

    /// 同上，但作用在工作副本根而不是某一行。
    fn launch_root_dialog(&mut self, command: &'static str) -> bool {
        if self.vcs() != Some(VcsKind::Svn) {
            return false;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        self.launch_svn_visual(working_copy_dialog(command, root))
    }

    /// 当前快照的 VCS 种类；None = 不在任何仓库里。
    pub fn vcs(&self) -> Option<VcsKind> {
        self.git.as_ref().map(|info| info.vcs)
    }

    /// `git add -A`: stage everything (the ⊕ button). SVN 没有暂存区，
    /// no-op（按钮层按 [`Self::vcs`] 直接不画）。
    pub fn git_stage_all(&mut self) {
        if self.vcs() != Some(VcsKind::Git) {
            return;
        }
        if self.git.as_ref().is_some_and(|g| !g.unstaged.is_empty()) && !self.op_running() {
            self.spawn_git(vec!["add".into(), "-A".into()]);
        }
    }

    /// `git add -- <path>`：单文件暂存（VS Code 行内 ＋ 的合同）。
    pub fn git_stage_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["add".into(), "--".into(), path.to_owned()]);
        }
    }

    /// `git restore --staged -- <path>`：单文件取消暂存（VS Code 行内 −）。
    pub fn git_unstage_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["restore".into(), "--staged".into(), "--".into(), path.to_owned()]);
        }
    }

    /// `git restore -- <path>`：丢弃工作区改动（调用方负责确认交互；
    /// untracked 文件不适用——restore 不删新文件，按钮层不对 `?` 提供）。
    pub fn git_discard_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["restore".into(), "--".into(), path.to_owned()]);
        }
    }

    /// Commit button: with staged changes, open the message input (Enter then
    /// commits via [`Self::git_commit_submit`]).
    pub fn git_begin_commit(&mut self) {
        if self.git.as_ref().is_some_and(|g| !g.staged.is_empty()) && !self.op_running() {
            self.commit_focus = true;
            self.commit_selection.clear();
        }
    }

    pub fn commit_input(&mut self, text: &str) {
        self.commit_selection.insert(&mut self.commit_msg, text);
    }

    pub fn commit_backspace(&mut self) {
        self.commit_selection.backspace(&mut self.commit_msg);
    }

    pub fn commit_select_all(&mut self) {
        self.commit_selection.select(&self.commit_msg);
    }

    pub fn commit_selected_text(&self) -> Option<String> {
        self.commit_selection.selected_text(&self.commit_msg)
    }

    pub fn commit_all_selected(&self) -> bool {
        self.commit_selection.is_selected()
    }

    pub fn commit_cancel(&mut self) {
        self.commit_focus = false;
        self.commit_msg.clear();
        self.commit_selection.clear();
    }

    pub fn commit_unfocus(&mut self) {
        self.commit_focus = false;
        self.commit_selection.clear();
    }

    /// Enter in the message box: run `git commit -m <msg>`.
    pub fn git_commit_submit(&mut self) {
        let msg = self.commit_msg.trim().to_string();
        if msg.is_empty() || self.op_running() {
            return;
        }
        self.commit_focus = false;
        self.commit_msg.clear();
        self.commit_selection.clear();
        self.vcs_commit_message(&msg);
    }

    /// 直接以给定消息提交（GPUI 壳的输入组件走这里，不经旧壳的内部输入
    /// 状态机）。按 VCS 分派：git 提交暂存区；svn 没有暂存区，提交整个
    /// 工作副本的修改。
    pub fn vcs_commit_message(&mut self, message: &str) {
        let message = message.trim();
        if message.is_empty() || self.op_running() {
            return;
        }
        match self.vcs() {
            Some(VcsKind::Git) => {
                self.spawn_git(vec!["commit".into(), "-m".into(), message.to_owned()]);
            },
            Some(VcsKind::Svn) => {
                self.spawn_svn_mutation(SvnMutation::Commit(message.to_owned()));
            },
            Some(VcsKind::SvnRepository) | None => {},
        }
    }

    /// Push button — only enabled with committed-but-unpushed work (`ahead`).
    /// SVN 的 `ahead` 恒 0（提交即发布），按钮自然不亮。
    pub fn git_push(&mut self) {
        if self.git.as_ref().is_some_and(|g| g.ahead > 0) && !self.op_running() {
            self.spawn_git(vec!["push".into()]);
        }
    }

    /// Pull only fast-forward updates, never creating an implicit merge commit.
    /// SVN 对应 `svn update`。
    pub fn git_pull(&mut self) {
        if self.op_running() {
            return;
        }
        match self.vcs() {
            Some(VcsKind::Git) => self.spawn_git(git_pull_args()),
            Some(VcsKind::Svn) => self.spawn_svn_mutation(SvnMutation::Update),
            Some(VcsKind::SvnRepository) | None => {},
        }
    }

    /// SVN 的“添加”只接纳 `?` 未版本化项，不引入 Git 暂存区语义。
    pub fn svn_add_all(&mut self) {
        if self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let paths: Vec<PathBuf> = self
            .git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::Svn)
            .into_iter()
            .flat_map(|info| info.unstaged.iter())
            .filter(|(status, _)| *status == '?')
            .filter_map(|(_, path)| svn_relative_target(&root, path))
            .collect();
        if !paths.is_empty() {
            self.spawn_svn_mutation(SvnMutation::Add(paths));
        }
    }

    pub fn svn_add_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Add(vec![path]));
    }

    pub fn svn_revert_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Revert(path));
    }

    pub fn svn_resolve_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Resolve(path));
    }

    pub fn svn_cleanup(&mut self) {
        if self.vcs() == Some(VcsKind::Svn) && !self.op_running() {
            self.spawn_svn_mutation(SvnMutation::Cleanup);
        }
    }

    // ---- SVN 可视化操作（全部委托 TortoiseSVN）----
    //
    // 为什么整批委托而不自己实现：装 SVN 的人里一多半只装了 TortoiseSVN，
    // 没装命令行客户端；而这些操作（日志、锁定、合并、属性…）真正的成本不在
    // 发命令，而在它们各自的对话框——修订区间选择、冲突三窗对比、属性编辑器。
    // 重做一遍那些界面既是重复劳动，行为还会和用户已经熟悉的小乌龟不一致。
    // 所以这里只做一件事：把面板当前的选择翻译成 TortoiseProc 的参数。

    /// 工作副本根的日志。
    pub fn svn_log(&mut self) {
        self.launch_root_dialog("log");
    }

    /// 单个文件/目录的日志。
    pub fn svn_log_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("log", path)
    }

    pub fn svn_diff_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("diff", path)
    }

    /// 责任追溯。TortoiseBlame 必须拿到修订区间才肯启动，缺了会静默退出。
    pub fn svn_blame_path(&mut self, path: &str) -> bool {
        if self.vcs() != Some(VcsKind::Svn) {
            return false;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        let Some(path) = svn_relative_target(&root, path) else { return false };
        self.launch_svn_visual(SvnVisual::WorkingCopy {
            command: "blame",
            path,
            extra: &["/startrev:1", "/endrev:HEAD"],
        })
    }

    /// 获得锁定 / 释放锁定。
    ///
    /// 这是 SVN 相对 Git 最本质的差异化能力：二进制资源（美术、文档、
    /// 关卡文件）没法合并，团队靠"改之前先锁住"避免白工。此前面板里完全
    /// 没有入口，等于把 SVN 用户最在意的一件事漏在外面。
    pub fn svn_lock_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("lock", path)
    }

    pub fn svn_unlock_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("unlock", path)
    }

    /// 加入忽略列表（写 `svn:ignore` 属性）。
    ///
    /// 走对话框而不是直接改属性：忽略的粒度（这一个文件 / 同类扩展名 /
    /// 递归）要用户选，而 `svn:ignore` 写坏了会让整个团队的状态列表都变样。
    pub fn svn_ignore_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("ignore", path)
    }

    /// 版本控制层面的删除与重命名——直接在磁盘上删/改名会让 SVN 报 `!`
    /// （missing），这两个入口才是正确做法。
    pub fn svn_delete_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("remove", path)
    }

    pub fn svn_rename_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("rename", path)
    }

    pub fn svn_properties_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("properties", path)
    }

    /// 冲突编辑器（TortoiseMerge 三窗对比）。面板里已有的「解决」是
    /// `--accept working`——那是"我已经手工改好了"，这个才是去改。
    pub fn svn_conflict_editor_path(&mut self, path: &str) -> bool {
        self.launch_working_copy_dialog("conflicteditor", path)
    }

    /// 更新至指定版本。`/rev` 不带值 = 让 TortoiseSVN 弹修订选择框。
    pub fn svn_update_to_revision(&mut self) -> bool {
        if self.vcs() != Some(VcsKind::Svn) {
            return false;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        self.launch_svn_visual(SvnVisual::WorkingCopy {
            command: "update",
            path: root,
            extra: &["/rev"],
        })
    }

    /// 检查修改：TortoiseSVN 的状态窗口能连远端一起比（面板自己的列表只看
    /// 本地），所以"别人改了什么"要靠它。
    pub fn svn_check_modifications(&mut self) -> bool {
        self.launch_root_dialog("repostatus")
    }

    /// 工作副本对应的版本库浏览器。TortoiseProc 从工作副本路径自己解析 URL。
    pub fn svn_browse_working_copy(&mut self) -> bool {
        self.launch_root_dialog("repobrowser")
    }

    pub fn svn_switch(&mut self) -> bool {
        self.launch_root_dialog("switch")
    }

    /// 分支 / 标记（SVN 里就是一次版本库内的 copy）。
    pub fn svn_branch_or_tag(&mut self) -> bool {
        self.launch_root_dialog("copy")
    }

    pub fn svn_merge(&mut self) -> bool {
        self.launch_root_dialog("merge")
    }

    /// 重定位：版本库换了地址（域名、协议、迁移过）之后修工作副本的指向。
    pub fn svn_relocate(&mut self) -> bool {
        self.launch_root_dialog("relocate")
    }

    /// 导出一份不带 `.svn` 的干净拷贝。
    pub fn svn_export(&mut self) -> bool {
        self.launch_root_dialog("export")
    }

    pub fn svn_revision_graph(&mut self) -> bool {
        self.launch_root_dialog("revisiongraph")
    }

    pub fn svn_properties(&mut self) -> bool {
        self.launch_root_dialog("properties")
    }

    // ---- 服务端版本库（`svnadmin create` 的产物，没有工作副本语义）----

    /// 当前快照指向的服务端版本库根。
    fn svn_repository_root(&self) -> Option<PathBuf> {
        self.git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::SvnRepository)
            .and_then(|info| info.repository_root.clone())
    }

    fn launch_repository_dialog(&mut self, command: &'static str, url_key: &'static str) -> bool {
        let Some(root) = self.svn_repository_root() else { return false };
        self.launch_svn_visual(SvnVisual::Repository { command, root, url_key })
    }

    pub fn svn_browse_repository(&mut self) {
        self.launch_repository_dialog("repobrowser", "/path:");
    }

    /// 检出。只给 `/url:`，落地目录由 TortoiseSVN 的检出窗口让用户选。
    pub fn svn_checkout_repository(&mut self) {
        self.launch_repository_dialog("checkout", "/url:");
    }

    /// 服务端版本库的历史。空库（HEAD=0）也能开，只是列表是空的。
    pub fn svn_repository_log(&mut self) -> bool {
        self.launch_repository_dialog("log", "/path:")
    }

    /// 把一个本地目录导入版本库（TortoiseSVN 的"导入"）。
    pub fn svn_repository_import(&mut self) -> bool {
        self.launch_repository_dialog("import", "/path:")
    }

    /// 建 trunk / branches / tags。
    ///
    /// TortoiseSVN 没有对应的自动化命令（GUI 里也是在版本库浏览器里手工新建
    /// 三个目录），所以这里分两条路：有命令行客户端就一次 `svn mkdir` 建完，
    /// 没有就把版本库浏览器打开并说清楚要做什么——总比给个点了没反应的按钮好。
    pub fn svn_create_standard_layout(&mut self) {
        let Some(root) = self.svn_repository_root() else { return };
        let url = local_repository_url(&root);
        match svn_cli() {
            Some(svn) => {
                let mut args: Vec<OsString> = vec![
                    "mkdir".into(),
                    "--parents".into(),
                    "--non-interactive".into(),
                    "-m".into(),
                    "创建标准布局 trunk/branches/tags".into(),
                ];
                args.extend(
                    ["trunk", "branches", "tags"]
                        .into_iter()
                        .map(|name| OsString::from(format!("{url}/{name}"))),
                );
                self.spawn_vcs_at(svn.to_path_buf(), args, root);
            },
            None => {
                let opened = self.launch_svn_visual(SvnVisual::Repository {
                    command: "repobrowser",
                    root,
                    url_key: "/path:",
                });
                // 顺序要紧：`launch_svn_visual` 成功时会清空错误栏。
                if opened {
                    self.set_op_error(
                        "未装 svn 命令行客户端：已打开版本库浏览器，请在其中新建 trunk / branches / tags",
                    );
                }
            },
        }
    }
}

/// Snapshot git state for `root`: branch, ±line counts, changed files./// `None` when git is missing or `root` isn't inside a work tree. Runs
/// synchronously — callers throttle (see [`SidePanel::sync`]).
pub(crate) fn read_git(root: &Path) -> Option<GitInfo> {
    use std::process::Command;
    // `safe.directory` scoped to this one invocation: repos owned by another
    // user — most commonly a `\\wsl$\…` UNC root, where every file belongs to
    // the WSL distro — make git bail with "dubious ownership" and the Git view
    // silently blanked while `git status` in the user's own shell worked fine
    // (个别情况 status 可用但面板不显示). Read-only status/diff on a directory
    // the user is already browsing carries none of the write risks the global
    // opt-in guards against.
    let safe_directory = format!("safe.directory={}", root.display());
    let location = root.display().to_string();
    collect_git_info(|args| {
        let mut cmd = Command::new("git");
        cmd.args(["-c", &safe_directory, "--no-optional-locks"]).args(args).current_dir(root);
        run_git(cmd, args, &location, None)
    })
}

/// 在 WSL 来宾里读 git 状态：`wsl.exe -d <发行版> -- git -C <来宾目录> …`。
///
/// # 为什么不走宿主 git
///
/// 宿主 git 要读 WSL 仓库，只能经 `\\wsl.localhost\…` UNC，而那条路依赖 9P
/// 文件重定向——实测在 WSL 2.7.8 + Windows 22631 上完全不可达（见
/// [`crate::shell_detect::wsl_unc_cwd`]）。来宾里的 git 反而什么都不缺：
/// 输出格式与宿主 `git status --porcelain -b` 逐字相同，所以解析与宿主
/// 路径完全共用 [`collect_git_info`]，没有第二套解析要维护。
///
/// 也不需要 `safe.directory`：来宾里的仓库属于来宾用户自己，不会触发
/// dubious ownership——那本来就是跨 UNC 读别人的文件才有的问题。
///
/// 代价是每次快照多一次 `wsl.exe` 进程往返（发行版没运行时还会把它拉起
/// 来）。快照本来就在后台线程上、且有节流，不进渲染路径。
pub(crate) fn read_git_wsl(located: &crate::shell_detect::WslCwd) -> Option<GitInfo> {
    use std::process::Command;
    let location = format!("{}:{}", located.distro, located.guest);
    collect_git_info(|args| {
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-d", &located.distro, "--", "git", "-C", &located.guest, "--no-optional-locks"])
            .args(args);
        run_git(cmd, args, &location, Some(WSL_COMMAND_TIMEOUT))
    })
}

/// 跑一条已配好的 git 命令，取 stdout。`location` 只用于失败日志。
pub(crate) fn run_git(
    mut cmd: std::process::Command,
    args: &[&str],
    location: &str,
    timeout: Option<Duration>,
) -> Option<String> {
    // Suppress the console window that `Command` flashes on Windows GUI apps.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = match command_output_with_timeout(cmd, timeout) {
        Ok(output) => output,
        Err(error) => {
            log::warn!("git {:?} failed in {location}: {error}", args.first());
            return None;
        },
    };
    if !out.status.success() {
        // Leave a trace instead of a silent blank panel: the first stderr
        // line names the actual refusal (ownership, not-a-repo, …).
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr.lines().next().unwrap_or("unknown error");
        log::debug!("git {:?} failed in {location}: {reason}", args.first());
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 宿主与 WSL 来宾两条运行路径共用的 porcelain 解析。`run` 收一组 git 参数、
/// 回 stdout（失败即 `None`）。
pub(crate) fn collect_git_info(run: impl Fn(&[&str]) -> Option<String>) -> Option<GitInfo> {
    // `-b --porcelain` yields `## branch...upstream [ahead N]` + one `XY path`
    // per change, X = index (staged) status, Y = worktree status.
    let status = run(&["status", "--porcelain", "-b"])?;
    let mut info = GitInfo::default();
    for line in status.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            // `main...origin/main [ahead 1]` → `main`; detached prints as-is.
            info.branch = head.split("...").next().unwrap_or(head).to_string();
            if let Some(idx) = head.find("ahead ") {
                info.ahead = head[idx + 6..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
            }
        } else if line.len() > 3 {
            let x = line.as_bytes()[0] as char;
            let y = line.as_bytes()[1] as char;
            let path = line[3..].trim().to_string();
            if x == '?' || y == '?' {
                info.unstaged.push(('?', path));
                continue;
            }
            // Merge conflicts (VS Code's "Merge Changes" group). The path
            // also stays in staged/unstaged below so the legacy view keeps
            // rendering it untouched.
            if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
                info.conflicts.push(('U', path.clone()));
            }
            // One file can be in BOTH lists (partially staged).
            if x != ' ' {
                info.staged.push((x, path.clone()));
            }
            if y != ' ' {
                info.unstaged.push((y, path));
            }
        }
    }

    // `x files changed, 140 insertions(+), 69 deletions(-)` → (140, 69).
    if let Some(stat) = run(&["diff", "--shortstat", "HEAD"]) {
        for part in stat.split(',') {
            let num: u64 = part.trim().split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if part.contains("insertion") {
                info.plus = num;
            } else if part.contains("deletion") {
                info.minus = num;
            }
        }
    }
    Some(info)
}

/// Cheap preference hint for nested checkouts. `read_svn` still runs the
/// authoritative `svn info` command, so this hint cannot make detection
/// incorrect when metadata is represented differently by an SVN client.
pub(crate) fn svn_dir_hint(root: &Path) -> bool {
    !matches!(crate::svn_status::classify_dir(root), crate::svn_status::SvnDirKind::Plain)
}

/// `svn status` 每行：第 1 列 item 状态（M/A/D/C/?/!…），第 8 列起路径。
/// SVN 没有暂存区——所有变化都归 `unstaged`，与 Git 视图同列渲染。
pub(crate) fn parse_svn_status(status: &str) -> Vec<(char, String)> {
    status
        .lines()
        .filter_map(|line| {
            let mut chars = line.chars();
            let state = chars.next()?;
            if state == ' ' || line.len() < 9 {
                return None;
            }
            let path = line[8..].trim();
            (!path.is_empty()).then(|| (state, path.replace('\\', "/")))
        })
        .collect()
}

/// Snapshot SVN state for `root`. The CLI（`svn info`/`svn status`）is tried
/// first for maximum fidelity; machines without a command-line client
/// (TortoiseSVN installs GUI only) fall back to reading `.svn/wc.db`
/// directly（`svn_status` 模块，TSVNCache 同款路线）。`None` means the path
/// is not a working copy at all.
pub(crate) fn read_svn(root: &Path) -> Option<GitInfo> {
    match crate::svn_status::classify_dir(root) {
        crate::svn_status::SvnDirKind::Repository(repository_root) => {
            let summary = crate::svn_status::repository_summary(&repository_root);
            Some(GitInfo {
                vcs: VcsKind::SvnRepository,
                // 分支位在 SVN 下放修订号；版本库没有工作副本修订，放 HEAD。
                branch: match summary.head {
                    Some(head) => format!("版本库 · HEAD r{head}"),
                    None => "SVN 版本库".to_owned(),
                },
                repository_root: Some(repository_root),
                repository: Some(summary),
                ..GitInfo::default()
            })
        },
        crate::svn_status::SvnDirKind::WorkingCopy(_) => {
            read_svn_cli(root).map(fill_svn_conflicts).or_else(|| read_svn_wc_db(root))
        },
        // 少数客户端的元数据布局可能不同，仍给权威 CLI 一次识别机会。
        crate::svn_status::SvnDirKind::Plain => read_svn_cli(root).map(fill_svn_conflicts),
    }
}

/// CLI 输出没有单独的冲突列表：从 `unstaged` 里把 `C` 行登记到
/// [`GitInfo::conflicts`]（路径保留原位，合同见字段注释）。
pub(crate) fn fill_svn_conflicts(mut info: GitInfo) -> GitInfo {
    info.conflicts = info
        .unstaged
        .iter()
        .filter(|(state, _)| *state == 'C')
        .map(|(_, path)| ('C', path.clone()))
        .collect();
    info
}

/// 零外部依赖的 SVN 快照：`svn_status` 读 `.svn/wc.db` 推导状态字母表，
/// 修订号取 NODES 根行。路径统一转成相对 `root` 的正斜杠形式，与 CLI
/// `svn status` 的展示合同一致（只显示 `root` 之下的条目）。
pub(crate) fn read_svn_wc_db(root: &Path) -> Option<GitInfo> {
    let crate::svn_status::SvnDirKind::WorkingCopy(wc_root) = crate::svn_status::classify_dir(root)
    else {
        return None;
    };
    let changes = crate::svn_status::working_copy_status(&wc_root).ok()?;
    let revision = crate::svn_status::working_copy_revision(&wc_root);
    let mut info = GitInfo {
        vcs: VcsKind::Svn,
        branch: revision.map(|value| format!("r{value}")).unwrap_or_else(|| "svn".to_owned()),
        ..GitInfo::default()
    };
    let prefix = root.strip_prefix(&wc_root).ok().map(|relative| {
        let mut text = relative.to_string_lossy().replace('\\', "/");
        if !text.is_empty() && !text.ends_with('/') {
            text.push('/');
        }
        text
    });
    for change in changes {
        let shown = match prefix.as_deref() {
            None | Some("") => change.rel_path.clone(),
            Some(prefix) => match change.rel_path.strip_prefix(prefix) {
                Some(inside) => inside.to_owned(),
                // `root` 子目录之外的变更不在本视图的展示合同内。
                None => continue,
            },
        };
        let state = change.state.letter().chars().next().unwrap_or('M');
        if change.state == crate::svn_status::SvnState::Conflicted {
            info.conflicts.push(('C', shown.clone()));
        }
        // SVN 无暂存区：全部归 unstaged（GPUI 会把冲突条目过滤出去单独分组）。
        info.unstaged.push((if state == 'U' { '?' } else { state }, shown));
    }
    Some(info)
}

/// CLI 路线（现代客户端的权威路径）。
pub(crate) fn read_svn_cli(root: &Path) -> Option<GitInfo> {
    use std::process::Command;
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = Command::new("svn");
        // 交互式认证提示会把无头子进程挂死；快照必须是非交互的。
        cmd.arg("--non-interactive").args(args).current_dir(root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr.lines().next().unwrap_or("unknown error");
            log::debug!("svn {:?} failed in {}: {reason}", args.first(), root.display());
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // `--show-item` is available in modern SVN. Fall back to regular
    // `svn info` so older clients still produce a revision number.
    let revision = run(&["info", "--show-item", "revision"])
        .and_then(|value| (!value.trim().is_empty()).then_some(value))
        .or_else(|| run(&["info"]).and_then(|value| parse_svn_revision(&value)))?;
    let status = run(&["status"])?;
    Some(GitInfo {
        vcs: VcsKind::Svn,
        branch: format!("r{}", revision.trim()),
        unstaged: parse_svn_status(&status),
        ..GitInfo::default()
    })
}

pub(crate) fn parse_svn_revision(info: &str) -> Option<String> {
    info.lines().find_map(|line| {
        let value = line.strip_prefix("Revision:")?.trim();
        (!value.is_empty()).then_some(value.to_owned())
    })
}
