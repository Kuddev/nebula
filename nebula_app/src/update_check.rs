//! Startup update check against GitHub Releases.
//!
//! Zero new dependencies: Windows 10+ ships `curl.exe`, so the probe is a
//! short-lived child process instead of an HTTP stack baked into the binary.
//! Everything is best-effort — no network, no curl, malformed JSON, or a
//! GitHub outage all degrade to "no banner", never to an error the user sees.

use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};
use crate::message_bar::{Message, MessageType};

const RELEASES_API: &str = "https://api.github.com/repos/Kuddev/nebula/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/Kuddev/nebula/releases";
const UPDATE_STATE_FILE: &str = "update_state.json";
const REMIND_LATER_SECS: u64 = 3 * 24 * 60 * 60;

static UPDATE_STATE_LOCK: Mutex<()> = Mutex::new(());

/// 自动提示状态独立于通用设置文件，避免后台版本检查改写用户设置正文。
/// 字段按版本生效，新版本不会继承旧版本的延迟或跳过选择。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct UpdatePromptState {
    last_prompted: Option<String>,
    remind_after: Option<u64>,
    skipped_version: Option<String>,
}

impl UpdatePromptState {
    fn should_prompt(&self, version: &str, now: u64) -> bool {
        if self.skipped_version.as_deref() == Some(version) {
            return false;
        }
        if self.last_prompted.as_deref() != Some(version) {
            return true;
        }
        self.remind_after.is_some_and(|deadline| now >= deadline)
    }

    fn mark_prompted(&mut self, version: &str) {
        self.last_prompted = Some(version.to_owned());
        self.remind_after = None;
        if self.skipped_version.as_deref() != Some(version) {
            self.skipped_version = None;
        }
    }

    fn remind_later(&mut self, version: &str, now: u64) {
        self.last_prompted = Some(version.to_owned());
        self.remind_after = Some(now.saturating_add(REMIND_LATER_SECS));
        if self.skipped_version.as_deref() == Some(version) {
            self.skipped_version = None;
        }
    }

    fn skip(&mut self, version: &str) {
        self.last_prompted = Some(version.to_owned());
        self.remind_after = None;
        self.skipped_version = Some(version.to_owned());
    }
}

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

/// GPUI 主壳使用自己的事件循环；检查结果进入现有 shell event pump，先显示
/// 轻通知，再由用户决定是否打开更新详情弹窗。
#[cfg(feature = "gpui-shell")]
pub fn spawn_gpui_once(sender: std::sync::mpsc::Sender<crate::gpui_shell::GpuiShellEvent>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-check-gpui".into()).spawn(move || {
        if cfg!(debug_assertions) {
            // 调试包启动后强制走一次完整的“事件 -> 右下角通知 -> 详情弹窗”链路。
            // STARTED 仍保证每进程仅一次；正式包的该条件恒为 false，不会触发。
            std::thread::sleep(Duration::from_secs(2));
            let current = env!("CARGO_PKG_VERSION").to_owned();
            let result = UpdateCheckResult {
                latest: format!("{current}-test"),
                current,
                update_available: true,
            };
            log::info!("update-check: forcing one GPUI update reminder in debug build");
            let _ = sender.send(crate::gpui_shell::GpuiShellEvent::UpdateAvailable(result));
            return;
        }

        // 对齐旧壳：首屏和首个终端会话稳定后再联网。
        std::thread::sleep(Duration::from_secs(12));
        let result = match check_now() {
            Ok(result) => result,
            Err(error) => {
                log::debug!("update-check: {error}");
                return;
            },
        };
        if !result.update_available {
            log::debug!("update-check: v{} is current (latest v{})", result.current, result.latest);
            return;
        }
        if !should_prompt(&result.latest) {
            log::debug!("update-check: automatic prompt suppressed for v{}", result.latest);
            return;
        }
        let _ = sender.send(crate::gpui_shell::GpuiShellEvent::UpdateAvailable(result));
    });
    if let Err(error) = spawned {
        log::debug!("update-check: GPUI thread spawn failed: {error}");
    }
}

