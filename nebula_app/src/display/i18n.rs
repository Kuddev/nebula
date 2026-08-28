use std::collections::HashMap;
use std::sync::OnceLock;

use crate::config::template::{self, TemplateLanguage};
use crate::provider_test::ProviderTestOutcome;
use crate::proxy_test::{ProxyTestFailure, ProxyTestOutcome, ProxyTestRoute};

const EN_US_JSON: &str = include_str!("../../i18n/en-US.json");
const ZH_CN_JSON: &str = include_str!("../../i18n/zh-CN.json");

#[derive(Debug)]
struct TranslationCatalogs {
    en_us: HashMap<String, String>,
    zh_cn: HashMap<String, String>,
}

static TRANSLATIONS: OnceLock<TranslationCatalogs> = OnceLock::new();

fn parse_catalog(source: &str, locale: &str) -> HashMap<String, String> {
    let root: serde_json::Value = serde_json::from_str(source).unwrap_or_else(|error| {
        panic!("embedded {locale} translation catalog is invalid: {error}")
    });
    let mut messages = HashMap::new();
    flatten_catalog(&root, "", &mut messages, locale);
    messages
}

fn flatten_catalog(
    value: &serde_json::Value,
    prefix: &str,
    output: &mut HashMap<String, String>,
    locale: &str,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                flatten_catalog(value, &path, output, locale);
            }
        },
        serde_json::Value::String(message) => {
            assert!(!prefix.is_empty(), "embedded {locale} catalog contains an empty message id");
            assert!(
                output.insert(prefix.to_owned(), message.clone()).is_none(),
                "embedded {locale} catalog contains duplicate message id {prefix}"
            );
        },
        _ => panic!("embedded {locale} catalog leaf {prefix} must be a string"),
    }
}

fn catalogs() -> &'static TranslationCatalogs {
    TRANSLATIONS.get_or_init(|| {
        let en_us = parse_catalog(EN_US_JSON, "en-US");
        let zh_cn = parse_catalog(ZH_CN_JSON, "zh-CN");
        let mut en_keys = en_us.keys().collect::<Vec<_>>();
        let mut zh_keys = zh_cn.keys().collect::<Vec<_>>();
        en_keys.sort_unstable();
        zh_keys.sort_unstable();
        assert_eq!(
            en_keys, zh_keys,
            "embedded en-US and zh-CN catalogs must have identical message ids"
        );
        TranslationCatalogs { en_us, zh_cn }
    })
}

/// Persisted UI language choice. The serialized values are stable because the
/// runtime settings file is also a supported hand-editing surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguagePreference {
    #[default]
    System,
    ZhCn,
    EnUs,
}

impl LanguagePreference {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "system" => Some(Self::System),
            "zh-CN" => Some(Self::ZhCn),
            "en-US" => Some(Self::EnUs),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }

    pub fn resolved(self) -> UiLanguage {
        let template = template::resolve_template_language(
            Some(self.as_str()),
            None,
            template::system_locale().as_deref(),
        )
        .unwrap_or(TemplateLanguage::EnUs);
        match template {
            TemplateLanguage::ZhCn => UiLanguage::ZhCn,
            TemplateLanguage::EnUs => UiLanguage::EnUs,
        }
    }
}

/// Resolved language used by the current process. `pick` keeps translations
/// adjacent at call sites, which makes missing English text obvious in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLanguage {
    ZhCn,
    EnUs,
}

