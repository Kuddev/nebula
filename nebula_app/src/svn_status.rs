//! SVN 工作拷贝状态：直接读 `.svn/wc.db`（SQLite），零外部进程依赖。
//!
//! 为什么不调 svn.exe：这台产品的目标机器未必装命令行客户端（TortoiseSVN
//! 默认只装 GUI），而且逐次 spawn 进程拉状态既慢又抖。SVN 1.7+ 的工作拷贝
//! 元数据是稳定的 SQLite 库（`NODES` / `ACTUAL_NODE` 表），TortoiseSVN 的
//! TSVNCache 走的就是这条路：只读打开、快速路径按 `recorded_size` /
//! `recorded_time` 判嫌疑、嫌疑文件再做 SHA-1 精判——本模块同一合同。
//!
//! 范围（v1）：
//! - 状态子集：Added / Replaced / Deleted / Modified / Missing / Conflicted /
//!   Unversioned；`svn:ignore` 属性尚未解析（未忽略项会以 Unversioned 出现）。
//! - `svnadmin create` 出来的**服务端仓库**（conf/db/hooks/format）不是
//!   工作拷贝，[`classify_dir`] 会识别并明确报告，不做 FSFS 解析。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use sha1::{Digest as _, Sha1};

/// 修改嫌疑文件做 SHA-1 精判的尺寸上限；超过就直接按 Modified 报告，
/// 避免状态刷新读几百 MB 大文件（TSVNCache 同款取舍）。
const SHA1_VERIFY_LIMIT: u64 = 4 * 1024 * 1024;

/// 一个目录相对 SVN 的身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SvnDirKind {
    /// 位于某个工作拷贝内；携带工作拷贝根（含 `.svn/` 的目录）。
    WorkingCopy(PathBuf),
    /// `svnadmin create` 生成的服务端仓库：只能被 svnserve/httpd 提供服务
    /// 或用 `file://` 协议 checkout，本身没有"文件状态"可言。
    Repository(PathBuf),
    /// 与 SVN 无关。
    Plain,
}

