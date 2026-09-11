//! Path-scoped Git ignore edits shared by the file tree adapters.

use std::path::{Path, PathBuf};

// ---- 手动忽略（写 `.gitignore`）----

/// 追加一条忽略规则的结果。区分这两种是因为 UI 要说不同的话：真写进去了要
/// 报出写的是哪一行，本来就有则什么都没动——把后者说成"已加入"会让用户以为
/// 文件被改了。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IgnoreOutcome {
    Added { entry: String, file: PathBuf },
    AlreadyPresent { entry: String },
    Removed { entry: String, file: PathBuf },
    AlreadyVisible { entry: String },
}

/// 从 `path` 往上找 Git 仓库根（含 `.git` 的目录）。
///
/// 不 spawn `git rev-parse --show-toplevel`：这里的判据只是"有没有 `.git`"，
/// 走文件系统就够，而这个函数在**构建右键菜单**时同步调用——起一个进程会让
/// 菜单弹出可感知地慢。`.git` 是目录（普通仓库）或文件（worktree 与 submodule
/// 的 gitdir 指针）都算。
pub(crate) fn git_repository_root(path: &Path) -> Option<PathBuf> {
    // 文件的规则要写进它所在目录的仓库，所以从父目录起步。
    let start = if path.is_dir() { path } else { path.parent()? };
    let mut probe = Some(start);
    while let Some(current) = probe {
        if current.join(".git").exists() {
            return Some(current.to_owned());
        }
        probe = current.parent();
    }
    None
}

/// 一条 gitignore 规则。
///
/// 三个刻意的选择：
/// - **锚定到仓库根**（前置 `/`）。不锚定的 `foo.txt` 会匹配仓库里**每一个**
///   同名文件；用户点的是这一个，忽略范围就该只有这一个。
/// - **目录带尾斜杠**，这样规则只匹配目录，不会连同名文件一起吃掉。
/// - **转义 glob 元字符**。`a[1].psd` 这种名字（Windows 上完全合法）不转义就
///   成了字符集，既漏掉本文件、又误伤 `a1.psd`。
///
/// 逐段走 [`Component`] 而不是对整个路径做 `replace('\\', "/")`：后者会把
/// 文件名里的反斜杠（Unix 上合法）也当成分隔符。
pub(crate) fn gitignore_entry(root: &Path, path: &Path, is_dir: bool) -> Option<String> {
    use std::path::Component;

    let relative = path.strip_prefix(root).ok()?;
    let mut segments = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => segments.push(escape_gitignore(&name.to_string_lossy())),
            // `..`、盘符、根目录都不该出现在仓库内的相对路径里。
            _ => return None,
        }
    }
    if segments.is_empty() {
        return None; // 仓库根自己不能忽略。
    }
    let mut entry = format!("/{}", segments.join("/"));
    if is_dir {
        entry.push('/');
    }
    Some(entry)
}

fn escape_gitignore(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for character in name.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    // gitignore 丢弃行尾空白，除非转义。名字以空格结尾时不转义就等于写错行。
    if escaped.ends_with(' ') {
        escaped.pop();
        escaped.push_str("\\ ");
    }
    escaped
}

/// 把 `path` 追加到所在 Git 仓库根的 `.gitignore`。
///
/// 行尾跟随原文件：已有 CRLF 的 `.gitignore` 继续写 CRLF。混行尾会让
/// `git diff` 把整个文件报成改动（见 hard lessons 里的 CRLF 假 diff 一案），
/// 而这个功能一次只该动一行。
pub(crate) fn append_to_gitignore(path: &Path, is_dir: bool) -> Result<IgnoreOutcome, String> {
    let root = git_repository_root(path).ok_or("这个位置不在 Git 仓库里")?;
    let entry =
        gitignore_entry(&root, path, is_dir).ok_or("无法为这条路径生成忽略规则".to_owned())?;
    let file = root.join(".gitignore");
    let existing = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读 .gitignore 失败：{error}")),
    };
    let lines: Vec<_> = existing.lines().collect();
    if let Some(last) = lines.iter().rposition(|line| line.trim() == entry)
        && !lines[last + 1..].iter().any(|line| line.starts_with('!'))
    {
        return Ok(IgnoreOutcome::AlreadyPresent { entry });
    }
    let newline = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let mut next = existing;
    // 末行没有换行符时先补一个，否则新规则会和它拼成一行。
    if !next.is_empty() && !next.ends_with('\n') {
        next.push_str(newline);
    }
    next.push_str(&entry);
    next.push_str(newline);
    crate::atomic_file::write(&file, next.as_bytes())
        .map_err(|error| format!("写 .gitignore 失败：{error}"))?;
    Ok(IgnoreOutcome::Added { entry, file })
}

struct IgnoreRule {
    source: PathBuf,
    pattern: String,
}

