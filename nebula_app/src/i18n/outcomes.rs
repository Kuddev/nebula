use crate::i18n::UiLanguage;
use crate::provider_test::ProviderTestOutcome;
use crate::proxy_test::{ProxyTestFailure, ProxyTestOutcome, ProxyTestRoute};

impl UiLanguage {
    pub fn provider_test_message(self, outcome: &ProviderTestOutcome) -> String {
        let status_message =
            |key: &'static str, status: u16| self.tr_args(key, &[("status", &status.to_string())]);
        match outcome {
            ProviderTestOutcome::Success { status } => {
                status_message("provider.test.success", *status)
            },
            ProviderTestOutcome::InvalidEndpoint => {
                self.tr("provider.test.invalid_endpoint").into()
            },
            ProviderTestOutcome::MissingModel => self.tr("provider.test.missing_model").into(),
            ProviderTestOutcome::MissingApiKey => self.tr("provider.test.missing_api_key").into(),
            ProviderTestOutcome::CredentialReadFailed => {
                self.tr("provider.test.credential_read_failed").into()
            },
            ProviderTestOutcome::InvalidCredentialEncoding => {
                self.tr("provider.test.invalid_credential_encoding").into()
            },
            ProviderTestOutcome::Timeout => self.tr("provider.test.timeout").into(),
            ProviderTestOutcome::HostNotFound => self.tr("provider.test.host_not_found").into(),
            ProviderTestOutcome::ConnectionFailed => {
                self.tr("provider.test.connection_failed").into()
            },
            ProviderTestOutcome::Io { kind } => {
                self.tr_args("provider.test.io_error", &[("kind", kind)])
            },
            ProviderTestOutcome::Tls => self.tr("provider.test.tls_failed").into(),
            ProviderTestOutcome::RequestFailed => self.tr("provider.test.request_failed").into(),
            ProviderTestOutcome::AuthFailed { status } => {
                status_message("provider.test.auth_failed", *status)
            },
            ProviderTestOutcome::EndpointNotFound { status } => {
                status_message("provider.test.endpoint_not_found", *status)
            },
            ProviderTestOutcome::RateLimited { status } => {
                status_message("provider.test.rate_limited", *status)
            },
            ProviderTestOutcome::HttpStatus { status } => {
                status_message("provider.test.http_status", *status)
            },
            ProviderTestOutcome::StartFailed { error } => {
                self.tr_args("provider.test.start_failed", &[("error", error)])
            },
        }
    }

    pub fn proxy_test_message(self, outcome: &ProxyTestOutcome, elapsed_ms: u64) -> String {
        match outcome {
            ProxyTestOutcome::Success(route) => {
                let route = self.proxy_test_route(route);
                self.tr_args(
                    "settings.network.status.success",
                    &[("route", &route), ("elapsed_ms", &elapsed_ms.to_string())],
                )
            },
            ProxyTestOutcome::Failed(failure) => self.proxy_test_failure(failure),
        }
    }

    fn proxy_test_route(self, route: &ProxyTestRoute) -> String {
        match route {
            ProxyTestRoute::Direct => self.tr("settings.network.route.direct").into(),
            ProxyTestRoute::DirectAddress => {
                self.tr("settings.network.route.direct_address").into()
            },
            ProxyTestRoute::CustomCommand => {
                self.tr("settings.network.route.custom_command").into()
            },
            ProxyTestRoute::ProxyServer(server) => {
                self.tr_args("settings.network.route.proxy_server", &[("server", server)])
            },
            ProxyTestRoute::SshJump(target) => {
                self.tr_args("settings.network.route.ssh_jump", &[("target", target)])
            },
        }
    }

    fn proxy_test_failure(self, failure: &ProxyTestFailure) -> String {
        match failure {
            ProxyTestFailure::LoadSettings(error) => {
                self.tr_args("settings.network.failure.load_settings", &[("error", error)])
            },
            ProxyTestFailure::InvalidSettings(error) => {
                self.tr_args("settings.network.failure.invalid_settings", &[("error", error)])
            },
            ProxyTestFailure::Timeout { seconds } => self
                .tr_args("settings.network.failure.timeout", &[("seconds", &seconds.to_string())]),
            ProxyTestFailure::SendRequest(error) => {
                self.tr_args("settings.network.failure.send_request", &[("error", error)])
            },
            ProxyTestFailure::ReadResponse(error) => {
                self.tr_args("settings.network.failure.read_response", &[("error", error)])
            },
            ProxyTestFailure::InvalidHttpStatusLine(line) => {
                self.tr_args("settings.network.failure.invalid_http_status_line", &[("line", line)])
            },
            ProxyTestFailure::HttpStatus { status } => self.tr_args(
                "settings.network.failure.http_status",
                &[("status", &status.to_string())],
            ),
            ProxyTestFailure::ProxyServer { server, error } => self.tr_args(
                "settings.network.failure.proxy_server",
                &[("server", server), ("error", error)],
            ),
            ProxyTestFailure::CustomCommand(error) => {
                self.tr_args("settings.network.failure.custom_command", &[("error", error)])
            },
            ProxyTestFailure::JumpTask(error) => {
                self.tr_args("settings.network.failure.jump_task", &[("error", error)])
            },
            ProxyTestFailure::JumpResolve { target, error } => self.tr_args(
                "settings.network.failure.jump_resolve",
                &[("target", target), ("error", error)],
            ),
            ProxyTestFailure::JumpConnect { target, error } => self.tr_args(
                "settings.network.failure.jump_connect",
                &[("target", target), ("error", error)],
            ),
            ProxyTestFailure::JumpChannel { target, error } => self.tr_args(
                "settings.network.failure.jump_channel",
                &[("target", target), ("error", error)],
            ),
            ProxyTestFailure::Direct(error) => {
                self.tr_args("settings.network.failure.direct", &[("error", error)])
            },
            ProxyTestFailure::Start(error) => {
                self.tr_args("settings.network.status.start_failed", &[("error", error)])
            },
            ProxyTestFailure::UnexpectedEnd => {
                self.tr("settings.network.status.unexpected_end").into()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UiLanguage;

    #[test]
    fn provider_outcomes_are_localized_at_the_display_boundary() {
        let outcome = crate::provider_test::ProviderTestOutcome::AuthFailed { status: 403 };
        assert_eq!(
            UiLanguage::EnUs.provider_test_message(&outcome),
            "Authentication failed (HTTP 403); check the API key"
        );
        assert_eq!(
            UiLanguage::ZhCn.provider_test_message(&outcome),
            "鉴权失败（HTTP 403），请检查 API Key"
        );
    }

    #[test]
    fn proxy_routes_are_data_until_the_display_boundary() {
        let outcome =
            crate::proxy_test::ProxyTestOutcome::Success(crate::proxy_test::ProxyTestRoute::Direct);
        assert_eq!(
            UiLanguage::EnUs.proxy_test_message(&outcome, 12),
            "Network connection succeeded via Direct connection in 12 ms"
        );
        assert_eq!(
            UiLanguage::ZhCn.proxy_test_message(&outcome, 12),
            "网络连接正常 · 直接连接 · 12 ms"
        );
    }
}
