//! 非本机 pane 的目录列表：补齐要问的是**来宾 / 远端**的文件系统。
//!
//! # 为什么要一层缓存
//!
//! 按键路径上不做 IO。WSL 一次 `wsl.exe -- find` 冷启动实测可达 7.5 秒（见
//! [`crate::display::side_panel`] 里 `WSL_COMMAND_TIMEOUT` 的实测记录），SSH 要
//! 一次网络往返——这种延迟挂在每次 Tab 上是不可用的。
//!
//! 所以分工是：[`suggest_update`](crate::display::suggest_engine::suggest_update)
//! 只**读**这里的缓存，miss 时把目录登记到
//! `NebulaPaneState::pending_remote_dir`；壳看到这个登记去异步拉取，回填后下
//! 一次重算就有候选了。用户的体感是"第一次 Tab 没反应，之后都有" ——
//! 而不是"每次 Tab 卡住整个 UI"。
//!
//! 缓存是进程级的：同一个发行版开三个 tab，`/usr/bin` 只拉一次。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::display::SuggestEnv;

/// 来宾 / 远端目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 缓存寿命。
///
/// 目录内容当然会变（用户就在那个 shell 里 `mkdir`），但补齐的职责是"少打
/// 字"，不是"当文件系统的权威视图"：拿到几十秒前的列表最坏也就是少补一项，
/// 真打错了 shell 自己会报。反过来，TTL 太短会让每次 Tab 都触发一次子进程 /
/// 网络往返，那正是这层缓存要消灭的东西。
const TTL: Duration = Duration::from_secs(45);

struct Cached {
    entries: Vec<RemoteEntry>,
    fetched: Instant,
}

