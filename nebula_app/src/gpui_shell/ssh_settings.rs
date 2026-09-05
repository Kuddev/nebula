//! 设置页的 SSH 区（`SettingsPane` 的 SSH 专属 impl 拆分文件）。
//!
//! 字段/事件仍住在 [`SettingsPane`](super::settings_pane::SettingsPane)；
//! 这里承载 SSH 主机列表、添加/编辑弹窗、连接测试与删除撤销的全部行为。
//! 数据合同与旧壳共享：列表三键走 `ssh_hosts::SshHostLists`（内部是
//! `display::merge_ssh_hosts` 单一权威），认证配置走 `ssh_profiles`，密码
//! 走 Windows 凭据管理器，测试连接走 `ssh_session::run_test`。
//!
//! 与旧壳的行为对齐要点：
//! - 文案合同（占位符/状态行）跟随旧壳 `ssh_editor_render`；
//! - 添加/编辑弹窗采用基本与高级两页，正文独立滚动，底部操作始终可见；
//! - 密钥模式保留文件列表与「添加私钥」，文件对话框用旧壳
//!   同一套 `pem` / `key` / `ppk` / `id_*` 过滤器；
//! - 系统口令弹窗修复在 `ssh_session`：解析失败不再误判为「缺口令」；
//! - 删除主机先移出列表并开一个 8 秒撤销窗口，窗口结束才清理 Profile 与
//!   凭据（旧壳 Undo 条同义）；
//! - 端口输入只接受至多 5 位数字（旧壳键入即过滤）。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Entity, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    anchored, deferred, div, px,
};

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::settings_pane::{SettingsPane, SettingsPaneEvent, SshStatus};
use crate::gpui_shell::widgets::NebulaButton;

mod editor;
mod advanced;

/// 删除撤销窗口时长，旧壳 Undo 条同值。
const SSH_DELETE_UNDO_SECS: u64 = 8;

const SSH_EDITOR_CTL_H: f32 = 36.0;
const SSH_EDITOR_AVATAR_H: f32 = 44.0;
const SSH_HOST_ROW_H: f32 = 58.0;
const SSH_HOST_GAP: f32 = 8.0;

