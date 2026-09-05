use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use markdown::{ParseOptions, mdast::Node, to_mdast};

pub const MAX_IMAGES: usize = 8;
pub const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;
pub const IMAGE_LANGUAGE: &str = "nebula-answer-image";

#[derive(Clone)]
pub struct AnswerImage {
    pub target: String,
    pub alt: String,
    pub placeholder_start: usize,
}

pub struct ReaderDocument {
    pub markdown: String,
    pub images: Vec<AnswerImage>,
    pub images_omitted: usize,
}

struct NormalizedSource {
    text: String,
    inline_math: Vec<Range<usize>>,
}

pub fn prepare(source: &str) -> ReaderDocument {
    let normalized = normalize_math_delimiters(source);
    let source = normalized.text.as_str();
    let Ok(root) = to_mdast(source, &parse_options()) else {
        return ReaderDocument {
            markdown: literal_markdown(source),
            images: Vec::new(),
            images_omitted: 0,
        };
    };
    let mut definitions = HashMap::new();
    let mut nodes = vec![&root];
    while let Some(node) = nodes.pop() {
        if let Node::Definition(definition) = node {
            definitions.entry(definition.identifier.as_str()).or_insert(definition.url.as_str());
        }
        if let Some(children) = node.children() {
            nodes.extend(children.iter().rev());
        }
    }
    let mut replacements: Vec<(Range<usize>, String, Option<AnswerImage>)> = Vec::new();
    let mut images_omitted = 0;
    let mut image_count = 0;
    nodes.push(&root);
    while let Some(node) = nodes.pop() {
        let image = match node {
            Node::Image(image) => Some((image.url.as_str(), image.alt.as_str())),
            Node::ImageReference(image) => definitions
                .get(image.identifier.as_str())
                .map(|target| (*target, image.alt.as_str())),
            _ => None,
        };
        let range = node.position().map(|position| position.start.offset..position.end.offset);
        match (node, range) {
            (_, Some(range)) if image.is_some() => {
                let (target, alt) = image.unwrap();
                if image_count >= MAX_IMAGES {
                    images_omitted += 1;
                    replacements.push((
                        range,
                        "\n\n图片数量超过本次阅读上限；原文仍可复制。\n\n".into(),
                        None,
                    ));
                    continue;
                }
                let alt = if alt.is_empty() {
                    format!("图片 {}", image_count + 1)
                } else {
                    alt.to_owned()
                };
                let image = AnswerImage { target: target.to_owned(), alt, placeholder_start: 0 };
                let replacement = format!("\n\n```{IMAGE_LANGUAGE}\n{image_count}\n```\n\n");
                replacements.push((range, replacement, Some(image)));
                image_count += 1;
            },
            (Node::Html(_), Some(range)) => {
                let escaped = source[range.clone()]
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                replacements.push((range, escaped, None));
            },
            (Node::InlineMath(math), Some(range))
                if !explicit_inline_math(&math.value)
                    && !normalized.inline_math.contains(&range) =>
            {
                let literal = source[range.clone()].replace('$', "\\$");
                replacements.push((range, literal, None));
            },
            _ => {
                if let Some(children) = node.children() {
                    nodes.extend(children.iter().rev());
                }
            },
        }
    }
    replacements.sort_by_key(|(range, _, _)| range.start);
    let mut markdown = String::with_capacity(source.len());
    let mut images = Vec::new();
    let mut offset = 0;
    for (range, replacement, image) in replacements {
        if range.start < offset {
            continue;
        }
        markdown.push_str(&source[offset..range.start]);
        if let Some(mut image) = image {
            image.placeholder_start = markdown.len() + 2;
            images.push(image);
        }
        markdown.push_str(&replacement);
        offset = range.end;
    }
    markdown.push_str(&source[offset..]);
    ReaderDocument { markdown, images, images_omitted }
}

fn parse_options() -> ParseOptions {
    let mut options = ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    options
}

fn explicit_inline_math(source: &str) -> bool {
    let trimmed = source.trim();
    source == trimmed
        && (trimmed.chars().count() == 1 && trimmed.chars().all(char::is_alphabetic)
            || trimmed.contains(['\\', '^', '_', '=', '+', '<', '>']))
}

