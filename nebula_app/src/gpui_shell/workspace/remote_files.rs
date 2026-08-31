//! 远端文件浏览器：抽屉在 SSH pane 上的另一种渲染。
//!
//! # 为什么不是一个独立面板
//!
//! 用户心里只有一个"文件"入口。远端浏览器如果做成第三个抽屉页签，同一件事
//! （看当前这台机器上的文件）就有了两个入口，而且用户得先知道自己在本地还是
//! 远端才能选对——那本来是程序该知道的事。
//!
//! 所以判据是：**抽屉的"文件"视图渲染谁，由聚焦 pane 的身份决定。** 聚焦
//! SSH pane 就画远端列表，切回本地 tab 就自动翻回本地目录树。浏览状态按
//! pane 留着，切回来还在原来那个目录。
//!
//! # 异步怎么回到界面
//!
//! 传输和列目录跑在本项目自己的网络 runtime 上（连接池和认证策略都在那儿），
//! 而界面更新必须回到 GPUI 的执行器。两者用一条 oneshot 接起来：网络侧算完
//! 把结果送进管道，GPUI 侧 `cx.spawn` 等在管道另一端，拿到就写状态 + `notify`。
//!
//! 落地时必须校验**世代号**。用户快速点几层目录时，先发的请求可能后到；不校验
//! 就会出现"点进 c 目录，界面却显示 b 目录的内容"。世代号对不上的响应直接
//! 丢弃——它描述的是一个已经不存在的意图。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, ExternalPaths, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, uniform_list,
};

use crate::gpui_shell::prelude::*;
use crate::ssh_sftp::{
    SftpConflictPolicy, SftpController, SftpEntry, SftpEntryKind, SftpPhase, SftpSnapshot,
    SftpTransferOptions,
};

use super::NebulaWorkspace;

/// 行距，与本地文件树同源——同一个抽屉里两种列表的行高不一致，切换时整列
/// 文字会跳一下。
const ROW_PITCH: f32 = 34.0;
/// 抽屉内容左右留白，同样与本地树对齐。
const TEXT_INSET: f32 = 4.0;

/// 远端浏览器的状态。
///
/// 一份状态服务所有 pane：连接是按目的地复用的，为每个 pane 存一套列表只会
/// 让同一台机器的同一个目录被列多次。按 pane 区分的只有"浏览到哪儿了"。
#[derive(Default)]
pub(super) struct RemoteBrowser {
    /// 当前绑定的 pane。`None` 表示抽屉此刻不该画远端内容。
    pane: Option<u64>,
    /// 当前绑定的远端目的地，用于标题和后续请求。
    destination: String,
    /// 每个 pane 各自浏览到的目录。切回这个 pane 时直接复原，不用重新导航
    /// 到家目录——用户在两个 tab 之间来回看两个目录是常态。
    visited: HashMap<u64, String>,
    /// 当前列出的目录。
    path: String,
    entries: Vec<SftpEntry>,
    /// 正在等一次列目录的响应。
    loading: bool,
    /// 上一次失败的原因。与 `entries` 并存：读不到新目录时旧列表还留在屏幕上
    /// 更有用（用户至少知道自己刚才在哪），错误单独一行说明为什么没动。
    error: Option<String>,
    /// 导航世代号。晚发早到的响应靠它被丢弃。
    generation: u64,
    selected: Option<String>,
    /// 当前唯一的传输控制器。任务可以跨 pane 切换继续跑，但只有目的地与当前
    /// pane 一致时才把完成结果写回列表，避免 A 主机的结果覆盖 B 主机。
    transfer: Option<SftpController>,
    /// 控制器世代号。旧控制器的 wake 可能晚到，不能因此读取刚装上的新控制器。
    transfer_id: u64,
    skip_unchanged: bool,
    /// 远端复制载荷必须带稳定的源 destination，不能从当前标题反推源主机。
    clipboard: Option<RemoteClipboard>,
}

#[derive(Clone)]
struct RemoteClipboard {
    source_destination: String,
    entry: SftpEntry,
}

#[derive(Clone)]
enum PendingRemoteTransfer {
    Upload(Vec<PathBuf>),
    Download { entry: SftpEntry, local_directory: PathBuf },
    Copy(RemoteClipboard),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteTransferTarget {
    pane: u64,
    destination: String,
    path: String,
    navigation_generation: u64,
}

impl RemoteBrowser {
    /// 抽屉此刻是否应该画远端内容。
    pub(super) fn active(&self) -> bool {
        self.pane.is_some()
    }

