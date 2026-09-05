//! 更新提示、应用内下载与安装确认弹窗。
//!
//! 后台检查先显示右下角通知；详情弹窗负责启动流式下载并展示进度。下载任务
//! 即使弹窗被遮罩点击关闭也会继续，完成或失败后再以右下角通知召回用户。

use super::*;

use crate::update_download::DownloadStatus;

const UPDATE_DIALOG_IDLE_HEIGHT: f32 = 250.0;
const UPDATE_DIALOG_STATUS_HEIGHT: f32 = 280.0;

struct UpdateNotification;

/// 自动检查只在右下角提示，不抢终端焦点；更新是待办，因此保持到用户处理。
pub(crate) fn show_update_notification(
    result: crate::update_check::UpdateCheckResult,
    window: &mut Window,
    cx: &mut App,
) {
    let language = workspace_ui_language();
    let title: SharedString = language.pick("发现新版本", "Update available").into();
    let message: SharedString = match language {
        crate::display::UiLanguage::ZhCn => {
            format!("Nebula v{} 已发布（当前 v{}）", result.latest, result.current)
        },
        crate::display::UiLanguage::EnUs => {
            format!("Nebula v{} is available (current v{})", result.latest, result.current)
        },
    }
    .into();
    let action_label: SharedString = language.pick("查看更新", "View update").into();
    let action_result = result.clone();
    let notification = Notification::warning(message)
        .id::<UpdateNotification>()
        .title(title)
        .autohide(false)
        .w_auto()
        .min_w(px(300.0))
        .max_w(px(440.0))
        .action(move |_, _, cx| {
            let result = action_result.clone();
            Button::new("view-nebula-update").label(action_label.clone()).primary().on_click(
                cx.listener(move |notification, _, window, cx| {
                    notification.dismiss(window, cx);
                    open_update_dialog(result.clone(), window, cx);
                }),
            )
        });
    log::info!(
        "update-check: showing update notification for v{} (current v{})",
        result.latest,
        result.current
    );
    crate::gpui_shell::toast::push_notification(window, cx, notification);
}

fn start_update_download(
    result: crate::update_check::UpdateCheckResult,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(asset) = result.asset.clone() else {
        cx.open_url(crate::update_check::RELEASES_PAGE);
        return;
    };
    match crate::update_download::begin(&asset) {
        Ok(true) => {},
        Ok(false) => {
            cx.refresh_windows();
            return;
        },
        Err(error) => {
            crate::gpui_shell::toast::toast(window, cx, crate::display::ToastKind::Warning, error);
            return;
        },
    }

    let background_asset = asset.clone();
    cx.background_executor()
        .spawn(async move { crate::update_download::run(background_asset) })
        .detach();

    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let status = crate::update_download::status(&asset);
            let finished = status.is_terminal();
            let notification_result = result.clone();
            let _ = window_handle.update(cx, move |_, window, cx| {
                cx.refresh_windows();
                if finished {
                    show_download_outcome_notification(notification_result, status, window, cx);
                }
            });
            if finished {
                break;
            }
        }
    })
    .detach();
    cx.refresh_windows();
}

fn show_download_outcome_notification(
    result: crate::update_check::UpdateCheckResult,
    status: DownloadStatus,
    window: &mut Window,
    cx: &mut App,
) {
    let language = workspace_ui_language();
    let downloaded_version = result
        .asset
        .as_ref()
        .map_or(result.latest.as_str(), |asset| asset.version.as_str())
        .to_owned();
    let (title, message, action, success): (SharedString, SharedString, SharedString, bool) =
        match status {
            DownloadStatus::Ready { bytes, .. } => (
                language.pick("更新已下载", "Update downloaded").into(),
                match language {
                    crate::display::UiLanguage::ZhCn => {
                        format!(
                            "v{} 安装包已通过 SHA-256 校验（{}）",
                            downloaded_version,
                            format_bytes(bytes)
                        )
                    },
                    crate::display::UiLanguage::EnUs => format!(
                        "The v{} installer passed SHA-256 verification ({})",
                        downloaded_version,
                        format_bytes(bytes)
                    ),
                }
                .into(),
                language.pick("安装更新", "Install update").into(),
                true,
            ),
            DownloadStatus::Failed(error) => (
                language.pick("更新下载失败", "Update download failed").into(),
                error.into(),
                language.pick("查看详情", "View details").into(),
                false,
            ),
            _ => return,
        };
    let action_result = result.clone();
    let mut notification =
        if success { Notification::success(message) } else { Notification::warning(message) };
    notification = notification
        .id::<UpdateNotification>()
        .title(title)
        .autohide(false)
        .w_auto()
        .min_w(px(320.0))
        .max_w(px(460.0))
        .action(move |_, _, cx| {
            let result = action_result.clone();
            Button::new("open-downloaded-nebula-update").label(action.clone()).primary().on_click(
                cx.listener(move |notification, _, window, cx| {
                    notification.dismiss(window, cx);
                    open_update_dialog(result.clone(), window, cx);
                }),
            )
        });
    crate::gpui_shell::toast::push_notification(window, cx, notification);
}

