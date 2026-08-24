//! 更新提示与更新详情弹窗。
//!
//! 这两个函数是同一条链的两端：后台检查命中新版本先出右下角轻提示（不抢终端
//! 焦点），用户点"查看更新"才进带延迟选择的弹窗。分层判据见提示三层的裁定
//! ——有待办动作才升级到弹窗。
//!
//! 从 `workspace.rs` 拆出来是因为它们和工作区状态零耦合：只吃一个
//! `UpdateCheckResult`，不碰 `NebulaWorkspace` 的任何字段。父模块把两个名字
//! 原样再导出，所以 `workspace::open_update_dialog` 这样的既有调用点不变。

use super::*;

/// Netcatty 式轻提示：自动检查只在右下角短暂出现，不抢终端焦点；用户明确
/// 点击“查看更新”后才进入带延迟选择的详情弹窗。
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
        .title(title)
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

/// TTY7 式延迟决策：关闭/Esc 与“3 天后提醒”同义；跳过只绑定当前版本，
/// 手动检查仍可强制打开本弹窗查看详情。
pub(crate) fn open_update_dialog(
    result: crate::update_check::UpdateCheckResult,
    window: &mut Window,
    cx: &mut App,
) {
    if let Err(error) = crate::update_check::mark_prompted(&result.latest) {
        log::warn!("update-check: failed to record prompt: {error}");
    }

    let language = workspace_ui_language();
    let title: SharedString = language.pick("Nebula 更新", "Nebula Update").into();
    let current_label: SharedString = language.pick("当前版本", "Current").into();
    let latest_label: SharedString = language.pick("最新版本", "Latest").into();
    let current_version: SharedString = format!("v{}", result.current).into();
    let latest_version: SharedString = format!("v{}", result.latest).into();
    let hint: SharedString = language
        .pick(
            "Nebula 目前不会在后台替换程序文件。打开 GitHub Releases 后，请下载适合当前平台的安装包。",
            "Nebula does not replace application files in the background. Open GitHub Releases to download the build for this platform.",
        )
        .into();
    let open_text: SharedString = language.pick("打开发布页", "Open Releases").into();
    let later_text: SharedString = language.pick("3 天后提醒", "Remind me in 3 days").into();
    let skip_text: SharedString = language.pick("跳过此版本", "Skip this version").into();
    let save_failed_prefix =
        language.pick("无法保存更新提醒设置", "Could not save update preference");
    let error_separator = language.pick("：", ": ");
    let muted = cx.theme().muted_foreground;
    let latest_color = cx.theme().warning;
    let version_background = cx.theme().muted;

    let skip_version = result.latest.clone();
    let remind_version = result.latest;
    window.open_dialog(cx, move |dialog, window, _cx| {
        let footer_skip_version = skip_version.clone();
        let footer_skip_text = skip_text.clone();
        let cancel_version = remind_version.clone();
        let footer_save_failed_prefix = save_failed_prefix.to_owned();
        let cancel_save_failed_prefix = save_failed_prefix.to_owned();
        let footer_error_separator = error_separator.to_owned();
        let cancel_error_separator = error_separator.to_owned();
        let body = v_flex()
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
                            .child(div().text_xs().text_color(muted).child(current_label.clone()))
                            .child(div().font_semibold().child(current_version.clone())),
                    )
                    .child(Icon::new(IconName::ArrowRight).small().text_color(muted))
                    .child(
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(latest_label.clone()))
                            .child(
                                div()
                                    .font_semibold()
                                    .text_color(latest_color)
                                    .child(latest_version.clone()),
                            ),
                    ),
            )
            .child(
                div().text_sm().line_height(relative(1.55)).text_color(muted).child(hint.clone()),
            );
        center_confirm_dialog(dialog, window)
            .title(title.clone())
            .confirm()
            .footer(move |ok, cancel, window, cx| {
                let version = footer_skip_version.clone();
                let save_failed_prefix = footer_save_failed_prefix.clone();
                let error_separator = footer_error_separator.clone();
                vec![
                    Button::new("skip-nebula-update")
                        .label(footer_skip_text.clone())
                        .ghost()
                        .on_click(move |_, window, cx| {
                            if let Err(error) = crate::update_check::skip_version(&version) {
                                crate::gpui_shell::toast::toast(
                                    window,
                                    cx,
                                    crate::display::ToastKind::Warning,
                                    format!("{save_failed_prefix}{error_separator}{error}"),
                                );
                            }
                            window.close_dialog(cx);
                        })
                        .into_any_element(),
                    div().flex_1().into_any_element(),
                    cancel(window, cx),
                    ok(window, cx),
                ]
            })
            .button_props(
                DialogButtonProps::default()
                    .ok_text(open_text.clone())
                    .cancel_text(later_text.clone()),
            )
            .child(body)
            .on_ok(|_, _, cx| {
                cx.open_url(crate::update_check::RELEASES_PAGE);
                true
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