    /// 解绑：聚焦回本地 pane 时调用。
    ///
    /// 只清"当前显示什么"，不清 `visited`——那是每个 pane 的浏览位置，下次
    /// 切回来还要用。连接本身由下层按目的地缓存，这里也不去动它。
    fn detach(&mut self) {
        self.pane = None;
        self.destination.clear();
        self.entries.clear();
        self.error = None;
        self.loading = false;
        self.selected = None;
        // 世代号往前走一格：已经在路上的响应回来时会发现自己过期了。
        self.generation = self.generation.wrapping_add(1);
    }
}

impl NebulaWorkspace {
    /// 聚焦 pane 的远端身份，`None` 表示本地 pane 或 SSH 还没就绪。
    ///
    /// 双条件门控在 [`TerminalView::ready_ssh_destination`] 里：既要是 SSH
    /// pane，又要握手已完成。
    pub(super) fn focused_remote_pane(&self, cx: &App) -> Option<(u64, String)> {
        let view = self.tabs.get(self.active).and_then(super::WorkspaceTab::focused_view)?;
        let view = view.read(cx);
        let destination = view.ready_ssh_destination()?.to_owned();
        Some((view.pane_id, destination))
    }

    /// 每帧把抽屉路由到聚焦 pane 的身份上。
    ///
    /// 返回抽屉这一帧是否该画远端内容。这是**唯一**的判据入口：渲染、跟随、
    /// 空态文案都问它，不各自重新判断一遍，否则三处判据迟早会分叉。
    pub(super) fn route_remote_browser(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let focused = self.focused_remote_pane(cx);
        match focused {
            Some((pane, destination)) => {
                let rebind = self.remote_browser.pane != Some(pane)
                    || self.remote_browser.destination != destination;
                if rebind {
                    self.attach_remote_browser(pane, destination, window, cx);
                }
                true
            },
            None => {
                if self.remote_browser.active() {
                    self.remote_browser.detach();
                    cx.notify();
                }
                false
            },
        }
    }

    /// 绑定到一个远端 pane 并开始列目录。
    fn attach_remote_browser(
        &mut self,
        pane: u64,
        destination: String,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.remote_browser.pane = Some(pane);
        self.remote_browser.destination = destination.clone();
        self.remote_browser.entries.clear();
        self.remote_browser.error = None;
        self.remote_browser.selected = None;

        // 起点优先用这个 pane 上次看到哪儿；第一次进来则问终端当前在哪个
        // 目录，问不到再退到根。直接从根开始是最差的选择：用户在终端里
        // `cd` 到很深的地方，浏览器却从 `/` 开始，等于每次都要重新点进去。
        if let Some(previous) = self.remote_browser.visited.get(&pane).cloned() {
            self.navigate_remote(previous, cx);
            return;
        }
        self.navigate_remote_to_shell_cwd(pane, destination, cx);
    }