/// 关闭、Esc 与遮罩点击均走“3 天后提醒”；这正是新 Dialog 的取消合同。
pub(crate) fn open_update_dialog(
    result: crate::update_check::UpdateCheckResult,
    window: &mut Window,
    cx: &mut App,
) {
    if let Err(error) = crate::update_check::mark_prompted(&result.latest) {
        log::warn!("update-check: failed to record prompt: {error}");
    }

    let dialog_result = result.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        let language = workspace_ui_language();
        let title: SharedString = language.pick("Nebula 更新", "Nebula Update").into();
        let current_label: SharedString = language.pick("当前版本", "Current").into();
        let latest_label: SharedString = language.pick("最新版本", "Latest").into();
        let current_version: SharedString = format!("v{}", dialog_result.current).into();
        let latest_version: SharedString = format!("v{}", dialog_result.latest).into();
        let later_text: SharedString =
            language.pick("3 天后提醒", "Remind me in 3 days").into();
        let skip_text: SharedString = language.pick("跳过此版本", "Skip this version").into();
        let muted = cx.theme().muted_foreground;
        let latest_color = cx.theme().warning;
        let version_background = cx.theme().muted;
        let danger = cx.theme().danger;
        let asset = dialog_result.asset.clone();
        let status = asset
            .as_ref()
            .map(crate::update_download::status)
            .unwrap_or(DownloadStatus::Idle);
        let verified_asset = asset.as_ref().is_some_and(|asset| asset.sha256.is_some());
        let downloading = matches!(status, DownloadStatus::Downloading { .. });
        // 居中 helper 需要内容高度估值；按实际渲染态区分，避免统一按 330px
        // 计算时让较矮的初始弹窗明显偏上。
        let estimated_height = match &status {
            DownloadStatus::Idle => UPDATE_DIALOG_IDLE_HEIGHT,
            DownloadStatus::Downloading { .. }
            | DownloadStatus::Ready { .. }
            | DownloadStatus::Failed(_) => UPDATE_DIALOG_STATUS_HEIGHT,
        };

        let hint: SharedString = match (&status, asset.as_ref()) {
            (DownloadStatus::Downloading { .. }, _) => language
                .pick(
                    "正在后台下载并校验 Windows x64 安装包。",
                    "Downloading and verifying the Windows x64 installer in the background.",
                )
                .into(),
            (DownloadStatus::Ready { .. }, _) => language
                .pick(
                    "安装包已通过 SHA-256 校验。点击“安装更新”后将启动安装向导并退出当前 Nebula。",
                    "The installer passed SHA-256 verification. Install Update starts the setup wizard and exits this Nebula instance.",
                )
                .into(),
            (DownloadStatus::Failed(_), _) => language
                .pick(
                    "下载未完成。可以重试自动下载，或打开发布页手动处理。",
                    "The download did not complete. Retry it here or use the Releases page.",
                )
                .into(),
            (_, Some(_)) if verified_asset => language
                .pick(
                    "Nebula 将自动下载并校验安装包；校验完成后由你确认安装。",
                    "Nebula will download and verify the installer, then wait for your confirmation before installing.",
                )
                .into(),
            // 非 Windows 目前没有自动安装路径（能力表 `self_update_install`）：
            // 不说「缺 Windows 安装包」，那对 Mac/Linux 用户是句错话。
            _ if !crate::platform::CAPABILITIES.self_update_install => language
                .pick(
                    "此平台暂不支持应用内自动更新；请到发布页下载对应的安装包。",
                    "In-app automatic updates are not available on this platform yet. Download the matching package from the Releases page.",
                )
                .into(),
            _ => language
                .pick(
                    "此 release 没有可验证的 Windows x64 安装包，已禁用自动执行；可打开发布页手动处理。",
                    "This release has no verifiable Windows x64 installer, so automatic execution is disabled. Use the Releases page instead.",
                )
                .into(),
        };

        let primary_text: SharedString = match status {
            DownloadStatus::Idle if verified_asset => {
                language.pick("下载更新", "Download update").into()
            },
            DownloadStatus::Downloading { .. } => {
                language.pick("正在下载", "Downloading").into()
            },
            DownloadStatus::Ready { .. } => {
                language.pick("安装更新", "Install update").into()
            },
            DownloadStatus::Failed(_) if verified_asset => {
                language.pick("重新下载", "Retry download").into()
            },
            _ => language.pick("打开发布页", "Open Releases").into(),
        };

        let mut body = v_flex()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .px_3()
                    .py_3()
                    .rounded_md()
                    .bg(version_background)
                    .child(
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_sm().text_color(muted).child(current_label))
                            .child(div().text_base().font_semibold().child(current_version)),
                    )
                    .child(Icon::new(IconName::ArrowRight).small().text_color(muted))
                    .child(
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_sm().text_color(muted).child(latest_label))
                            .child(
                                div()
                                    .text_base()
                                    .font_semibold()
                                    .text_color(latest_color)
                                    .child(latest_version),
                            ),
                    ),
            )
            .child(div().text_base().line_height(relative(1.5)).text_color(muted).child(hint));

        match &status {
            DownloadStatus::Downloading { downloaded, total } => {
                let percent = total
                    .filter(|total| *total > 0)
                    .map(|total| (*downloaded as f32 / total as f32 * 100.0).clamp(0.0, 100.0));
                let progress_text: SharedString = total.map_or_else(
                    || format!("{}", format_bytes(*downloaded)),
                    |total| {
                        format!(
                            "{} / {}  ({:.0}%)",
                            format_bytes(*downloaded),
                            format_bytes(total),
                            percent.unwrap_or(0.0)
                        )
                    },
                ).into();
                body = body
                    .child(
                        gpui_component::progress::Progress::new("nebula-update-download-progress")
                            .small()
                            .loading(percent.is_none())
                            .value(percent.unwrap_or(0.0)),
                    )
                    .child(div().text_sm().text_color(muted).child(progress_text));
            },
            DownloadStatus::Ready { path, bytes } => {
                let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("{} · {}", file_name, format_bytes(*bytes))),
                );
            },
            DownloadStatus::Failed(error) => {
                body = body.child(
                    div()
                        .text_sm()
                        .line_height(relative(1.4))
                        .text_color(danger)
                        .child(error.clone()),
                );
            },
            DownloadStatus::Idle => {},
        }

        let skip_version = dialog_result.latest.clone();
        let cancel_version = dialog_result.latest.clone();
        let action_result = dialog_result.clone();
        let save_failed_prefix = language.pick("无法保存更新提醒设置", "Could not save update preference");
        let error_separator = language.pick("：", ": ");
        let cancel_save_failed_prefix = save_failed_prefix.to_owned();
        let cancel_error_separator = error_separator.to_owned();
        let mut footer = DialogFooter::new()
            .child(
                Button::new("skip-nebula-update")
                    .label(skip_text)
                    .ghost()
                    .disabled(downloading)
                    .on_click(move |_, window, cx| {
                        if let Err(error) = crate::update_check::skip_version(&skip_version) {
                            crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Warning,
                                format!("{save_failed_prefix}{error_separator}{error}"),
                            );
                        }
                        window.close_dialog(cx);
                    }),
            )
            .child(div().flex_1())
            .child(DialogClose::new().child(Button::new("cancel").label(later_text)));
        let primary = Button::new("ok").label(primary_text).primary().disabled(downloading);
        footer = if downloading {
            footer.child(primary)
        } else {
            footer.child(DialogAction::new().child(primary))
        };

        center_modal_dialog(dialog, window, estimated_height)
            .close_button(false)
            // 保留新 Dialog 的遮罩点击取消；它与 Esc、取消按钮共用 on_cancel。
            .overlay_closable(true)
            .title(div().text_lg().font_semibold().line_height(relative(1.0)).child(title))
            .footer(footer)
            .child(body)
            .on_ok(move |_, window, cx| {
                let Some(asset) = action_result.asset.as_ref() else {
                    cx.open_url(crate::update_check::RELEASES_PAGE);
                    return true;
                };
                match crate::update_download::status(asset) {
                    DownloadStatus::Idle | DownloadStatus::Failed(_) if asset.sha256.is_some() => {
                        start_update_download(action_result.clone(), window, cx);
                        false
                    },
                    DownloadStatus::Downloading { .. } => false,
                    DownloadStatus::Ready { .. } => {
                        match crate::update_download::launch_ready(asset) {
                            Ok(()) => {
                                window.close_dialog(cx);
                                windowing::quit_all(cx);
                            },
                            Err(error) => crate::gpui_shell::toast::toast(
                                window,
                                cx,
                                crate::display::ToastKind::Warning,
                                error,
                            ),
                        }
                        false
                    },
                    _ => {
                        cx.open_url(crate::update_check::RELEASES_PAGE);
                        true
                    },
                }
            })
            .on_cancel(move |_, window, cx| {
                if let Err(error) = crate::update_check::remind_later(&cancel_version) {
                    crate::gpui_shell::toast::toast(
                        window,
                        cx,
                        crate::display::ToastKind::Warning,
                        format!("{cancel_save_failed_prefix}{cancel_error_separator}{error}"),
                    );
                }
                true
            })
    });
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}
