use std::env;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CHINESE_TEMPLATE: &str = include_str!("templates/nebula.zh-CN.lua");
pub const ENGLISH_TEMPLATE: &str = include_str!("templates/nebula.en-US.lua");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateLanguage {
    ZhCn,
    EnUs,
}

impl TemplateLanguage {
    fn contents(self) -> &'static str {
        match self {
            Self::ZhCn => CHINESE_TEMPLATE,
            Self::EnUs => ENGLISH_TEMPLATE,
        }
    }
}

#[derive(Debug)]
pub enum TemplateError {
    AlreadyExists(PathBuf),
    InvalidLanguage(String),
    Io(std::io::Error),
}

impl Display for TemplateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists(path) => write!(
                formatter,
                "configuration already exists at {}; use --force to back it up and replace it",
                path.display()
            ),
            Self::InvalidLanguage(language) => write!(
                formatter,
                "unsupported language {language:?}; expected system, zh-CN, or en-US"
            ),
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<std::io::Error> for TemplateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateWrite {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub created: bool,
}

pub fn resolve_template_language(
    explicit: Option<&str>,
    saved: Option<&str>,
    locale: Option<&str>,
) -> Result<TemplateLanguage, TemplateError> {
    let requested = explicit.or(saved).unwrap_or("system");
    match requested {
        "zh-CN" => Ok(TemplateLanguage::ZhCn),
        "en-US" => Ok(TemplateLanguage::EnUs),
        "system" => Ok(if locale.is_some_and(is_chinese_locale) {
            TemplateLanguage::ZhCn
        } else {
            TemplateLanguage::EnUs
        }),
        other => Err(TemplateError::InvalidLanguage(other.to_owned())),
    }
}

pub fn system_locale() -> Option<String> {
    system_locale_impl().or_else(environment_locale)
}

#[cfg(windows)]
fn system_locale_impl() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    let mut buffer = [0_u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 1 {
        return None;
    }
    String::from_utf16(&buffer[..length as usize - 1]).ok()
}

#[cfg(not(windows))]
fn system_locale_impl() -> Option<String> {
    None
}

fn environment_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
}

fn is_chinese_locale(locale: &str) -> bool {
    locale
        .split(['-', '_', '.', '@'])
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("zh"))
}

pub fn write_template(
    path: &Path,
    language: TemplateLanguage,
    force: bool,
) -> Result<TemplateWrite, TemplateError> {
    let existed = path.exists();
    if existed && !force {
        return Err(TemplateError::AlreadyExists(path.to_owned()));
    }

    let parent =
        path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;

    // 必须先完成备份；备份失败时绝不能触碰用户当前可用的配置。
    let backup = if existed { Some(create_backup(path)?) } else { None };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(language.contents().as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| TemplateError::Io(error.error))?;

    Ok(TemplateWrite { path: path.to_owned(), backup, created: !existed })
}

pub fn ensure_user_lua_config(
    path: &Path,
    language: TemplateLanguage,
) -> Result<TemplateWrite, TemplateError> {
    if path.exists() {
        return Ok(TemplateWrite { path: path.to_owned(), backup: None, created: false });
    }
    write_template(path, language, false)
}

fn create_backup(path: &Path) -> Result<PathBuf, std::io::Error> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    for sequence in 0..1000_u16 {
        let suffix = if sequence == 0 {
            format!("bak-{timestamp}")
        } else {
            format!("bak-{timestamp}-{sequence}")
        };
        let backup = path.with_extension(format!("lua.{suffix}"));
        let mut destination =
            match fs::OpenOptions::new().write(true).create_new(true).open(&backup) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
        let mut source = fs::File::open(path)?;
        std::io::copy(&mut source, &mut destination)?;
        destination.sync_all()?;
        return Ok(backup);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate a unique configuration backup name",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::lua::runtime::ReloadSignal;

    fn active_lines(template: &str) -> Vec<&str> {
        template
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect()
    }

    #[test]
    fn localized_templates_have_equivalent_valid_lua() {
        assert_eq!(active_lines(CHINESE_TEMPLATE), active_lines(ENGLISH_TEMPLATE));
        let temp = tempfile::tempdir().unwrap();
        for (name, contents) in [("zh.lua", CHINESE_TEMPLATE), ("en.lua", ENGLISH_TEMPLATE)] {
            let path = temp.path().join(name);
            fs::write(&path, contents).unwrap();
            crate::config::lua::load_lua_file(&path, ReloadSignal::default()).unwrap();
        }
    }

    #[test]
    fn language_resolution_supports_all_public_values() {
        assert_eq!(
            resolve_template_language(Some("zh-CN"), None, None).unwrap(),
            TemplateLanguage::ZhCn
        );
        assert_eq!(
            resolve_template_language(Some("en-US"), None, Some("zh_CN")).unwrap(),
            TemplateLanguage::EnUs
        );
        assert_eq!(
            resolve_template_language(Some("system"), None, Some("zh-Hans-CN")).unwrap(),
            TemplateLanguage::ZhCn
        );
    }

    #[test]
    fn existing_file_requires_force_and_force_creates_backup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nebula.lua");
        fs::write(&path, "return { existing = true }").unwrap();
        assert!(matches!(
            write_template(&path, TemplateLanguage::ZhCn, false),
            Err(TemplateError::AlreadyExists(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "return { existing = true }");

        let result = write_template(&path, TemplateLanguage::EnUs, true).unwrap();
        assert_eq!(
            fs::read_to_string(result.backup.unwrap()).unwrap(),
            "return { existing = true }"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), ENGLISH_TEMPLATE);
    }

    #[test]
    fn ensure_never_rewrites_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nebula.lua");
        let first = ensure_user_lua_config(&path, TemplateLanguage::EnUs).unwrap();
        assert!(first.created);
        let original = fs::read_to_string(&path).unwrap();
        let second = ensure_user_lua_config(&path, TemplateLanguage::ZhCn).unwrap();
        assert!(!second.created);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
