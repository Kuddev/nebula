use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use log::LevelFilter;
use serde::Deserialize;
use toml::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownFieldPolicy {
    Warn,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    UnknownField,
    DeprecatedField,
    InvalidValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub kind: DiagnosticKind,
    pub field: Option<String>,
    pub message: String,
    pub error: bool,
}

impl ConfigDiagnostic {
    pub fn is_error(&self) -> bool {
        self.error
    }
}

struct DiagnosticScope {
    unknown_fields: UnknownFieldPolicy,
    diagnostics: Vec<ConfigDiagnostic>,
}

thread_local! {
    static DIAGNOSTIC_SCOPES: RefCell<Vec<DiagnosticScope>> = const { RefCell::new(Vec::new()) };
}

struct DiagnosticGuard {
    active: bool,
}

impl DiagnosticGuard {
    fn finish(mut self) -> Vec<ConfigDiagnostic> {
        self.active = false;
        DIAGNOSTIC_SCOPES.with(|scopes| {
            scopes.borrow_mut().pop().map(|scope| scope.diagnostics).unwrap_or_default()
        })
    }
}

impl Drop for DiagnosticGuard {
    fn drop(&mut self) {
        if self.active {
            DIAGNOSTIC_SCOPES.with(|scopes| {
                scopes.borrow_mut().pop();
            });
        }
    }
}

pub fn capture_diagnostics<T>(
    policy: UnknownFieldPolicy,
    operation: impl FnOnce() -> T,
) -> (T, Vec<ConfigDiagnostic>) {
    DIAGNOSTIC_SCOPES.with(|scopes| {
        scopes
            .borrow_mut()
            .push(DiagnosticScope { unknown_fields: policy, diagnostics: Vec::new() });
    });
    let guard = DiagnosticGuard { active: true };
    let result = operation();
    let diagnostics = guard.finish();
    (result, diagnostics)
}

pub fn report_unknown_field(target: &'static str, field: &str) {
    let message = format!("Unused config key: {field}");
    let captured = DIAGNOSTIC_SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        let Some(scope) = scopes.last_mut() else { return false };
        scope.diagnostics.push(ConfigDiagnostic {
            kind: DiagnosticKind::UnknownField,
            field: Some(field.to_owned()),
            message: message.clone(),
            error: scope.unknown_fields == UnknownFieldPolicy::Deny,
        });
        true
    });
    if !captured {
        log::warn!(target: target, "{message}");
    }
}

pub fn report_deprecated_field(target: &'static str, field: &str, message: &str) {
    let captured = DIAGNOSTIC_SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        let Some(scope) = scopes.last_mut() else { return false };
        scope.diagnostics.push(ConfigDiagnostic {
            kind: DiagnosticKind::DeprecatedField,
            field: Some(field.to_owned()),
            message: message.to_owned(),
            error: false,
        });
        true
    });
    if !captured {
        log::warn!(target: target, "{message}");
    }
}

pub fn report_invalid_value(target: &'static str, field: &str, detail: &str) {
    let message = format!("Config error: {field}: {}", detail.trim());
    let captured = DIAGNOSTIC_SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        let Some(scope) = scopes.last_mut() else { return false };
        scope.diagnostics.push(ConfigDiagnostic {
            kind: DiagnosticKind::InvalidValue,
            field: Some(field.to_owned()),
            message: message.clone(),
            error: true,
        });
        true
    });
    if !captured {
        log::error!(target: target, "{message}");
    }
}

pub trait SerdeReplace {
    fn replace(&mut self, value: Value) -> Result<(), Box<dyn Error>>;
}

#[macro_export]
macro_rules! impl_replace {
    ($($ty:ty),*$(,)*) => {
        $(
            impl SerdeReplace for $ty {
                fn replace(&mut self, value: Value) -> Result<(), Box<dyn Error>> {
                    replace_simple(self, value)
                }
            }
        )*
    };
}

#[rustfmt::skip]
impl_replace!(
    usize, u8, u16, u32, u64, u128,
    isize, i8, i16, i32, i64, i128,
    f32, f64,
    bool,
    char,
    String,
    PathBuf,
    LevelFilter,
);

fn replace_simple<'de, D>(data: &mut D, value: Value) -> Result<(), Box<dyn Error>>
where
    D: Deserialize<'de>,
{
    *data = D::deserialize(value)?;

    Ok(())
}

impl<'de, T: Deserialize<'de>> SerdeReplace for Vec<T> {
    fn replace(&mut self, value: Value) -> Result<(), Box<dyn Error>> {
        replace_simple(self, value)
    }
}

impl<'de, T: SerdeReplace + Deserialize<'de>> SerdeReplace for Option<T> {
    fn replace(&mut self, value: Value) -> Result<(), Box<dyn Error>> {
        match self {
            Some(inner) => inner.replace(value),
            None => replace_simple(self, value),
        }
    }
}

impl<'de, T: Deserialize<'de>> SerdeReplace for HashMap<String, T> {
    fn replace(&mut self, value: Value) -> Result<(), Box<dyn Error>> {
        // Deserialize replacement as HashMap.
        let hashmap: HashMap<String, T> = Self::deserialize(value)?;

        // Merge the two HashMaps, replacing existing values.
        for (key, value) in hashmap {
            self.insert(key, value);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate as nebula_config;
    use nebula_config_derive::ConfigDeserialize;

    #[test]
    fn captures_unknown_and_invalid_fields_without_logging() {
        #[derive(ConfigDeserialize, Default)]
        struct Subject {
            value: usize,
        }

        let (_, diagnostics) = capture_diagnostics(UnknownFieldPolicy::Deny, || {
            let value: Value = toml::from_str("value='wrong'\nunknown=1").unwrap();
            Subject::deserialize(value).unwrap()
        });

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == DiagnosticKind::UnknownField
                && diagnostic.field.as_deref() == Some("unknown")
                && diagnostic.is_error()
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == DiagnosticKind::InvalidValue
                && diagnostic.field.as_deref() == Some("value")
                && diagnostic.is_error()
        }));
    }

    #[test]
    fn replace_option() {
        #[derive(ConfigDeserialize, Default, PartialEq, Eq, Debug)]
        struct ReplaceOption {
            a: usize,
            b: usize,
        }

        let mut subject: Option<ReplaceOption> = None;

        let value: Value = toml::from_str("a=1").unwrap();
        SerdeReplace::replace(&mut subject, value).unwrap();

        let value: Value = toml::from_str("b=2").unwrap();
        SerdeReplace::replace(&mut subject, value).unwrap();

        assert_eq!(subject, Some(ReplaceOption { a: 1, b: 2 }));
    }
}
