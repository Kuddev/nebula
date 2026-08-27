//! 非 TTY 的 pane 进程执行。
//!
//! `pane.run` 仍然把命令送进现有交互 shell；这里刻意走独立 child，确保
//! stdout/stderr 可以分开捕获，也不会改变 shell history、cwd 或环境。公开参数是
//! argv 而不是命令行字符串，因此不存在一层隐式 shell 展开。

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime_api::{ApiError, RuntimeDispatch};

#[derive(Clone, Debug)]
enum ExecLocation {
    Host,
    Wsl { distro: Option<String> },
}

/// 创建 pane 时冻结的、可由独立 child 安全复用的上下文。
///
/// 环境语义是“继承 Nebula 宿主进程，再叠加 PTY 创建时的 overrides”。运行中
/// shell 里的 `export` / `$env:` 不会反向流回父进程，因此不能假装能继承它们。
#[derive(Clone, Debug)]
pub(crate) struct PaneExecContext {
    location: ExecLocation,
    env: HashMap<String, String>,
    fallback_cwd: Option<PathBuf>,
}

impl PaneExecContext {
    pub(crate) fn from_pty_options(options: &nebula_terminal::tty::Options) -> Self {
        let location = options
            .shell
            .as_ref()
            .filter(|shell| is_wsl_program(shell.program()))
            .map_or(ExecLocation::Host, |shell| ExecLocation::Wsl {
                distro: crate::shell_detect::wsl_launch_distro(shell.program(), shell.args())
                    .map(str::to_owned),
            });
        Self {
            location,
            env: options.env.clone(),
            fallback_cwd: options
                .working_directory
                .clone()
                .or_else(|| std::env::current_dir().ok()),
        }
    }

    #[cfg(test)]
    fn host(cwd: PathBuf) -> Self {
        Self { location: ExecLocation::Host, env: HashMap::new(), fallback_cwd: Some(cwd) }
    }
}

fn is_wsl_program(program: &str) -> bool {
    Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("wsl"))
}

#[derive(Debug)]
struct CapturedBytes {
    bytes: Vec<u8>,
    total_bytes: u64,
}

#[derive(Debug, Serialize)]
struct CapturedText {
    text: String,
    encoding: &'static str,
    total_bytes: u64,
    captured_bytes: usize,
    truncated: bool,
}

