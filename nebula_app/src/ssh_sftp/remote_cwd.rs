//! 远端工作目录跟随：找出用户正在打字的那个 shell，报告它的工作目录。
//!
//! # 为什么这件事不平凡
//!
//! 远端浏览器要跟着终端走——用户在终端里 `cd` 到某个目录，右侧的文件列表
//! 就该在那儿。本地 pane 靠 shell 集成（OSC 7）主动上报，但远端 shell 是
//! 用户自己的，我们没法假定它装了集成钩子。所以只能反过来问系统：**这条
//! SSH 连接上，用户正在操作的是哪个进程，它的 cwd 是什么。**
//!
//! 我们唯一的抓手是在同一条连接上开一个 exec 通道跑命令。难点全在于那个
//! 通道里的进程**不是**用户的 shell，得先把它找出来：
//!
//! 1. **新旧 sshd 的进程树形状不同。** 新版把一条连接的 pty 会话和 exec
//!    通道挂在同一个进程下，两者是兄弟，顺着 `$PPID` 一步就找到。老版
//!    （企业 Linux 长期支持版里仍很常见）给每个通道单独派生子进程，用户的
//!    shell 变成表兄弟，兄弟搜索完全找不到。通用的认亲凭据是
//!    `SSH_CONNECTION`——sshd 给同一条连接的每个通道注入同一个四元组，
//!    而四元组含客户端端口，所以跨连接必然不同。
//!
//! 2. **一条连接上可能有多个 shell。** 判据是控制终端：用户的 shell 有，
//!    我们这个探测通道没有（`ps` 显示 `?`），所以"有 tty"这一条同时把自己
//!    排除掉了。真的有多个带 tty 的 shell，说明多个终端共用了这条连接，
//!    此时"当前目录"没有唯一答案——报歧义，不猜。猜错的代价是用户在错误的
//!    目录里改文件。
//!
//! 3. **`su` / `sudo` 之后前台换人了。** 用户提权后打字的是新 shell，登录
//!    shell 只是它的祖先。所以要从登录 shell 出发遍历子树，找**前台进程组**
//!    里最深的那个 shell——前台进程组才是控制终端把键盘输入送去的地方。
//!    只看直接子进程会停在 `su` 那一层，读到提权前的目录。
//!
//! # 为什么脚本走标准输入
//!
//! 脚本里有 `awk` 程序和 `sed` 表达式，既含单引号也含 `$1` 这类会被 shell
//! 展开的记号。把它拼进命令行要做两层转义，错一个字符就是难查的静默失败。
//! 走标准输入完全绕开引号问题：命令行只有 `exec sh` 三个字符。
//!
//! `exec` 不能省。sshd 执行 exec 通道请求时跑的是 `$SHELL -c "<命令>"`，
//! 中间多一层登录 shell；多数 shell 会对末尾命令自动 exec，但这是优化而非
//! 保证（`fish`、`csh` 未必）。显式 `exec` 让 `sh` 顶替登录 shell 的进程位，
//! `$PPID` 才确定指向 sshd。

/// 远端工作目录探测的结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoteCwd {
    /// 定位到唯一的前台 shell，并读到了它的工作目录。
    Located(String),
    /// 这条连接上有多个带控制终端的 shell，"当前目录"没有唯一答案。
    Ambiguous,
    /// 远端不提供可用的进程自省手段，或这条连接上没有交互 shell。
    Unavailable,
}

/// 成功时的输出前缀。选一个不可能出现在正常命令输出里的记号，这样即使
/// 远端 shell 的启动脚本往 stdout 写了东西也不会被误认成路径。
const CWD_PREFIX: &str = "__nebula_cwd=";
/// 多个交互 shell。
const AMBIGUOUS_MARK: &str = "__nebula_cwd_ambiguous";
/// 无从判定。
const UNAVAILABLE_MARK: &str = "__nebula_cwd_unavailable";

/// 喂给远端 `sh` 标准输入的探测脚本。
///
/// 只用 POSIX shell 加 `ps`/`awk`/`sed`，不依赖 GNU 扩展——远端可能是精简
/// 容器、BSD 或嵌入式设备。每一步失败都收敛到一个明确的标记，绝不让
/// 半个结果漏出去。
const PROBE_SCRIPT: &str = r#"
self=$$
shell_re='^(bash|sh|zsh|fish|ksh|dash|ash|csh|tcsh)$'