/// 判定 `dir` 的 SVN 身份：沿祖先链同时查找 `.svn/wc.db`（工作拷贝）和
/// `format` + `conf/db/hooks`（服务端仓库），并返回离 `dir` 最近的根。
pub fn classify_dir(dir: &Path) -> SvnDirKind {
    let mut probe = Some(dir);
    while let Some(current) = probe {
        if current.join(".svn").join("wc.db").is_file() {
            return SvnDirKind::WorkingCopy(current.to_owned());
        }
        let is_repository = current.join("format").is_file()
            && current.join("conf").is_dir()
            && current.join("db").is_dir()
            && current.join("hooks").is_dir();
        if is_repository {
            return SvnDirKind::Repository(current.to_owned());
        }
        probe = current.parent();
    }
    SvnDirKind::Plain
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SvnState {
    Added,
    Replaced,
    Deleted,
    Modified,
    Missing,
    Conflicted,
    Unversioned,
}

impl SvnState {
    /// 单字母角标（与 `svn status` 输出的第一列同字符集）。
    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Replaced => "R",
            Self::Deleted => "D",
            Self::Modified => "M",
            Self::Missing => "!",
            Self::Conflicted => "C",
            Self::Unversioned => "?",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SvnChange {
    /// 相对工作拷贝根的正斜杠路径。
    pub rel_path: String,
    pub state: SvnState,
}

/// 读出整个工作拷贝的变更清单（按路径排序）。
///
/// 干净判定的快速路径：`recorded_size` 不同 → Modified；大小相同而
/// `recorded_time`（µs）不同 → ≤ [`SHA1_VERIFY_LIMIT`] 时做 SHA-1 精判，
/// 只有内容真的不同才报 Modified（touch 过的文件不误报）。
pub fn working_copy_status(root: &Path) -> Result<Vec<SvnChange>, String> {
    let db_path = root.join(".svn").join("wc.db");
    let connection =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("无法打开 {}: {error}", db_path.display()))?;
    // svn 客户端可能正持有写锁；等一小会而不是立刻失败。
    connection.busy_timeout(std::time::Duration::from_millis(300)).ok();

    #[derive(Debug)]
    struct NodeRow {
        op_depth: i64,
        presence: String,
        kind: String,
        checksum: Option<String>,
        recorded_size: Option<i64>,
        recorded_time: Option<i64>,
        has_base: bool,
    }

    // 每个 relpath 取 op_depth 最大的行（当前操作层），并记录是否存在
    // base 层（Added vs Replaced 的分界）。
    let mut nodes: BTreeMap<String, NodeRow> = BTreeMap::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT local_relpath, op_depth, presence, kind, checksum,
                        recorded_size, recorded_time
                 FROM nodes ORDER BY local_relpath, op_depth",
            )
            .map_err(|error| format!("wc.db 结构不符合预期: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    NodeRow {
                        op_depth: row.get(1)?,
                        presence: row.get(2)?,
                        kind: row.get(3)?,
                        checksum: row.get(4)?,
                        recorded_size: row.get(5)?,
                        recorded_time: row.get(6)?,
                        has_base: false,
                    },
                ))
            })
            .map_err(|error| format!("读取 NODES 失败: {error}"))?;
        for row in rows {
            let (relpath, node) = row.map_err(|error| format!("读取 NODES 失败: {error}"))?;
            match nodes.get_mut(&relpath) {
                // 行按 op_depth 升序到达：后到的层覆盖前面的，base 层的
                // 存在性单独记账。
                Some(existing) => {
                    let has_base = existing.has_base || existing.op_depth == 0;
                    if node.op_depth >= existing.op_depth {
                        *existing = NodeRow { has_base, ..node };
                    } else {
                        existing.has_base = has_base || node.op_depth == 0;
                    }
                },
                None => {
                    let has_base = node.op_depth == 0;
                    nodes.insert(relpath, NodeRow { has_base, ..node });
                },
            }
        }
    }

    // 冲突从 ACTUAL_NODE 来（树冲突/文本冲突统一报 Conflicted）。
    let mut conflicted: HashSet<String> = HashSet::new();
    if let Ok(mut statement) =
        connection.prepare("SELECT local_relpath FROM actual_node WHERE conflict_data IS NOT NULL")
    {
        if let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) {
            for relpath in rows.flatten() {
                conflicted.insert(relpath);
            }
        }
    }

    let mut changes = Vec::new();
    let mut versioned_dirs: Vec<String> = Vec::new();
    for (relpath, node) in &nodes {
        if relpath.is_empty() {
            versioned_dirs.push(String::new());
            continue;
        }
        if conflicted.contains(relpath) {
            changes.push(SvnChange { rel_path: relpath.clone(), state: SvnState::Conflicted });
            continue;
        }
        let disk = root.join(relpath.replace('/', std::path::MAIN_SEPARATOR_STR));
        match node.presence.as_str() {
            "base-deleted" => {
                changes.push(SvnChange { rel_path: relpath.clone(), state: SvnState::Deleted });
            },
            "normal" if node.op_depth > 0 => {
                let state = if node.has_base { SvnState::Replaced } else { SvnState::Added };
                changes.push(SvnChange { rel_path: relpath.clone(), state });
                if node.kind == "dir" {
                    versioned_dirs.push(relpath.clone());
                }
            },
            "normal" => {
                if node.kind == "dir" {
                    versioned_dirs.push(relpath.clone());
                    continue;
                }
                let Ok(metadata) = std::fs::metadata(&disk) else {
                    changes.push(SvnChange { rel_path: relpath.clone(), state: SvnState::Missing });
                    continue;
                };
                if is_modified(
                    &disk,
                    &metadata,
                    node.recorded_size,
                    node.recorded_time,
                    node.checksum.as_deref(),
                ) {
                    changes
                        .push(SvnChange { rel_path: relpath.clone(), state: SvnState::Modified });
                }
            },
            // excluded/server-excluded/not-present 等：本地无实体，不报告。
            _ => {},
        }
    }

    // 未版本化：已版本化目录的磁盘直接子项里，不在 NODES 且不是 .svn 的。
    for dir in versioned_dirs {
        let disk_dir = if dir.is_empty() {
            root.to_owned()
        } else {
            root.join(dir.replace('/', std::path::MAIN_SEPARATOR_STR))
        };
        let Ok(entries) = std::fs::read_dir(&disk_dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".svn" {
                continue;
            }
            let relpath = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
            if !nodes.contains_key(&relpath) {
                changes.push(SvnChange { rel_path: relpath, state: SvnState::Unversioned });
            }
        }
    }

    changes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(changes)
}

