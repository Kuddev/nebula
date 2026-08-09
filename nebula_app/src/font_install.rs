use std::path::{Path, PathBuf};

#[cfg(windows)]
use sha2::{Digest, Sha256};

pub const REQUIRED_FONT_FAMILY: &str = "Maple Mono Normal NF CN";

/// 一个系统已安装字体族，连同平台给出的等宽判定。
///
/// 平台枚举本身留在这个类型之外：目录装配只吃普通字符串与布尔，因此可以
/// 在任何平台上被测试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFontFamily {
    pub name: String,
    pub monospaced: bool,
}

/// 一个字体族在**字体目录**中的来源。同名冲突时系统记录优先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSource {
    System,
    Imported,
}

/// 字体目录中的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontCatalogEntry {
    pub name: String,
    /// 非等宽族在界面上要给出比例字体警告，但仍可选择。
    pub monospaced: bool,
    pub source: FontSource,
}

/// 由系统字体族、Nebula 导入字体族、过滤开关与查询串装配**字体目录**。
///
/// - 同名（忽略大小写）合并为一项，系统记录优先。
/// - 默认只列等宽族；`show_all` 临时显示全部。导入字体没有系统那样的等宽
///   元数据，视作可用，默认视图不隐藏它们。
/// - 搜索匹配的是列表上显示的那个名字，不维护跨语言别名。
/// - `current` 是当前生效字体：即使不满足过滤或搜索条件也保留，否则用户
///   会以为自己的选择丢了。
pub fn font_catalog(
    system: &[SystemFontFamily],
    imported: &[String],
    show_all: bool,
    query: &str,
    current: &str,
) -> Vec<FontCatalogEntry> {
    let mut entries: Vec<FontCatalogEntry> = Vec::with_capacity(system.len() + imported.len());
    let mut seen: Vec<String> = Vec::with_capacity(system.len() + imported.len());
    let key = |name: &str| name.to_lowercase();

    for family in system {
        let k = key(&family.name);
        if seen.contains(&k) {
            continue;
        }
        seen.push(k);
        entries.push(FontCatalogEntry {
            name: family.name.clone(),
            monospaced: family.monospaced,
            source: FontSource::System,
        });
    }
    for name in imported {
        let k = key(name);
        if seen.contains(&k) {
            continue;
        }
        seen.push(k);
        entries.push(FontCatalogEntry {
            name: name.clone(),
            monospaced: true,
            source: FontSource::Imported,
        });
    }

    let needle = query.trim().to_lowercase();
    let current_key = key(current);
    entries.retain(|entry| {
        let is_current = key(&entry.name) == current_key;
        let passes_filter = show_all || entry.monospaced;
        let passes_query = needle.is_empty() || entry.name.to_lowercase().contains(&needle);
        is_current || (passes_filter && passes_query)
    });
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    entries
}
pub const REQUIRED_FONT_FILE: &str = "MapleMonoNormal-NF-CN-Regular.ttf";

#[cfg(windows)]
const MAX_IMPORTED_FONT_BYTES: usize = 64 * 1024 * 1024;

#[cfg(windows)]
pub struct StoredFont {
    pub path: PathBuf,
    pub created: bool,
}

/// 导入字体的存放目录。**纯拼路径，不创建**——写入侧（`store_font`）自己
/// `create_dir_all` 并把失败报给用户，读取侧（`imported_font_files`）容忍
/// 目录不存在。这里若顺手创建，既让读路径每次多一次无谓 IO，也会让写侧
/// 那条错误提示永不触发。
#[cfg(windows)]
pub fn imported_font_directory() -> PathBuf {
    crate::platform::dirs::data_dir().join("fonts")
}

#[cfg(windows)]
pub fn imported_font_files() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(imported_font_directory()) else { return Vec::new() };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && supported_font_extension(path))
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[cfg(windows)]
pub fn store_imported_font(source: &Path) -> Result<StoredFont, String> {
    if !supported_font_extension(source) {
        return Err("只支持 .ttf、.otf、.ttc 和 .otc 字体文件".to_owned());
    }
    let bytes = std::fs::read(source)
        .map_err(|error| format!("无法读取字体 {}: {error}", source.display()))?;
    if bytes.is_empty() || bytes.len() > MAX_IMPORTED_FONT_BYTES {
        return Err("字体文件为空或超过 64 MB 限制".to_owned());
    }

    let digest = Sha256::digest(&bytes);
    let id = digest[..12].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let extension = source.extension().and_then(|value| value.to_str()).unwrap_or("ttf");
    let directory = imported_font_directory();
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建字体目录 {}: {error}", directory.display()))?;
    let path = directory.join(format!("{id}.{}", extension.to_ascii_lowercase()));
    let created = !path.exists();
    if created {
        std::fs::write(&path, bytes)
            .map_err(|error| format!("无法保存导入字体 {}: {error}", path.display()))?;
    }
    Ok(StoredFont { path, created })
}

