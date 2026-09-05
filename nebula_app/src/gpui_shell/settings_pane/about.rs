use super::*;

impl SettingsPane {
    pub(super) fn about_action_row(
        id: &'static str,
        icon: IconName,
        title: &'static str,
        url: String,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().list_hover;
        h_flex()
            .id(id)
            .w_full()
            .h(px(48.0))
            .flex_shrink_0()
            .px_1()
            .gap_3()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(Icon::new(icon).small().text_color(muted))
            .child(div().flex_1().min_w_0().child(title))
            .child(Icon::new(IconName::ExternalLink).xsmall().text_color(muted))
            .into_any_element()
    }

    pub(super) fn about_value_row(
        label: &'static str,
        value: impl IntoElement,
        cx: &Context<Self>,
    ) -> gpui::Div {
        h_flex()
            .w_full()
            .h(px(48.0))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_4()
            .child(div().flex_1().min_w_0().text_color(cx.theme().foreground).child(label))
            .child(value)
    }

    pub(super) fn section_home(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let ink = theme.foreground;
        let success = theme.success;
        let warning = theme.warning;
        let danger = theme.danger;
        let base_px = self.font_size_px(cx);
        let checking = matches!(self.about_update, AboutUpdateState::Checking);
        let (status, status_color): (SharedString, Hsla) = match &self.about_update {
            AboutUpdateState::Idle => (language.pick("尚未检查", "Not checked yet").into(), muted),
            AboutUpdateState::Checking => {
                (language.pick("正在检查更新…", "Checking for updates...").into(), muted)
            },
            AboutUpdateState::UpToDate(latest) => (
                format!("{} (GitHub v{latest})", language.pick("已是最新版本", "Up to date"))
                    .into(),
                success,
            ),
            AboutUpdateState::Available(latest) => (
                format!("{} v{latest}", language.pick("发现新版本", "New version available"))
                    .into(),
                warning,
            ),
            AboutUpdateState::Failed(error) => (
                format!("{}: {error}", language.pick("检查失败", "Update check failed")).into(),
                danger,
            ),
        };
        let update_button = NebulaButton::new("about-check-updates")
            .label(if checking {
                language.pick("正在检查…", "Checking...")
            } else {
                language.pick("检查更新", "Check for updates")
            })
            .primary()
            .disabled(checking)
            .on_click(cx.listener(|this, _, window, cx| this.check_for_updates(window, cx)));

        let status_badge = h_flex()
            .min_w_0()
            .max_w(px(360.0))
            .gap_1()
            .items_center()
            .text_size(px(base_px * 0.82))
            .text_color(status_color)
            .when(matches!(self.about_update, AboutUpdateState::UpToDate(_)), |badge| {
                badge.child(Icon::new(IconName::Check).xsmall())
            })
            .child(div().min_w_0().truncate().child(status));
        let identity =
            h_flex()
                .w_full()
                .items_center()
                .gap(px(24.0))
                .pb(px(64.0))
                .when_some(
                    crate::app_icon::preview(
                        crate::app_icon::selected(),
                        (96.0 * window.scale_factor()).round() as u32,
                    ),
                    |row, logo| row.child(img(logo).size(px(96.0)).flex_shrink_0()),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(7.0))
                        .child(
                            div()
                                .text_size(px(base_px * 2.15))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(ink)
                                .child(crate::brand::NAME),
                        )
                        .child(div().text_color(muted).child(
                            language.pick(
                                "GPU 加速终端 · Windows",
                                "GPU-accelerated terminal · Windows",
                            ),
                        ))
                        .child(
                            h_flex()
                                .mt(px(12.0))
                                .items_center()
                                .gap_4()
                                .child(
                                    div()
                                        .text_size(px(base_px * 0.88))
                                        .text_color(muted)
                                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                                )
                                .child(status_badge),
                        ),
                )
                .child(div().flex_shrink_0().child(update_button));

        let auto_update_switch =
            crate::gpui_shell::widgets::NebulaSwitch::new("auto-check-updates")
                .checked(self.runtime.auto_check_updates)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.persist(&[("auto_check_updates", (*checked as u8).to_string())], cx);
                }));
        let auto_update = h_flex()
            .items_center()
            .gap_2()
            .child(div().text_size(px(base_px * 0.78)).text_color(muted).child(
                if self.runtime.auto_check_updates {
                    language.pick("开启", "On")
                } else {
                    language.pick("关闭", "Off")
                },
            ))
            .child(auto_update_switch);
        let last_checked: SharedString = self
            .about_last_checked
            .clone()
            .unwrap_or_else(|| language.pick("尚未检查", "Not checked yet").to_owned())
            .into();
        let section_title = |title: &'static str| {
            div()
                .h(px(30.0))
                .text_size(px(base_px * 0.85))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title)
        };
        let update_column = v_flex()
            .flex_1()
            .min_w(px(280.0))
            .child(section_title(language.pick("版本与更新", "Version and updates")))
            .child(Self::about_value_row(
                language.pick("自动检查更新", "Automatically check for updates"),
                auto_update,
                cx,
            ))
            .child(Self::about_value_row(
                language.pick("更新通道", "Update channel"),
                div().text_color(muted).child("Stable"),
                cx,
            ))
            .child(Self::about_value_row(
                language.pick("上次检查", "Last checked"),
                div().text_color(muted).child(last_checked),
                cx,
            ));
        let actions = v_flex()
            .flex_1()
            .min_w(px(280.0))
            .child(section_title(language.pick("项目与支持", "Project and support")))
            .child(Self::about_action_row(
                "about-report-issue",
                IconName::TriangleAlert,
                language.pick("反馈问题", "Report an issue"),
                issue_url(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-github",
                IconName::Github,
                language.pick("GitHub 仓库", "GitHub repository"),
                REPOSITORY_URL.to_owned(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-releases",
                IconName::BookOpen,
                language.pick("更新内容", "Release notes"),
                crate::update_check::RELEASES_PAGE.to_owned(),
                cx,
            ));

        v_flex().w_full().child(identity).child(
            h_flex()
                .w_full()
                .flex_wrap()
                .items_start()
                .gap(px(64.0))
                .child(update_column)
                .child(actions),
        )
    }
}
