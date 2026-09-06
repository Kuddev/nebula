use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use nebula_settings::LanguagePref;

#[path = "i18n/catalog.rs"]
mod catalog;

pub fn generate(directory: &Path, output: &Path) -> Result<(), String> {
    let mut catalogs = BTreeMap::new();
    for info in LanguagePref::LANGUAGES {
        let path = directory.join(format!("{}.json", info.code));
        println!("cargo:rerun-if-changed={}", path.display());
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let messages =
            catalog::parse(&source).map_err(|error| format!("{}: {error}", info.code))?;
        catalogs.insert(info.code, messages);
    }
    let english = &catalogs["en-US"];
    let chinese = &catalogs["zh-CN"];
    if english.is_empty() || !english.keys().eq(chinese.keys()) {
        return Err(
            "English and Simplified Chinese must have identical nonempty message ids".into()
        );
    }
    for (locale, messages) in &catalogs {
        for (key, message) in messages {
            let base =
                english.get(key).ok_or_else(|| format!("{locale}: unknown message id {key}"))?;
            if catalog::placeholders(base)? != catalog::placeholders(message)? {
                return Err(format!("{locale}: placeholder mismatch for {key}"));
            }
        }
    }
    let mut variants = BTreeSet::new();
    let keys = english.keys().collect::<Vec<_>>();
    let mut code = String::new();
    code.push_str(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n#[repr(usize)]\npub enum Message {\n",
    );
    for key in &keys {
        let variant = message_variant(key)?;
        if !variants.insert(variant.clone()) {
            return Err(format!("message variant collision: {key}"));
        }
        writeln!(code, "{variant},").unwrap();
    }
    code.push_str(
        "}\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n#[repr(usize)]\npub enum UiLanguage {\n",
    );
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "{},", info.rust_variant).unwrap();
    }
    code.push_str("}\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub enum LanguagePreference {\n#[default]\nSystem,\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "{},", info.rust_variant).unwrap();
    }
    code.push_str("}\nimpl LanguagePreference {\npub const ALL: &'static [Self] = &[Self::System,");
    for info in LanguagePref::LANGUAGES {
        write!(code, "Self::{},", info.rust_variant).unwrap();
    }
    code.push_str("];\npub const fn shared(self) -> nebula_settings::LanguagePref { match self {\nSelf::System => nebula_settings::LanguagePref::System,\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "Self::{0} => nebula_settings::LanguagePref::{0},", info.rust_variant)
            .unwrap();
    }
    code.push_str("}}\npub const fn explicit(self) -> Option<UiLanguage> { match self {\nSelf::System => None,\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "Self::{0} => Some(UiLanguage::{0}),", info.rust_variant).unwrap();
    }
    code.push_str("}}}\nimpl From<nebula_settings::LanguagePref> for LanguagePreference {\nfn from(value: nebula_settings::LanguagePref) -> Self {match value {\nnebula_settings::LanguagePref::System => Self::System,\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "nebula_settings::LanguagePref::{0} => Self::{0},", info.rust_variant)
            .unwrap();
    }
    code.push_str("}}}\nimpl UiLanguage {\npub const ALL: &'static [Self] = &[");
    for info in LanguagePref::LANGUAGES {
        write!(code, "Self::{},", info.rust_variant).unwrap();
    }
    code.push_str("];\npub const fn code(self) -> &'static str { match self {\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "Self::{} => {:?},", info.rust_variant, info.code).unwrap();
    }
    code.push_str("}}\npub const fn gpui_component_locale(self) -> &'static str { match self {\n");
    for info in LanguagePref::LANGUAGES {
        writeln!(code, "Self::{} => {:?},", info.rust_variant, info.component_locale).unwrap();
    }
    code.push_str("}}}\nfn message_id(key: &str) -> Option<Message> { match key {\n");
    for key in &keys {
        writeln!(code, "{key:?} => Some(Message::{}),", message_variant(key)?).unwrap();
    }
    code.push_str("_ => None,\n}}\n");
    let mut aliases = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (index, key) in keys.iter().enumerate() {
        let source = &english[*key];
        if let Some(previous) = aliases.insert(source, index) {
            if catalogs.values().any(|messages| messages.get(*key) != messages.get(keys[previous]))
            {
                ambiguous.insert(source);
            }
        }
    }
    code.push_str("fn source_id(english: &str) -> Option<Message> { match english {\n");
    for (source, index) in aliases {
        if !ambiguous.contains(source) {
            writeln!(code, "{source:?} => Some(Message::{}),", message_variant(keys[index])?)
                .unwrap();
        }
    }
    code.push_str("_ => None,\n}}\n");
    writeln!(code, "const MESSAGES: [[&str; {}]; {}] = [", keys.len(), catalogs.len()).unwrap();
    for info in LanguagePref::LANGUAGES {
        code.push_str("[\n");
        for key in &keys {
            let message = catalogs[info.code].get(*key).unwrap_or(&english[*key]);
            writeln!(code, "{message:?},").unwrap();
        }
        code.push_str("],\n");
    }
    code.push_str("];\n");
    writeln!(code, "pub const MESSAGE_COUNT: usize = {};", keys.len()).unwrap();
    let bytes =
        catalogs.values().flat_map(|messages| messages.values()).map(String::len).sum::<usize>();
    writeln!(code, "pub const TRANSLATED_BYTES: usize = {bytes};").unwrap();
    std::fs::write(output, code).map_err(|error| error.to_string())
}