/// 立即检查 GitHub 最新 release；调用方必须把它放到后台执行器，避免
/// `curl` 的网络等待阻塞 UI 线程。
pub fn check_now() -> Result<UpdateCheckResult, String> {
    let latest = fetch_latest_version()?;
    let current = env!("CARGO_PKG_VERSION").to_owned();
    Ok(UpdateCheckResult { update_available: is_newer(&latest, &current), current, latest })
}

fn update_state_path() -> std::path::PathBuf {
    nebula_settings::settings_dir().join(UPDATE_STATE_FILE)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

fn load_prompt_state() -> UpdatePromptState {
    let path = update_state_path();
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("update-check: ignored invalid {}: {error}", path.display());
                UpdatePromptState::default()
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => UpdatePromptState::default(),
        Err(error) => {
            log::warn!("update-check: failed to read {}: {error}", path.display());
            UpdatePromptState::default()
        },
    }
}

fn update_prompt_state(change: impl FnOnce(&mut UpdatePromptState)) -> Result<(), String> {
    let _guard = UPDATE_STATE_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let path = update_state_path();
    let Some(_file_lock) = crate::atomic_file::try_lock(&path)
        .map_err(|error| format!("无法锁定更新提醒状态：{error}"))?
    else {
        return Err("更新提醒状态正由另一个 Nebula 进程写入".to_owned());
    };
    let mut state = load_prompt_state();
    change(&mut state);
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|error| format!("无法序列化更新提醒状态：{error}"))?;
    crate::atomic_file::write(&path, &bytes)
        .map_err(|error| format!("无法保存更新提醒状态：{error}"))
}

pub fn should_prompt(version: &str) -> bool {
    let _guard = UPDATE_STATE_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    load_prompt_state().should_prompt(version, now_secs())
}

pub fn mark_prompted(version: &str) -> Result<(), String> {
    update_prompt_state(|state| state.mark_prompted(version))
}

pub fn remind_later(version: &str) -> Result<(), String> {
    let now = now_secs();
    update_prompt_state(|state| state.remind_later(version, now))
}

pub fn skip_version(version: &str) -> Result<(), String> {
    update_prompt_state(|state| state.skip(version))
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
    use super::{REMIND_LATER_SECS, UpdatePromptState, is_newer};

    #[test]
    fn version_comparison_is_numeric_per_segment() {
        assert!(is_newer("0.7.1", "0.7.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.7.0", "0.7.0"));
        assert!(!is_newer("0.6.9", "0.7.0"));
        assert!(!is_newer("0.7.0-rc1", "0.7.0"));
    }

    #[test]
    fn prompt_state_snoozes_only_the_prompted_version_for_three_days() {
        let mut state = UpdatePromptState::default();
        assert!(state.should_prompt("1.2.0", 1_000));

        state.mark_prompted("1.2.0");
        assert!(!state.should_prompt("1.2.0", 1_000));

        state.remind_later("1.2.0", 1_000);
        assert!(!state.should_prompt("1.2.0", 1_000 + REMIND_LATER_SECS - 1));
        assert!(state.should_prompt("1.2.0", 1_000 + REMIND_LATER_SECS));
        assert!(state.should_prompt("1.3.0", 1_001), "新版本不继承旧版本延迟");
    }

    #[test]
    fn prompt_state_skips_one_version_without_hiding_the_next() {
        let mut state = UpdatePromptState::default();
        state.skip("1.2.0");

        assert!(!state.should_prompt("1.2.0", 1_000));
        assert!(state.should_prompt("1.3.0", 1_000));

        state.remind_later("1.2.0", 1_000);
        assert_eq!(state.skipped_version, None, "稍后提醒应重新启用该版本");
    }
}