fn push_username_suggestion(suggestions: &mut Vec<String>, username: &str) {
    let username = username.trim();
    if !username.is_empty() && !suggestions.iter().any(|existing| existing == username) {
        suggestions.push(username.to_owned());
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SshValidationError {
    InvalidPort,
    MissingDestination,
    UnsafeDestination,
}

impl SshValidationError {
    pub(super) fn text(self, language: crate::display::UiLanguage) -> &'static str {
        match self {
            Self::InvalidPort => language
                .pick("端口需要是 1–65535 之间的数字", "The port must be a number from 1 to 65535"),
            Self::MissingDestination => language.pick(
                "请输入 SSH 地址，例如 user@example.com",
                "Enter an SSH address, for example user@example.com",
            ),
            Self::UnsafeDestination => language.pick(
                "地址不能包含空白、控制字符或 shell 分隔符",
                "The address cannot contain whitespace, control characters, or shell separators",
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum SshEditorTestStatus {
    Connecting,
    Succeeded(u64),
    Failed(String),
    TaskEnded,
}

impl SshEditorTestStatus {
    fn is_error(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::TaskEnded)
    }

    fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Connecting => language.pick("正在连接…", "Connecting...").into(),
            Self::Succeeded(elapsed_ms) => {
                format!("{} · {elapsed_ms} ms", language.pick("连接成功", "Connection succeeded"))
            },
            Self::Failed(message) => message.clone(),
            Self::TaskEnded => language
                .pick(
                    "连接测试任务意外结束，请重试",
                    "The connection test ended unexpectedly; try again",
                )
                .into(),
        }
    }
}

/// 设置页 SSH 添加/编辑面板的非文本草稿。文字实体常驻在 `SettingsPane`，
/// 这样输入事件可以统一使测试结果失效；草稿只保存认证、密钥和编辑身份。
#[derive(Clone)]
pub(super) struct SshEditorState {
    /// 每次打开递增。文件选择器等异步回调只可写回发起它的编辑会话。
    pub(super) id: u64,
    pub(super) original_destination: Option<String>,
    pub(super) advanced: bool,
    pub(super) connection: crate::ssh_profiles::SshConnectionOptions,
    pub(super) original_connection: crate::ssh_profiles::SshConnectionOptions,
    pub(super) clear_proxy_password: bool,
    pub(super) jump_picker_open: bool,
    pub(super) jump_choices: Vec<(String, String)>,
    pub(super) auth: crate::ssh_profiles::SshAuthMode,
    pub(super) icon: Option<String>,
    pub(super) private_keys: Vec<std::path::PathBuf>,
    pub(super) save_password: bool,
    pub(super) show_password: bool,
    /// 最近使用用户名 + 已保存/config 主机里解析出的用户名。只作候选，不
    /// 限制自由输入；最终仍拼回原有 destination 存储合同。
    pub(super) username_suggestions: Vec<String>,
    pub(super) revision: u64,
    pub(super) test_request_id: Option<u64>,
    pub(super) test_status: Option<SshEditorTestStatus>,
}

impl SshEditorState {
    pub(super) fn new(id: u64, original_destination: Option<String>) -> Self {
        Self {
            id,
            original_destination,
            advanced: false,
            connection: crate::ssh_profiles::SshConnectionOptions::default(),
            original_connection: crate::ssh_profiles::SshConnectionOptions::default(),
            clear_proxy_password: false,
            jump_picker_open: false,
            jump_choices: Vec::new(),
            // 与旧壳新增主机一致：第一次通常有密码，默认密码模式避免让
            // 新用户先因 Auto 没有可用私钥而得到一次无意义的认证失败。
            auth: crate::ssh_profiles::SshAuthMode::Password,
            icon: None,
            private_keys: Vec::new(),
            save_password: crate::platform::credentials::can_store(),
            show_password: false,
            username_suggestions: Vec::new(),
            revision: 0,
            test_request_id: None,
            test_status: None,
        }
    }

    pub(super) fn testing(&self) -> bool {
        self.test_request_id.is_some()
    }
}

/// 一次未决的删除：列表已改，Profile/凭据的清理延迟到撤销窗口结束。
/// 快照持有删除前的三键列表，撤销即整体恢复。
pub(super) struct SshDeleteUndo {
    pub(super) host: String,
    pub(super) lists_before: crate::gpui_shell::ssh_hosts::SshHostLists,
    pub(super) from_config: bool,
    pub(super) seq: u64,
}

impl SettingsPane {
    /// SSH 非破坏性列表操作的统一收尾：写盘、报状态、清确认态。
    /// Profile/凭据的最终删除走 [`Self::delete_ssh_host`]，不能混进置顶/恢复。
    pub(super) fn ssh_apply(
        &mut self,
        mutate: impl FnOnce(&mut crate::gpui_shell::ssh_hosts::SshHostLists),
        status: SshStatus,
        cx: &mut Context<Self>,
    ) {
        // 列表将被改写，未决删除的快照会过期：先提交它。
        self.commit_pending_ssh_delete();
        mutate(&mut self.ssh_hosts);
        match self.ssh_hosts.persist() {
            Ok(()) => self.ssh_status = Some(status),
            Err(err) => self.ssh_status = Some(SshStatus::PersistFailed(err.to_string())),
        }
        self.ssh_delete_confirm = None;
        cx.notify();
    }

    /// 二次确认后的删除：立刻移出列表并写盘，但 Profile 与凭据留到 8 秒
    /// 撤销窗口结束（旧壳 Undo 合同——期内撤销可完整恢复）。
    pub(super) fn delete_ssh_host(&mut self, host: &str, cx: &mut Context<Self>) {
        let profile_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
        let profiles = match crate::ssh_profiles::SshProfiles::load(&profile_path) {
            Ok(profiles) => profiles,
            Err(error) => {
                self.ssh_status = Some(SshStatus::ProfileLoadFailed(error.to_string()));
                self.ssh_delete_confirm = None;
                cx.notify();
                return;
            },
        };
        let dependents = profiles.jump_dependents(host);
        if !dependents.is_empty() {
            let language = crate::gpui_shell::config::ui_language(cx);
            self.ssh_status = Some(SshStatus::Error(format!(
                "{}: {}",
                language.pick(
                    "此主机仍被用作跳板，请先修改以下主机的高级设置后再删除",
                    "This host is still used as a jump host. Update these hosts' advanced settings before deleting it",
                ),
                dependents.join(", "),
            )));
            self.ssh_delete_confirm = None;
            cx.notify();
            return;
        }
        // 一次只挂一个撤销窗口：新删除先提交上一个。
        self.commit_pending_ssh_delete();

        let lists_before = self.ssh_hosts.clone();
        let from_config = self.ssh_hosts.is_from_config(host);
        let mut hosts = self.ssh_hosts.clone();
        hosts.remove(host);
        if let Err(error) = hosts.persist() {
            self.ssh_status = Some(SshStatus::DeleteFailed(error.to_string()));
            self.ssh_delete_confirm = None;
            cx.notify();
            return;
        }
        self.ssh_hosts = hosts;
        self.ssh_undo_seq = self.ssh_undo_seq.wrapping_add(1).max(1);
        let seq = self.ssh_undo_seq;
        self.ssh_delete_undo =
            Some(SshDeleteUndo { host: host.to_owned(), lists_before, from_config, seq });
        self.ssh_delete_confirm = None;
        self.ssh_status = None;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(SSH_DELETE_UNDO_SECS))
                .await;
            let _ = this.update(cx, |pane, cx| {
                if pane.ssh_delete_undo.as_ref().is_some_and(|undo| undo.seq == seq) {
                    pane.commit_pending_ssh_delete();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// 撤销窗口结束（或被新操作顶替）时的最终提交：清 Profile 与凭据。
    /// 与旧壳撤销期结束后的提交同义。
    pub(super) fn commit_pending_ssh_delete(&mut self) {
        let Some(undo) = self.ssh_delete_undo.take() else { return };
        let host = undo.host;

        let mut cleanup_errors = Vec::new();
        let profile_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
        match crate::ssh_profiles::SshProfiles::load(&profile_path) {
            Ok(mut profiles) => {
                let proxy_credential = profiles.for_destination(&host).connection.proxy_credential_target(&host);
                profiles.remove(&host);
                if let Err(error) = profiles.save(&profile_path) {
                    cleanup_errors.push(format!("Profile: {error}"));
                } else if let Some(target) = proxy_credential {
                    if let Err(error) = crate::ssh_credentials::delete_generic_secret(&target) {
                        cleanup_errors.push(format!("Proxy credential: {error}"));
                    }
                }
            },
            Err(error) => cleanup_errors.push(format!("Profile: {error}")),
        }
        #[cfg(windows)]
        if let Err(error) = crate::ssh_credentials::forget_password(&host) {
            cleanup_errors.push(format!("Credential: {error}"));
        }

        self.ssh_status = if cleanup_errors.is_empty() {
            Some(SshStatus::DeleteCommitted { hidden_config: undo.from_config })
        } else {
            Some(SshStatus::CleanupPartial(cleanup_errors.join("; ")))
        };
    }

    /// 撤销未决删除：恢复删除前的三键快照（Profile/凭据从未被动过）。
    pub(super) fn undo_ssh_delete(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.ssh_delete_undo.take() else { return };
        if let Err(error) = undo.lists_before.persist() {
            self.ssh_status = Some(SshStatus::UndoFailed(error.to_string()));
            cx.notify();
            return;
        }
        self.ssh_hosts = undo.lists_before;
        self.ssh_status = Some(SshStatus::Restored(undo.host));
        cx.notify();
    }

    /// 一旦草稿字段变动，之前那次测试就不再能证明当前配置。请求本身不取消
    /// （网络任务应当自行收尾），但它返回时会按 revision 丢弃过期结果。
    pub(super) fn touch_ssh_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.ssh_editor.as_mut() {
            editor.revision = editor.revision.wrapping_add(1);
            editor.test_request_id = None;
            editor.test_status = None;
            self.ssh_status = None;
            cx.notify();
        }
    }

    pub(super) fn set_ssh_editor_masking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let masked = self.ssh_editor.as_ref().is_none_or(|editor| !editor.show_password);
        self.ssh_password_input.update(cx, |input, cx| input.set_masked(masked, window, cx));
    }

    pub(super) fn open_ssh_editor(
        &mut self,
        destination: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let adding = destination.is_none();
        let profile_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
        let profiles =
            crate::ssh_profiles::SshProfiles::load(&profile_path).unwrap_or_else(|err| {
                log::warn!("加载 SSH Profile 失败，使用默认草稿: {err}");
                crate::ssh_profiles::SshProfiles::default()
            });
        let profile = destination.as_deref().map(|host| profiles.for_destination(host));
        let (address_with_user, port) =
            destination.as_deref().map(crate::display::split_destination_port).unwrap_or_default();
        let (mut username, address) = crate::display::split_destination_user(&address_with_user);
        // 新增主机的常用服务器账号直接给出可编辑实值；placeholder 不会参与
        // destination 拼装，用户只填 IP 时此前实际会退回本机账号。
        if adding {
            username = "root".to_owned();
        }
        self.ssh_editor_seq = self.ssh_editor_seq.wrapping_add(1).max(1);
        let mut editor = SshEditorState::new(self.ssh_editor_seq, destination);
        editor.username_suggestions = profiles.usernames();
        let labels = profiles.labels();
        editor.jump_choices = self.ssh_hosts.merged().into_iter()
            .filter(|host| editor.original_destination.as_deref() != Some(host))
            .map(|host| {
                let label = labels.get(&host).cloned().unwrap_or_else(|| host.clone());
                (host, label)
            })
            .collect();
        for host in self.ssh_hosts.merged() {
            let (address, _) = crate::display::split_destination_port(&host);
            let (username, _) = crate::display::split_destination_user(&address);
            push_username_suggestion(&mut editor.username_suggestions, &username);
        }
        if let Some(profile) = profile.as_ref() {
            editor.auth = profile.auth;
            editor.icon = profile.icon.clone();
            editor.private_keys = profile.private_keys.clone();
            editor.connection = profile.connection.clone();
            editor.original_connection = profile.connection.clone();
        }
        let connection = &editor.connection;
        self.ssh_proxy_host_input.update(cx, |input, cx| input.set_value(connection.proxy_host.clone(), window, cx));
        self.ssh_proxy_port_input.update(cx, |input, cx| {
            input.set_value(connection.proxy_port.map(|port| port.to_string()).unwrap_or_default(), window, cx);
            input.set_placeholder(connection.effective_proxy_port().to_string(), window, cx);
        });
        self.ssh_proxy_username_input.update(cx, |input, cx| input.set_value(connection.proxy_username.clone(), window, cx));
        self.ssh_proxy_password_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
        self.ssh_jump_host_input.update(cx, |input, cx| input.set_value(connection.jump_host.clone(), window, cx));
        let label = profile.and_then(|profile| profile.label).unwrap_or_default();
        self.ssh_username_input.update(cx, |input, cx| input.set_value(username, window, cx));
        self.ssh_destination_input.update(cx, |input, cx| input.set_value(address, window, cx));
        self.ssh_port_input.update(cx, |input, cx| input.set_value(port, window, cx));
        self.ssh_label_input.update(cx, |input, cx| input.set_value(label, window, cx));
        // 存储的秘密绝不回填文本框；空值意味着编辑已有主机时保留原凭据。
        self.ssh_password_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            let language = crate::gpui_shell::config::ui_language(cx);
            input.set_placeholder(if adding {
                language.pick("留空则连接时询问", "Leave empty to ask when connecting")
            } else {
                language.pick("留空则保留原有凭据", "Leave empty to keep existing credentials")
            }, window, cx);
        });
        // 图标选择器每次开编辑器都从收起、无搜索词开始：上一台主机的搜索词
        // 留在框里，下一台打开时列表看着像被莫名筛过。
        self.ssh_icon_picker_open = false;
        self.ssh_icon_trigger_bounds = None;
        self.ssh_username_picker_open = false;
        self.ssh_username_trigger_bounds = None;
        self.ssh_icon_filter_input.update(cx, |input, cx| input.set_value("", window, cx));
        // 弹层只能有一个；字体目录若还开着会用它的页面级拦截层盖住
        // SSH 编辑器，因此先把它收起。
        self.font_picker_open = false;
        self.ssh_editor = Some(editor);
        self.ssh_status = None;
        self.set_ssh_editor_masking(window, cx);
        self.ssh_destination_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn close_ssh_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_editor = None;
        self.ssh_username_picker_open = false;
        self.ssh_username_trigger_bounds = None;
        self.ssh_password_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.ssh_proxy_password_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
        self.ssh_icon_picker_open = false;
        self.ssh_icon_trigger_bounds = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(super) fn ssh_destination_from_draft(
        &self,
        cx: &gpui::App,
    ) -> Result<String, SshValidationError> {
        let port = self.ssh_port_input.read(cx).value().trim().to_string();
        if !port.is_empty() && !port.parse::<u16>().is_ok_and(|value| value > 0) {
            // en-dash 区间写法与旧壳一字不差。
            return Err(SshValidationError::InvalidPort);
        }
        let username = self.ssh_username_input.read(cx).value().trim().to_string();
        let address = self.ssh_destination_input.read(cx).value().trim().to_string();
        let (_, host) = crate::display::split_destination_user(&address);
        if host.is_empty() {
            return Err(SshValidationError::MissingDestination);
        }
        let address = crate::display::join_destination_user(&username, &address);
        let destination = crate::display::join_destination_port(&address, &port);
        if destination.is_empty() {
            return Err(SshValidationError::MissingDestination);
        }
        if destination
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ";&|<>\"'`".contains(ch))
        {
            return Err(SshValidationError::UnsafeDestination);
        }
        Ok(destination)
    }

    pub(super) fn add_ssh_private_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor_id) = self.ssh_editor.as_ref().map(|editor| editor.id) else {
            return;
        };
        // 与旧壳同一套过滤器（pem/key/ppk/id_*），但绝不能在 update 借用里
        // 同步转 GetOpenFileNameW：模态对话框的消息泵会重入 GPUI wndproc，
        // AppCell 二次可变借用直接 panic（1.1.0 包「点私钥闪退」的根因）。
        // 对话框挪到专用线程（不是后台执行器——用户可能把对话框开着很久，
        // 不能占死池线程），owner HWND 跨线程挂靠是 Win32 支持的用法；
        // 结果回 UI 线程后先核对编辑器代际，面板已关/重开就丢弃。
        let hwnd = ssh_key_dialog_owner(window);
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(pick_ssh_private_key_blocking(hwnd));
        });
        cx.spawn(async move |this, cx| {
            let Ok(Some(result)) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.ssh_editor.as_ref().map(|editor| editor.id) != Some(editor_id) {
                    return;
                }
                match result {
                    Ok(path) => {
                        if let Some(editor) = this.ssh_editor.as_mut() {
                            if crate::display::push_private_key(&mut editor.private_keys, path) {
                                editor.revision = editor.revision.wrapping_add(1);
                                editor.test_request_id = None;
                                editor.test_status = None;
                                this.ssh_status = None;
                            }
                        }
                    },
                    Err(message) => this.ssh_status = Some(SshStatus::Error(message)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn test_ssh_editor(&mut self, cx: &mut Context<Self>) {
        let destination = match self.ssh_destination_from_draft(cx) {
            Ok(destination) => destination,
            Err(error) => {
                if let Some(editor) = self.ssh_editor.as_mut() {
                    editor.advanced = false;
                }
                self.ssh_status = Some(SshStatus::Validation(error));
                cx.notify();
                return;
            },
        };
        let connection = match self.ssh_connection_from_draft(&destination, cx) {
            Ok(connection) => connection,
            Err(error) => {
                if let Some(editor) = self.ssh_editor.as_mut() {
                    editor.advanced = true;
                }
                self.ssh_status = Some(SshStatus::Error(error));
                cx.notify();
                return;
            },
        };
        let (proxy_password, password) = match self.ssh_test_passwords(&destination, &connection, cx) {
            Ok(passwords) => passwords,
            Err(error) => {
                self.ssh_status = Some(SshStatus::Error(error.to_string()));
                cx.notify();
                return;
            },
        };
        let Some(editor) = self.ssh_editor.as_mut() else { return };
        if editor.testing() {
            return;
        }
        self.ssh_test_seq = self.ssh_test_seq.wrapping_add(1).max(1);
        let request_id = self.ssh_test_seq;
        let revision = editor.revision;
        let request = crate::ssh_session::SshTestRequest {
            request_id,
            destination: destination.clone(),
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
            connection,
            proxy_password,
            password,
        };
        let receiver = match crate::ssh_session::start_test(request) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.ssh_status = Some(SshStatus::TestStartFailed(error.to_string()));
                cx.notify();
                return;
            },
        };
        editor.test_request_id = Some(request_id);
        editor.test_status = Some(SshEditorTestStatus::Connecting);
        self.ssh_status = None;
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            let _ = this.update(cx, |pane, cx| {
                let Some(editor) = pane.ssh_editor.as_mut() else { return };
                if editor.revision != revision || editor.test_request_id != Some(request_id) {
                    return;
                }
                editor.test_request_id = None;
                editor.test_status = Some(match result {
                    Ok(result) if result.ok => SshEditorTestStatus::Succeeded(result.elapsed_ms),
                    Ok(result) => SshEditorTestStatus::Failed(result.message),
                    Err(_) => SshEditorTestStatus::TaskEnded,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn save_ssh_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let destination = match self.ssh_destination_from_draft(cx) {
            Ok(destination) => destination,
            Err(error) => {
                if let Some(editor) = self.ssh_editor.as_mut() {
                    editor.advanced = false;
                }
                self.ssh_status = Some(SshStatus::Validation(error));
                cx.notify();
                return;
            },
        };
        let connection = match self.ssh_connection_from_draft(&destination, cx) {
            Ok(connection) => connection,
            Err(error) => {
                if let Some(editor) = self.ssh_editor.as_mut() {
                    editor.advanced = true;
                }
                self.ssh_status = Some(SshStatus::Error(error));
                cx.notify();
                return;
            },
        };
        let proxy_password = self.ssh_proxy_password_from_draft(cx);
        if proxy_password.as_deref().is_some_and(|password| !password.is_empty())
            && !crate::platform::credentials::can_store()
        {
            self.ssh_status = Some(SshStatus::Error(crate::gpui_shell::config::ui_language(cx).pick(
                "系统凭据库不可用，无法安全保存代理密码。请先启用系统凭据库。",
                "The system credential store is unavailable. Enable it before saving a proxy password.",
            ).to_owned()));
            if let Some(editor) = self.ssh_editor.as_mut() {
                editor.advanced = true;
            }
            cx.notify();
            return;
        }
        let Some(mut editor) = self.ssh_editor.take() else { return };
        let original = editor.original_destination.clone();
        let profile_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
        let mut profiles = match crate::ssh_profiles::SshProfiles::load(&profile_path) {
            Ok(profiles) => profiles,
            Err(error) => {
                self.ssh_status = Some(SshStatus::ProfileLoadFailed(error.to_string()));
                self.ssh_editor = Some(editor);
                cx.notify();
                return;
            },
        };
        let previous_connection = original.as_deref().map(|_| editor.original_connection.clone());
        // 保存会改列表：未决删除的快照过期，先提交。
        self.commit_pending_ssh_delete();
        // 先在副本里计算新列表，直到 Profile 与 settings 两份数据都写成功
        // 才替换页面状态；Profile 写盘失败时背景列表不能先显示新地址。
        let mut hosts = self.ssh_hosts.clone();
        if let Some(original) = original.as_deref().filter(|old| *old != destination) {
            profiles.rename(original, &destination);
            hosts.remove(original);
        }
        let label = match self.ssh_label_input.read(cx).value().trim() {
            "" => profiles.next_default_label(
                crate::gpui_shell::config::ui_language(cx).pick("主机", "Host"),
            ),
            value => value.to_owned(),
        };
        let (address, _) = crate::display::split_destination_port(&destination);
        let (username, _) = crate::display::split_destination_user(&address);
        profiles.remember_username(&username);
        profiles.upsert(crate::ssh_profiles::SshProfileAuth {
            destination: destination.clone(),
            auth: editor.auth,
            private_keys: editor.private_keys.clone(),
            label: Some(label),
            icon: editor.icon.clone(),
            connection: connection.clone(),
        });
        if let Err(error) = profiles.save(&profile_path) {
            self.ssh_status = Some(SshStatus::ProfileSaveFailed(error.to_string()));
            self.ssh_editor = Some(editor);
            cx.notify();
            return;
        }
        hosts.remember(&destination);
        if let Err(error) = hosts.persist() {
            self.ssh_status = Some(SshStatus::HostListSaveFailed(error.to_string()));
            self.ssh_editor = Some(editor);
            cx.notify();
            return;
        }
        self.ssh_hosts = hosts;
        if let Err(error) = advanced::save_proxy_credential(
            &destination,
            &connection,
            original.as_deref().zip(previous_connection.as_ref()),
            proxy_password,
        ) {
            let language = crate::gpui_shell::config::ui_language(cx);
            self.ssh_status = Some(SshStatus::Error(format!("{}: {error}", language.pick(
                "主机信息已保存，但代理凭据更新失败，请重试",
                "Host details were saved, but proxy credentials could not be updated; try again",
            ))));
            editor.advanced = true;
            self.ssh_editor = Some(editor);
            cx.notify();
            return;
        }
        let mut password = zeroize::Zeroizing::new(self.ssh_password_input.read(cx).value().to_string());
        if password.is_empty() && editor.save_password && crate::display::auth_sections(editor.auth).0 {
            if let Some(original) = original.as_deref().filter(|old| *old != destination) {
                match crate::ssh_credentials::load_stored_password(original) {
                    Ok(Some(secret)) => {
                        let secret = zeroize::Zeroizing::new(secret);
                        match std::str::from_utf8(&secret) {
                            Ok(secret) => *password = secret.to_owned(),
                            Err(error) => {
                                self.ssh_status = Some(SshStatus::CredentialSaveFailed(error.to_string()));
                                self.ssh_editor = Some(editor);
                                cx.notify();
                                return;
                            },
                        }
                    },
                    Ok(None) => {},
                    Err(error) => {
                        self.ssh_status = Some(SshStatus::CredentialSaveFailed(error.to_string()));
                        self.ssh_editor = Some(editor);
                        cx.notify();
                        return;
                    },
                }
            }
        }
        if crate::display::auth_sections(editor.auth).0
            && editor.save_password
            && !password.is_empty()
        {
            if let Err(error) =
                crate::ssh_credentials::store_password(&destination, password.as_bytes())
            {
                self.ssh_status = Some(SshStatus::CredentialSaveFailed(error.to_string()));
                self.ssh_editor = Some(editor);
                cx.notify();
                return;
            }
        }
        // Credential Manager 以 destination 为键。重命名后旧键既不会被新
        // 连接使用，也不应无限留下；与旧壳一致，在配置和列表都成功写入后
        // 才删除它，避免前面的落盘失败反而丢掉仍可用的凭据。
        let credential_cleanup_error = original
            .as_deref()
            .filter(|old| *old != destination)
            .and_then(|old| crate::ssh_credentials::forget_password(old).err());
        // 只在落盘流程结束后清掉明文；编辑已有主机且密码框为空时不触碰
        // 当前 destination 的凭据。
        self.ssh_password_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.ssh_proxy_password_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
        self.ssh_icon_picker_open = false;
        self.ssh_username_picker_open = false;
        editor.private_keys.clear();
        self.ssh_editor = None;
        self.ssh_status = credential_cleanup_error.map_or_else(
            || Some(SshStatus::Saved(destination.clone())),
            |error| {
                Some(SshStatus::SavedWithCleanupError {
                    destination: destination.clone(),
                    error: error.to_string(),
                })
            },
        );
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    // ---- UI：添加/编辑弹窗 ----

    // ---- UI：可编辑用户名历史 ----

    fn ssh_username_control(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pane = cx.entity().downgrade();
        let open = self.ssh_username_picker_open;
        let language = crate::gpui_shell::config::ui_language(cx);
        h_flex()
            .relative()
            .w_full()
            // 输入框/下拉按钮都属于弹层触发器；阻止鼠标冒泡到模态遮罩，
            // 否则聚焦刚把候选打开，同一次点击又会被遮罩立即关闭。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Input::new(&self.ssh_username_input)
                    .h(px(SSH_EDITOR_CTL_H))
                    .text_sm()
                    .aria_label(language.pick("用户名", "Username"))
                    .bg(cx.theme().popover)
                    .suffix(Button::new("ssh-username-history")
                    .icon(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                    .ghost()
                    .xsmall()
                    .tooltip(crate::gpui_shell::config::ui_language(cx).pick(
                        "最近使用的用户名",
                        "Recently used usernames",
                    ))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_ssh_username_picker(window, cx);
                    }))),
            )
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = pane.update(cx, |pane, cx| {
                            if pane.ssh_username_trigger_bounds == Some(bounds) {
                                return;
                            }
                            pane.ssh_username_trigger_bounds = Some(bounds);
                            if pane.ssh_username_picker_open {
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    fn toggle_ssh_username_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_username_picker_open = !self.ssh_username_picker_open;
        self.ssh_username_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn select_ssh_username(
        &mut self,
        username: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh_username_input.update(cx, |input, cx| {
            input.set_value(username, window, cx);
            input.focus(window, cx);
        });
        self.ssh_username_picker_open = false;
        cx.notify();
    }

    fn ssh_username_popup(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.ssh_username_picker_open {
            return None;
        }
        let trigger = self.ssh_username_trigger_bounds?;
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let query = self.ssh_username_input.read(cx).value().trim().to_ascii_lowercase();
        let current = self.ssh_username_input.read(cx).value().trim().to_owned();
        let suggestions = self.ssh_editor.as_ref()?.username_suggestions.clone();
        let rows: Vec<gpui::AnyElement> = suggestions
            .into_iter()
            .filter(|username| query.is_empty() || username.to_ascii_lowercase().contains(&query))
            .take(8)
            .enumerate()
            .map(|(index, username)| {
                let selected = username == current;
                let picked = username.clone();
                h_flex()
                    .id(SharedString::from(format!("ssh-username-row-{index}")))
                    .h(px(30.0))
                    .w_full()
                    .px_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.list_active))
                    .hover(|row| row.bg(theme.list_hover))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(username))
                    .when(selected, |row| row.child(Icon::new(IconName::CircleCheck).xsmall()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_ssh_username(picked.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let row_count = rows.len();
        let panel = v_flex()
            .w(trigger.size.width.max(px(200.0)))
            .p_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(if rows.is_empty() {
                div()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(language.pick("没有匹配的历史用户名", "No matching usernames"))
                    .into_any_element()
            } else {
                v_flex()
                    .h(px((row_count as f32 * 30.0).min(240.0)))
                    .overflow_y_scrollbar()
                    .children(rows)
                    .into_any_element()
            });
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .position(trigger.bottom_left())
                    .offset(gpui::point(px(0.0), px(6.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(panel),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    // ---- UI：身份条头像与它的图标选择器 ----

    /// 身份条头像：画当前图标，点它开合选择器（旧壳 `SshEditorHit::Avatar`）。
    /// 头像本身就是那个控件——图标是「这台机器长什么样」的一部分，不是一个
    /// 摆在右上角、和名字并列的独立字段。
    fn ssh_avatar(
        &self,
        icon: &'static crate::display::ui::os_icons::OsIcon,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        const BASE_ICON_SIZE: f32 = 22.0;

        let theme = cx.theme();
        let open = self.ssh_icon_picker_open;
        let pane = cx.entity().downgrade();
        let target_ink_width = SSH_EDITOR_AVATAR_H * 0.46;
        let icon_size = BASE_ICON_SIZE
            * crate::display::ui::os_icons::scale_for(icon, BASE_ICON_SIZE * 0.6, target_ink_width);
        div()
            .id("ssh-icon-avatar")
            .relative()
            .size(px(SSH_EDITOR_AVATAR_H))
            .flex_shrink_0()
            .rounded(px(10.0))
            .border_1()
            .border_color(if open { theme.primary } else { theme.border })
            .bg(theme.group_box)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|avatar| avatar.bg(theme.list_hover))
            // Nerd Font 的 advance 固定为 0.6em，墨迹却能宽到 1.2em；直接
            // 居中文本盒会把溢出的那一半全留在右边。固定墨迹槽后，视觉中心
            // 才不会随图标变化。
            .child(
                div()
                    .w(px(target_ink_width))
                    .font_family(self.current_font_chain(cx))
                    .text_size(px(icon_size))
                    .child(icon.glyph.to_string()),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_ssh_icon_picker(window, cx);
            }))
            // 与字体目录同法：零绘制 canvas 捕获头像的真实窗口坐标，弹层
            // 据此锚定；滚动与 DPI 变化后依然贴着头像。
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = pane.update(cx, |pane, _| {
                            pane.ssh_icon_trigger_bounds = Some(bounds);
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    fn toggle_ssh_icon_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_icon_picker_open = !self.ssh_icon_picker_open;
        if self.ssh_icon_picker_open {
            // 打开即聚焦搜索框：记得名字的人比记得形状的人多，二十一个剪影
            // 摊开时，能直接打字才是最快的一条路（旧壳 `IconSearch` 同义）。
            self.ssh_icon_filter_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
        } else {
            self.ssh_destination_input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn select_ssh_icon(&mut self, id: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.ssh_editor.as_mut() {
            // 空 = 「自动识别」：存空串而不是某个具体形状，换了机器上的系统
            // 图标会自己跟着变（`os_icons::AUTO_ID` 的语义）。
            editor.icon = id.map(str::to_owned);
        }
        self.ssh_icon_picker_open = false;
        // 图标不影响连接有效性，因此保留测试结果；只清掉上一条保存/校验
        // 提示，避免它看起来仍在描述当前草稿。
        self.ssh_status = None;
        self.ssh_destination_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// 弹出的图标选择器：顶部搜索框 + 分组过的目录，锚在头像下方。
    /// 返回 `None` 表示收起态（或头像还没量到坐标）。
    fn ssh_icon_popup(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use crate::display::ui::os_icons::{
            AUTO_ID, CATALOG, PickerRow, picker_rows, resolve, scale_for,
        };

        const PICKER_ICON_SLOT_W: f32 = 22.0;
        const PICKER_ICON_INK_W: f32 = 16.0;
        const PICKER_ICON_BASE_SIZE: f32 = 15.0;

        if !self.ssh_icon_picker_open {
            return None;
        }
        let language = crate::gpui_shell::config::ui_language(cx);
        let trigger = self.ssh_icon_trigger_bounds?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover_bg = theme.list_hover;
        let selected_bg = theme.list_active;
        let font_chain = self.current_font_chain(cx);
        let current = self
            .ssh_editor
            .as_ref()
            .and_then(|editor| editor.icon.clone())
            .filter(|id| !id.is_empty() && id != AUTO_ID);
        let query = self.ssh_icon_filter_input.read(cx).value().to_string();
        // 目录、分组标题与搜索匹配全部走共享的 `picker_rows`：两壳的选择器
        // 因此永远列出同一批图标、按同一种方式分组、对同一个词命中。
        let picker_model = picker_rows(&query, language == crate::display::UiLanguage::ZhCn);
        let picker_content_h = picker_model
            .iter()
            .map(|row| match row {
                PickerRow::Group(_) => 24.0,
                PickerRow::Option(_) => 30.0,
            })
            .sum::<f32>()
            + picker_model.len().saturating_sub(1) as f32;
        let rows: Vec<gpui::AnyElement> = picker_model
            .into_iter()
            .enumerate()
            .map(|(ix, row)| match row {
                PickerRow::Group(title) => div()
                    .h(px(24.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(title)
                    .into_any_element(),
                PickerRow::Option(option) => {
                    let (icon, name, id) = match option.and_then(|index| CATALOG.get(index)) {
                        Some(icon) => (icon, language.pick(icon.zh, icon.en), Some(icon.id)),
                        // 未识别时头像本来就回落到 DEFAULT_ID；选择器也显示
                        // 同一张脸，避免挑选前后出现两枚不同的“终端”图标。
                        None => (resolve(None), language.pick("自动识别", "Auto detect"), None),
                    };
                    let icon_size = PICKER_ICON_BASE_SIZE
                        * scale_for(icon, PICKER_ICON_BASE_SIZE * 0.6, PICKER_ICON_INK_W);
                    let selected = match id {
                        Some(id) => current.as_deref() == Some(id),
                        None => current.is_none(),
                    };
                    let picked = id.map(str::to_owned);
                    h_flex()
                        .id(SharedString::from(format!("ssh-icon-row-{ix}")))
                        .h(px(30.0))
                        .w_full()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |row| row.bg(selected_bg))
                        .hover(|row| row.bg(hover_bg))
                        .child(
                            div()
                                .w(px(PICKER_ICON_SLOT_W))
                                .h_full()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .w(px(PICKER_ICON_INK_W))
                                        .font_family(font_chain.clone())
                                        .text_size(px(icon_size))
                                        .child(icon.glyph.to_string()),
                                ),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_ssh_icon(picked.as_deref(), window, cx);
                        }))
                        .into_any_element()
                },
            })
            .collect();

        let panel = v_flex()
            .w(px(240.0))
            .p_2()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(Input::new(&self.ssh_icon_filter_input).small())
            .child(if rows.is_empty() {
                div()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(language.pick("没有匹配的图标", "No matching icons"))
                    .into_any_element()
            } else {
                // 滚动组件必须拿到确定高度；只设 max-height 时，它在自动高度
                // 弹层中会按内容测量，滚动视口和滚动条都无法建立。
                v_flex()
                    .h(px(picker_content_h.min(260.0)))
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().gap(px(1.0)).children(rows))
                    .into_any_element()
            });

        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .position(trigger.bottom_left())
                    .offset(gpui::point(px(0.0), px(6.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(panel),
            )
            // 弹层必须压在模态遮罩之上，否则鼠标到不了搜索框和候选行。
            .with_priority(3)
            .into_any_element(),
        )
    }

    // ---- UI：设置页 SSH 分区 ----

    /// 主机列表：一个圆角卡片，行间细分隔线，操作按钮 hover 显形（连接为
    /// 文本按钮，编辑/置顶/删除为图标按钮）。删除走 8 秒撤销窗口。
    pub(super) fn section_ssh(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let muted = theme.muted_foreground;
        let hosts = self.ssh_hosts.merged();
        let host_count = hosts.len();
        let profiles = crate::ssh_profiles::SshProfiles::load(
            &crate::display::nebula_data_dir().join("ssh_profiles.json"),
        )
        .ok();
        let labels = profiles.as_ref().map(|profiles| profiles.labels()).unwrap_or_default();
        let icons = profiles.as_ref().map(|profiles| profiles.icons()).unwrap_or_default();
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let font_px = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.base_font_size_px)
            .unwrap_or(15.0);
        // 只保留标题→地址的行距（旧壳 title_y + 0.95*cell_h / 副行 0.78）。
        // 图标槽、行内 gap、卡片描边退回改前那套：整行重排后观感反而不如原来。
        let title_h = font_px;
        let subtitle_h = font_px * 0.78;
        let hidden: Vec<String> = self.ssh_hosts.hidden_hosts().to_vec();
        let delete_confirm = self.ssh_delete_confirm.clone();

        let host_rows = hosts.into_iter().enumerate().map(|(ix, host)| {
            let pinned = self.ssh_hosts.is_pinned(&host);
            let from_config = self.ssh_hosts.is_from_config(&host);
            let confirm = delete_confirm.as_deref() == Some(host.as_str());
            let label = labels.get(&host).cloned().unwrap_or_else(|| host.clone());
            // 行首 OS 图标（旧壳裁定 2026-08-09）：id 取自 ssh_profiles 存储，
            // 未认出回落通用终端形状；mono 字体渲染 Nerd Font 字位。
            let os_icon =
                crate::display::ui::os_icons::resolve(icons.get(&host).map(String::as_str));
            let connect_host = host.clone();
            let edit_host = host.clone();
            let pin_host = host.clone();
            let delete_host = host.clone();
            let row_group = SharedString::from(format!("ssh-host-actions-{ix}"));
            h_flex()
                .id(SharedString::from(format!("ssh-host-row-{ix}")))
                .group(row_group.clone())
                // 旧壳 `SSH_HOST_ROW_H` 固定 58px；两行文字与 OS 图标在
                // 这个高度里共用中线，不能压成普通 48px 设置行。
                .h(px(SSH_HOST_ROW_H))
                .w_full()
                .px_3()
                .items_center()
                .gap_3()
                .when(ix + 1 < host_count, |row| {
                    row.border_b_1().border_color(theme.border.opacity(0.5))
                })
                .hover(move |row| row.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .h_full()
                        .flex_shrink_0()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(symbol_family.clone())
                        .text_size(px(18.0))
                        .text_color(muted)
                        .text_center()
                        .child(os_icon.glyph.to_string())
                        // 旧壳把置顶记号压在图标槽右缘，不额外占一列；否则
                        // 置顶行的主机标题会比其它行整体右移。
                        .when(pinned, |slot| {
                            slot.child(
                                div()
                                    .absolute()
                                    .right(px(-2.0))
                                    .bottom(px(7.0))
                                    .text_size(px(8.0))
                                    .text_color(theme.primary)
                                    .child("\u{eab4}"),
                            )
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .justify_center()
                        .child(
                            div()
                                .h(px(title_h * 0.95))
                                .flex()
                                .items_center()
                                .text_size(px(title_h))
                                .line_height(px(title_h))
                                .truncate()
                                .child(label),
                        )
                        .child(
                            h_flex()
                                .h(px(subtitle_h))
                                .gap_2()
                                .items_center()
                                // 副行与旧壳同合同：只放目的地本身；来源用
                                // 小徽章表达（config 源的删除语义是隐藏）。
                                .child(
                                    div()
                                        .text_size(px(subtitle_h))
                                        .line_height(px(subtitle_h))
                                        .text_color(muted)
                                        .truncate()
                                        .child(host.clone()),
                                )
                                .when(from_config, |line| {
                                    line.child(
                                        div()
                                            .flex_shrink_0()
                                            .px(px(5.0))
                                            .rounded_sm()
                                            .text_xs()
                                            .text_color(muted)
                                            .border_1()
                                            .border_color(theme.border)
                                            .child("config"),
                                    )
                                }),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-connect-{ix}")))
                        .label(language.pick("连接", "Connect"))
                        .small()
                        .primary()
                        .invisible()
                        .group_hover(row_group.clone(), |button| button.visible())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.emit(SettingsPaneEvent::LaunchSsh(connect_host.clone()));
                            this.ssh_status = Some(SshStatus::Opening(connect_host.clone()));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-edit-{ix}")))
                        .icon(IconName::Settings2)
                        .ghost()
                        .small()
                        .tooltip(language.pick("编辑主机", "Edit host"))
                        .invisible()
                        .group_hover(row_group.clone(), |button| button.visible())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_ssh_editor(Some(edit_host.clone()), window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-pin-{ix}")))
                        .icon(if pinned { IconName::StarOff } else { IconName::Star })
                        .ghost()
                        .small()
                        .tooltip(if pinned {
                            language.pick("取消置顶", "Unpin")
                        } else {
                            language.pick("置顶", "Pin")
                        })
                        .invisible()
                        .group_hover(row_group.clone(), |button| button.visible())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ssh_apply(
                                |lists| lists.toggle_pin(&pin_host),
                                SshStatus::Pinned,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-delete-{ix}")))
                        .map(|button| {
                            if confirm {
                                button
                                    .label(language.pick("确认删除", "Confirm delete"))
                                    .danger()
                                    .small()
                            } else {
                                button
                                    .icon(IconName::Delete)
                                    .ghost()
                                    .small()
                                    .tooltip(if from_config {
                                        language.tr("settings.ssh.hide_config_host")
                                    } else {
                                        language.pick("删除", "Delete")
                                    })
                            }
                        })
                        // 进了确认态就常显：指针移开还让它隐形，等于把「再点
                        // 一次才真删」这个状态藏起来。
                        .when(!confirm, |button| {
                            button
                                .invisible()
                                .group_hover(row_group.clone(), |button| button.visible())
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.ssh_delete_confirm.as_deref() == Some(delete_host.as_str()) {
                                this.delete_ssh_host(&delete_host, cx);
                            } else {
                                this.ssh_delete_confirm = Some(delete_host.clone());
                                cx.notify();
                            }
                        })),
                )
        });

        let hidden_rows = self.ssh_show_hidden.then(|| {
            hidden
                .iter()
                .enumerate()
                .map(|(ix, host)| {
                    let restore_host = host.clone();
                    h_flex()
                        .h(px(32.0))
                        .w_full()
                        .px_3()
                        .items_center()
                        .gap_2()
                        .when(ix + 1 < hidden.len(), |row| {
                            row.border_b_1().border_color(theme.border.opacity(0.5))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(muted)
                                .truncate()
                                .child(host.clone()),
                        )
                        .child(
                            NebulaButton::new(SharedString::from(format!("ssh-restore-{ix}")))
                                .label(language.pick("恢复", "Restore"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ssh_apply(
                                        |lists| lists.restore_hidden(&restore_host),
                                        SshStatus::Restored(restore_host.clone()),
                                        cx,
                                    );
                                })),
                        )
                })
                .collect::<Vec<_>>()
        });

        let hidden_count = self.ssh_hosts.hidden_hosts().len();
        let undo_bar = self.ssh_delete_undo.as_ref().map(|undo| (undo.host.clone(), undo.seq));

        self.group(language.pick("SSH 主机", "SSH hosts"), cx)
            .child(
                h_flex()
                    .h(px(32.0))
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(language.pick("已保存主机", "Saved hosts")),
                    )
                    .when(host_count > 0, |header| {
                        header.child(
                            div()
                                .px(px(6.0))
                                .rounded_sm()
                                .text_xs()
                                .text_color(muted)
                                .bg(theme.muted)
                                .child(SharedString::from(host_count.to_string())),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        NebulaButton::new("ssh-add-host")
                            .label(language.pick("+ 添加主机", "+ Add host"))
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_ssh_editor(None, window, cx);
                            })),
                    ),
            )
            .child(div().h(px(SSH_HOST_GAP)))
            .child(
                v_flex()
                    .w_full()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .children(host_rows)
                    .when(host_count == 0, |card| {
                        card.child(
                            v_flex()
                                .py_6()
                                .gap_1()
                                .items_center()
                                .child(
                                    div().text_color(muted).child(
                                        language.pick("还没有保存的主机", "No saved hosts yet"),
                                    ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(language.tr("settings.ssh.empty_hint")),
                                ),
                        )
                    }),
            )
            .child(div().h(px(SSH_HOST_GAP)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        NebulaButton::new("ssh-import")
                            .label(language.pick("导入 ~/.ssh/config", "Import ~/.ssh/config"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ssh_hosts = crate::gpui_shell::ssh_hosts::SshHostLists::load();
                                let count = crate::ssh::ssh_config_hosts().len();
                                this.ssh_status = Some(SshStatus::Imported(count));
                                cx.notify();
                            })),
                    )
                    .when(hidden_count > 0, |row| {
                        let show = self.ssh_show_hidden;
                        row.child(
                            NebulaButton::new("ssh-toggle-hidden")
                                .label(if show {
                                    SharedString::from(
                                        language.pick("收起已隐藏", "Collapse hidden"),
                                    )
                                } else {
                                    SharedString::from(format!(
                                        "{} {hidden_count}",
                                        language.pick("已隐藏", "Hidden")
                                    ))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ssh_show_hidden = !this.ssh_show_hidden;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(language.tr("settings.ssh.shared_data_hint")),
                    ),
            )
            .when_some(hidden_rows, |group, rows| {
                group.child(div().h(px(8.0))).child(
                    v_flex()
                        .w_full()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .children(rows),
                )
            })
            .when_some(undo_bar, |group, (host, _)| {
                group.child(div().h(px(8.0))).child(
                    h_flex()
                        .h(px(36.0))
                        .px_3()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .bg(theme.muted)
                        .child(Icon::new(IconName::Undo2).xsmall().text_color(muted))
                        .child(div().flex_1().text_sm().child(SharedString::from(format!(
                            "{} {host}; {} {SSH_DELETE_UNDO_SECS} {}",
                            language.pick("已删除", "Deleted"),
                            language.pick("可在", "undo within"),
                            language.pick("秒", "seconds")
                        ))))
                        .child(
                            NebulaButton::new("ssh-undo-delete")
                                .label(language.pick("撤销", "Undo"))
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| this.undo_ssh_delete(cx))),
                        ),
                )
            })
            .when_some(self.ssh_status.clone(), |group, status| {
                let error = status.is_error();
                let message = status.text(language);
                group.child(
                    div()
                        .pt(px(6.0))
                        .text_sm()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }
}

/// 在 UI 线程捕获 owner HWND（以 usize 传递给后台线程；HWND 裸指针不是
/// Send）。拿不到就退化为无主对话框。
fn ssh_key_dialog_owner(window: &Window) -> usize {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        HasWindowHandle::window_handle(window)
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
                _ => None,
            })
            .unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        0
    }
}

/// 后台线程里转模态文件对话框；owner 由 [`ssh_key_dialog_owner`] 捕获。
fn pick_ssh_private_key_blocking(owner: usize) -> Option<Result<std::path::PathBuf, String>> {
    #[cfg(windows)]
    {
        crate::display::file_dialog::pick_private_key_file_with_hwnd(owner as _)
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        crate::display::file_dialog::pick_private_key_file_unowned()
    }
}

/// 旧壳 `path_tail`：私钥路径太长时留文件名，省略号在前面。
fn ssh_key_path_tail(path: &std::path::Path, max_chars: usize) -> String {
    let value = path.to_string_lossy();
    let count = value.chars().count();
    if count <= max_chars {
        value.into_owned()
    } else {
        format!("…{}", value.chars().skip(count - max_chars + 1).collect::<String>())
    }
}