/// 工作拷贝根的当前修订号（`NODES` 根行的 `revision` 列）。
pub fn working_copy_revision(root: &Path) -> Option<i64> {
    let connection = rusqlite::Connection::open_with_flags(
        root.join(".svn").join("wc.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection
        .query_row(
            "SELECT revision FROM nodes WHERE local_relpath = '' AND op_depth = 0",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
}

/// 干净判定：大小 → 时间戳 → SHA-1 三级快速路径。
fn is_modified(
    path: &Path,
    metadata: &std::fs::Metadata,
    recorded_size: Option<i64>,
    recorded_time: Option<i64>,
    checksum: Option<&str>,
) -> bool {
    if let Some(size) = recorded_size {
        if metadata.len() != size as u64 {
            return true;
        }
    }
    let disk_micros = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_micros() as i64);
    if let (Some(disk), Some(recorded)) = (disk_micros, recorded_time) {
        if disk == recorded {
            return false;
        }
    }
    // 时间戳变了（或缺记录）：尺寸相同的小文件做内容精判，防 touch 误报。
    let Some(expected) = checksum.and_then(|value| value.strip_prefix("$sha1$")) else {
        return true;
    };
    if metadata.len() > SHA1_VERIFY_LIMIT {
        return true;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return true;
    };
    let digest = Sha1::digest(&bytes);
    let mut actual = String::with_capacity(40);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(actual, "{byte:02x}");
    }
    actual != expected
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工构造最小 wc.db：只建状态推导用到的列，与真实 schema 的子集
    /// 保持同名同义。
    fn fake_working_copy(dir: &Path) -> rusqlite::Connection {
        std::fs::create_dir_all(dir.join(".svn")).unwrap();
        let connection = rusqlite::Connection::open(dir.join(".svn").join("wc.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE nodes (
                     local_relpath TEXT, op_depth INTEGER, presence TEXT, kind TEXT,
                     checksum TEXT, recorded_size INTEGER, recorded_time INTEGER
                 );
                 CREATE TABLE actual_node (local_relpath TEXT, conflict_data BLOB);",
            )
            .unwrap();
        connection
    }

    fn insert_node(
        connection: &rusqlite::Connection,
        relpath: &str,
        op_depth: i64,
        presence: &str,
        kind: &str,
        checksum: Option<String>,
        size: Option<i64>,
        time: Option<i64>,
    ) {
        connection
            .execute(
                "INSERT INTO nodes VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![relpath, op_depth, presence, kind, checksum, size, time],
            )
            .unwrap();
    }

    fn sha1_of(content: &str) -> String {
        let digest = Sha1::digest(content.as_bytes());
        let mut hex = String::new();
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        format!("$sha1${hex}")
    }

    fn state_of<'a>(changes: &'a [SvnChange], path: &str) -> Option<&'a SvnChange> {
        changes.iter().find(|change| change.rel_path == path)
    }

    #[test]
    fn classifies_repository_and_working_copy() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        for sub in ["conf", "db", "hooks"] {
            std::fs::create_dir_all(repo.join(sub)).unwrap();
        }
        std::fs::write(repo.join("format"), "8\n").unwrap();
        assert_eq!(classify_dir(&repo), SvnDirKind::Repository(repo.clone()));
        let repo_child = repo.join("db").join("revs");
        std::fs::create_dir_all(&repo_child).unwrap();
        assert_eq!(classify_dir(&repo_child), SvnDirKind::Repository(repo.clone()));

        let wc = temp.path().join("wc");
        fake_working_copy(&wc);
        let inner = wc.join("src");
        std::fs::create_dir_all(&inner).unwrap();
        assert_eq!(classify_dir(&inner), SvnDirKind::WorkingCopy(wc.clone()));

        assert_eq!(classify_dir(temp.path()), SvnDirKind::Plain);
    }

    #[test]
    fn derives_the_full_state_alphabet_from_wc_db() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let connection = fake_working_copy(root);

        insert_node(&connection, "", 0, "normal", "dir", None, None, None);
        // 干净文件：大小/时间都与记录一致。
        std::fs::write(root.join("clean.txt"), "clean").unwrap();
        let clean_meta = std::fs::metadata(root.join("clean.txt")).unwrap();
        let clean_micros = clean_meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;
        insert_node(
            &connection,
            "clean.txt",
            0,
            "normal",
            "file",
            Some(sha1_of("clean")),
            Some(5),
            Some(clean_micros),
        );
        // touch 过但内容相同：时间戳不同 + SHA-1 相同 → 不报告。
        std::fs::write(root.join("touched.txt"), "same!").unwrap();
        insert_node(
            &connection,
            "touched.txt",
            0,
            "normal",
            "file",
            Some(sha1_of("same!")),
            Some(5),
            Some(1),
        );
        // 真改过：大小不同。
        std::fs::write(root.join("changed.txt"), "different content").unwrap();
        insert_node(
            &connection,
            "changed.txt",
            0,
            "normal",
            "file",
            Some(sha1_of("old")),
            Some(3),
            Some(1),
        );
        // 新增 / 替换 / 删除 / 丢失。
        std::fs::write(root.join("added.txt"), "new").unwrap();
        insert_node(&connection, "added.txt", 1, "normal", "file", None, None, None);
        std::fs::write(root.join("replaced.txt"), "re-added").unwrap();
        insert_node(&connection, "replaced.txt", 0, "normal", "file", None, Some(1), Some(1));
        insert_node(&connection, "replaced.txt", 1, "normal", "file", None, None, None);
        insert_node(&connection, "deleted.txt", 0, "normal", "file", None, Some(1), Some(1));
        insert_node(&connection, "deleted.txt", 1, "base-deleted", "file", None, None, None);
        insert_node(&connection, "missing.txt", 0, "normal", "file", None, Some(1), Some(1));
        // 冲突覆盖其他状态。
        std::fs::write(root.join("conflict.txt"), "x").unwrap();
        insert_node(&connection, "conflict.txt", 0, "normal", "file", None, Some(1), Some(1));
        connection.execute("INSERT INTO actual_node VALUES ('conflict.txt', x'00')", []).unwrap();
        // 未版本化的散文件。
        std::fs::write(root.join("stray.txt"), "?").unwrap();
        drop(connection);

        let changes = working_copy_status(root).unwrap();

        assert!(state_of(&changes, "clean.txt").is_none(), "clean file must stay silent");
        assert!(state_of(&changes, "touched.txt").is_none(), "touch without edit must stay silent");
        assert_eq!(state_of(&changes, "changed.txt").unwrap().state, SvnState::Modified);
        assert_eq!(state_of(&changes, "added.txt").unwrap().state, SvnState::Added);
        assert_eq!(state_of(&changes, "replaced.txt").unwrap().state, SvnState::Replaced);
        assert_eq!(state_of(&changes, "deleted.txt").unwrap().state, SvnState::Deleted);
        assert_eq!(state_of(&changes, "missing.txt").unwrap().state, SvnState::Missing);
        assert_eq!(state_of(&changes, "conflict.txt").unwrap().state, SvnState::Conflicted);
        assert_eq!(state_of(&changes, "stray.txt").unwrap().state, SvnState::Unversioned);
    }
}