# 列出带控制终端的交互 shell。第一个参数非空时只要该父进程的直接子进程。
# 没有 tty 的进程一律跳过：这既是"用户的 shell"的判据，也顺手排除了我们
# 自己这个探测通道。
list_shells() {
  ps -eo pid=,ppid=,tty=,comm= 2>/dev/null | awk -v self="$self" -v want="$1" -v re="$shell_re" '
    function base(c) { sub(/^.*\//, "", c); sub(/^-/, "", c); return c }
    $1 != self && $3 !~ /^\?+$/ && base($4) ~ re { if (want == "" || $2 == want) print $1 }
  '
}

# 读一个进程的 SSH_CONNECTION。同一条连接的所有通道值相同。
conn_of() {
  tr '\0' '\n' < "/proc/$1/environ" 2>/dev/null | sed -n 's/^SSH_CONNECTION=//p' | head -n1
}

# 快路：同一个 sshd 进程下的兄弟。命中时无需遍历 /proc。
found=$(list_shells "$PPID")

if [ -z "$found" ]; then
  # 慢路：按连接四元组认亲，覆盖每通道独立派生子进程的老版 sshd。
  mine=$(conn_of "$self")
  if [ -n "$mine" ]; then
    for pid in $(list_shells ""); do
      if [ "$(conn_of "$pid")" = "$mine" ]; then
        found="$found $pid"
      fi
    done
  fi
fi

set -- $found
if [ $# -eq 0 ]; then
  echo __nebula_cwd_unavailable
  exit 0
fi
if [ $# -gt 1 ]; then
  echo __nebula_cwd_ambiguous
  exit 0
fi
login=$1

# 穿过 su/sudo：在登录 shell 的子树里找前台进程组中最深的 shell。
active=$(ps -eo pid=,ppid=,stat=,comm= 2>/dev/null | awk -v root="$login" -v re="$shell_re" '
  function base(c) { sub(/^.*\//, "", c); sub(/^-/, "", c); return c }
  { parent[$1] = $2; state[$1] = $3; comm[$1] = $4; order[NR] = $1 }
  END {
    best = -1; pick = root
    for (i = 1; i <= NR; i++) {
      p = order[i]
      if (base(comm[p]) !~ re) continue
      if (index(state[p], "+") == 0) continue
      q = p; d = 0
      while (q != "" && q != root && d < 64) { q = parent[q]; d++ }
      if (q != root) continue
      if (d > best) { best = d; pick = p }
    }
    print pick
  }
')
[ -n "$active" ] || active=$login

# 读工作目录：Linux 走 /proc，没有 /proc 的系统退 lsof。
cwd=$(readlink "/proc/$active/cwd" 2>/dev/null)
if [ -z "$cwd" ] && command -v lsof >/dev/null 2>&1; then
  cwd=$(lsof -a -p "$active" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -n1)
fi

if [ -n "$cwd" ]; then
  printf '__nebula_cwd=%s\n' "$cwd"
else
  echo __nebula_cwd_unavailable
fi
"#;

/// 探测脚本的字节形式，喂给远端 `sh` 的标准输入。
pub(crate) fn probe_script() -> &'static [u8] {
    PROBE_SCRIPT.as_bytes()
}

/// 探测的时间预算。
///
/// 三秒：这条路径上要跑两次 `ps -e`，负载高的机器上一次几百毫秒是常见的，
/// 但超过三秒说明远端本身不健康，再等下去也拿不到有用的答案。跟随失败的
/// 代价只是浏览器停在原地，而 UI 卡三秒以上用户就会以为程序挂了。
const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// 问远端"用户此刻在哪个目录"。
///
/// 失败一律收敛成 [`RemoteCwd::Unavailable`] 而不是错误：跟随不上是**正常**
/// 结果（远端可能没有 `/proc`、没有 `ps`、或者这条连接上根本没有交互
/// shell），不该弹错误横幅，也不该改动浏览器已有的状态。
pub(crate) async fn probe(destination: &str) -> RemoteCwd {
    // `exec sh` 而不是 `sh`：见模块文档——要让 sh 顶替登录 shell 的进程位，
    // `$PPID` 才确定指向 sshd。
    match crate::ssh_session::exec_capture(destination, "exec sh", probe_script(), PROBE_BUDGET)
        .await
    {
        Ok(stdout) => parse_probe_output(&stdout),
        Err(err) => {
            log::debug!("远端工作目录探测失败（{destination}）: {err}");
            RemoteCwd::Unavailable
        },
    }
}

/// 从脚本的标准输出里取结论。
///
/// 逐行扫而不是只看最后一行：远端 shell 的启动脚本可能往 stdout 写东西，
/// 我们的标记必须在噪声里也能认出来。只认绝对路径——相对路径对浏览器没有
/// 意义（我们不知道它相对于什么），宁可当作探测失败。
pub(crate) fn parse_probe_output(stdout: &str) -> RemoteCwd {
    let mut verdict = RemoteCwd::Unavailable;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix(CWD_PREFIX) {
            let path = path.trim();
            if path.starts_with('/') {
                // 找到路径就立即定案：后面的行都是噪声。
                return RemoteCwd::Located(path.to_owned());
            }
        } else if line == AMBIGUOUS_MARK {
            // 歧义比"读不到"信息量更大（它告诉用户为什么跟不上），所以记下来
            // 继续扫——万一同一份输出里还有真路径，那条优先。
            verdict = RemoteCwd::Ambiguous;
        } else if line == UNAVAILABLE_MARK && verdict == RemoteCwd::Unavailable {
            verdict = RemoteCwd::Unavailable;
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_is_taken_as_the_working_directory() {
        assert_eq!(
            parse_probe_output("__nebula_cwd=/home/dev/project\n"),
            RemoteCwd::Located("/home/dev/project".to_owned())
        );
    }

    #[test]
    fn login_script_noise_before_the_marker_is_ignored() {
        // 远端 shell 的 rc 文件往 stdout 打招呼是常见的；探测不能因此失败。
        let stdout = "Welcome to example.net\nLast login: Mon\n__nebula_cwd=/srv/www\n";
        assert_eq!(parse_probe_output(stdout), RemoteCwd::Located("/srv/www".to_owned()));
    }

    #[test]
    fn multiple_interactive_shells_report_ambiguity_instead_of_guessing() {
        assert_eq!(parse_probe_output("__nebula_cwd_ambiguous\n"), RemoteCwd::Ambiguous);
    }

    #[test]
    fn a_relative_path_is_refused() {
        // 相对路径不知道相对于什么，拿它当浏览起点会打开一个错误的目录。
        assert_eq!(parse_probe_output("__nebula_cwd=project/src\n"), RemoteCwd::Unavailable);
    }

    #[test]
    fn empty_or_garbage_output_is_unavailable() {
        assert_eq!(parse_probe_output(""), RemoteCwd::Unavailable);
        assert_eq!(parse_probe_output("sh: ps: not found\n"), RemoteCwd::Unavailable);
        assert_eq!(parse_probe_output("__nebula_cwd_unavailable\n"), RemoteCwd::Unavailable);
    }

    #[test]
    fn a_real_path_wins_over_an_ambiguity_mark_in_the_same_output() {
        // 不该同时出现，但真出现时路径是更强的证据。
        let stdout = "__nebula_cwd_ambiguous\n__nebula_cwd=/opt\n";
        assert_eq!(parse_probe_output(stdout), RemoteCwd::Located("/opt".to_owned()));
    }

    #[test]
    fn paths_with_spaces_survive_parsing() {
        assert_eq!(
            parse_probe_output("__nebula_cwd=/home/dev/My Projects\n"),
            RemoteCwd::Located("/home/dev/My Projects".to_owned())
        );
    }

    #[test]
    fn the_script_carries_no_single_quote_ambiguity_into_the_command_line() {
        // 脚本走标准输入，所以它含单引号是允许的——这个断言守的是另一件事：
        // 脚本必须自带全部三个结论标记，解析端和脚本端不能各写一套。
        let script = PROBE_SCRIPT;
        assert!(script.contains(CWD_PREFIX));
        assert!(script.contains(AMBIGUOUS_MARK));
        assert!(script.contains(UNAVAILABLE_MARK));
    }
}
