//! 设置页的网络代理区（`SettingsPane` 的网络专属 impl 拆分文件）。
//!
//! 合同对齐旧壳 `NebulaSettingsSection::Proxy`：测试横幅在最前，下面是
//! 「代理方式」；只有 Custom 才展开协议下拉 + 地址。绕过列表、扫描、跳板
//! 和每主机覆盖当前都不画。出网测试走 `ssh_session::start_proxy_test`，
//! 读的是落盘后的 `SshProxyConfig::load_global`。

use gpui::prelude::FluentBuilder as _;
use gpui::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use gpui_component::input::InputEvent;
use nebula_settings::ProxyModeName;

use crate::display::{
    MANUAL_PROXY_PROTOCOL_OPTIONS, ManualProxyProtocol, ProxyTestStatus, manual_proxy_parts,
    manual_proxy_value,
};
use crate::gpui_shell::prelude::*;
use crate::gpui_shell::settings_pane::SettingsPane;
use crate::gpui_shell::widgets::NebulaButton;

/// 旧壳 `ssh_proxy_test` 横幅高度。
const PROXY_TEST_BANNER_H: f32 = 54.0;
/// 旧壳测试横幅与方式行之间的 18px 空隙。
const PROXY_TEST_GAP: f32 = 18.0;
/// 旧壳 `ssh_proxy_mode_control` 紧凑宽度。
const PROXY_MODE_SELECT_W: f32 = 156.0;
/// 旧壳 `ssh_proxy_manual_controls` 协议下拉。
const PROXY_PROTOCOL_SELECT_W: f32 = 112.0;
const PROXY_MANUAL_GAP: f32 = 8.0;
/// 旧壳 `ssh_proxy_test_button`：约 108，下限 88。
const PROXY_TEST_BUTTON_W: f32 = 108.0;
const PROXY_TEST_BUTTON_MIN_W: f32 = 88.0;

/// Custom 才画出地址行。Off / System 的命中与绘制都不含 URL / bypass。
pub(super) fn shows_manual_proxy_address(mode: ProxyModeName) -> bool {
    mode == ProxyModeName::Custom
}

/// 序号或状态对不上时丢掉过期结果，避免旧成功冒充新配置。
pub(super) fn apply_proxy_test_result(
    seq: u64,
    status: &ProxyTestStatus,
    request_id: u64,
    ok: bool,
    message: &str,
    elapsed_ms: u64,
) -> Option<ProxyTestStatus> {
    if request_id != seq || !matches!(status, ProxyTestStatus::Running) {
        return None;
    }
    Some(if ok {
        ProxyTestStatus::Success { elapsed_ms, route: message.to_owned() }
    } else {
        ProxyTestStatus::Failed { message: message.to_owned() }
    })
}

impl SettingsPane {
    pub(super) fn invalidate_proxy_test(&mut self) {
        self.proxy_test_seq = self.proxy_test_seq.wrapping_add(1);
        self.proxy_test_status = ProxyTestStatus::Idle;
    }

    fn current_proxy_protocol(&self, cx: &gpui::App) -> ManualProxyProtocol {
        let row = self
            .proxy_protocol_select
            .read(cx)
            .selected_index(cx)
            .map(|path| path.row)
            .unwrap_or(0);
        MANUAL_PROXY_PROTOCOL_OPTIONS.get(row).copied().unwrap_or_default()
    }

    fn composed_proxy_url(&self, cx: &gpui::App) -> String {
        let protocol = self.current_proxy_protocol(cx);
        let typed = self.proxy_url_input.read(cx).value();
        let (_, host) = manual_proxy_parts(typed.trim());
        manual_proxy_value(protocol, host)
    }

    /// 把协议 + 地址写成 `ssh_proxy_url`。测试线程随后读落盘值。
    pub(super) fn commit_proxy_address(&mut self, cx: &mut Context<Self>) {
        let url = self.composed_proxy_url(cx);
        self.persist(&[("ssh_proxy_url", url)], cx);
    }

