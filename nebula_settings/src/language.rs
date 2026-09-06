#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanguageInfo {
    pub preference: LanguagePref,
    pub code: &'static str,
    pub native_name: &'static str,
    pub component_locale: &'static str,
    pub rust_variant: &'static str,
}

macro_rules! languages {
    ($( $variant:ident => ($code:literal, $native:literal, $component:literal) ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum LanguagePref {
            #[default]
            System,
            $( $variant, )+
        }

        impl LanguagePref {
            pub const ALL: &'static [Self] = &[Self::System, $( Self::$variant, )+];
            pub const VALUES: &'static [&'static str] = &["system", $( $code, )+];
            pub const LANGUAGES: &'static [LanguageInfo] = &[
                $( LanguageInfo {
                    preference: Self::$variant,
                    code: $code,
                    native_name: $native,
                    component_locale: $component,
                    rust_variant: stringify!($variant),
                }, )+
            ];

            pub fn from_settings(value: &str) -> Option<Self> {
                match value.trim() {
                    "system" => Some(Self::System),
                    $( $code => Some(Self::$variant), )+
                    _ => None,
                }
            }

            pub const fn settings_value(self) -> &'static str {
                match self {
                    Self::System => "system",
                    $( Self::$variant => $code, )+
                }
            }

            pub const fn native_name(self) -> &'static str {
                match self {
                    Self::System => "System",
                    $( Self::$variant => $native, )+
                }
            }
        }
    };
}

languages! {
    ZhCn => ("zh-CN", "简体中文", "zh-CN"),
    EnUs => ("en-US", "English", "en"),
    ZhTw => ("zh-TW", "繁體中文", "zh-TW"),
    FrFr => ("fr-FR", "Français", "en"),
    DeDe => ("de-DE", "Deutsch", "en"),
    EsEs => ("es-ES", "Español", "en"),
    PtBr => ("pt-BR", "Português (Brasil)", "en"),
    ItIt => ("it-IT", "Italiano", "it"),
    RuRu => ("ru-RU", "Русский", "en"),
    JaJp => ("ja-JP", "日本語", "en"),
    KoKr => ("ko-KR", "한국어", "en"),
}

impl LanguagePref {
    pub fn from_locale(locale: &str) -> Option<Self> {
        let locale = locale.trim().split(['.', '@']).next()?;
        let mut parts = locale.split(['-', '_']);
        let primary = parts.next()?;
        if primary.eq_ignore_ascii_case("zh") {
            let mut traditional_script = false;
            let mut simplified_script = false;
            let mut traditional_region = false;
            for part in parts {
                traditional_script |= part.eq_ignore_ascii_case("Hant");
                simplified_script |= part.eq_ignore_ascii_case("Hans");
                traditional_region |=
                    ["TW", "HK", "MO"].iter().any(|region| part.eq_ignore_ascii_case(region));
            }
            if traditional_script {
                return Some(Self::ZhTw);
            }
            if simplified_script {
                return Some(Self::ZhCn);
            }
            return Some(if traditional_region { Self::ZhTw } else { Self::ZhCn });
        }
        Self::LANGUAGES
            .iter()
            .find(|info| {
                info.code.split('-').next().is_some_and(|base| base.eq_ignore_ascii_case(primary))
            })
            .map(|info| info.preference)
    }
}

#[cfg(test)]
mod tests {
    use super::LanguagePref;

    #[test]
    fn every_language_round_trips_without_changing_saved_values() {
        assert_eq!(&LanguagePref::VALUES[..3], &["system", "zh-CN", "en-US"]);
        for (preference, value) in LanguagePref::ALL.iter().zip(LanguagePref::VALUES) {
            assert_eq!(preference.settings_value(), *value);
            assert_eq!(LanguagePref::from_settings(value), Some(*preference));
            assert!(!preference.native_name().is_empty());
        }
        assert_eq!(LanguagePref::from_settings("zh"), None);
        assert_eq!(LanguagePref::from_settings("unknown"), None);
    }

    #[test]
    fn system_locale_negotiates_regions_scripts_and_posix_suffixes() {
        for (locale, expected) in [
            ("fr_CA.UTF-8", LanguagePref::FrFr),
            ("DE-at", LanguagePref::DeDe),
            ("es_MX", LanguagePref::EsEs),
            ("pt-PT", LanguagePref::PtBr),
            ("ja_JP.UTF-8", LanguagePref::JaJp),
            ("ko-KR", LanguagePref::KoKr),
            ("ru_RU@variant", LanguagePref::RuRu),
            ("it-CH", LanguagePref::ItIt),
            ("en-GB", LanguagePref::EnUs),
            ("zh-Hant", LanguagePref::ZhTw),
            ("zh_HK", LanguagePref::ZhTw),
            ("zh-Hans-TW", LanguagePref::ZhCn),
            ("zh_CN.UTF-8", LanguagePref::ZhCn),
        ] {
            assert_eq!(LanguagePref::from_locale(locale), Some(expected), "{locale}");
        }
        assert_eq!(LanguagePref::from_locale("C.UTF-8"), None);
        assert_eq!(LanguagePref::from_locale("unknown"), None);
    }
}
