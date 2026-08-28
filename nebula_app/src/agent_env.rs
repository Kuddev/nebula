//! 本地 pane 的 Agent 身份契约。
//!
//! 让 pane 里的任何进程——尤其是 Codex / Claude Code 这类 AI CLI——在不读源码、
//! 不扫进程、不猜端口的前提下回答三个问题：
//!
//! | 问题 | 变量 |
//! | --- | --- |
//! | 我在什么终端里？ | `TERM_PROGRAM` / `TERM_PROGRAM_VERSION` |
//! | 我是哪个 pane？ | `NEBULA_PANE_ID` |
//! | 控制面在哪、怎么调？ | `NEBULA_CLI` / `NEBULA_BIN_DIR` / `PATH` |
//!
//! 前两行是**识别**，第三行是**可达性**，两者都不解决"什么时候该调"——那是
//! Skill 的职责。三者不能互相替代：没有身份契约，Skill 里写的命令是空头承诺；
//! 没有 Skill，Agent 至少还能靠 `nebula env` 自己发现完整能力清单。把发现层
//! 全押在 Skill 上（用户必须手工安装一段提示词）是不可靠的单点。
//!
//! # 只进本地 PTY
//!
//! 这些变量只注入本机 PTY。SSH pane 走
//! [`crate::ssh_session`]，那里注入的是 `NEBULA_PANE_REMOTE=1`——远端主机上
//! 没有这个 `nebula.exe`，把本机绝对路径送过去只会造出一个"变量有了但永远
//! 执行不了"的伪能力，还会绕开远端 pane 不得取用本地上下文的护栏。
//!
//! # 幂等
//!
//! 在 Nebula 的 pane 里再开一个 Nebula 是常规操作（嵌套 shell、`wsl.exe`、
//! 测试用的隔离实例）。因此 `PATH` 前置与 `WSLENV` 追加都按值判重：套多少层，
//! 环境块都不增长。

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};

pub(crate) use crate::ai_hook::PANE_ENV;

/// `TERM_PROGRAM` 的取值。这是程序判断"我跑在哪个终端里"的事实标准入口，
/// 其它终端（`vscode`、`iTerm.app`、`WezTerm`…）都用同一个变量，所以第三方
/// 工具的既有识别逻辑不需要为 Nebula 改代码。
pub const TERM_PROGRAM: &str = "nebula";

/// Nebula 可执行文件的绝对路径。
///
/// 便携版（解压即用）不一定在 `PATH` 上，而 Agent 需要的是"现在就能执行"，
/// 不是"也许装过"。给精确路径就不必让模型去猜安装位置。
pub const CLI_ENV: &str = "NEBULA_CLI";

/// [`CLI_ENV`] 所在目录，同时被前置到 `PATH`，这样 `nebula` 裸命令也可用。
pub const BIN_DIR_ENV: &str = "NEBULA_BIN_DIR";

const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";
const TERM_PROGRAM_VERSION_ENV: &str = "TERM_PROGRAM_VERSION";
const PATH_ENV: &str = "PATH";

/// 把身份契约写进一个本地 PTY 的环境表。
///
/// 调用点必须在所有其它 `env` 写入**之后**：`WSLENV` 是共享变量，
/// [`crate::shell_detect::wsl_cwd_report_env`] 也会往里追加条目，本函数以
/// 环境表里的现值为基准合并，从而与调用顺序无关地保住两边的条目。
pub fn apply(env: &mut HashMap<String, String>, pane_id: impl Display) {
    insert_env(env, PANE_ENV, pane_id.to_string());
    insert_env(env, TERM_PROGRAM_ENV, TERM_PROGRAM.to_owned());
    insert_env(env, TERM_PROGRAM_VERSION_ENV, env!("VERSION").to_owned());

    if let Some(executable) = executable() {
        insert_env(env, CLI_ENV, executable.display().to_string());
        if let Some(directory) = executable.parent() {
            insert_env(env, BIN_DIR_ENV, directory.display().to_string());
            if let Some(path) = prepended_path(directory, env_value(env, PATH_ENV)) {
                insert_env(env, PATH_ENV, path);
            }
        }
    }

    merge_wslenv(env);
}

fn env_value<'a>(env: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    #[cfg(windows)]
    {
        env.iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
    #[cfg(not(windows))]
    {
        env.get(name).map(String::as_str)
    }
}

fn insert_env(env: &mut HashMap<String, String>, name: &str, value: String) {
    #[cfg(windows)]
    env.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
    env.insert(name.to_owned(), value);
}