fn supported_font_extension(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()).is_some_and(|value| {
        matches!(value.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")
    })
}

fn packaged_font_directory(executable: &Path) -> PathBuf {
    executable.parent().unwrap_or_else(|| Path::new(".")).join("fonts")
}

/// Locate the packaged font directory without installing or copying anything.
pub fn bundled_font_directory() -> PathBuf {
    let packaged = std::env::current_exe().ok().map(|exe| packaged_font_directory(&exe));
    if let Some(directory) = packaged.as_ref() {
        if directory.join(REQUIRED_FONT_FILE).is_file() {
            return directory.clone();
        }
    }

    // Local release builds run from target/release rather than an extracted
    // package, so keep the repository asset as a development-only fallback.
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .join("assets")
        .join("fonts");
    if source.join(REQUIRED_FONT_FILE).is_file() {
        return source;
    }

    packaged.unwrap_or_else(|| PathBuf::from("fonts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(name: &str, monospaced: bool) -> SystemFontFamily {
        SystemFontFamily { name: name.to_owned(), monospaced }
    }

    fn names(entries: &[FontCatalogEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn the_default_view_lists_only_monospaced_families() {
        let system = [sys("Consolas", true), sys("Arial", false), sys("Cascadia Mono", true)];
        let catalog = font_catalog(&system, &[], false, "", "Consolas");
        assert_eq!(names(&catalog), ["Cascadia Mono", "Consolas"]);
    }

    #[test]
    fn show_all_reveals_proportional_families_and_flags_them() {
        let system = [sys("Consolas", true), sys("Arial", false)];
        let catalog = font_catalog(&system, &[], true, "", "Consolas");
        assert_eq!(names(&catalog), ["Arial", "Consolas"]);
        let arial = catalog.iter().find(|entry| entry.name == "Arial").unwrap();
        assert!(!arial.monospaced, "比例字体要能被标记出来，界面才谈得上给兼容性警告");
    }

    #[test]
    fn a_system_family_absorbs_the_imported_one_of_the_same_name() {
        // 用户可能把系统里已有的字体又导入了一份；目录里只该出现一个条目，
        // 且以系统记录为准。
        let system = [sys("Consolas", true)];
        let imported = ["consolas".to_owned(), "Maple Mono".to_owned()];
        let catalog = font_catalog(&system, &imported, false, "", "Consolas");
        assert_eq!(names(&catalog), ["Consolas", "Maple Mono"]);
        let consolas = catalog.iter().find(|entry| entry.name == "Consolas").unwrap();
        assert_eq!(consolas.source, FontSource::System);
    }

    #[test]
    fn imported_families_are_assumed_usable_in_the_default_view() {
        // 导入字体是用户特意装进来的终端字体，没有系统那样的等宽元数据，
        // 默认视图不能把它们藏起来。
        let imported = ["Maple Mono".to_owned()];
        let catalog = font_catalog(&[], &imported, false, "", "Maple Mono");
        assert_eq!(names(&catalog), ["Maple Mono"]);
        assert_eq!(catalog[0].source, FontSource::Imported);
    }

    #[test]
    fn search_matches_the_displayed_name_case_insensitively() {
        let system = [sys("Consolas", true), sys("Cascadia Mono", true), sys("Courier New", true)];
        // 当前字体取列表外的值，否则它会被「当前项始终保留」的规则固定住，
        // 这条测试就测不到纯粹的搜索结果了。
        let current = "Maple Mono";
        assert_eq!(names(&font_catalog(&system, &[], false, "cas", current)), ["Cascadia Mono"]);
        assert_eq!(names(&font_catalog(&system, &[], false, "COURIER", current)), ["Courier New"]);
        assert!(font_catalog(&system, &[], false, "没有这个字体", current).is_empty());
    }

    #[test]
    fn the_current_font_stays_visible_even_when_it_would_be_filtered_out() {
        // 当前生效字体从列表里消失，会让用户以为自己的选择丢了。
        let system = [sys("Arial", false), sys("Consolas", true)];
        let catalog = font_catalog(&system, &[], false, "", "Arial");
        assert!(names(&catalog).contains(&"Arial"), "比例字体正在使用时不得被默认过滤藏掉");

        let searched = font_catalog(&system, &[], false, "conso", "Arial");
        assert!(names(&searched).contains(&"Arial"), "搜索不命中当前字体时也要保留它");
    }

    #[test]
    fn packaged_font_directory_is_next_to_the_executable() {
        let executable = Path::new(r"C:\Nebula\nebula.exe");
        assert_eq!(packaged_font_directory(executable), PathBuf::from(r"C:\Nebula\fonts"));
    }

    #[test]
    fn imported_font_extensions_are_limited_to_directwrite_font_containers() {
        assert!(supported_font_extension(Path::new("terminal.TTF")));
        assert!(supported_font_extension(Path::new("terminal.otf")));
        assert!(!supported_font_extension(Path::new("terminal.woff2")));
        assert!(!supported_font_extension(Path::new("terminal.exe")));
    }
}
