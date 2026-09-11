//! The same parsed heading index drives source navigation and preview blocks.
//! Split only at root block boundaries; fenced code, lists and quotes remain intact.

use markdown::mdast::Node;
use std::collections::{HashMap, HashSet};

const MAX_PREVIEW_BYTES: usize = 512 * 1024;
const MAX_BLOCK_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Heading {
    pub(super) label: String,
    pub(super) depth: u8,
    pub(super) row: u32,
    pub(super) block: usize,
    pub(super) number: String,
    pub(super) parent: Option<usize>,
    pub(super) indent: usize,
    text_offset: Option<usize>,
}

impl Heading {
    fn prefix(&self) -> String {
        let token = self.label.split_whitespace().next().unwrap_or_default();
        let numeric = token.trim_end_matches(['.', '、']);
        let authored = (token.contains('.') || token.ends_with('、'))
            && numeric
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        if authored { String::new() } else { format!("{} ", self.number) }
    }

    pub(super) fn display_label(&self) -> String {
        format!("{}{}", self.prefix(), self.label)
    }
}

#[derive(Default)]
pub(super) struct Outline {
    pub(super) headings: Vec<Heading>,
    pub(super) blocks: Vec<String>,
    pub(super) limited: bool,
    definitions: HashMap<String, (String, Vec<String>)>,
    references: Vec<Vec<String>>,
    heading_ranges: Vec<std::ops::Range<usize>>,
}

impl Outline {
    fn block_headings(&self, block: usize) -> &[Heading] {
        self.heading_ranges.get(block).map_or(&[], |range| &self.headings[range.clone()])
    }

    pub(super) fn block_source(&self, index: usize) -> String {
        let Some(block) = self.blocks.get(index) else { return String::new() };
        let Some(definitions) = self.reference_sources(index) else {
            return literal_preview(block);
        };
        let mut text = block.clone();
        for heading in self.block_headings(index).iter().rev() {
            if let Some(offset) =
                heading.text_offset.filter(|offset| text.is_char_boundary(*offset))
            {
                text.insert_str(offset, &heading.prefix());
            }
        }
        for definition in definitions {
            text.push_str("\n\n");
            text.push_str(definition);
        }
        text
    }

    fn reference_sources(&self, index: usize) -> Option<Vec<&str>> {
        let mut bytes = self.blocks.get(index)?.len();
        bytes += self
            .block_headings(index)
            .iter()
            .filter(|h| h.text_offset.is_some())
            .map(|h| h.prefix().len())
            .sum::<usize>();
        if bytes > MAX_BLOCK_BYTES {
            return None;
        }
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut pending: Vec<&str> =
            self.references.get(index).into_iter().flatten().map(String::as_str).collect();
        while let Some(reference) = pending.pop() {
            if !visited.insert(reference) {
                continue;
            }
            if let Some((definition, references)) = self.definitions.get(reference) {
                bytes = bytes.saturating_add(definition.len() + 2);
                if bytes > MAX_BLOCK_BYTES {
                    return None;
                }
                result.push(definition.as_str());
                pending.extend(references.iter().map(String::as_str));
            }
        }
        Some(result)
    }

    pub(super) fn parse(source: &str) -> Self {
        let limited = source.len() > MAX_PREVIEW_BYTES;
        let source = &source[..source.floor_char_boundary(MAX_PREVIEW_BYTES.min(source.len()))];
        let mut options = markdown::ParseOptions::gfm();
        options.constructs.math_flow = true;
        options.constructs.math_text = true;
        let Ok(Node::Root(root)) = markdown::to_mdast(source, &options) else {
            return Self {
                headings: vec![],
                blocks: vec![source.to_owned()],
                limited,
                ..Self::default()
            };
        };
        // Resolve reference links/images from any part of the original document.
        let definitions = root
            .children
            .iter()
            .filter_map(|node| {
                if limited && node.position().is_some_and(|p| p.end.offset == source.len()) {
                    return None;
                }
                let key = match node {
                    Node::Definition(definition) => {
                        format!("link:{}", identifier(&definition.identifier))
                    },
                    Node::FootnoteDefinition(definition) => {
                        format!("note:{}", identifier(&definition.identifier))
                    },
                    _ => return None,
                };
                let position = node.position()?;
                let text = source.get(position.start.offset..position.end.offset)?.to_owned();
                let mut references = Vec::new();
                collect_references(node, &mut references);
                Some((key, (text, references)))
            })
            .fold(HashMap::new(), |mut definitions, (key, value)| {
                definitions.entry(key).or_insert(value);
                definitions
            });
        let mut result = Self { definitions, limited, ..Self::default() };
        for node in &root.children {
            if matches!(node, Node::Definition(_) | Node::FootnoteDefinition(_)) {
                continue;
            }
            let Some(position) = node.position() else { continue };
            let Some(text) = source.get(position.start.offset..position.end.offset) else {
                continue;
            };
            let heading_start = result.headings.len();
            collect_headings(
                node,
                result.blocks.len(),
                position.start.offset,
                &mut result.headings,
            );
            result.heading_ranges.push(heading_start..result.headings.len());
            let mut references = Vec::new();
            collect_references(node, &mut references);
            result.references.push(references);
            let unfinished =
                limited && node.position().is_some_and(|p| p.end.offset == source.len());
            if unfinished || text.len() > MAX_BLOCK_BYTES {
                result.limited = true;
                result.references.last_mut().unwrap().clear();
                result.blocks.push(literal_preview(text));
            } else {
                result.blocks.push(text.to_owned());
            }
        }
        number_headings(&mut result.headings);
        let references_limited =
            (0..result.blocks.len()).any(|index| result.reference_sources(index).is_none());
        result.limited |= references_limited;
        result
    }