    /// 问远端"用户此刻在哪个目录"，然后导航过去。
    fn navigate_remote_to_shell_cwd(
        &mut self,
        pane: u64,
        destination: String,
        cx: &mut Context<'_, Self>,
    ) {
        self.remote_browser.loading = true;
        cx.notify();
        let generation = self.bump_remote_generation();
        cx.spawn(async move |this, cx| {
            let probed =
                remote_call(
                    || async move { crate::ssh_sftp::remote_cwd::probe(&destination).await },
                )
                .await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.remote_browser.generation != generation
                    || workspace.remote_browser.pane != Some(pane)
                {
                    return;
                }
                // 跟不上就从根开始，但那是回退而不是目标。歧义（多个终端共用
                // 一条连接）也走这条路：宁可让用户自己点，也不要打开一个可能
                // 属于另一个终端的目录。
                let start = match probed {
                    Some(crate::ssh_sftp::remote_cwd::RemoteCwd::Located(path)) => path,
                    _ => "/".to_owned(),
                };
                workspace.navigate_remote(start, cx);
            });
        })
        .detach();
    }

    /// 世代号 +1 并返回新值。所有会改变"当前该显示什么"的操作都要先过这里。
    fn bump_remote_generation(&mut self) -> u64 {
        self.remote_browser.generation = self.remote_browser.generation.wrapping_add(1);
        self.remote_browser.generation
    }

    /// 列出一个远端目录并显示它。
    pub(super) fn navigate_remote(&mut self, path: String, cx: &mut Context<'_, Self>) {
        let Some(pane) = self.remote_browser.pane else { return };
        let destination = self.remote_browser.destination.clone();
        let generation = self.bump_remote_generation();
        self.remote_browser.loading = true;
        cx.notify();

        let target = path.clone();
        cx.spawn(async move |this, cx| {
            let listed =
                remote_call(
                    || async move { crate::ssh_sftp::list_dir(&destination, &target).await },
                )
                .await;
            let _ = this.update(cx, |workspace, cx| {
                // 世代号和 pane 都要对：前者防乱序，后者防用户在等待期间切到
                // 别的 pane（那时这份结果属于另一台机器）。
                if workspace.remote_browser.generation != generation
                    || workspace.remote_browser.pane != Some(pane)
                {
                    return;
                }
                workspace.remote_browser.loading = false;
                match listed {
                    Some(Ok(entries)) => {
                        workspace.remote_browser.entries = entries;
                        workspace.remote_browser.path = path.clone();
                        workspace.remote_browser.visited.insert(pane, path);
                        workspace.remote_browser.error = None;
                        workspace.remote_browser.selected = None;
                    },
                    Some(Err(message)) => workspace.remote_browser.error = Some(message),
                    None => {
                        workspace.remote_browser.error =
                            Some("远端连接不可用，请稍后重试".to_owned())
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 回到上一级。
    fn remote_parent(&mut self, cx: &mut Context<'_, Self>) {
        let parent = crate::ssh_sftp::normalize_remote_path(&self.remote_browser.path, "..");
        self.navigate_remote(parent, cx);
    }

    /// 重新列当前目录。
    fn remote_refresh(&mut self, cx: &mut Context<'_, Self>) {
        let path = self.remote_browser.path.clone();
        self.navigate_remote(path, cx);
    }

    fn selected_remote_entry(&self) -> Option<SftpEntry> {
        let selected = self.remote_browser.selected.as_deref()?;
        self.remote_browser.entries.iter().find(|entry| entry.path == selected).cloned()
    }

    fn remote_transfer_snapshot(&self) -> Option<SftpSnapshot> {
        self.remote_browser.transfer.as_ref().map(SftpController::snapshot)
    }

    fn remote_transfer_working(&self) -> bool {
        self.remote_transfer_snapshot().is_some_and(|snapshot| snapshot.phase == SftpPhase::Working)
    }

    /// 为当前 pane/目录取得控制器，并启动一条常驻 wake 接收协程。
    ///
    /// 接收协程只持有 channel，不持有 controller；否则 controller 的 wake 闭包
    /// 持有 sender、协程再持有 controller，会形成直到进程退出才释放的环。
    fn remote_transfer_controller(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) -> Result<SftpController, String> {
        let pane = self.remote_browser.pane.ok_or_else(|| "当前没有可用的 SSH pane".to_owned())?;
        let destination = self.remote_browser.destination.clone();
        let path = self.remote_browser.path.clone();
        if destination.is_empty() || path.is_empty() {
            return Err("远端目录尚未就绪".to_owned());
        }

        if let Some(controller) = self.remote_browser.transfer.as_ref() {
            let snapshot = controller.snapshot();
            if snapshot.phase == SftpPhase::Working {
                return Err(format!("已有传输正在进行：{}", snapshot.destination));
            }
            if snapshot.destination == destination && snapshot.path == path {
                return Ok(controller.clone());
            }
        }

        self.remote_browser.transfer_id = self.remote_browser.transfer_id.wrapping_add(1).max(1);
        let transfer_id = self.remote_browser.transfer_id;
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let controller = SftpController::new_at(destination.clone(), path, wake)
            .map_err(|error| error.to_string())?;
        self.remote_browser.transfer = Some(controller.clone());

        cx.spawn(async move |this, cx| {
            while wake_rx.recv().await.is_some() {
                let finished = this
                    .update(cx, |workspace, cx| {
                        workspace.sync_remote_transfer(transfer_id, pane, &destination, cx)
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        })
        .detach();
        Ok(controller)
    }

    /// 把 controller 快照落回 GPUI 状态。返回 true 表示接收协程可以退出。
    fn sync_remote_transfer(
        &mut self,
        transfer_id: u64,
        pane: u64,
        destination: &str,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if self.remote_browser.transfer_id != transfer_id {
            return true;
        }
        let Some(controller) = self.remote_browser.transfer.as_ref() else {
            return true;
        };
        let snapshot = controller.snapshot();
        if snapshot.destination != destination {
            return true;
        }

        let visible = self.remote_browser.pane == Some(pane)
            && self.remote_browser.destination == destination;
        if visible {
            match snapshot.phase {
                SftpPhase::Ready => {
                    // 用户可在传输期间继续浏览；只有仍停在传输起始目录时才替换
                    // 列表，否则完成结果会把用户从刚进入的目录拉回去。
                    if self.remote_browser.path == snapshot.path {
                        self.remote_browser.entries = snapshot.entries.clone();
                        self.remote_browser.selected = None;
                        self.remote_browser.visited.insert(pane, snapshot.path.clone());
                    }
                    self.remote_browser.error = None;
                },
                SftpPhase::Error => self.remote_browser.error = snapshot.error.clone(),
                SftpPhase::Working => {},
                SftpPhase::Connecting | SftpPhase::Loading => {},
            }
        }

        let finished = snapshot.phase == SftpPhase::Ready;
        if finished {
            self.remote_browser.transfer = None;
        }
        cx.notify();
        finished
    }

    fn start_remote_transfer(
        &mut self,
        pending: PendingRemoteTransfer,
        conflict: SftpConflictPolicy,
        target: &RemoteTransferTarget,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.remote_transfer_target_matches(target) {
            self.remote_browser.error =
                Some("远端目标已改变，请在当前目录重新发起并确认传输".to_owned());
            cx.notify();
            return;
        }
        let controller = match self.remote_transfer_controller(cx) {
            Ok(controller) => controller,
            Err(message) => {
                self.remote_browser.error = Some(message);
                cx.notify();
                return;
            },
        };
        let options =
            SftpTransferOptions { conflict, skip_unchanged: self.remote_browser.skip_unchanged };
        self.remote_browser.error = None;
        match pending {
            PendingRemoteTransfer::Upload(paths) => {
                controller.upload_paths_with_options(paths, options)
            },
            PendingRemoteTransfer::Download { entry, local_directory } => {
                controller.download_with_options(entry, local_directory, options)
            },
            PendingRemoteTransfer::Copy(source) => {
                controller.copy_from(source.source_destination, source.entry, options)
            },
        }
        cx.notify();
    }

    fn current_remote_transfer_target(&self) -> Option<RemoteTransferTarget> {
        Some(RemoteTransferTarget {
            pane: self.remote_browser.pane?,
            destination: self.remote_browser.destination.clone(),
            path: self.remote_browser.path.clone(),
            navigation_generation: self.remote_browser.generation,
        })
    }

    fn remote_transfer_target_matches(&self, target: &RemoteTransferTarget) -> bool {
        self.remote_browser.pane == Some(target.pane)
            && self.remote_browser.destination == target.destination
            && self.remote_browser.path == target.path
            && self.remote_browser.generation == target.navigation_generation
    }

    fn remote_cancel_transfer(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(controller) = self.remote_browser.transfer.as_ref() {
            controller.cancel();
            cx.notify();
        }
    }

    /// 返回根级冲突数，以及当前已知类型是否允许覆盖。
    ///
    /// 这里只负责决定是否询问用户；真正执行前内核还会重新 stat，防止对话框
    /// 打开期间目标被别的进程替换。
    fn pending_remote_conflicts(&self, pending: &PendingRemoteTransfer) -> (usize, bool, bool) {
        match pending {
            PendingRemoteTransfer::Upload(paths) => {
                let mut conflicts = 0;
                let mut overwrite_allowed = true;
                let mut follows_symlink = false;
                let mut root_names = HashSet::with_capacity(paths.len());
                for path in paths {
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    let collides_with_batch = !root_names.insert(name.to_owned());
                    let target =
                        self.remote_browser.entries.iter().find(|entry| entry.name == name);
                    let source = std::fs::symlink_metadata(path).ok();
                    if !collides_with_batch
                        && self.remote_browser.skip_unchanged
                        && source
                            .as_ref()
                            .zip(target)
                            .is_some_and(|(source, target)| local_metadata_matches(source, target))
                    {
                        continue;
                    }
                    if !collides_with_batch && target.is_none() {
                        continue;
                    }
                    conflicts += 1;
                    follows_symlink |=
                        target.is_some_and(|target| target.kind == SftpEntryKind::Symlink);
                    if collides_with_batch {
                        overwrite_allowed = false;
                    }
                    if let (Some(target), Some(source)) = (target, source) {
                        let compatible = (source.is_dir()
                            && target.kind == SftpEntryKind::Directory)
                            || (source.is_file()
                                && matches!(
                                    target.kind,
                                    SftpEntryKind::File | SftpEntryKind::Symlink
                                ));
                        overwrite_allowed &= compatible;
                    }
                }
                (conflicts, overwrite_allowed, follows_symlink)
            },
            PendingRemoteTransfer::Download { entry, local_directory } => {
                let target = local_directory.join(&entry.name);
                let Ok(metadata) = std::fs::symlink_metadata(target) else {
                    return (0, true, false);
                };
                if self.remote_browser.skip_unchanged && local_metadata_matches(&metadata, entry) {
                    return (0, true, false);
                }
                let compatible = match entry.kind {
                    SftpEntryKind::Directory => metadata.is_dir(),
                    SftpEntryKind::File => metadata.is_file() || metadata.file_type().is_symlink(),
                    // 链接的目标类型必须由远端 lstat/readlink 后才能确定，交给
                    // 内核的执行时校验，UI 不凭列表图标猜。
                    SftpEntryKind::Symlink => true,
                };
                (1, compatible, metadata.file_type().is_symlink())
            },
            PendingRemoteTransfer::Copy(source) => {
                let Some(target) = self
                    .remote_browser
                    .entries
                    .iter()
                    .find(|entry| entry.name == source.entry.name)
                else {
                    return (0, true, false);
                };
                if self.remote_browser.skip_unchanged
                    && source.entry.kind == SftpEntryKind::File
                    && target.kind == SftpEntryKind::File
                    && source.entry.size == target.size
                    && source.entry.modified != 0
                    && source.entry.modified == target.modified
                {
                    return (0, true, false);
                }
                let compatible = match source.entry.kind {
                    SftpEntryKind::Directory => target.kind == SftpEntryKind::Directory,
                    SftpEntryKind::File => {
                        matches!(target.kind, SftpEntryKind::File | SftpEntryKind::Symlink)
                    },
                    SftpEntryKind::Symlink => true,
                };
                (1, compatible, target.kind == SftpEntryKind::Symlink)
            },
        }
    }

    fn request_remote_transfer(
        &mut self,
        pending: PendingRemoteTransfer,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(target) = self.current_remote_transfer_target() else {
            self.remote_browser.error = Some("远端目录尚未就绪".to_owned());
            cx.notify();
            return;
        };
        if self.remote_transfer_working() {
            self.remote_browser.error = Some("已有传输正在进行，请等待完成或先取消".to_owned());
            cx.notify();
            return;
        }
        let (conflicts, overwrite_allowed, follows_symlink) =
            self.pending_remote_conflicts(&pending);
        if conflicts == 0 {
            self.start_remote_transfer(pending, SftpConflictPolicy::Overwrite, &target, cx);
            return;
        }
        self.open_remote_conflict_dialog(
            pending,
            target,
            conflicts,
            overwrite_allowed,
            follows_symlink,
            window,
            cx,
        );
    }

    fn open_remote_conflict_dialog(
        &mut self,
        pending: PendingRemoteTransfer,
        target: RemoteTransferTarget,
        conflicts: usize,
        overwrite_allowed: bool,
        follows_symlink: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let skip_workspace = workspace.clone();
            let skip_pending = pending.clone();
            let skip_target = target.clone();
            let keep_workspace = workspace.clone();
            let keep_pending = pending.clone();
            let keep_target = target.clone();
            let overwrite_workspace = workspace.clone();
            let overwrite_pending = pending.clone();
            let overwrite_target = target.clone();
            let footer = DialogFooter::new()
                .child(DialogClose::new().child(Button::new("sftp-conflict-cancel").label("取消")))
                .child(div().flex_1())
                .child(DialogClose::new().child(
                    Button::new("sftp-conflict-skip").label("跳过").on_click(move |_, _, cx| {
                        let Some(workspace) = skip_workspace.upgrade() else { return };
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.start_remote_transfer(
                                skip_pending.clone(),
                                SftpConflictPolicy::Skip,
                                &skip_target,
                                cx,
                            );
                        });
                    }),
                ))
                .child(DialogClose::new().child(
                    Button::new("sftp-conflict-keep-both").label("保留两者").on_click(
                        move |_, _, cx| {
                            let Some(workspace) = keep_workspace.upgrade() else { return };
                            let _ = workspace.update(cx, |workspace, cx| {
                                workspace.start_remote_transfer(
                                    keep_pending.clone(),
                                    SftpConflictPolicy::KeepBoth,
                                    &keep_target,
                                    cx,
                                );
                            });
                        },
                    ),
                ))
                .child(
                    DialogClose::new().child(
                        Button::new("sftp-conflict-overwrite")
                            .label("覆盖")
                            .danger()
                            .disabled(!overwrite_allowed)
                            .on_click(move |_, _, cx| {
                                let Some(workspace) = overwrite_workspace.upgrade() else { return };
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.start_remote_transfer(
                                        overwrite_pending.clone(),
                                        SftpConflictPolicy::Overwrite,
                                        &overwrite_target,
                                        cx,
                                    );
                                });
                            }),
                    ),
                );

            center_modal_dialog(dialog, window, 220.0)
                .close_button(false)
                .overlay_closable(true)
                .title(div().text_lg().font_semibold().child("发现同名项目"))
                .footer(footer)
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(format!("目标位置已有 {conflicts} 个同名根项目。"))
                        .when(!overwrite_allowed, |body| {
                            body.child(
                                div()
                                    .text_sm()
                                    .text_color(_cx.theme().danger)
                                    .child("其中包含文件与目录类型不一致的项目，不能覆盖。"),
                            )
                        })
                        .when(follows_symlink, |body| {
                            body.child(
                                div()
                                    .text_sm()
                                    .text_color(_cx.theme().danger)
                                    .child(
                                        "覆盖符号链接会直接改写链接指向的实际文件；该路径不具备原子回滚，失败或取消可能留下部分内容。",
                                    ),
                            )
                        }),
                )
        });
    }

    fn remote_copy_selected(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.selected_remote_entry() else { return };
        self.remote_browser.clipboard = Some(RemoteClipboard {
            source_destination: self.remote_browser.destination.clone(),
            entry: entry.clone(),
        });
        crate::gpui_shell::toast::toast(
            window,
            cx,
            crate::display::ToastKind::Info,
            format!("已复制远端项目：{}", entry.name),
        );
        cx.notify();
    }

    fn remote_paste(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(source) = self.remote_browser.clipboard.clone() else { return };
        self.request_remote_transfer(PendingRemoteTransfer::Copy(source), window, cx);
    }

    fn remote_pick_upload_files(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        #[cfg(windows)]
        let picked = pick_remote_upload_files(window);
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要上传的文件".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            #[cfg(windows)]
            let Ok(paths) = picked.await else { return };
            #[cfg(not(windows))]
            let paths = {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                paths
            };
            if paths.is_empty() {
                return;
            }
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer(PendingRemoteTransfer::Upload(paths), window, cx);
            });
        })
        .detach();
    }

    fn remote_pick_upload_directory(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        #[cfg(windows)]
        let picked = pick_remote_directory(window, "选择要上传的文件夹");
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择要上传的文件夹".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            #[cfg(windows)]
            let Ok(Some(path)) = picked.await else { return };
            #[cfg(not(windows))]
            let path = {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                let Some(path) = paths.into_iter().next() else { return };
                path
            };
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer(
                    PendingRemoteTransfer::Upload(vec![path]),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn remote_pick_download_directory(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.selected_remote_entry() else { return };
        #[cfg(windows)]
        let picked = pick_remote_directory(window, "选择下载位置");
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择下载位置".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            #[cfg(windows)]
            let Ok(Some(local_directory)) = picked.await else { return };
            #[cfg(not(windows))]
            let local_directory = {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                let Some(path) = paths.into_iter().next() else { return };
                path
            };
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.request_remote_transfer(
                    PendingRemoteTransfer::Download { entry, local_directory },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    /// 当前目录里可见的行，含合成的"上一级"。
    ///
    /// `..` 是导航项而不是真实条目，所以必须显式标记：双击它是换目录，而下载
    /// 或删除对它没有意义。判据和本地树的 `is_parent` 同源。
    fn remote_rows(&self) -> Vec<SftpEntry> {
        let mut rows = Vec::with_capacity(self.remote_browser.entries.len() + 1);
        if self.remote_browser.path != "/" {
            rows.push(SftpEntry {
                name: "..".to_owned(),
                path: crate::ssh_sftp::normalize_remote_path(&self.remote_browser.path, ".."),
                kind: SftpEntryKind::Directory,
                size: 0,
                modified: 0,
                permissions: String::new(),
                is_parent: true,
            });
        }
        rows.extend(self.remote_browser.entries.iter().cloned());
        rows
    }

    /// 单击选中，双击目录进入。
    fn remote_activate(&mut self, row: SftpEntry, open: bool, cx: &mut Context<'_, Self>) {
        if open && matches!(row.kind, SftpEntryKind::Directory | SftpEntryKind::Symlink) {
            self.navigate_remote(row.path, cx);
            return;
        }
        // 目录也必须能被选中，否则下载目录和跨主机复制目录没有入口。双击才
        // 导航，文件双击仍只选中，传输动作统一由工具栏明确触发。
        self.remote_browser.selected = Some(row.path);
        cx.notify();
    }

    /// 抽屉的远端形态。几何与本地文件树共用同一套行距和留白。
    pub(super) fn render_remote_files(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        // 视图切换条先建：它要可变借 `cx`，而下面取的主题色是从 `cx` 借出来的
        // 不可变引用。顺序颠倒的话两个借用会重叠。
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let transfer_status = self.render_remote_transfer_status(cx);
        let skip_unchanged = self.remote_browser.skip_unchanged;
        let transfer_working = self.remote_transfer_working();
        let has_selection = self.selected_remote_entry().is_some();
        let has_clipboard = self.remote_browser.clipboard.is_some();
        let skip_toggle = crate::gpui_shell::widgets::NebulaSwitch::new("sftp-skip-unchanged")
            .checked(skip_unchanged)
            .disabled(transfer_working)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.remote_browser.skip_unchanged = *checked;
                cx.notify();
            }));
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let popover = theme.popover;
        let is_dark = theme.is_dark();
        let drop_highlight = theme.accent.opacity(0.18);
        let rows = self.remote_rows();
        let row_count = rows.len();
        let destination = self.remote_browser.destination.clone();
        let path = if self.remote_browser.path.is_empty() {
            "正在定位远端目录…".to_owned()
        } else {
            self.remote_browser.path.clone()
        };
        let at_root = self.remote_browser.path == "/" || self.remote_browser.path.is_empty();
        let notice = self.remote_notice();

        v_flex()
            .h_full()
            .w(px(320.0))
            .flex_shrink_0()
            .p_2()
            .gap_2()
            .rounded_tl(crate::gpui_shell::theme::card_radius(cx))
            .rounded_bl(crate::gpui_shell::theme::card_radius(cx))
            .bg(popover)
            .shadow(gpui_component::popover_shadow(is_dark))
            .occlude()
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_highlight))
            .on_drop(cx.listener(
                |this, paths: &ExternalPaths, window: &mut Window, cx| {
                    let paths = paths.paths().to_owned();
                    if !paths.is_empty() {
                        this.request_remote_transfer(
                            PendingRemoteTransfer::Upload(paths),
                            window,
                            cx,
                        );
                    }
                },
            ))
            .child(view_switch)
            // 主机名单独一行：远端浏览器最危险的误操作是"以为在另一台机器上"，
            // 所以目的地必须一直在视野里，而不是只在标题栏或 tab 上。
            .child(
                h_flex().px(px(TEXT_INSET)).items_center().gap_1().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(destination),
                ),
            )
            .child(
                h_flex()
                    .h(px(30.0))
                    .px(px(TEXT_INSET))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(path),
                    )
                    .child(
                        Button::new("remote-files-up")
                            .icon(IconName::ArrowUp)
                            .ghost()
                            .xsmall()
                            .disabled(at_root)
                            .tooltip("上一级")
                            .on_click(cx.listener(|this, _, _, cx| this.remote_parent(cx))),
                    )
                    .child(
                        Button::new("remote-files-refresh")
                            // 与本地树的"重新跟随"同一个字位：两个列表的刷新
                            // 动作长得不一样会让用户以为它们干的不是同一件事。
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("重新读取")
                            .on_click(cx.listener(|this, _, _, cx| this.remote_refresh(cx))),
                    ),
            )
            .child(
                h_flex()
                    .h(px(30.0))
                    .px(px(TEXT_INSET))
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("remote-files-upload-files")
                            .icon(IconName::ArrowUp)
                            .ghost()
                            .xsmall()
                            .disabled(transfer_working)
                            .tooltip("上传文件")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remote_pick_upload_files(window, cx);
                            })),
                    )
                    .child(
                        Button::new("remote-files-upload-directory")
                            .icon(IconName::FolderOpen)
                            .ghost()
                            .xsmall()
                            .disabled(transfer_working)
                            .tooltip("上传文件夹")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remote_pick_upload_directory(window, cx);
                            })),
                    )
                    .child(
                        Button::new("remote-files-download")
                            .icon(IconName::ArrowDown)
                            .ghost()
                            .xsmall()
                            .disabled(!has_selection || transfer_working)
                            .tooltip("下载到本地")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remote_pick_download_directory(window, cx);
                            })),
                    )
                    .child(
                        Button::new("remote-files-copy")
                            .icon(IconName::Copy)
                            .ghost()
                            .xsmall()
                            .disabled(!has_selection)
                            .tooltip("复制远端项目")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remote_copy_selected(window, cx);
                            })),
                    )
                    .child(
                        Button::new("remote-files-paste")
                            .icon(IconName::Inbox)
                            .ghost()
                            .xsmall()
                            .disabled(!has_clipboard || transfer_working)
                            .tooltip("粘贴到当前远端目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remote_paste(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted).child("跳过未变"))
                    .child(skip_toggle),
            )
            .when_some(notice, |panel, text| {
                panel.child(
                    div()
                        .px(px(TEXT_INSET))
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(text),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    // 必须裁：虚拟列表会把行画到自己的 bounds 之外，没有这层
                    // 末行会压过抽屉的下内边距一直画到底边，把留白和下两角的
                    // 圆角都盖掉。
                    .overflow_hidden()
                    .child(
                        uniform_list("remote-files-rows", row_count, {
                            let rows = rows.clone();
                            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                                range
                                    .filter_map(|index| rows.get(index).cloned())
                                    .map(|row| this.render_remote_row(row, cx))
                                    .collect()
                            })
                        })
                        .w_full()
                        .size_full()
                        .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
                        .track_scroll(&self.remote_files_scroll),
                    ),
            )
            .child(transfer_status)
            .into_any_element()
    }

    /// 固定高度的传输状态区。预留空间后，开始/结束传输不会挤动文件列表。
    fn render_remote_transfer_status(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        const STATUS_HEIGHT: f32 = 50.0;
        let Some(snapshot) = self.remote_transfer_snapshot().filter(|snapshot| {
            snapshot.destination == self.remote_browser.destination
                && matches!(snapshot.phase, SftpPhase::Working | SftpPhase::Error)
        }) else {
            return div().h(px(STATUS_HEIGHT)).w_full().flex_shrink_0().into_any_element();
        };

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let is_working = snapshot.phase == SftpPhase::Working;
        let progress = snapshot.progress.clone();
        let label = progress.as_ref().map(|progress| progress.label.clone()).unwrap_or_else(|| {
            if is_working { "正在准备传输…".to_owned() } else { "传输失败".to_owned() }
        });
        let detail = if let Some(progress) = progress.as_ref() {
            format!(
                "{} / {}",
                format_transfer_bytes(progress.transferred),
                format_transfer_bytes(progress.total)
            )
        } else {
            snapshot.error.clone().unwrap_or_default()
        };
        let loading = progress.as_ref().is_none_or(|progress| progress.total == 0);
        let percent = progress.as_ref().map(|progress| progress.fraction() * 100.0).unwrap_or(0.0);

        v_flex()
            .h(px(STATUS_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .px(px(TEXT_INSET))
            .gap_1()
            .child(
                h_flex()
                    .h(px(22.0))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .child(div().text_xs().text_color(muted).whitespace_nowrap().child(detail))
                    .when(is_working, |row| {
                        row.child(
                            Button::new("remote-files-cancel-transfer")
                                .icon(IconName::CircleX)
                                .ghost()
                                .xsmall()
                                .tooltip("取消传输")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.remote_cancel_transfer(cx);
                                })),
                        )
                    }),
            )
            .child(
                gpui_component::progress::Progress::new("remote-files-transfer-progress")
                    .small()
                    .loading(is_working && loading)
                    .value(percent),
            )
            .into_any_element()
    }

    /// 一行远端条目。
    fn render_remote_row(&self, row: SftpEntry, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let selected = self.remote_browser.selected.as_deref() == Some(row.path.as_str());
        // 图标和颜色与本地树同源：同一个"文件"入口不该在本地和远端看起来
        // 像两套产品。
        let (icon, ink) = match row.kind {
            SftpEntryKind::Directory => {
                (crate::display::side_panel::folder_icon(false), theme.accent_foreground)
            },
            SftpEntryKind::Symlink => ("\u{ea71}", theme.muted_foreground),
            SftpEntryKind::File => {
                (crate::display::side_panel::file_type_icon(&row.name), theme.muted_foreground)
            },
        };
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let label = row.name.clone();
        let activate = row.clone();

        div()
            .h(px(ROW_PITCH))
            .flex()
            .items_center()
            .child(
                h_flex()
                    .id(SharedString::from(format!("remote-row-{}", row.path)))
                    .h(px(ROW_PITCH - 4.0))
                    .w_full()
                    .px(px(TEXT_INSET + 2.0))
                    .items_center()
                    .gap_2()
                    .rounded(px(6.0))
                    .when(selected, |row| row.bg(theme.tab_active))
                    .hover(|row| row.bg(theme.list_hover))
                    .child(
                        div()
                            .font_family(symbol_family)
                            .text_color(ink)
                            .flex_shrink_0()
                            .child(icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.foreground)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.remote_activate(activate.clone(), event.click_count() >= 2, cx);
                    })),
            )
            .into_any_element()
    }

    /// 列表上方那行说明：正在读、读失败、或者目录确实是空的。
    ///
    /// 三种情况必须分开说。"读不到"和"是空的"混为一谈，用户就不知道该重试
    /// 还是该换目录——这是空态里最常见也最误导人的一处偷懒。
    fn remote_notice(&self) -> Option<String> {
        if let Some(error) = self.remote_browser.error.as_deref() {
            return Some(format!("{error}（点右上角重新读取）"));
        }
        if let Some(snapshot) = self.remote_transfer_snapshot()
            && snapshot.phase == SftpPhase::Working
            && snapshot.destination != self.remote_browser.destination
        {
            return Some(format!("另一主机正在传输：{}", snapshot.destination));
        }
        if self.remote_browser.loading {
            return Some("正在读取远端目录…".to_owned());
        }
        self.remote_browser.entries.is_empty().then(|| "此目录为空。".to_owned())
    }
}