fn normalize_math_delimiters(source: &str) -> NormalizedSource {
    let mut protected = Vec::new();
    let Ok(root) = to_mdast(source, &parse_options()) else {
        return NormalizedSource { text: source.to_owned(), inline_math: Vec::new() };
    };
    let mut nodes = vec![&root];
    while let Some(node) = nodes.pop() {
        if matches!(
            node,
            Node::Code(_)
                | Node::InlineCode(_)
                | Node::Html(_)
                | Node::Math(_)
                | Node::InlineMath(_)
                | Node::Image(_)
                | Node::ImageReference(_)
                | Node::Link(_)
                | Node::LinkReference(_)
        ) {
            if let Some(position) = node.position() {
                protected.push(position.start.offset..position.end.offset);
            }
        } else if let Some(children) = node.children() {
            nodes.extend(children.iter().rev());
        }
    }
    protected.sort_by_key(|range| range.start);
    let mut result = String::with_capacity(source.len());
    let mut inline_math = Vec::new();
    let mut offset = 0;
    let mut protected_index = 0;
    let mut no_closing_display = false;
    let mut inline_search_after = 0;
    while offset < source.len() {
        while protected_index < protected.len() && protected[protected_index].end <= offset {
            protected_index += 1;
        }
        if let Some(range) = protected.get(protected_index).filter(|range| range.contains(&offset))
        {
            result.push_str(&source[offset..range.end]);
            offset = range.end;
            continue;
        }
        let remaining = &source[offset..];
        if remaining.starts_with(r"\\") {
            result.push_str(r"\\");
            offset += 2;
            continue;
        }
        if !no_closing_display
            && remaining.starts_with(r"\[")
            && (offset == 0 || source.as_bytes()[offset - 1] == b'\n')
        {
            let opening_end = remaining.find('\n').unwrap_or(remaining.len());
            if remaining[..opening_end].trim() == r"\[" {
                let body_start = offset + opening_end;
                let mut closing_start = body_start;
                for line in source[body_start..].split_inclusive('\n') {
                    if line.trim() == r"\]" {
                        result.push_str("$$");
                        result.push_str(&source[body_start..closing_start]);
                        result.push_str("$$");
                        if line.ends_with('\n') {
                            result.push('\n');
                        }
                        offset = closing_start + line.len();
                        break;
                    }
                    closing_start += line.len();
                }
                if offset > body_start {
                    continue;
                }
                no_closing_display = true;
            }
        } else if offset >= inline_search_after && remaining.starts_with(r"\(") {
            let line_end = remaining.find('\n').unwrap_or(remaining.len());
            if let Some(closing) = remaining[2..line_end].find(r"\)") {
                let body = &remaining[2..closing + 2];
                if !body.contains(['$', '`']) {
                    let start = result.len();
                    result.push('$');
                    result.push_str(body);
                    result.push('$');
                    inline_math.push(start..result.len());
                    offset += closing + 4;
                    continue;
                }
            } else {
                inline_search_after = offset + line_end;
            }
        }
        let character = remaining.chars().next().unwrap();
        result.push(character);
        offset += character.len_utf8();
    }
    NormalizedSource { text: result, inline_math }
}

pub fn literal_markdown(source: &str) -> String {
    let fence = "`".repeat(
        source.split(|character| character != '`').map(str::len).max().unwrap_or(0).max(2) + 1,
    );
    format!("{fence}text\n{source}\n{fence}\n")
}

pub fn local_image_path(target: &str, base: &Path) -> Result<PathBuf, String> {
    if target.len() > 4096 || target.chars().any(char::is_control) {
        return Err("图片路径无效，未读取文件。".into());
    }
    let lower = target.to_ascii_lowercase();
    if lower.contains("://")
        || lower.starts_with("data:")
        || lower.starts_with("file:")
        || network_path(target)
        || network_path(&base.to_string_lossy())
    {
        return Err("网络图片不会自动下载；可选择本地图片查看。".into());
    }
    let path = Path::new(target);
    let path = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let root = base.canonicalize().map_err(|_| "无法确认当前本地目录，未读取图片。".to_owned())?;
    let path = path.canonicalize().map_err(|_| "图片文件不存在或无法访问。".to_owned())?;
    if !path.starts_with(root) {
        return Err("图片不在当前目录内；请主动选择文件后查看。".into());
    }
    Ok(path)
}

fn network_path(path: &str) -> bool {
    path.starts_with(['\\', '/'])
        && path.as_bytes().get(1).is_some_and(|next| matches!(next, b'\\' | b'/'))
}