#[derive(Default)]
struct Cache {
    by_dir: HashMap<String, Cached>,
    /// 正在拉取的目录。连按 Tab 不该排出一串子进程。
    inflight: HashSet<String>,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

/// 缓存代际：每有一份新目录落地就 +1。
///
/// 补齐把结果缓存在 `cwd + 行` 上，而异步拉取回来时这两者都没变——少了这个
/// 计数，拉回来的条目要等用户再多打一个字符才会显形。与 PATH 探针填充命令
/// 目录时用的 `command_generation` 是同一手法。
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 当前缓存代际，进补齐的重算键。
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

fn cache() -> MutexGuard<'static, Cache> {
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 缓存键。环境必须进键：同一条 `/home/kud` 在 Debian 来宾和某台远端上是两
/// 个不同的目录，混用会把一台机器的文件补进另一台的命令行。
fn key(env: &SuggestEnv, dir: &str) -> String {
    match env {
        SuggestEnv::Local => format!("local\u{0}{dir}"),
        SuggestEnv::Wsl { distro } => format!("wsl:{distro}\u{0}{dir}"),
        SuggestEnv::Ssh { destination } => format!("ssh:{destination}\u{0}{dir}"),
    }
}

/// 已缓存且未过期的条目。
pub fn lookup(env: &SuggestEnv, dir: &str) -> Option<Vec<RemoteEntry>> {
    let cache = cache();
    let cached = cache.by_dir.get(&key(env, dir))?;
    (cached.fetched.elapsed() < TTL).then(|| cached.entries.clone())
}

/// 认领一次拉取。`false` = 已经有人在拉这个目录，调用方不要再起一个。
pub fn begin_fetch(env: &SuggestEnv, dir: &str) -> bool {
    cache().inflight.insert(key(env, dir))
}

/// 拉取结束。`None` = 这次失败了——只解锁不落缓存，下次还会重试；落一个空
/// 列表会让"暂时连不上"在整个 TTL 里都表现成"这个目录是空的"。
pub fn finish_fetch(env: &SuggestEnv, dir: &str, entries: Option<Vec<RemoteEntry>>) {
    let key = key(env, dir);
    let mut cache = cache();
    cache.inflight.remove(&key);
    if let Some(entries) = entries {
        cache.by_dir.insert(key, Cached { entries, fetched: Instant::now() });
        GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// 从来宾里列一个目录。**阻塞**（子进程往返），只能在后台线程上调用。
pub fn fetch_wsl(distro: &str, dir: &str) -> Option<Vec<RemoteEntry>> {
    let entries = crate::display::side_panel::wsl_list_one_dir(distro, dir)?;
    Some(entries.into_iter().map(|(is_dir, name)| RemoteEntry { name, is_dir }).collect())
}

/// 一个补齐请求要问哪个目录、拿什么前缀去筛。
///
/// 与本机分支的区别在于这里只认 POSIX 语义：`\` 在 Linux 上是合法文件名字符
/// 而不是分隔符，把它当分隔符会把 `a\ b`（转义的空格）切成两段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRequest {
    /// 要列的绝对目录。
    pub dir: String,
    /// 目录里用来筛条目的前缀。
    pub prefix: String,
    /// token 里属于路径部分的原文（`src/ma` 的 `src/`），拼回候选时要带上。
    pub dir_part: String,
}

/// 把最后一个 token 拆成"问哪个目录 + 筛什么前缀"。
///
/// `None` = 这个 token 问不出目录来：`~` 要来宾自己展开（宿主不知道来宾的
/// home），空 cwd 时相对路径也无从谈起。
pub fn path_request(token: &str, cwd: &str) -> Option<PathRequest> {
    if token.starts_with('~') {
        return None;
    }
    let (dir_part, prefix) = match token.rfind('/') {
        Some(index) => (&token[..=index], &token[index + 1..]),
        None => ("", token),
    };
    let dir = if dir_part.starts_with('/') {
        // 绝对路径：`/usr/` → `/usr`，`/` → `/`。
        let trimmed = dir_part.trim_end_matches('/');
        if trimmed.is_empty() { "/".to_owned() } else { trimmed.to_owned() }
    } else {
        let cwd = cwd.trim_end_matches('/');
        if cwd.is_empty() || !cwd.starts_with('/') {
            // cwd 还没上报，或它根本不是 POSIX 路径（WSL tab 在 shell 发出
            // 第一个 OSC 7 之前就是这样）。相对路径这时无解。
            return None;
        }
        match dir_part.trim_end_matches('/') {
            "" => cwd.to_owned(),
            relative => format!("{cwd}/{relative}"),
        }
    };
    Some(PathRequest { dir, prefix: prefix.to_owned(), dir_part: dir_part.to_owned() })
}

/// 目录条目 → 补齐候选的插入余量（用户已经打了 `prefix`，补的是剩下的）。
///
/// 返回 `(余量, 是否目录)`。目录带尾随 `/`，和本机分支一样一次补一段，
/// 让用户可以连按 Tab 往下钻。
pub fn candidates(request: &PathRequest, entries: &[RemoteEntry]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = entries
        .iter()
        .filter(|entry| {
            // 点文件只在用户明确打了 `.` 时出现；否则一个 `ls ` 会被 `.bashrc`
            // 之类淹掉。
            if entry.name.starts_with('.') && !request.prefix.starts_with('.') {
                return false;
            }
            entry.name.len() > request.prefix.len() && entry.name.starts_with(&request.prefix)
        })
        .map(|entry| {
            let mut rest = entry.name[request.prefix.len()..].to_owned();
            if entry.is_dir {
                rest.push('/');
            }
            (rest, entry.is_dir)
        })
        .collect();
    // 短的优先（离用户打的最近），同长按字典序——与 `nebula_command_hints`
    // 的收敛规则一致。
    out.sort_by(|left, right| {
        left.0.chars().count().cmp(&right.0.chars().count()).then_with(|| left.0.cmp(&right.0))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_without_a_slash_asks_the_working_directory() {
        let request = path_request("ma", "/home/kud/src").unwrap();
        assert_eq!(request.dir, "/home/kud/src");
        assert_eq!(request.prefix, "ma");
        assert_eq!(request.dir_part, "");
    }

    #[test]
    fn a_relative_token_walks_down_from_the_working_directory() {
        let request = path_request("src/ma", "/home/kud").unwrap();
        assert_eq!(request.dir, "/home/kud/src");
        assert_eq!(request.prefix, "ma");
        assert_eq!(request.dir_part, "src/", "拼回候选时要带上已打的路径段");
    }

    #[test]
    fn an_absolute_token_ignores_the_working_directory() {
        let request = path_request("/us", "/home/kud").unwrap();
        assert_eq!(request.dir, "/", "还没打完第一段，问的是根");
        assert_eq!(request.prefix, "us");

        let request = path_request("/usr/lo", "/home/kud").unwrap();
        assert_eq!(request.dir, "/usr");
        assert_eq!(request.prefix, "lo");
    }

    /// 宿主路径不是 POSIX 路径。WSL tab 在 shell 发出第一个 OSC 7 之前，`cwd`
    /// 还是启动时那个 `D:\...`——拿它拼出 `D:\x/src` 去问来宾只会白跑一趟。
    #[test]
    fn a_windows_working_directory_yields_no_request() {
        assert_eq!(path_request("ma", "D:\\temp_build"), None);
        assert_eq!(path_request("ma", ""), None);
        // 绝对 token 不依赖 cwd，照样成立。
        assert!(path_request("/us", "D:\\temp_build").is_some());
    }

    /// `~` 只有来宾自己知道展开成什么。
    #[test]
    fn a_tilde_is_left_to_the_guest() {
        assert_eq!(path_request("~/pro", "/home/kud"), None);
    }

    #[test]
    fn candidates_complete_one_segment_and_mark_directories() {
        let entries = vec![
            RemoteEntry { name: "main.rs".to_owned(), is_dir: false },
            RemoteEntry { name: "man".to_owned(), is_dir: true },
            RemoteEntry { name: "other".to_owned(), is_dir: false },
            RemoteEntry { name: ".mabhidden".to_owned(), is_dir: false },
        ];
        let request = path_request("ma", "/home/kud").unwrap();
        assert_eq!(
            candidates(&request, &entries),
            vec![("n/".to_owned(), true), ("in.rs".to_owned(), false)],
            "目录带尾斜杠好让用户连按 Tab 往下钻；短的排前面"
        );
    }

    /// 点文件默认不出现——否则一个 `ls ` 会被 `.bashrc`、`.profile` 淹掉。
    #[test]
    fn dotfiles_appear_only_once_the_dot_is_typed() {
        let entries = vec![
            RemoteEntry { name: ".bashrc".to_owned(), is_dir: false },
            RemoteEntry { name: "bin".to_owned(), is_dir: true },
        ];
        let plain = path_request("", "/home/kud").unwrap();
        assert_eq!(candidates(&plain, &entries), vec![("bin/".to_owned(), true)]);

        let dotted = path_request(".bash", "/home/kud").unwrap();
        assert_eq!(candidates(&dotted, &entries), vec![("rc".to_owned(), false)]);
    }

    /// 环境必须进缓存键：同一条 `/home/kud` 在两台机器上是两个目录。
    #[test]
    fn the_cache_key_separates_machines() {
        let wsl = SuggestEnv::Wsl { distro: "Debian".to_owned() };
        let other = SuggestEnv::Wsl { distro: "Ubuntu".to_owned() };
        let ssh = SuggestEnv::Ssh { destination: "kud@box".to_owned() };
        assert_ne!(key(&wsl, "/home/kud"), key(&other, "/home/kud"));
        assert_ne!(key(&wsl, "/home/kud"), key(&ssh, "/home/kud"));
        assert_ne!(key(&wsl, "/home/kud"), key(&wsl, "/home/other"));
    }

    /// 连按 Tab 不该排出一串子进程。
    #[test]
    fn only_one_fetch_is_claimed_per_directory() {
        let env = SuggestEnv::Wsl { distro: "test-inflight".to_owned() };
        assert!(begin_fetch(&env, "/tmp/a"), "第一次认领成功");
        assert!(!begin_fetch(&env, "/tmp/a"), "已经在拉了");
        assert!(begin_fetch(&env, "/tmp/b"), "另一个目录不受影响");

        finish_fetch(&env, "/tmp/a", Some(vec![RemoteEntry {
            name: "x".to_owned(),
            is_dir: false,
        }]));
        assert!(lookup(&env, "/tmp/a").is_some(), "落了缓存");
        assert!(begin_fetch(&env, "/tmp/a"), "拉完就解锁");
    }

    /// 失败只解锁、不落缓存：落一个空列表会让"暂时连不上"在整个 TTL 里都
    /// 表现成"这个目录是空的"。
    #[test]
    fn a_failed_fetch_leaves_no_cache_behind() {
        let env = SuggestEnv::Wsl { distro: "test-failure".to_owned() };
        assert!(begin_fetch(&env, "/tmp/gone"));
        finish_fetch(&env, "/tmp/gone", None);
        assert_eq!(lookup(&env, "/tmp/gone"), None);
        assert!(begin_fetch(&env, "/tmp/gone"), "下次还能重试");
    }
}