fn matching_rule(root: &Path, path: &Path) -> Result<Option<IgnoreRule>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
    let mut command = Command::new("git");
    command
        .args(["--no-optional-locks", "check-ignore", "-v", "-z", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("git stdin unavailable")?;
    let relative = relative.to_string_lossy();
    #[cfg(windows)]
    let relative = relative.replace('\\', "/");
    stdin
        .write_all(relative.as_bytes())
        .and_then(|_| stdin.write_all(&[0]))
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let parts: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    if parts.len() < 4 || parts[2].is_empty() || parts[2].starts_with(b"!") {
        return Ok(None);
    }
    let source = std::str::from_utf8(parts[0]).map_err(|error| error.to_string())?;
    let pattern = std::str::from_utf8(parts[2]).map_err(|error| error.to_string())?;
    Ok(Some(IgnoreRule { source: root.join(source), pattern: pattern.to_owned() }))
}

/// Remove a path-specific rule where possible. A shared glob or an ignored
/// ancestor instead gets a narrow exception, keeping sibling paths ignored.
pub(crate) fn remove_from_gitignore(path: &Path, is_dir: bool) -> Result<IgnoreOutcome, String> {
    let root = git_repository_root(path).ok_or("This path is outside a Git repository")?;
    let entry = gitignore_entry(&root, path, is_dir).ok_or("Invalid ignore path")?;
    if matching_rule(&root, path)?.is_none() {
        return Ok(IgnoreOutcome::AlreadyVisible { entry });
    }
    let mut writes = Vec::<(PathBuf, Option<Vec<u8>>, Vec<u8>)>::new();
    let result = (|| {
        for _ in 0..64 {
            let Some(rule) = matching_rule(&root, path)? else { return Ok(()) };
            let source_is_repo_ignore = rule.source.starts_with(&root)
                && rule.source.file_name().is_some_and(|name| name == ".gitignore");
            let file = if source_is_repo_ignore { rule.source } else { root.join(".gitignore") };
            // A symlinked ignore file could otherwise edit a different repository.
            for ancestor in file.ancestors().take_while(|ancestor| *ancestor != root) {
                if std::fs::symlink_metadata(ancestor)
                    .is_ok_and(|meta| meta.file_type().is_symlink())
                {
                    return Err("Cannot edit a symlinked .gitignore path".to_owned());
                }
            }
            let base = file.parent().ok_or("Invalid .gitignore location")?;
            let local_entry = gitignore_entry(base, path, is_dir).ok_or("Invalid rule location")?;
            let previous = match std::fs::read(&file) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            let existing = std::str::from_utf8(previous.as_deref().unwrap_or_default())
                .map_err(|error| error.to_string())?;
            let mut next = existing.to_owned();
            // Only anchored literal rules describe exactly the clicked path.
            if source_is_repo_ignore
                && (rule.pattern == local_entry
                    || (is_dir && rule.pattern == local_entry.trim_end_matches('/')))
            {
                next = existing
                    .split_inclusive('\n')
                    .filter(|line| line.trim_end_matches(['\r', '\n']) != rule.pattern)
                    .collect();
            } else {
                let newline = if existing.contains("\r\n") { "\r\n" } else { "\n" };
                let mut rules = Vec::new();
                let mut parents: Vec<_> = path
                    .parent()
                    .into_iter()
                    .flat_map(Path::ancestors)
                    .take_while(|parent| *parent != base)
                    .collect();
                parents.reverse();
                for parent in parents {
                    if matching_rule(&root, parent)?.is_some() {
                        let parent_entry =
                            gitignore_entry(base, parent, true).ok_or("Invalid parent rule")?;
                        rules.push(format!("!{parent_entry}"));
                        rules.push(format!("{parent_entry}*"));
                    }
                }
                rules.push(format!("!{local_entry}"));
                if !next.is_empty() && !next.ends_with('\n') {
                    next.push_str(newline);
                }
                for rule in rules {
                    next.push_str(&rule);
                    next.push_str(newline);
                }
            }
            if next == existing {
                return Err("The ignore rule could not be changed".to_owned());
            }
            crate::atomic_file::write(&file, next.as_bytes()).map_err(|error| error.to_string())?;
            writes.push((file, previous, next.into_bytes()));
        }
        Err("Too many overlapping ignore rules".to_owned())
    })();
    if let Err(error) = result {
        for (file, before, written) in writes.into_iter().rev() {
            // Preserve a concurrent external edit rather than replacing it on rollback.
            if std::fs::read(&file).ok().as_deref() == Some(written.as_slice()) {
                match before {
                    Some(bytes) => {
                        let _ = crate::atomic_file::write(&file, &bytes);
                    },
                    None => {
                        let _ = std::fs::remove_file(&file);
                    },
                }
            }
        }
        return Err(error);
    }
    let file =
        writes.last().map(|(file, _, _)| file.clone()).unwrap_or_else(|| root.join(".gitignore"));
    Ok(IgnoreOutcome::Removed { entry, file })
}

#[cfg(test)]
#[path = "gitignore_tests.rs"]
mod tests;
