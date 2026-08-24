use std::path::{Path, PathBuf};

#[cfg(windows)]
use sha2::{Digest, Sha256};

pub const REQUIRED_FONT_FAMILY: &str = "Maple Mono Normal NF CN";

/// The bundled face is shared by the legacy rasterizer and the GPUI text
/// system. Keeping one static byte slice avoids letting the two shells drift
/// to different font revisions.
#[cfg(windows)]
pub static REQUIRED_FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/MapleMonoNormal-NF-CN-Regular.ttf");

/// 一个系统已安装字体族，连同平台给出的等宽判定。
///
/// 平台枚举本身留在这个类型之外：目录装配只吃普通字符串与布尔，因此可以
/// 在任何平台上被测试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFontFamily {
    pub name: String,
    pub monospaced: bool,
}

/// 一个字体族在**字体目录**中的来源。同名冲突时内置记录优先，确保即使
/// 系统未安装 Maple，设置页也能准确说明应用自带的最终兜底字体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSource {
    Bundled,
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

/// 由内置 Maple、系统字体族、Nebula 导入字体族、过滤开关与查询串装配
/// **字体目录**。
///
/// - 同名（忽略大小写）合并为一项，内置记录优先，其次是系统记录。
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
    let mut entries: Vec<FontCatalogEntry> = Vec::with_capacity(system.len() + imported.len() + 1);
    let mut seen: Vec<String> = Vec::with_capacity(system.len() + imported.len() + 1);
    let key = |name: &str| name.to_lowercase();

    entries.push(FontCatalogEntry {
        name: REQUIRED_FONT_FAMILY.to_owned(),
        monospaced: true,
        source: FontSource::Bundled,
    });
    seen.push(key(REQUIRED_FONT_FAMILY));

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

/// 解析逗号分隔的持久化字体链：清理空项、按名称去重，但不改变用户顺序。
pub fn font_family_chain(value: &str) -> Vec<String> {
    let mut families = Vec::<String>::new();
    for family in value.split(',').map(str::trim).filter(|family| !family.is_empty()) {
        if !families.iter().any(|known| known.eq_ignore_ascii_case(family)) {
            families.push(family.to_owned());
        }
    }
    families
}

fn format_font_family_chain(families: &[String]) -> String {
    families.join(", ")
}

/// 规范化用户直接输入的字体链：逗号后统一留一个空格，并按首次出现去重。
pub fn normalize_font_family_chain(value: &str) -> String {
    format_font_family_chain(&font_family_chain(value))
}

/// 用建议项替换输入框最后一个逗号段。用户先键入尾逗号即可追加 fallback；
/// 没有逗号时则替换主字体，这与 Windows Terminal 的 AutoSuggestBox 一致。
pub fn complete_font_family_input(value: &str, completion: &str) -> String {
    let completion = completion.trim();
    if completion.is_empty() {
        return normalize_font_family_chain(value);
    }

    match value.rsplit_once(',') {
        Some((prefix, _)) => append_font_fallback(prefix, completion),
        None => completion.to_owned(),
    }
}

/// 替换主字体并保留有序 fallback。字体选择器只拥有首项，不能因为用户
/// 更换主字体就静默丢掉手写的字形覆盖链。
pub fn replace_primary_font_family(current: &str, replacement: &str) -> String {
    let replacement = replacement.trim();
    if replacement.is_empty() {
        return current.to_owned();
    }

    let mut families = vec![replacement.to_owned()];
    families.extend(
        font_family_chain(current)
            .into_iter()
            .skip(1)
            .filter(|fallback| !fallback.eq_ignore_ascii_case(replacement)),
    );
    format_font_family_chain(&families)
}

/// 追加一个尚未出现的 fallback；空链的第一次选择自然成为主字体。
pub fn append_font_fallback(current: &str, addition: &str) -> String {
    let addition = addition.trim();
    if addition.is_empty() {
        return current.to_owned();
    }

    let mut families = font_family_chain(current);
    if !families.iter().any(|family| family.eq_ignore_ascii_case(addition)) {
        families.push(addition.to_owned());
    }
    format_font_family_chain(&families)
}

/// 移除字体组中的一项，但至少保留一个主字体。
pub fn remove_font_family(current: &str, index: usize) -> String {
    let mut families = font_family_chain(current);
    if families.len() <= 1 || index >= families.len() {
        return current.to_owned();
    }
    families.remove(index);
    format_font_family_chain(&families)
}

/// 移动字体组中的一项；移到下标 0 的字体自然成为新的主字体。
pub fn move_font_family(current: &str, index: usize, direction: i32) -> String {
    let mut families = font_family_chain(current);
    if index >= families.len() || direction == 0 {
        return current.to_owned();
    }
    let target =
        if direction < 0 { index.saturating_sub(1) } else { (index + 1).min(families.len() - 1) };
    if target == index {
        return current.to_owned();
    }
    families.swap(index, target);
    format_font_family_chain(&families)
}