pub fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    if !path
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_IMAGE_BYTES)
    {
        return Err("图片不是普通文件，或超过 12 MiB 上限。".into());
    }
    let file = std::fs::File::open(path).map_err(|_| "无法打开图片文件。".to_owned())?;
    let metadata = file.metadata().map_err(|_| "无法读取图片文件信息。".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES {
        return Err("图片不是普通文件，或超过 12 MiB 上限。".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法完整读取图片。".to_owned())?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("图片超过 12 MiB 上限，未解码。".into());
    }
    if !matches!(
        image::guess_format(&bytes),
        Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("本版阅读视图仅支持 PNG / JPEG 图片。".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_source_keeps_every_operator_and_matrix_row() {
        let source = "# 回答\n\n$$\n\\widehat{f}(\\xi)\n=\n+ \\int_0^1 x\\,dx\n\\begin{pmatrix}a&b\\\\\nc&d\\end{pmatrix}\n$$\n";
        assert_eq!(prepare(source).markdown, source);
    }

    #[test]
    fn bracket_math_is_protected_before_markdown_headings_and_lists() {
        let source = "前文 \\(x^2\\)\n\n\\[\n\\widehat{f}(\\xi)\n=\n+ \\int_0^1 x\\,dx\n\\begin{pmatrix}a&b\\\\\nc&d\\end{pmatrix}\n\\]\n";
        let document = prepare(source);
        assert!(document.markdown.contains("前文 $x^2$"));
        assert!(document.markdown.contains("$$\n\\widehat{f}"));
        let root = to_mdast(&document.markdown, &parse_options()).unwrap();
        let formulas = root
            .children()
            .unwrap()
            .iter()
            .filter_map(|node| match node {
                Node::Math(math) => Some(math.value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(formulas.len(), 1);
        assert!(formulas[0].contains("\n=\n+ \\int"));
        assert!(formulas[0].contains("a&b\\\\\nc&d"));
    }

    #[test]
    fn math_delimiters_inside_code_and_escaped_backslashes_are_literal() {
        let source = "`\\(x\\)`\n\n```tex\n\\[\nx=1\n\\]\n```\n\n\\\\(x\\\\)\n";
        assert_eq!(normalize_math_delimiters(source).text, source);
        assert_eq!(normalize_math_delimiters("\\[\nx=1\n").text, "\\[\nx=1\n");
    }

    #[test]
    fn explicit_inline_brackets_do_not_need_operator_guessing() {
        assert_eq!(
            prepare(r"函数 \(f(x)\)，价格 $5 and $10").markdown,
            r"函数 $f(x)$，价格 \$5 and \$10"
        );
    }

    #[test]
    fn reference_images_and_extra_images_remain_bounded() {
        let source = format!("{}\n[plot]: plot.png\n", "![图][plot]\n\n".repeat(MAX_IMAGES + 2));
        let document = prepare(&source);
        assert_eq!(document.images.len(), MAX_IMAGES);
        assert_eq!(document.images_omitted, 2);
        assert!(
            document.images.iter().all(|image| image.target == "plot.png" && image.alt == "图")
        );
    }

    #[test]
    fn image_loader_rejects_wrong_formats_directories_and_oversized_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("not-an-image.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(read_image(&path).is_err());
        assert!(read_image(directory.path()).is_err());
        std::fs::File::create(&path).unwrap().set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert!(read_image(&path).is_err());
    }

    #[test]
    fn images_are_explicit_placeholders_not_automatic_network_requests() {
        let document = prepare(
            "文字\n\n![图](plot.png)\n\n下一段\n\n![远程](https://example.invalid/private.png)",
        );
        assert_eq!(document.images.len(), 2);
        assert!(!document.markdown.contains("https://"));
        assert!(!document.markdown.contains("plot.png"));
        for image in &document.images {
            assert!(
                document.markdown[image.placeholder_start..].starts_with("```nebula-answer-image")
            );
        }
        assert!(document.markdown.contains("下一段"));
    }

    #[test]
    fn code_and_money_stay_literal_and_html_cannot_fetch_images() {
        let source = "价格 $5 and $10\n\n```md\n![code](secret.png)\n$$x=1$$\n```\n\n<img src=\"https://example.invalid/a\">\n";
        let document = prepare(source);
        assert!(document.images.is_empty());
        assert!(document.markdown.contains("价格 \\$5 and \\$10"));
        assert!(document.markdown.contains("![code](secret.png)"));
        assert!(!document.markdown.contains("<img"));
    }

    #[test]
    fn raw_view_cannot_escape_its_code_fence() {
        let literal = literal_markdown("```\n![x](secret.png)\n````");
        assert!(literal.starts_with("`````text\n"));
        assert!(prepare(&literal).images.is_empty());
    }

    #[test]
    fn image_paths_cannot_escape_the_reading_directory() {
        let directory = tempfile::tempdir().unwrap();
        let inside = directory.path().join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(inside.join("plot.png"), b"test").unwrap();
        std::fs::write(directory.path().join("private.png"), b"test").unwrap();
        assert!(local_image_path("plot.png", &inside).is_ok());
        assert!(local_image_path("../private.png", &inside).is_err());
        for path in [
            "https://example.invalid/a.png",
            "data:image/png;base64,AA",
            "\\\\server\\share\\a.png",
            "//server/share/a.png",
        ] {
            assert!(local_image_path(path, &inside).is_err());
        }
    }
}
