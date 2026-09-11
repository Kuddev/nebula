use std::path::Path;

/// 把文档里的本地 GIF / 动画 WebP 换成缓存里的单帧 PNG。
/// 网络图走 HTTP 客户端同一套压帧；本地图不经过 HTTP，不换的话 gpui
/// 仍会播 GIF，markdown 多图共用 element id 时越界 panic。
pub(super) fn rewrite_doc_images(text: &str, base: Option<&Path>) -> String {
    use std::sync::LazyLock;
    static MARKDOWN_IMG: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(!\[[^\]]*\]\()([^)\s]+)"#).unwrap());
    // `regex` crate 不支持反向引用 `\2`，双引号/单引号拆开写。
    static HTML_IMG_DQ: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*")([^"]+)""#).unwrap());
    static HTML_IMG_SQ: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*')([^']+)'"#).unwrap());
    let rewritten = MARKDOWN_IMG.replace_all(text, |caps: &regex::Captures<'_>| {
        format!("{}{}", &caps[1], flatten_local_image_url(&caps[2], base))
    });
    let rewritten = HTML_IMG_DQ.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
        format!(r#"{}{}""#, &caps[1], flatten_local_image_url(&caps[2], base))
    });
    HTML_IMG_SQ
        .replace_all(&rewritten, |caps: &regex::Captures<'_>| {
            format!("{}{}'", &caps[1], flatten_local_image_url(&caps[2], base))
        })
        .into_owned()
}

fn flatten_local_image_url(url: &str, base: Option<&Path>) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:") {
        return url.to_owned();
    }
    // Static images do not need animation flattening. In particular, do not
    // reread every large PNG/JPEG occurrence while parsing a long document.
    let extension = Path::new(url).extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    if !extension.eq_ignore_ascii_case("gif") && !extension.eq_ignore_ascii_case("webp") {
        return url.to_owned();
    }
    let path = Path::new(url);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match base {
            Some(base) => base.join(path),
            None => return url.to_owned(),
        }
    };
    let Ok(bytes) = std::fs::read(&resolved) else {
        return url.to_owned();
    };
    let Some(png) = crate::gpui_shell::http::flatten_animated_to_png(&bytes) else {
        return url.to_owned();
    };
    let cache = std::env::temp_dir().join("nebula-md-img");
    if std::fs::create_dir_all(&cache).is_err() {
        return url.to_owned();
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&bytes, &mut hasher);
    let dest = cache.join(format!("{:016x}.png", std::hash::Hasher::finish(&hasher)));
    if !dest.exists() && std::fs::write(&dest, png).is_err() {
        return url.to_owned();
    }
    dest.to_string_lossy().replace('\\', "/")
}