    pub(super) fn request_proxy_test(&mut self, cx: &mut Context<Self>) {
        if matches!(self.proxy_test_status, ProxyTestStatus::Running) {
            return;
        }
        // 先落盘当前输入，再跑测试：验证的是下一条真实连接会读到的值。
        self.commit_proxy_address(cx);
        self.proxy_test_seq = self.proxy_test_seq.wrapping_add(1);
        let request_id = self.proxy_test_seq;
        self.proxy_test_status = ProxyTestStatus::Running;
        let receiver = match crate::ssh_session::start_proxy_test(request_id) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.proxy_test_status = ProxyTestStatus::Failed {
                    message: format!("无法启动网络测试：{error}"),
                };
                cx.notify();
                return;
            },
        };
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            let _ = this.update(cx, |pane, cx| {
                match result {
                    Ok(result) => pane.finish_proxy_test(
                        result.request_id,
                        result.ok,
                        &result.message,
                        result.elapsed_ms,
                    ),
                    Err(_) => {
                        pane.finish_proxy_test(request_id, false, "网络测试任务意外结束，请重试", 0)
                    },
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_proxy_test(&mut self, request_id: u64, ok: bool, message: &str, elapsed_ms: u64) {
        if let Some(status) = apply_proxy_test_result(
            self.proxy_test_seq,
            &self.proxy_test_status,
            request_id,
            ok,
            message,
            elapsed_ms,
        ) {
            self.proxy_test_status = status;
        }
    }

    pub(super) fn on_proxy_address_event(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        if matches!(event, InputEvent::Change) {
            self.commit_proxy_address(cx);
        }
    }

    pub(super) fn section_network(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let custom = shows_manual_proxy_address(self.runtime.ssh_proxy_mode);
        self.group("网络代理", cx)
            .child(self.proxy_test_banner(cx))
            .child(div().h(px(PROXY_TEST_GAP)).w_full().flex_shrink_0())
            .child(self.proxy_mode_row(cx))
            .when(custom, |page| page.child(self.proxy_address_row(cx)))
    }

    fn proxy_test_banner(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        let running = matches!(self.proxy_test_status, ProxyTestStatus::Running);
        let (status, status_color) = match &self.proxy_test_status {
            ProxyTestStatus::Idle => {
                (SharedString::from("测试当前设置是否可以访问网络"), theme.muted_foreground)
            },
            ProxyTestStatus::Running => {
                (SharedString::from("正在通过当前设置测试网络…"), theme.link)
            },
            ProxyTestStatus::Success { elapsed_ms, route } => (
                SharedString::from(format!("网络连接正常 · {route} · {elapsed_ms} ms")),
                theme.success,
            ),
            ProxyTestStatus::Failed { message } => {
                (SharedString::from(format!("测试失败：{message}")), theme.danger)
            },
        };
        let caption = if running { "测试中…" } else { "测试网络" };
        h_flex()
            .w_full()
            .h(px(PROXY_TEST_BANNER_H))
            .flex_shrink_0()
            .items_center()
            .pl(px(14.0))
            .pr(px(12.0))
            .gap_3()
            .rounded(px(crate::display::ui::tokens::radius::OVERLAY))
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(div().flex_1().min_w_0().text_color(status_color).child(status))
            .child(
                div()
                    .w(px(PROXY_TEST_BUTTON_W))
                    .min_w(px(PROXY_TEST_BUTTON_MIN_W))
                    .flex_shrink_0()
                    .child(
                        NebulaButton::new("proxy-test-network")
                            .label(caption)
                            .outline()
                            .disabled(running)
                            .on_click(cx.listener(|this, _, _, cx| this.request_proxy_test(cx))),
                    ),
            )
    }

    fn proxy_mode_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let select = self.select_of("ssh_proxy_mode");
        self.row(
            "代理方式",
            "",
            div()
                .w(px(PROXY_MODE_SELECT_W))
                .text_color(cx.theme().link)
                .children(select.map(|state| Select::new(&state))),
            cx,
        )
    }

    fn proxy_address_row(&self, cx: &Context<Self>) -> impl IntoElement {
        self.row(
            "代理地址",
            "",
            h_flex()
                .flex_1()
                .min_w(px(PROXY_PROTOCOL_SELECT_W + PROXY_MANUAL_GAP + 80.0))
                .max_w(px(360.0))
                .items_center()
                .gap(px(PROXY_MANUAL_GAP))
                .child(
                    div()
                        .w(px(PROXY_PROTOCOL_SELECT_W))
                        .flex_shrink_0()
                        .text_color(cx.theme().link)
                        .child(Select::new(&self.proxy_protocol_select)),
                )
                .child(div().flex_1().min_w_0().child(Input::new(&self.proxy_url_input))),
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_proxy_test_result, shows_manual_proxy_address};
    use crate::display::ProxyTestStatus;
    use nebula_settings::ProxyModeName;

    #[test]
    fn custom_mode_is_the_only_one_that_expands_the_address_row() {
        assert!(shows_manual_proxy_address(ProxyModeName::Custom));
        assert!(!shows_manual_proxy_address(ProxyModeName::Off));
        assert!(!shows_manual_proxy_address(ProxyModeName::System));
    }

    #[test]
    fn stale_proxy_test_results_are_discarded_after_invalidate() {
        let seq = 7;
        let running = ProxyTestStatus::Running;
        assert_eq!(apply_proxy_test_result(seq, &running, 6, true, "直接连接", 12), None);
        assert_eq!(
            apply_proxy_test_result(seq, &ProxyTestStatus::Idle, seq, true, "直接连接", 12),
            None
        );
        assert_eq!(
            apply_proxy_test_result(seq, &running, seq, true, "直接连接", 41),
            Some(ProxyTestStatus::Success { elapsed_ms: 41, route: "直接连接".into() })
        );
        assert_eq!(
            apply_proxy_test_result(seq, &running, seq, false, "连接被拒绝", 8),
            Some(ProxyTestStatus::Failed { message: "连接被拒绝".into() })
        );
    }
}