fn local_metadata_matches(metadata: &std::fs::Metadata, remote: &SftpEntry) -> bool {
    remote.kind == SftpEntryKind::File
        && metadata.is_file()
        && metadata.len() == remote.size
        && remote.modified != 0
        && metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|elapsed| elapsed.as_secs() == remote.modified)
}

fn format_transfer_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    for (index, unit) in UNITS.iter().enumerate() {
        if value < 1024.0 || index == UNITS.len() - 1 {
            return if value >= 10.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    unreachable!()
}

#[cfg(windows)]
fn remote_dialog_owner(window: &Window) -> usize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window)
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
            _ => None,
        })
        .unwrap_or(0)
}

#[cfg(windows)]
fn pick_remote_upload_files(window: &Window) -> futures::channel::oneshot::Receiver<Vec<PathBuf>> {
    let owner = remote_dialog_owner(window);
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let paths = crate::display::file_dialog::pick_upload_files_with_hwnd(owner as _);
        let _ = tx.send(paths);
    });
    rx
}

#[cfg(windows)]
fn pick_remote_directory(
    window: &Window,
    title: &'static str,
) -> futures::channel::oneshot::Receiver<Option<PathBuf>> {
    let owner = remote_dialog_owner(window);
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let path = crate::display::file_dialog::pick_folder_with_hwnd(owner as _, title);
        let _ = tx.send(path);
    });
    rx
}

/// 把一次网络 runtime 上的调用桥到 GPUI 的执行器。
///
/// 返回 `None` 表示网络 runtime 起不来或任务被丢弃——调用方据此报"连接不
/// 可用"，而不是把它和"远端答了个错误"混为一谈。
async fn remote_call<T, F, Fut>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send,
{
    let runtime = crate::ssh_session::runtime().ok()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
        let _ = tx.send(work().await);
    });
    rx.await.ok()
}

#[cfg(test)]
mod tests {
    use super::format_transfer_bytes;

    #[test]
    fn transfer_byte_counts_use_binary_units() {
        assert_eq!(format_transfer_bytes(0), "0 B");
        assert_eq!(format_transfer_bytes(1024), "1.0 KiB");
        assert_eq!(format_transfer_bytes(10 * 1024), "10 KiB");
        assert_eq!(format_transfer_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