/// 当前运行的 Nebula 可执行文件。
///
/// 优先 `current_exe()` 而不是继承来的 [`CLI_ENV`]：嵌套启动时，继承值指向
/// 外层那个二进制，可能已经是旧版本或另一个便携副本；PTY 应当指向**正在为它
/// 提供控制面的那个**进程。
pub fn executable() -> Option<PathBuf> {
    std::env::current_exe().ok().or_else(|| std::env::var_os(CLI_ENV).map(PathBuf::from))
}

/// 把 `directory` 挪到 `PATH` 首位，返回新值；已经在首位时返回 `None`。
///
/// 先移除所有等值项再前置，所以重复调用是稳定的——否则每层嵌套都会给环境块
/// 多加一份同样的目录。
fn prepended_path(directory: &Path, current: Option<&str>) -> Option<String> {
    let current = match current {
        Some(value) => OsString::from(value),
        None => std::env::var_os(PATH_ENV).unwrap_or_default(),
    };
    let mut entries: Vec<PathBuf> = std::env::split_paths(&current).collect();
    if entries.first().is_some_and(|first| same_directory(first, directory)) {
        return None;
    }
    entries.retain(|entry| !same_directory(entry, directory));
    entries.insert(0, directory.to_path_buf());
    // `join_paths` 在某条目自带分隔符时失败。那种 `PATH` 本来就已经坏了，
    // 保留原值比写回一个被截断的列表安全。
    std::env::join_paths(entries).ok().map(|joined| joined.to_string_lossy().into_owned())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        // Windows 路径大小写不敏感：`D:\Nebula` 与 `d:\nebula` 是同一个目录，
        // 不归一化判重就等于没判。
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// 需要穿过 WSL 边界的条目。
///
/// `/p` 让 WSL 把 Windows 路径翻译成 `/mnt/...`；不加这个标志，来宾 shell 里
/// 拿到的 `NEBULA_CLI` 是一个 `D:\…` 字面量，在 Linux 侧谁也执行不了。
/// 带标志位的条目只能写成字面量（`concat!` 不接受常量），[`wslenv_entries_match_variables`]
/// 负责在变量改名时让编译测试失败。
#[cfg(windows)]
const WSLENV_ENTRIES: &[&str] =
    &[PANE_ENV, TERM_PROGRAM_ENV, TERM_PROGRAM_VERSION_ENV, "NEBULA_CLI/p", "NEBULA_BIN_DIR/p"];

/// 把 [`WSLENV_ENTRIES`] 合并进 `WSLENV`，保留已有条目。
///
/// 基准依次取：本次 PTY 环境表里的现值（`wsl_cwd_report_env` 可能刚写过
/// `PROMPT_COMMAND`）→ 宿主进程的值（别的工具设的）。两个来源都不能丢，否则
/// 会静默掐断另一方的透传。
#[cfg(windows)]
fn merge_wslenv(env: &mut HashMap<String, String>) {
    const WSLENV: &str = "WSLENV";
    let existing = env_value(env, WSLENV)
        .map(str::to_owned)
        .or_else(|| std::env::var(WSLENV).ok())
        .unwrap_or_default();
    let mut entries: Vec<String> =
        existing.split(':').filter(|entry| !entry.is_empty()).map(str::to_owned).collect();
    for entry in WSLENV_ENTRIES {
        // 只按变量名判重：宿主若已用别的标志位传同一个变量，尊重它的选择，
        // 不去覆盖成我们的标志。
        let name = variable_name(entry);
        if !entries.iter().any(|existing| variable_name(existing) == name) {
            entries.push((*entry).to_owned());
        }
    }
    insert_env(env, WSLENV, entries.join(":"));
}

/// `WSLENV` 条目里的变量名部分（`NEBULA_CLI/p` → `NEBULA_CLI`）。
#[cfg(windows)]
fn variable_name(entry: &str) -> &str {
    entry.split('/').next().unwrap_or(entry)
}

#[cfg(not(windows))]
fn merge_wslenv(_env: &mut HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(pane_id: u64) -> HashMap<String, String> {
        let mut env = HashMap::new();
        apply(&mut env, pane_id);
        env
    }

    #[test]
    fn identity_is_complete() {
        let env = applied(17);
        assert_eq!(env.get(PANE_ENV).map(String::as_str), Some("17"));
        assert_eq!(env.get("TERM_PROGRAM").map(String::as_str), Some(TERM_PROGRAM));
        assert_eq!(env.get("TERM_PROGRAM_VERSION").map(String::as_str), Some(env!("VERSION")));
        // 测试二进制的 `current_exe` 一定存在，所以可达性字段必须齐。
        let cli = PathBuf::from(env.get(CLI_ENV).expect("cli path"));
        let bin_dir = PathBuf::from(env.get(BIN_DIR_ENV).expect("bin dir"));
        assert_eq!(cli.parent(), Some(bin_dir.as_path()));
    }

    #[test]
    fn cli_directory_leads_path() {
        let env = applied(1);
        let bin_dir = PathBuf::from(env.get(BIN_DIR_ENV).expect("bin dir"));
        let path = env.get("PATH").expect("PATH is rewritten when it lacks the bin dir");
        let first = std::env::split_paths(path).next().expect("first PATH entry");
        assert!(same_directory(&first, &bin_dir), "{first:?} should lead PATH");
    }

    #[cfg(windows)]
    #[test]
    fn agent_path_uses_the_refreshed_environment() {
        let fresh = PathBuf::from(r"C:\fresh-registry-path");
        let mut env =
            HashMap::from([("Path".to_owned(), fresh.as_os_str().to_string_lossy().into_owned())]);

        apply(&mut env, 2);

        let bin_dir = PathBuf::from(env.get(BIN_DIR_ENV).expect("bin dir"));
        let entries: Vec<PathBuf> =
            std::env::split_paths(env.get(PATH_ENV).expect("PATH")).collect();
        assert_eq!(entries, vec![bin_dir, fresh]);
        assert_eq!(env.keys().filter(|name| name.eq_ignore_ascii_case(PATH_ENV)).count(), 1);
    }

    #[test]
    fn path_prepend_is_idempotent() {
        let directory = Path::new("/opt/nebula");
        let joined = std::env::join_paths(["/usr/bin", "/bin"]).expect("join");
        let base = joined.to_string_lossy().into_owned();

        let once =
            prepended_path(directory, Some(base.as_str())).expect("first call rewrites PATH");
        // 已在首位 → 不再重写，嵌套启动因此不会让环境块增长。
        assert_eq!(prepended_path(directory, Some(once.as_str())), None);

        // 目录在中段时移到首位，且不留下重复项。
        let middle = std::env::join_paths(["/usr/bin", "/opt/nebula", "/bin"]).expect("join");
        let middle = middle.to_string_lossy().into_owned();
        let moved =
            prepended_path(directory, Some(middle.as_str())).expect("a non-leading entry is moved");
        let entries: Vec<PathBuf> = std::env::split_paths(&moved).collect();
        assert!(same_directory(&entries[0], directory));
        assert_eq!(entries.iter().filter(|entry| same_directory(entry, directory)).count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn wslenv_entries_match_variables() {
        // 透传表里的路径条目是字面量。变量改名而这里忘记跟着改，就会静默丢掉
        // WSL 侧的可达性——用一个断言把它变成编译期之后立刻可见的失败。
        let names: Vec<&str> = WSLENV_ENTRIES.iter().copied().map(variable_name).collect();
        assert!(names.contains(&CLI_ENV), "{CLI_ENV} missing from WSLENV passthrough: {names:?}");
        assert!(names.contains(&BIN_DIR_ENV), "{BIN_DIR_ENV} missing: {names:?}");
    }

    #[cfg(windows)]
    #[test]
    fn wslenv_keeps_foreign_entries() {
        let mut env = HashMap::new();
        // `shell_detect::wsl_cwd_report_env` 先写过 cwd 上报的透传条目。
        env.insert("WSLENV".to_owned(), "PROMPT_COMMAND".to_owned());
        apply(&mut env, 3);

        let wslenv = env.get("WSLENV").expect("WSLENV");
        let entries: Vec<&str> = wslenv.split(':').collect();
        assert!(entries.contains(&"PROMPT_COMMAND"), "cwd reporting must survive: {wslenv}");
        assert!(entries.contains(&PANE_ENV), "pane identity must cross into WSL: {wslenv}");
        // 路径必须带 `/p`，否则来宾拿到无法执行的 `D:\…` 字面量。
        assert!(entries.contains(&"NEBULA_CLI/p"), "cli path needs translation: {wslenv}");
    }

    #[cfg(windows)]
    #[test]
    fn wslenv_merge_is_idempotent() {
        let mut env = HashMap::new();
        apply(&mut env, 4);
        let once = env.get("WSLENV").cloned().expect("WSLENV");
        apply(&mut env, 4);
        assert_eq!(env.get("WSLENV"), Some(&once));
    }

    #[cfg(windows)]
    #[test]
    fn wslenv_respects_foreign_flags_for_same_variable() {
        let mut env = HashMap::new();
        // 宿主已用 `/l`（列表）传同名变量：尊重它，不改成我们的 `/p`，
        // 否则会破坏那个工具原有的转换语义。
        env.insert("WSLENV".to_owned(), "NEBULA_CLI/l".to_owned());
        apply(&mut env, 5);

        let wslenv = env.get("WSLENV").expect("WSLENV");
        let entries: Vec<&str> = wslenv.split(':').collect();
        assert!(entries.contains(&"NEBULA_CLI/l"));
        assert!(!entries.contains(&"NEBULA_CLI/p"));
    }
}
