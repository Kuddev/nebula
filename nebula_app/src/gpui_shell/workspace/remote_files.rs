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

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, uniform_list,
};

use crate::gpui_shell::prelude::*;
use crate::ssh_sftp::{SftpEntry, SftpEntryKind};

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

    /// 单击选中，双击进目录。
    fn remote_activate(&mut self, row: SftpEntry, cx: &mut Context<'_, Self>) {
        if matches!(row.kind, SftpEntryKind::Directory | SftpEntryKind::Symlink) {
            self.navigate_remote(row.path, cx);
            return;
        }
        // 文件暂不在这里打开：远端文件要先下载才能看，那是一次带进度和取消的
        // 传输，不该由一次双击静默触发。选中即止，动作交给工具栏。
        self.remote_browser.selected = Some(row.path);
        cx.notify();
    }

    /// 抽屉的远端形态。几何与本地文件树共用同一套行距和留白。
    pub(super) fn render_remote_files(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        // 视图切换条先建：它要可变借 `cx`，而下面取的主题色是从 `cx` 借出来的
        // 不可变引用。顺序颠倒的话两个借用会重叠。
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let popover = theme.popover;
        let is_dark = theme.is_dark();
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
                        // 目录单击即进：远端每次列目录都是一次网络往返，要求双击
                        // 等于把每个动作的等待时间摊两次。文件仍然只选中。
                        if event.click_count() >= 1 {
                            this.remote_activate(activate.clone(), cx);
                        }
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
        if self.remote_browser.loading {
            return Some("正在读取远端目录…".to_owned());
        }
        self.remote_browser.entries.is_empty().then(|| "此目录为空。".to_owned())
    }
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