impl UiLanguage {
    pub const fn gpui_component_locale(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en",
        }
    }

    pub const fn pick<'a>(self, zh_cn: &'a str, en_us: &'a str) -> &'a str {
        match self {
            Self::ZhCn => zh_cn,
            Self::EnUs => en_us,
        }
    }

    /// Look up a message in the embedded in-memory catalog. Every locale uses
    /// the stable target-locale -> English -> key fallback chain.
    pub fn tr(self, key: &'static str) -> &'static str {
        let catalogs = catalogs();
        let current = match self {
            Self::ZhCn => &catalogs.zh_cn,
            Self::EnUs => &catalogs.en_us,
        };
        current.get(key).or_else(|| catalogs.en_us.get(key)).map(String::as_str).unwrap_or(key)
    }

    /// Named template substitution is centralized so call sites do not invent
    /// incompatible placeholder syntaxes.
    pub fn tr_args(self, key: &'static str, args: &[(&str, &str)]) -> String {
        let mut message = self.tr(key).to_owned();
        for (name, value) in args {
            message = message.replace(&format!("{{{name}}}"), value);
        }
        message
    }

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
    use super::{EN_US_JSON, LanguagePreference, UiLanguage, ZH_CN_JSON, parse_catalog};

    #[test]
    fn persisted_language_values_are_stable() {
        for (raw, expected) in [
            ("system", LanguagePreference::System),
            ("zh-CN", LanguagePreference::ZhCn),
            ("en-US", LanguagePreference::EnUs),
        ] {
            assert_eq!(LanguagePreference::parse(raw), Some(expected));
            assert_eq!(expected.as_str(), raw);
        }
        assert_eq!(LanguagePreference::parse("zh"), None);
    }

    #[test]
    fn explicit_languages_do_not_depend_on_the_system_locale() {
        assert_eq!(LanguagePreference::ZhCn.resolved(), UiLanguage::ZhCn);
        assert_eq!(LanguagePreference::EnUs.resolved(), UiLanguage::EnUs);
    }

    #[test]
    fn embedded_catalogs_have_identical_string_keys() {
        let en_us = parse_catalog(EN_US_JSON, "en-US");
        let zh_cn = parse_catalog(ZH_CN_JSON, "zh-CN");
        let mut en_keys = en_us.keys().collect::<Vec<_>>();
        let mut zh_keys = zh_cn.keys().collect::<Vec<_>>();
        en_keys.sort_unstable();
        zh_keys.sort_unstable();
        assert!(!en_keys.is_empty());
        assert_eq!(en_keys, zh_keys);
    }

    #[test]
    fn lookup_uses_locale_then_english_then_key() {
        assert_eq!(UiLanguage::ZhCn.tr("settings.sidebar.network"), "网络");
        assert_eq!(UiLanguage::EnUs.tr("settings.sidebar.network"), "Network");
        assert_eq!(UiLanguage::EnUs.tr("missing.message.id"), "missing.message.id");
    }

    #[test]
    fn named_arguments_are_substituted() {
        assert_eq!(
            UiLanguage::EnUs.tr_args(
                "settings.network.status.success",
                &[("route", "Direct"), ("elapsed_ms", "12")],
            ),
            "Network connection succeeded via Direct in 12 ms"
        );
    }

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

    #[test]
    #[ignore = "manual i18n microbenchmark"]
    fn measure_embedded_catalog_costs() {
        use std::hint::black_box;
        use std::time::Instant;

        let init_started = Instant::now();
        let en_us = parse_catalog(EN_US_JSON, "en-US");
        let zh_cn = parse_catalog(ZH_CN_JSON, "zh-CN");
        let mut en_keys = en_us.keys().collect::<Vec<_>>();
        let mut zh_keys = zh_cn.keys().collect::<Vec<_>>();
        en_keys.sort_unstable();
        zh_keys.sort_unstable();
        assert_eq!(en_keys, zh_keys);
        let init_elapsed = init_started.elapsed();

        let lookup_iterations = 2_000_000u128;
        let lookup_started = Instant::now();
        for _ in 0..lookup_iterations {
            black_box(UiLanguage::EnUs.tr("settings.sidebar.network"));
        }
        let lookup_ns = lookup_started.elapsed().as_nanos() / lookup_iterations;

        let template_iterations = 200_000u128;
        let template_started = Instant::now();
        for _ in 0..template_iterations {
            black_box(UiLanguage::EnUs.tr_args(
                "provider.test.success",
                &[("status", "200")],
            ));
        }
        let template_ns = template_started.elapsed().as_nanos() / template_iterations;

        eprintln!(
            "i18n resources={} bytes, messages={}, init={} us, lookup={} ns, template={} ns",
            EN_US_JSON.len() + ZH_CN_JSON.len(),
            en_us.len(),
            init_elapsed.as_micros(),
            lookup_ns,
            template_ns,
        );
    }
}
