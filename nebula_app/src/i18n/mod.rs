mod format;
mod locale;

pub use locale::system_locale;

include!(concat!(env!("OUT_DIR"), "/translations.rs"));

impl LanguagePreference {
    pub fn parse(value: &str) -> Option<Self> {
        nebula_settings::LanguagePref::from_settings(value).map(Self::from)
    }

    pub const fn as_str(self) -> &'static str {
        self.shared().settings_value()
    }

    pub fn resolved(self) -> UiLanguage {
        self.explicit().unwrap_or_else(|| UiLanguage::for_locale(system_locale().as_deref()))
    }
}

impl UiLanguage {
    pub fn for_locale(locale: Option<&str>) -> Self {
        locale
            .and_then(nebula_settings::LanguagePref::from_locale)
            .and_then(|preference| LanguagePreference::from(preference).explicit())
            .unwrap_or(Self::EnUs)
    }

    pub const fn text(self, message: Message) -> &'static str {
        MESSAGES[self as usize][message as usize]
    }

    pub fn pick<'text>(self, zh_cn: &'text str, en_us: &'text str) -> &'text str {
        match self {
            Self::ZhCn => zh_cn,
            Self::EnUs => en_us,
            _ => source_id(en_us).map(|message| self.text(message)).unwrap_or(en_us),
        }
    }

    pub fn tr(self, key: &'static str) -> &'static str {
        message_id(key).map(|message| self.text(message)).unwrap_or(key)
    }

    pub fn format(self, message: Message, args: &[(&str, &str)]) -> String {
        format::substitute(self.text(message), args)
    }

    pub fn tr_args(self, key: &'static str, args: &[(&str, &str)]) -> String {
        format::substitute(self.tr(key), args)
    }
}

#[cfg(test)]
mod tests;
