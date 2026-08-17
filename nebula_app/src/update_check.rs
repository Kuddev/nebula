//! Startup update check against GitHub Releases.
//!
//! Zero new dependencies: Windows 10+ ships `curl.exe`, so the probe is a
//! short-lived child process instead of an HTTP stack baked into the binary.
//! Everything is best-effort — no network, no curl, malformed JSON, or a
//! GitHub outage all degrade to "no banner", never to an error the user sees.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};
use crate::message_bar::{Message, MessageType};

const RELEASES_API: &str = "https://api.github.com/repos/Kuddev/nebula/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/Kuddev/nebula/releases";

/// 设置主页手动检查所需的完整结果。启动检查与手动检查共用这一份版本
/// 比较，避免两处对预发布后缀或分段数字产生不同结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
}

/// Kick off the once-per-process background check. The result (if any)
/// arrives as a regular [`EventType::Message`] banner, which the message bar
/// already knows how to display, deduplicate and dismiss.
pub fn spawn_once(proxy: EventLoopProxy<Event>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        // 等窗口与首个会话安顿好再查，别和启动抢磁盘/网络。
        std::thread::sleep(Duration::from_secs(12));
        let latest = match fetch_latest_version() {
            Ok(latest) => latest,
            Err(error) => {
                log::debug!("update-check: {error}");
                return;
            },
        };
        let current = env!("CARGO_PKG_VERSION");
        if !is_newer(&latest, current) {
            log::debug!("update-check: v{current} is current (latest v{latest})");
            return;
        }
        let text = format!("Nebula v{latest} 已发布（当前 v{current}），下载：{RELEASES_PAGE}");
        let _ = proxy.send_event(Event::new(
            EventType::Message(Message::new(text, MessageType::Warning)),
            None,
        ));
    });
    if let Err(error) = spawned {
        log::debug!("update-check: thread spawn failed: {error}");
    }
}

/// 立即检查 GitHub 最新 release；调用方必须把它放到后台执行器，避免
/// `curl` 的网络等待阻塞 UI 线程。
pub fn check_now() -> Result<UpdateCheckResult, String> {
    let latest = fetch_latest_version()?;
    let current = env!("CARGO_PKG_VERSION").to_owned();
    Ok(UpdateCheckResult { update_available: is_newer(&latest, &current), current, latest })
}

fn fetch_latest_version() -> Result<String, String> {
    let mut command = Command::new("curl");
    command.args([
        "-fsSL",
        "--max-time",
        "10",
        "-H",
        "User-Agent: nebula-terminal",
        "-H",
        "Accept: application/vnd.github+json",
        RELEASES_API,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().map_err(|error| format!("无法启动 curl：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            format!("GitHub 请求失败（curl {}）", output.status)
        } else {
            format!("GitHub 请求失败：{detail}")
        });
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("GitHub 返回了无效数据：{error}"))?;
    let tag = json
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "GitHub release 缺少 tag_name".to_owned())?;
    let version = tag.trim().trim_start_matches(['v', 'V']);
    if version.is_empty() {
        Err("GitHub release 的版本号为空".to_owned())
    } else {
        Ok(version.to_owned())
    }
}

/// Compare dotted numeric prefixes ("0.7.10" > "0.7.9"); anything after the
/// digits in a segment is ignored, so "1.0.0-rc1" reads as `[1, 0, 0]`.
fn is_newer(latest: &str, current: &str) -> bool {
    fn segments(version: &str) -> Vec<u64> {
        version
            .split('.')
            .map(|segment| {
                let digits: String = segment.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    }
    let (latest, current) = (segments(latest), segments(current));
    for index in 0..latest.len().max(current.len()) {
        let new = latest.get(index).copied().unwrap_or(0);
        let old = current.get(index).copied().unwrap_or(0);
        if new != old {
            return new > old;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn version_comparison_is_numeric_per_segment() {
        assert!(is_newer("0.7.1", "0.7.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.7.0", "0.7.0"));
        assert!(!is_newer("0.6.9", "0.7.0"));
        assert!(!is_newer("0.7.0-rc1", "0.7.0"));
    }
}
