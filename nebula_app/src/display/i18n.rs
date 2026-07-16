use crate::config::template::{self, TemplateLanguage};

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
    pub const fn pick<'a>(self, zh_cn: &'a str, en_us: &'a str) -> &'a str {
        match self {
            Self::ZhCn => zh_cn,
            Self::EnUs => en_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LanguagePreference, UiLanguage};

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
}