fn message_variant(key: &str) -> Result<String, String> {
    let mut variant = String::new();
    for part in key.split(['.', '_', '-']) {
        let mut letters = part.chars();
        let first = letters.next().ok_or_else(|| format!("empty id segment: {key}"))?;
        if !first.is_ascii_alphabetic()
            || !letters.clone().all(|letter| letter.is_ascii_alphanumeric())
        {
            return Err(format!("invalid message id: {key}"));
        }
        variant.push(first.to_ascii_uppercase());
        variant.extend(letters);
    }
    Ok(variant)
}

#[cfg(test)]
mod tests {
    use super::catalog;

    fn fixture(english: &str, chinese: &str, french: &str) -> Result<String, String> {
        static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("pebrel-catalog-test-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for info in nebula_settings::LanguagePref::LANGUAGES {
            let source = match info.code {
                "en-US" => english,
                "zh-CN" => chinese,
                "fr-FR" => french,
                _ => "{}",
            };
            std::fs::write(root.join(format!("{}.json", info.code)), source).unwrap();
        }
        let output = root.join("generated.rs");
        let result = super::generate(&root, &output)
            .and_then(|()| std::fs::read_to_string(output).map_err(|error| error.to_string()));
        std::fs::remove_dir_all(&root).unwrap();
        result
    }

    #[test]
    fn partial_catalogs_fall_back_at_build_time() {
        let source =
            fixture(r#"{"greeting":"Hello {name}"}"#, r#"{"greeting":"你好 {name}"}"#, "{}")
                .unwrap();
        assert!(source.contains("pub enum Message"));
        assert!(source.contains("Greeting,"));
        assert!(source.matches("\"Hello {name}\",").count() >= 10);
    }

    #[test]
    fn rejects_unknown_keys_placeholder_mismatches_and_id_collisions() {
        let english = r#"{"greeting":"Hello {name}"}"#;
        let chinese = r#"{"greeting":"你好 {name}"}"#;
        assert!(fixture(english, chinese, r#"{"extra":"Unknown"}"#).is_err());
        assert!(fixture(english, chinese, r#"{"greeting":"Bonjour {user}"}"#).is_err());
        let colliding = r#"{"a_b":"First","a":{"b":"Second"}}"#;
        assert!(fixture(colliding, colliding, "{}").is_err());
    }

    #[test]
    fn ambiguous_source_phrases_require_contextual_ids() {
        let source = fixture(
            r#"{"verb":"Open","state":"Open"}"#,
            r#"{"verb":"打开","state":"打开状态"}"#,
            r#"{"verb":"Ouvrir","state":"Ouvert"}"#,
        )
        .unwrap();
        assert!(!source.contains("\"Open\" => Some("));
        assert!(source.contains("\"verb\" => Some(Message::Verb)"));
    }

    #[test]
    fn rejects_duplicate_empty_and_nonstring_messages() {
        for source in
            [r#"{"a":"A","a":"B"}"#, r#"{"a.b":"A","a":{"b":"B"}}"#, r#"{"a":null}"#, r#"{"a":""}"#]
        {
            assert!(catalog::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn placeholders_support_unicode_text_repetitions_and_escaped_braces() {
        let names = catalog::placeholders("你好 {name}: {{code}} {name} {count}").unwrap();
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["count", "name"]);
        assert!(catalog::placeholders("broken {name").is_err());
        assert!(catalog::placeholders("broken }").is_err());
    }
}
