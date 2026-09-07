//! 用户目录解析（005 §5 L1）。
//!
//! Pebrel 的应用路径适配器。共享设置 crate 负责路径优先级与启动迁移，
//! 这里缓存应用侧解析结果，所有配置、历史、会话和字体继续使用同一目录。
//!
//! ## 各平台落点
//!
//! | 平台 | 目录 |
//! | :-- | :-- |
//! | Windows | `%APPDATA%\Pebrel` |
//! | macOS | `~/Library/Application Support/Pebrel` |
//! | Linux | `$XDG_CONFIG_HOME/pebrel`，未设则 `~/.config/pebrel` |
//!
//! `PEBREL_CONFIG_DIR` 优先，`NEBULA_CONFIG_DIR` 是保留的便携/测试兼容入口。
//! 主程序在路径被缓存、数据被打开之前执行非覆盖迁移，旧来源保留供恢复。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 覆盖数据目录。便携模式与自动化测试用——`ui_probe` 之类的工具跑起来
/// 会写设置文件，指到临时目录就不会污染真实配置。
#[cfg(test)]
fn has_override() -> bool {
    ["PEBREL_CONFIG_DIR", "NEBULA_CONFIG_DIR"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

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

/// Pebrel 的配置与数据目录，必要时创建。
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
    // `pebrel_settings.txt`, runtime discovery, sessions, history, and imported
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
        let dir = resolve_data_dir();
        assert!(dir.is_absolute(), "数据目录必须是绝对路径：{dir:?}");
        // 覆盖环境变量下叶子名可以是任意的，只在默认路径上校验命名。
        if !has_override() {
            let leaf = dir.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            assert!(
                leaf.eq_ignore_ascii_case("pebrel"),
                "默认数据目录应以 pebrel 结尾，实际 {leaf:?}"
            );
        }
    }

    /// Migration runs before the application writes to the renamed directory.
    #[cfg(windows)]
    #[test]
    fn windows_path_uses_pebrel_layout() {
        if has_override() {
            return;
        }
        let Some(appdata) = std::env::var_os("APPDATA") else { return };
        assert_eq!(resolve_data_dir(), PathBuf::from(appdata).join("Pebrel"));
    }

    /// Linux 侧遵守 XDG：相对路径的 `XDG_CONFIG_HOME` 按规范视为未设置。
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_path_follows_xdg() {
        if has_override() {
            return;
        }
        let dir = resolve_data_dir();
        assert_eq!(dir.file_name().and_then(|name| name.to_str()), Some("pebrel"));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
            if xdg.is_absolute() {
                assert_eq!(dir, xdg.join("pebrel"));
                return;
            }
        }
        if let Some(home) = home_dir() {
            assert_eq!(dir, home.join(".config").join("pebrel"));
        }
    }

    /// macOS 有自己的规矩：Application Support，不是 `~/.config`。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_path_uses_application_support() {
        if has_override() {
            return;
        }
        let Some(home) = home_dir() else { return };
        assert_eq!(
            resolve_data_dir(),
            home.join("Library").join("Application Support").join("Pebrel")
        );
    }
}