    pub(super) fn visible_headings<'a>(
        &'a self,
        collapsed: &'a HashSet<usize>,
    ) -> impl Iterator<Item = (usize, &'a Heading)> + 'a {
        let mut hidden_below = None;
        self.headings.iter().enumerate().filter(move |(index, heading)| {
            if hidden_below.is_some_and(|depth| heading.depth > depth) {
                return false;
            }
            hidden_below = collapsed.contains(index).then_some(heading.depth);
            true
        })
    }

    pub(super) fn has_children(&self, index: usize) -> bool {
        self.headings.get(index + 1).is_some_and(|next| next.parent == Some(index))
    }

    /// Map code-action offsets from numbered presentation back to its raw block.
    pub(super) fn source_span(
        &self,
        block: usize,
        start: usize,
        end: usize,
    ) -> Option<(usize, usize)> {
        let map = |offset: usize| {
            let mut added = 0;
            for heading in self.block_headings(block) {
                let Some(at) = heading.text_offset else { continue };
                let length = heading.prefix().len();
                if offset < at + added {
                    break;
                }
                if offset < at + added + length {
                    return None;
                }
                added += length;
            }
            offset.checked_sub(added)
        };
        Some((map(start)?, map(end)?))
    }

    pub(super) fn replace_block(&mut self, block: usize, next: String, changed_end: usize) {
        let Some(source) = self.blocks.get_mut(block) else { return };
        let delta = next.len() as isize - source.len() as isize;
        *source = next;
        if let Some(range) = self.heading_ranges.get(block) {
            for heading in &mut self.headings[range.clone()] {
                if let Some(offset) = heading.text_offset.filter(|offset| *offset >= changed_end) {
                    heading.text_offset = offset.checked_add_signed(delta);
                }
            }
        }
    }
}

fn number_headings(headings: &mut [Heading]) {
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut roots = 0;
    for index in 0..headings.len() {
        while stack
            .last()
            .is_some_and(|(parent, _)| headings[*parent].depth >= headings[index].depth)
        {
            stack.pop();
        }
        let (parent, number) = if let Some((parent, children)) = stack.last_mut() {
            *children += 1;
            (Some(*parent), format!("{}.{}", headings[*parent].number, children))
        } else {
            roots += 1;
            (None, roots.to_string())
        };
        headings[index].parent = parent;
        headings[index].indent = stack.len();
        headings[index].number = number;
        stack.push((index, 0));
    }
}

fn literal_preview(source: &str) -> String {
    let end = source.floor_char_boundary(MAX_BLOCK_BYTES.min(source.len()));
    let source = &source[..end];
    let fence =
        "`".repeat(source.split(|ch| ch != '`').map(str::len).max().unwrap_or(0).max(2) + 1);
    format!("{fence}text\n{source}\n{fence}")
}