impl CapturedBytes {
    fn into_text(self) -> CapturedText {
        let captured_bytes = self.bytes.len();
        let truncated = self.total_bytes > captured_bytes as u64;
        match String::from_utf8(self.bytes) {
            Ok(text) => CapturedText {
                text,
                encoding: "utf-8",
                total_bytes: self.total_bytes,
                captured_bytes,
                truncated,
            },
            Err(error) => CapturedText {
                text: String::from_utf8_lossy(error.as_bytes()).into_owned(),
                encoding: "utf-8-lossy",
                total_bytes: self.total_bytes,
                captured_bytes,
                truncated,
            },
        }
    }
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<CapturedBytes> {
    let mut kept = Vec::with_capacity(limit.min(64 * 1024));
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        let remaining = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(CapturedBytes { bytes: kept, total_bytes })
}

fn capture_thread(
    name: &'static str,
    reader: impl Read + Send + 'static,
    limit: usize,
) -> io::Result<std::thread::JoinHandle<io::Result<CapturedBytes>>> {
    std::thread::Builder::new().name(name.to_owned()).spawn(move || read_bounded(reader, limit))
}

fn host_cwd(reported: &str, fallback: Option<&Path>) -> Result<PathBuf, ApiError> {
    if !reported.trim().is_empty() {
        let path = PathBuf::from(reported);
        if path.is_dir() {
            return Ok(path);
        }
        return Err(ApiError::new(
            "exec_cwd_unavailable",
            "the pane's reported working directory is not a local filesystem directory",
        )
        .details(json!({ "cwd": reported })));
    }
    fallback.filter(|path| path.is_dir()).map(Path::to_path_buf).ok_or_else(|| {
        ApiError::new(
            "exec_cwd_unavailable",
            "the pane has not reported a usable local working directory",
        )
    })
}

fn build_command(
    context: &PaneExecContext,
    reported_cwd: &str,
    argv: &[String],
) -> Result<(Command, Value), ApiError> {
    let program = argv.first().expect("validated pane.exec argv");
    let mut command;
    let execution;
    match &context.location {
        ExecLocation::Host => {
            let cwd = host_cwd(reported_cwd, context.fallback_cwd.as_deref())?;
            command = Command::new(program);
            command.args(&argv[1..]).current_dir(&cwd);
            execution = json!({ "environment": "host", "cwd": cwd });
        },
        ExecLocation::Wsl { distro } => {
            command = Command::new("wsl.exe");
            if let Some(distro) = distro {
                command.args(["--distribution", distro]);
            }
            let guest_cwd = crate::shell_detect::wsl_guest_cwd(reported_cwd);
            if let Some(cwd) = guest_cwd {
                command.args(["--cd", cwd]);
            } else if !reported_cwd.trim().is_empty() {
                // WSL accepts a host cwd by inheriting CreateProcessW's working directory.
                let cwd = host_cwd(reported_cwd, context.fallback_cwd.as_deref())?;
                command.current_dir(cwd);
            }
            command.arg("--exec").arg(program).args(&argv[1..]);
            execution = json!({
                "environment": "wsl",
                "distribution": distro,
                "cwd": guest_cwd
            });
        },
    }
    command.envs(&context.env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok((command, execution))
}

pub(crate) fn spawn(
    dispatch: Arc<RuntimeDispatch>,
    context: PaneExecContext,
    cwd: String,
    argv: Vec<String>,
    timeout_ms: u64,
    max_output_bytes: usize,
) {
    let worker_dispatch = dispatch.clone();
    let result = std::thread::Builder::new().name("nebula-pane-exec".to_owned()).spawn(move || {
        worker_dispatch.respond(execute(context, cwd, argv, timeout_ms, max_output_bytes));
    });
    if let Err(error) = result {
        dispatch.respond(Err(ApiError::new(
            "exec_spawn_failed",
            format!("failed to start the pane.exec worker: {error}"),
        )));
    }
}

fn execute(
    context: PaneExecContext,
    cwd: String,
    argv: Vec<String>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<Value, ApiError> {
    let started = Instant::now();
    let (mut command, execution) = build_command(&context, &cwd, &argv)?;
    let mut child = command.spawn().map_err(|error| {
        ApiError::new("exec_spawn_failed", format!("failed to start {:?}: {error}", argv[0]))
            .details(json!({ "argv": argv, "execution": execution }))
    })?;
    let group = ProcessGroup::attach(&child).map_err(|error| {
        let _ = child.kill();
        let _ = child.wait();
        ApiError::new(
            "exec_spawn_failed",
            format!("failed to isolate the pane.exec process: {error}"),
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ApiError::new("exec_capture_failed", "pane.exec child did not expose stdout")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ApiError::new("exec_capture_failed", "pane.exec child did not expose stderr")
    })?;
    // 两条 pipe 必须并行排水；顺序 read_to_end 会在另一条 pipe 填满时死锁。
    let stdout = capture_thread("nebula-pane-exec-stdout", stdout, max_output_bytes)
        .map_err(|error| ApiError::new("exec_capture_failed", error.to_string()))?;
    let stderr = capture_thread("nebula-pane-exec-stderr", stderr, max_output_bytes)
        .map_err(|error| ApiError::new("exec_capture_failed", error.to_string()))?;

    let timeout = Duration::from_millis(timeout_ms);
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status, false),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            },
            Ok(None) => {
                group.terminate(&mut child);
                let status = child.wait().map_err(|error| {
                    ApiError::new(
                        "exec_wait_failed",
                        format!("failed to reap timed-out pane.exec child: {error}"),
                    )
                })?;
                break (status, true);
            },
            Err(error) => {
                group.terminate(&mut child);
                let _ = child.wait();
                return Err(ApiError::new(
                    "exec_wait_failed",
                    format!("failed while waiting for pane.exec child: {error}"),
                ));
            },
        }
    };
    // 直接进程退出后仍存活的后台后代也属于这次一次性 exec；先收掉它们，
    // stdout/stderr reader 才不会因继承的 pipe handle 永久等不到 EOF。
    group.finish();