/// 按终端渲染合同构造 GPUI 字体：即使用户没显式列出，内置 Maple 也始终
/// 作为最后一层字形兜底。
#[cfg(feature = "gpui-shell")]
pub fn gpui_font_with_fallbacks(value: &str) -> gpui::Font {
    let mut families = font_family_chain(value);
    if families.is_empty() {
        families.push(REQUIRED_FONT_FAMILY.to_owned());
    }
    let primary = families.remove(0);
    if !primary.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY)
        && !families.iter().any(|family| family.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY))
    {
        families.push(REQUIRED_FONT_FAMILY.to_owned());
    }

    let mut font = gpui::font(primary);
    if !families.is_empty() {
        font.fallbacks = Some(gpui::FontFallbacks::from_fonts(families));
    }
    font
}
pub const REQUIRED_FONT_FILE: &str = "MapleMonoNormal-NF-CN-Regular.ttf";

/// 枚举系统已安装字体族，带 DirectWrite 的权威等宽判定（`IsMonospacedFont`）。
///
/// 两壳共用的唯一实现（旧壳栅格化器与 GPUI 字体选择器都吃它）。逐族取首
/// 字体问等宽在装了几百个字体的机器上是实打实的开销——调用方自己决定
/// 惰性时机（首次展开目录 / 后台线程），不要放在启动路径上。
#[cfg(windows)]
pub fn enumerate_system_font_families() -> Vec<SystemFontFamily> {
    let collection = dwrote::FontCollection::system();
    let mut families = collection
        .families_iter()
        .filter_map(|family| {
            let name = family.family_name().ok()?;
            // 取该族的首个字体问等宽：族内字重不同但等宽属性一致。
            // 拿不到就按非等宽处理——宁可让它落进「显示全部」，也不要
            // 把一个比例字体混进默认的等宽视图。
            let monospaced =
                family.font(0).ok().and_then(|font| font.is_monospace()).unwrap_or(false);
            Some(SystemFontFamily { name, monospaced })
        })
        .collect::<Vec<_>>();
    families.sort_by_key(|family| family.name.to_lowercase());
    families.dedup_by(|left, right| left.name.eq_ignore_ascii_case(&right.name));
    families
}