fn identifier(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

fn collect_references(node: &Node, output: &mut Vec<String>) {
    match node {
        Node::LinkReference(reference) => {
            output.push(format!("link:{}", identifier(&reference.identifier)))
        },
        Node::ImageReference(reference) => {
            output.push(format!("link:{}", identifier(&reference.identifier)))
        },
        Node::FootnoteReference(reference) => {
            output.push(format!("note:{}", identifier(&reference.identifier)))
        },
        _ => {},
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_references(child, output);
        }
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

fn collect_headings(node: &Node, block: usize, block_start: usize, headings: &mut Vec<Heading>) {
    if let Node::Heading(heading) = node {
        let mut text = String::new();
        label(node, &mut text);
        headings.push(Heading {
            label: text,
            depth: heading.depth,
            row: heading.position.as_ref().map_or(0, |p| p.start.line.saturating_sub(1) as u32),
            block,
            number: String::new(),
            parent: None,
            indent: 0,
            text_offset: heading
                .children
                .first()
                .and_then(Node::position)
                .and_then(|position| position.start.offset.checked_sub(block_start)),
        });
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_headings(child, block, block_start, headings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering_and_nested_folds_share_one_tree_without_rewriting_source() {
        let source = "# First\n\n## Sub\n\n### Deep\n\n## Other\n\n# Next\n";
        let outline = Outline::parse(source);
        assert_eq!(
            outline.headings.iter().map(|h| h.number.as_str()).collect::<Vec<_>>(),
            ["1", "1.1", "1.1.1", "1.2", "2"]
        );
        assert_eq!(outline.blocks[0], "# First");
        assert_eq!(outline.block_source(0), "# 1 First");
        assert_eq!(outline.headings[1].display_label(), "1.1 Sub");
        let mut collapsed = HashSet::from([0, 1]);
        assert_eq!(
            outline.visible_headings(&collapsed).map(|(i, _)| i).collect::<Vec<_>>(),
            [0, 4]
        );
        collapsed.remove(&0);
        assert_eq!(
            outline.visible_headings(&collapsed).map(|(i, _)| i).collect::<Vec<_>>(),
            [0, 1, 3, 4]
        );
        assert!(outline.has_children(1));
        assert!(!outline.has_children(2));
    }

    #[test]
    fn numbering_preserves_setext_and_inline_markup_and_maps_action_offsets() {
        let outline = Outline::parse(
            "Title\n===\n\n## **Bold**\n\n> ### Quote\n>\n> ```rust\n> old\n> ```\n",
        );
        assert_eq!(outline.block_source(0), "1 Title\n===");
        assert_eq!(outline.block_source(1), "## 1.1 **Bold**");
        let shown = outline.block_source(2);
        assert!(shown.starts_with("> ### 1.1.1 Quote"));
        let displayed = shown.find("```rust").unwrap();
        let raw = outline.blocks[2].find("```rust").unwrap();
        assert_eq!(outline.source_span(2, displayed, displayed + 7), Some((raw, raw + 7)));
        assert_eq!(Outline::parse("# 1. Existing").block_source(0), "# 1. Existing");
    }

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
        assert!(outline.block_source(0).contains("[ref]: https://example.com"));
        assert!(outline.block_source(1).contains("[img]: images/test.png"));
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    #[test]
    fn unrelated_reference_definitions_are_not_copied_into_every_block() {
        let mut source = (0..500).map(|id| format!("paragraph {id}\n\n")).collect::<String>();
        source.push_str("![selected][img-499]\n\n");
        for id in 0..500 {
            source.push_str(&format!("[img-{id}]: local-{id}.png\n"));
        }
        let outline = Outline::parse(&source);
        assert!(outline.block_source(0).len() < 30);
        let referenced = outline.block_source(outline.blocks.len() - 1);
        assert!(referenced.contains("local-499.png"));
        assert!(!referenced.contains("local-0.png"));
        assert!(outline.blocks.iter().map(String::len).sum::<usize>() < source.len());
    }
}

#[cfg(test)]
mod preview_budget_tests {
    use super::*;
    #[test]
    fn large_document_is_bounded_before_ast_construction_and_partial_math_is_literal() {
        let source = "paragraph\n\n".repeat(MAX_PREVIEW_BYTES / 11 + 100);
        let outline = Outline::parse(&source);
        assert!(outline.limited);
        assert!(outline.blocks.iter().map(String::len).sum::<usize>() <= MAX_PREVIEW_BYTES + 128);
        let source = format!("$$\n{}\n$$", "x+".repeat(MAX_BLOCK_BYTES));
        let outline = Outline::parse(&source);
        assert!(outline.limited);
        assert!(outline.blocks[0].starts_with("```text"));
    }

    #[test]
    fn first_reference_definition_keeps_markdown_precedence() {
        let outline = Outline::parse("[label][ref]\n\n[ref]: first.png\n[ref]: second.png\n");
        assert!(outline.block_source(0).contains("first.png"));
        assert!(!outline.block_source(0).contains("second.png"));
    }
}

#[cfg(test)]
mod reference_budget_tests {
    use super::*;
    #[test]
    fn transitive_footnotes_cannot_bypass_the_preview_block_budget() {
        let mut source = String::from("body[^n0]\n\n");
        for index in 0..100 {
            source.push_str(&format!(
                "[^n{index}]: {} [^n{}]\n\n",
                "x".repeat(512),
                (index + 1) % 100
            ));
        }
        let outline = Outline::parse(&source);
        assert!(outline.limited);
        let preview = outline.block_source(0);
        assert!(preview.starts_with("```text"));
        assert!(preview.len() < 128);
    }
}
