//! 用户目录解析（005 §5 L1）。
//!
//! 这里是 Nebula 全部落盘路径的唯一真相。改造前同一段
//! 「`APPDATA` → `HOME/.config` → `Nebula`」的逻辑在仓库里被复制了 5 份
//! （`display/mod.rs`、`nebula_history.rs`、`notify.rs`、`font_install.rs`、
//! `nebula_terminal/tty/windows`），其中 `notify.rs` 只读 `APPDATA`——在
//! Unix 上直接返回 `None`，通知图标永远写不出来。
//!
//! ## 各平台落点
//!
//! | 平台 | 目录 |
//! | :-- | :-- |
//! | Windows | `%APPDATA%\Nebula` |
//! | macOS | `~/Library/Application Support/Nebula` |
//! | Linux | `$XDG_CONFIG_HOME/nebula`，未设则 `~/.config/nebula` |
//!
//! **Windows 路径与改造前逐字节相同**——存量用户零迁移，这是硬约束。
//!
//! Linux 改用 XDG 规范目录、且目录名转小写（`nebula` 而非 `Nebula`），
//! 是因为 Linux 目标从未编译成功过（见 004 的探测结论：交叉编译死在
//! fontconfig/ring 的 native build script），**绝无存量用户**，此刻是唯一
//! 能免费修正的时机。macOS 同理，从 `~/.config` 改到系统规定的
//! Application Support。
//!
//! ## 为什么 config 与 data 暂不分家
//!
//! XDG 分 `XDG_CONFIG_HOME` 与 `XDG_DATA_HOME`，但 Windows 侧现在把设置、
//! 历史、会话、字体全放在同一个 `%APPDATA%\Nebula` 下。此刻拆开就要给
//! Windows 做数据迁移，风险远大于收益。先统一成一个正确的入口，拆分留到
//! 真有需求时（那时只需改这个文件）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 覆盖数据目录。便携模式与自动化测试用——`ui_probe` 之类的工具跑起来
/// 会写设置文件，指到临时目录就不会污染真实配置。
const DIR_OVERRIDE_ENV: &str = "NEBULA_CONFIG_DIR";

/// 用户主目录。Windows `USERPROFILE`，Unix `HOME`。
///
/// 存在的理由是仓库里有 6 处只读 `USERPROFILE` 而没有 `HOME` 回落
/// （`ai_hook.rs` 找 `.claude` / `.codex` / `.pi` 的四处最典型），那些路径
/// 在 Linux/macOS 上恒为 `None`，对应功能静默消失。
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let raw = std::env::var_os("USERPROFILE");
    #[cfg(unix)]
    let raw = std::env::var_os("HOME");

    raw.map(PathBuf::from).filter(|path| !path.as_os_str().is_empty())
}

/// Nebula 的配置与数据目录，必要时创建。
///
/// 结果缓存在 `OnceLock`：改造前 26 个调用点每次都要读环境变量 +
/// `create_dir_all` 一次。缓存同时保证进程内路径恒定——半途改环境变量
/// 不会让一部分文件写到旧目录、另一部分写到新目录。
pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = resolve_data_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

fn resolve_data_dir() -> PathBuf {
    // `nebula_settings.txt`, runtime discovery, sessions, history, and imported
    // fonts must share one directory. The zero-dependency settings crate owns
    // the environment/OS resolution so consumers outside `nebula_app` cannot
    // accidentally revive the old Linux/macOS paths.
    nebula_settings::settings_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_rejects_empty_env() {
        // 空字符串环境变量在实践中出现过（某些登录管理器），当作未设置。
        // 这里只验证过滤器本身——直接改进程环境会污染其他并发测试。
        let empty = PathBuf::from("");
        assert!(Some(empty).filter(|p: &PathBuf| !p.as_os_str().is_empty()).is_none());
    }

    #[test]
    fn data_dir_is_absolute_and_named() {
        let dir = data_dir();
        assert!(dir.is_absolute(), "数据目录必须是绝对路径：{dir:?}");
        // 覆盖环境变量下叶子名可以是任意的，只在默认路径上校验命名。
        if std::env::var_os(DIR_OVERRIDE_ENV).is_none() {
            let leaf = dir.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            assert!(
                leaf.eq_ignore_ascii_case("nebula"),
                "默认数据目录应以 nebula 结尾，实际 {leaf:?}"
            );
        }
    }

    /// Windows 侧路径是硬约束：必须逐字节等于改造前的 `%APPDATA%\Nebula`，
    /// 否则存量用户的设置、历史、会话快照集体失联。
    #[cfg(windows)]
    #[test]
    fn windows_path_matches_pre_refactor_layout() {
        if std::env::var_os(DIR_OVERRIDE_ENV).is_some() {
            return;
        }
        let Some(appdata) = std::env::var_os("APPDATA") else { return };
        assert_eq!(data_dir(), PathBuf::from(appdata).join("Nebula"));
    }

    /// Linux 侧遵守 XDG：相对路径的 `XDG_CONFIG_HOME` 按规范视为未设置。
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_path_follows_xdg() {
        if std::env::var_os(DIR_OVERRIDE_ENV).is_some() {
            return;
        }
        let dir = data_dir();
        assert_eq!(dir.file_name().and_then(|name| name.to_str()), Some("nebula"));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
            if xdg.is_absolute() {
                assert_eq!(dir, xdg.join("nebula"));
                return;
            }
        }
        if let Some(home) = home_dir() {
            assert_eq!(dir, home.join(".config").join("nebula"));
        }
    }

    /// macOS 有自己的规矩：Application Support，不是 `~/.config`。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_path_uses_application_support() {
        if std::env::var_os(DIR_OVERRIDE_ENV).is_some() {
            return;
        }
        let Some(home) = home_dir() else { return };
        assert_eq!(data_dir(), home.join("Library").join("Application Support").join("Nebula"));
    }
}