/// 探测一个字体文件包含的字体族名（DirectWrite 私有集合，不安装不注册）。
/// 导入路径两壳共用：旧壳把文件加进 crossfont 私有集合，GPUI 把字节交给
/// 自己的 text system；族名判定都以这里为准。
#[cfg(windows)]
pub fn probe_font_file_families(path: &Path) -> Result<Vec<String>, String> {
    let file = dwrote::FontFile::new_from_path(path)
        .ok_or_else(|| format!("DirectWrite 无法解析 {}", path.display()))?;
    let loader = dwrote::CustomFontCollectionLoaderImpl::new(&[file]);
    let collection = dwrote::FontCollection::from_loader(loader);
    let families = collection
        .families_iter()
        .filter_map(|family| family.family_name().ok())
        .collect::<Vec<_>>();
    if families.is_empty() {
        return Err("字体文件不含可用的字体族".to_owned());
    }
    Ok(families)
}

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
    let mut files = font_files_in(&imported_font_directory());
    // 隔离启动把 `data_dir` 指到临时配置时，用户以前导入到
    // `%APPDATA%\Nebula\fonts` 的字体会「消失」。探测实例仍应能看见它们。
    if std::env::var_os("NEBULA_CONFIG_DIR").is_some() {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let user_fonts = PathBuf::from(appdata).join("Nebula").join("fonts");
            if user_fonts != imported_font_directory() {
                files.extend(font_files_in(&user_fonts));
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

#[cfg(windows)]
fn font_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && supported_font_extension(path))
        .collect()
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

/// 「安装字体」提示要打开的目录，保证里面真有 ttf 可装。1.1.0 起 zip 不再
/// 附带 20MB 字体副本（exe 内嵌同一字节），此时把内嵌字体落到数据目录，
/// 与包内副本完全等价。落盘失败（只读盘等）退回原目录，提示仍可关闭。
#[cfg(windows)]
pub fn ensure_bundled_font_on_disk() -> PathBuf {
    let directory = bundled_font_directory();
    if directory.join(REQUIRED_FONT_FILE).is_file() {
        return directory;
    }
    let fallback = imported_font_directory();
    let path = fallback.join(REQUIRED_FONT_FILE);
    if !path.is_file()
        && (std::fs::create_dir_all(&fallback).is_err()
            || std::fs::write(&path, REQUIRED_FONT_BYTES).is_err())
    {
        return directory;
    }
    fallback
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
        assert_eq!(names(&catalog), ["Cascadia Mono", "Consolas", REQUIRED_FONT_FAMILY]);
        assert_eq!(
            catalog.iter().find(|entry| entry.name == REQUIRED_FONT_FAMILY).unwrap().source,
            FontSource::Bundled
        );
    }

    #[test]
    fn show_all_reveals_proportional_families_and_flags_them() {
        let system = [sys("Consolas", true), sys("Arial", false)];
        let catalog = font_catalog(&system, &[], true, "", "Consolas");
        assert_eq!(names(&catalog), ["Arial", "Consolas", REQUIRED_FONT_FAMILY]);
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
        assert_eq!(names(&catalog), ["Consolas", "Maple Mono", REQUIRED_FONT_FAMILY]);
        let consolas = catalog.iter().find(|entry| entry.name == "Consolas").unwrap();
        assert_eq!(consolas.source, FontSource::System);
    }

    #[test]
    fn imported_families_are_assumed_usable_in_the_default_view() {
        // 导入字体是用户特意装进来的终端字体，没有系统那样的等宽元数据，
        // 默认视图不能把它们藏起来。
        let imported = ["Maple Mono".to_owned()];
        let catalog = font_catalog(&[], &imported, false, "", "Maple Mono");
        assert_eq!(names(&catalog), ["Maple Mono", REQUIRED_FONT_FAMILY]);
        assert_eq!(
            catalog.iter().find(|entry| entry.name == "Maple Mono").unwrap().source,
            FontSource::Imported
        );
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
    fn replacing_the_primary_font_preserves_ordered_fallbacks() {
        assert_eq!(
            replace_primary_font_family("Consolas, Microsoft YaHei, Symbols", "Cascadia Mono"),
            "Cascadia Mono, Microsoft YaHei, Symbols"
        );
    }

    #[test]
    fn replacing_the_primary_font_does_not_duplicate_an_existing_fallback() {
        assert_eq!(
            replace_primary_font_family("Consolas, Cascadia Mono, Symbols", "Cascadia Mono"),
            "Cascadia Mono, Symbols"
        );
        assert_eq!(
            replace_primary_font_family("Consolas, cascadia mono, Symbols", "Cascadia Mono"),
            "Cascadia Mono, Symbols"
        );
        assert_eq!(replace_primary_font_family("Consolas", "JetBrains Mono"), "JetBrains Mono");
    }

    #[test]
    fn blank_primary_replacement_leaves_the_setting_unchanged() {
        assert_eq!(replace_primary_font_family("Consolas, Symbols", "  "), "Consolas, Symbols");
    }

    #[test]
    fn family_chain_operations_preserve_order_and_keep_one_primary() {
        assert_eq!(
            font_family_chain(" Consolas, Microsoft YaHei, consolas, Symbols, "),
            ["Consolas", "Microsoft YaHei", "Symbols"]
        );
        assert_eq!(
            append_font_fallback("Consolas, Microsoft YaHei", "Symbols"),
            "Consolas, Microsoft YaHei, Symbols"
        );
        assert_eq!(
            append_font_fallback("Consolas, Microsoft YaHei", "microsoft yahei"),
            "Consolas, Microsoft YaHei"
        );
        assert_eq!(remove_font_family("Consolas", 0), "Consolas");
        assert_eq!(remove_font_family("Consolas, CJK, Symbols", 0), "CJK, Symbols");
        assert_eq!(remove_font_family("Consolas, CJK, Symbols", 1), "Consolas, Symbols");
        assert_eq!(move_font_family("Consolas, CJK, Symbols", 2, -1), "Consolas, Symbols, CJK");
        assert_eq!(move_font_family("Consolas, CJK, Symbols", 1, -1), "CJK, Consolas, Symbols");
        assert_eq!(move_font_family("Consolas, CJK, Symbols", 0, 1), "CJK, Consolas, Symbols");
    }

    #[test]
    fn font_input_completion_replaces_only_the_last_comma_segment() {
        assert_eq!(complete_font_family_input("Consolas", "Cascadia Mono"), "Cascadia Mono");
        assert_eq!(
            complete_font_family_input("Consolas, Microsoft Ya", "Microsoft YaHei"),
            "Consolas, Microsoft YaHei"
        );
        assert_eq!(
            complete_font_family_input("Consolas, Microsoft YaHei,", "Symbols"),
            "Consolas, Microsoft YaHei, Symbols"
        );
        assert_eq!(
            normalize_font_family_chain(" Consolas,  CJK, consolas, Symbols, "),
            "Consolas, CJK, Symbols"
        );
    }

    #[test]
    #[cfg(feature = "gpui-shell")]
    fn gpui_font_uses_the_configured_order_then_the_bundled_fallback() {
        let font = gpui_font_with_fallbacks("Consolas, Microsoft YaHei");
        assert_eq!(font.family.as_ref(), "Consolas");
        assert_eq!(
            font.fallbacks.as_ref().unwrap().fallback_list(),
            &["Microsoft YaHei".to_owned(), REQUIRED_FONT_FAMILY.to_owned()]
        );

        let bundled = gpui_font_with_fallbacks(REQUIRED_FONT_FAMILY);
        assert_eq!(bundled.family.as_ref(), REQUIRED_FONT_FAMILY);
        assert!(bundled.fallbacks.is_none());
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
