//! The same parsed heading index drives source navigation and preview blocks.
//! Split only at root block boundaries; fenced code, lists and quotes remain intact.

use markdown::mdast::Node;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Heading {
    pub(super) label: String,
    pub(super) depth: u8,
    pub(super) row: u32,
    pub(super) block: usize,
}

#[derive(Default)]
pub(super) struct Outline {
    pub(super) headings: Vec<Heading>,
    pub(super) blocks: Vec<String>,
}

impl Outline {
    pub(super) fn parse(source: &str) -> Self {
        let mut options = markdown::ParseOptions::gfm();
        options.constructs.math_flow = true;
        options.constructs.math_text = true;
        let Ok(Node::Root(root)) = markdown::to_mdast(source, &options) else {
            return Self { headings: vec![], blocks: vec![source.to_owned()] };
        };
        // Resolve reference links/images from any part of the original document.
        let definitions = root
            .children
            .iter()
            .filter_map(|node| {
                if !matches!(node, Node::Definition(_) | Node::FootnoteDefinition(_)) {
                    return None;
                }
                let position = node.position()?;
                source.get(position.start.offset..position.end.offset)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut result = Self::default();
        for node in &root.children {
            if matches!(node, Node::Definition(_) | Node::FootnoteDefinition(_)) {
                continue;
            }
            let Some(position) = node.position() else { continue };
            let Some(text) = source.get(position.start.offset..position.end.offset) else {
                continue;
            };
            collect_headings(node, result.blocks.len(), &mut result.headings);
            result.blocks.push(if definitions.is_empty() {
                text.to_owned()
            } else {
                format!("{text}\n\n{definitions}")
            });
        }
        result
    }
}

fn label(node: &Node, text: &mut String) {
    match node {
        Node::Text(node) => text.push_str(&node.value),
        Node::InlineCode(node) => text.push_str(&node.value),
        Node::InlineMath(node) => text.push_str(&node.value),
        Node::Image(node) => text.push_str(&node.alt),
        Node::ImageReference(node) => text.push_str(&node.alt),
        Node::Break(_) => text.push(' '),
        _ => {
            if let Some(children) = node.children() {
                for child in children {
                    label(child, text);
                }
            }
        },
    }
}

fn collect_headings(node: &Node, block: usize, headings: &mut Vec<Heading>) {
    if let Node::Heading(heading) = node {
        let mut text = String::new();
        label(node, &mut text);
        headings.push(Heading {
            label: text,
            depth: heading.depth,
            row: heading.position.as_ref().map_or(0, |p| p.start.line.saturating_sub(1) as u32),
            block,
        });
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_headings(child, block, headings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_keep_hierarchy_source_rows_and_duplicate_targets() {
        let outline = Outline::parse(
            "# 标题 *one*\n\n```md\n# not a heading\n```\n\nTitle\n---\n\n## 标题 `two`\n\n## 标题 `two`\n",
        );
        assert_eq!(
            outline.headings.iter().map(|h| (h.label.as_str(), h.depth, h.row)).collect::<Vec<_>>(),
            [("标题 one", 1, 0), ("Title", 2, 6), ("标题 two", 2, 9), ("标题 two", 2, 11)]
        );
        assert_ne!(outline.headings[2].block, outline.headings[3].block);
        assert!(outline.blocks.iter().any(|text| text.starts_with("```md\n# not")));
    }

    #[test]
    fn nested_headings_keep_their_container_and_references_resolve_across_blocks() {
        let outline = Outline::parse(
            "> ## [linked][ref]\n> body\n\n![image][img]\n\n[ref]: https://example.com\n[img]: images/test.png\n",
        );
        assert_eq!(outline.headings[0].label, "linked");
        assert_eq!(outline.headings[0].block, 0);
        assert!(outline.blocks[0].starts_with("> ##"));
        assert!(outline.blocks[0].contains("[ref]: https://example.com"));
        assert!(outline.blocks[1].contains("[img]: images/test.png"));
    }
}