    let stdout = join_capture(stdout, "stdout")?.into_text();
    let stderr = join_capture(stderr, "stderr")?.into_text();
    // Keep the common path as simple as `result.stdout`, while retaining the
    // byte/encoding evidence needed to reason about truncation and lossy UTF-8.
    let capture = json!({
        "stdout": {
            "encoding": stdout.encoding,
            "total_bytes": stdout.total_bytes,
            "captured_bytes": stdout.captured_bytes,
            "truncated": stdout.truncated,
        },
        "stderr": {
            "encoding": stderr.encoding,
            "total_bytes": stderr.total_bytes,
            "captured_bytes": stderr.captured_bytes,
            "truncated": stderr.truncated,
        }
    });
    Ok(json!({
        "argv": argv,
        "execution": execution,
        "exit_code": status.code(),
        "success": status.success() && !timed_out,
        "timed_out": timed_out,
        "duration_ms": started.elapsed().as_millis() as u64,
        "stdout": stdout.text,
        "stderr": stderr.text,
        "capture": capture,
        "stdout_is_tty": false,
        "stderr_is_tty": false
    }))
}

fn join_capture(
    handle: std::thread::JoinHandle<io::Result<CapturedBytes>>,
    stream: &str,
) -> Result<CapturedBytes, ApiError> {
    handle
        .join()
        .map_err(|_| ApiError::new("exec_capture_failed", format!("{stream} reader panicked")))?
        .map_err(|error| {
            ApiError::new(
                "exec_capture_failed",
                format!("failed to read pane.exec {stream}: {error}"),
            )
        })
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_: &mut Command) {}

struct ProcessGroup {
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    process_group: i32,
}

impl ProcessGroup {
    fn attach(child: &Child) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::mem::{size_of, zeroed};
            use std::os::windows::io::AsRawHandle as _;
            use std::ptr;
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };

            // SAFETY: all pointers reference initialized POD values for the duration of each
            // call. The returned job handle is owned by ProcessGroup and closed exactly once.
            unsafe {
                let job = CreateJobObjectW(ptr::null(), ptr::null());
                if job.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    let error = io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error);
                }
                if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error);
                }
                return Ok(Self { job });
            }
        }
        #[cfg(unix)]
        {
            Ok(Self { process_group: child.id() as i32 })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    fn terminate(&self, child: &mut Child) {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            let _ = TerminateJobObject(self.job, 1);
        }
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(-self.process_group, libc::SIGKILL);
        }
        let _ = child.kill();
    }

    fn finish(self) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(-self.process_group, libc::SIGKILL);
        }
        // Windows uses JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE in Drop.
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: `job` is the live handle created and exclusively owned by this guard.
            unsafe {
                let _ = windows_sys::Win32::Foundation::CloseHandle(self.job);
            }
        }
        #[cfg(unix)]
        {
            // A reader-thread creation failure must not leave the child tree
            // alive with one inherited pipe still open.
            unsafe {
                let _ = libc::kill(-self.process_group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_drains_but_keeps_only_the_limit() {
        let captured = read_bounded(std::io::Cursor::new(b"abcdefghij"), 4).unwrap();
        assert_eq!(captured.bytes, b"abcd");
        assert_eq!(captured.total_bytes, 10);
        let text = captured.into_text();
        assert!(text.truncated);
        assert_eq!(text.captured_bytes, 4);
    }

    #[test]
    fn independent_child_keeps_stdout_and_stderr_separate() {
        let cwd = std::env::current_dir().unwrap();
        #[cfg(windows)]
        let argv = vec![
            "cmd.exe".to_owned(),
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "(echo out)&(echo err 1>&2)&exit /b 7".to_owned(),
        ];
        #[cfg(not(windows))]
        let argv =
            vec!["sh".to_owned(), "-c".to_owned(), "printf out; printf err >&2; exit 7".to_owned()];

        let result = execute(PaneExecContext::host(cwd), String::new(), argv, 5_000, 1024)
            .expect("child executes");
        assert_eq!(result["exit_code"], 7);
        assert!(result["stdout"].as_str().unwrap().contains("out"));
        assert!(result["stderr"].as_str().unwrap().contains("err"));
        assert_eq!(result["capture"]["stdout"]["encoding"], "utf-8");
        assert_eq!(result["stdout_is_tty"], false);
        assert_eq!(result["stderr_is_tty"], false);
    }
}
