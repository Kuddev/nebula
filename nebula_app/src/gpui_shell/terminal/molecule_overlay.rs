//! SMILES blocks use the same depiction as Markdown, within their source rows.
//! The terminal grid remains the copy/selection authority.

use super::view::TerminalView;
use gpui::{App, Bounds, Corners, Entity, Pixels, Window, fill, point, px, size};
use nebula_terminal::{render::RenderSnapshot, term::TermMode};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::Arc,
};

#[derive(Default)]
pub(super) struct MoleculeOverlay {
    fingerprint: Option<u64>,
    blocks: Arc<Vec<Block>>,
}

impl MoleculeOverlay {
    fn scan(&mut self, snapshot: &RenderSnapshot) -> Arc<Vec<Block>> {
        let mut hash = DefaultHasher::new();
        snapshot.cols.hash(&mut hash);
        snapshot.rows.hash(&mut hash);
        for segment in &snapshot.segments {
            segment.row.hash(&mut hash);
            for cell in &segment.cells {
                cell.col.hash(&mut hash);
                cell.text.hash(&mut hash);
            }
        }
        let fingerprint = hash.finish();
        if self.fingerprint == Some(fingerprint) {
            return self.blocks.clone();
        }
        let mut grid = vec![vec![' '; snapshot.cols as usize]; snapshot.rows as usize];
        for segment in &snapshot.segments {
            if let Some(row) = grid.get_mut(segment.row as usize) {
                for cell in &segment.cells {
                    if let Some(slot) = row.get_mut(cell.col as usize) {
                        *slot = cell.text.chars().next().unwrap_or(' ');
                    }
                }
            }
        }
        let lines: Vec<String> = grid.into_iter().map(|row| row.into_iter().collect()).collect();
        self.fingerprint = Some(fingerprint);
        self.blocks = Arc::new(blocks(&lines));
        self.blocks.clone()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Block {
    rows: Range<usize>,
    column: usize,
    source: String,
}

fn blocks(lines: &[String]) -> Vec<Block> {
    let mut result = Vec::new();
    let mut row = 0;
    while row < lines.len() && result.len() < 8 {
        let title = lines[row].trim();
        // Several AI CLIs display the fence language as a heading and omit
        // Markdown delimiters. Only an explicit SMILES label enables this path.
        let labeled = if title.eq_ignore_ascii_case("smiles") {
            lines.get(row + 1).map(|line| (line.trim(), row + 2))
        } else {
            title
                .split_once(':')
                .filter(|(label, _)| label.eq_ignore_ascii_case("smiles"))
                .map(|(_, source)| (source.trim(), row + 1))
        };
        if let Some((source, mut end)) = labeled {
            while end < lines.len() && end - row < 6 && lines[end].trim().is_empty() {
                end += 1;
            }
            if !source.is_empty() && source.len() <= crate::chemistry::MAX_SOURCE_BYTES {
                result.push(Block {
                    rows: row..end,
                    column: lines[row].len() - lines[row].trim_start().len(),
                    source: source.to_owned(),
                });
            }
            row = end;
            continue;
        }
        let marker = if title.eq_ignore_ascii_case("```smiles") {
            "```"
        } else if title.eq_ignore_ascii_case("~~~smiles") {
            "~~~"
        } else {
            row += 1;
            continue;
        };
        let start = row;
        row += 1;
        let mut source = None;
        let mut valid = true;
        while row < lines.len() && lines[row].trim() != marker {
            let line = lines[row].trim();
            if !line.is_empty() {
                if source.is_some() {
                    valid = false;
                }
                source = Some(line.to_owned());
            }
            if row - start > 12 {
                valid = false;
                break;
            }
            row += 1;
        }
        if valid && row < lines.len() && lines[row].trim() == marker {
            if let Some(source) =
                source.filter(|source| source.len() <= crate::chemistry::MAX_SOURCE_BYTES)
            {
                result.push(Block {
                    rows: start..row + 1,
                    column: lines[start].len() - lines[start].trim_start().len(),
                    source,
                });
            }
        }
        row += 1;
    }
    result
}

pub(super) fn paint(
    view: &Entity<TerminalView>,
    snapshot: &RenderSnapshot,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    dark: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if !crate::chemistry::STRUCTURE_RENDERING_ENABLED {
        // Leave source cells intact and avoid scanning or scheduling depiction.
        return;
    }
    if !snapshot.selection_runs.is_empty()
        || view
            .read(cx)
            .session
            .as_ref()
            .is_some_and(|session| session.term.lock().mode().contains(TermMode::VI))
    {
        return;
    }
    if !snapshot.segments.iter().any(|segment| {
        segment.cells.iter().any(|cell| matches!(cell.text.as_str(), "`" | "~"))
            || segment.cells.windows(6).any(|cells| {
                cells
                    .iter()
                    .zip(["s", "m", "i", "l", "e", "s"])
                    .all(|(cell, letter)| cell.text.eq_ignore_ascii_case(letter))
            })
    }) {
        return;
    }
    let blocks = view.update(cx, |view, _| view.math.molecules.scan(snapshot));
    let resources = super::super::scientific_render::assets(cx);
    for block in blocks.iter() {
        if snapshot
            .cursor
            .as_ref()
            .is_some_and(|cursor| block.rows.contains(&(cursor.row as usize)))
        {
            continue;
        }
        let Some(Some(image)) = resources.molecule(block.source.clone().into(), dark) else {
            continue;
        };
        let x = cell_width * block.column as f32;
        let available = bounds.size.width - x;
        let height = line_height * block.rows.len() as f32;
        if available < px(64.0) || height < px(40.0) {
            continue;
        }
        let area = Bounds::new(
            point(bounds.origin.x + x, bounds.origin.y + line_height * block.rows.start as f32),
            size(available, height),
        );
        let target_width = available.min(height * 2.0);
        let target_height = target_width / 2.0;
        let target = Bounds::new(
            point(area.origin.x, area.origin.y + (height - target_height) / 2.0),
            size(target_width, target_height),
        );
        window.paint_quad(fill(area, view.read(cx).palette.background));
        let _ = window.paint_image(target, target, Corners::all(px(0.0)), image, 0, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendered_cli_labels_use_only_source_and_available_blank_rows() {
        let lines = ["smiles", "c1ccccc1", "", "next paragraph"].map(str::to_owned);
        assert_eq!(
            blocks(&lines),
            vec![Block { rows: 0..3, column: 0, source: "c1ccccc1".into() }]
        );
        let lines = ["SMILES: CCO", "", "prompt"].map(str::to_owned);
        assert_eq!(blocks(&lines), vec![Block { rows: 0..2, column: 0, source: "CCO".into() }]);
        assert!(blocks(&["The word smiles is ordinary prose.".into()]).is_empty());
    }
    #[test]
    fn only_complete_explicit_blocks_are_replaced() {
        let lines =
            ["ordinary CCO", "  ```SMILES", "  c1ccccc1", "  ```", "after"].map(str::to_owned);
        assert_eq!(
            blocks(&lines),
            vec![Block { rows: 1..4, column: 2, source: "c1ccccc1".into() }]
        );
        assert!(blocks(&lines[..3]).is_empty());
        assert!(blocks(&["```smiles".into(), "CCO".into(), "CC".into(), "```".into()]).is_empty());
    }
}
