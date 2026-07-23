//! Incremental terminal damage tracking.

use std::cmp;
use std::slice;

use crate::index::Point;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineDamageBounds {
    pub line: usize,
    pub left: usize,
    pub right: usize,
}

impl LineDamageBounds {
    #[inline]
    pub fn new(line: usize, left: usize, right: usize) -> Self {
        Self { line, left, right }
    }

    #[inline]
    pub fn undamaged(line: usize, num_cols: usize) -> Self {
        Self { line, left: num_cols, right: 0 }
    }

    #[inline]
    pub fn reset(&mut self, num_cols: usize) {
        *self = Self::undamaged(self.line, num_cols);
    }

    #[inline]
    pub fn expand(&mut self, left: usize, right: usize) {
        self.left = cmp::min(self.left, left);
        self.right = cmp::max(self.right, right);
    }

    #[inline]
    pub fn is_damaged(&self) -> bool {
        self.left <= self.right
    }
}

/// Terminal damage information collected since the last reset.
#[derive(Debug)]
pub enum TermDamage<'a> {
    Full,
    Partial(TermDamageIterator<'a>),
}

/// Iterator over damaged lines that are currently visible in the viewport.
#[derive(Clone, Debug)]
pub struct TermDamageIterator<'a> {
    line_damage: slice::Iter<'a, LineDamageBounds>,
    display_offset: usize,
}

impl<'a> TermDamageIterator<'a> {
    pub fn new(line_damage: &'a [LineDamageBounds], display_offset: usize) -> Self {
        let num_lines = line_damage.len();
        let line_damage = &line_damage[..num_lines.saturating_sub(display_offset)];
        Self { display_offset, line_damage: line_damage.iter() }
    }
}

impl Iterator for TermDamageIterator<'_> {
    type Item = LineDamageBounds;

    fn next(&mut self) -> Option<Self::Item> {
        self.line_damage.find_map(|line| {
            line.is_damaged().then_some(LineDamageBounds::new(
                line.line + self.display_offset,
                line.left,
                line.right,
            ))
        })
    }
}

/// Mutable damage bookkeeping owned by a terminal.
pub(super) struct TermDamageState {
    pub(super) full: bool,
    pub(super) lines: Vec<LineDamageBounds>,
    pub(super) last_cursor: Point,
}

impl TermDamageState {
    pub(super) fn new(num_cols: usize, num_lines: usize) -> Self {
        let lines =
            (0..num_lines).map(|line| LineDamageBounds::undamaged(line, num_cols)).collect();
        Self { full: true, lines, last_cursor: Default::default() }
    }

    #[inline]
    pub(super) fn resize(&mut self, num_cols: usize, num_lines: usize) {
        self.last_cursor = Default::default();
        self.full = true;
        self.lines.clear();
        self.lines.reserve(num_lines);
        for line in 0..num_lines {
            self.lines.push(LineDamageBounds::undamaged(line, num_cols));
        }
    }

    #[inline]
    pub(super) fn damage_point(&mut self, point: Point<usize>) {
        self.damage_line(point.line, point.column.0, point.column.0);
    }

    #[inline]
    pub(super) fn damage_line(&mut self, line: usize, left: usize, right: usize) {
        self.lines[line].expand(left, right);
    }

    pub(super) fn reset(&mut self, num_cols: usize) {
        self.full = false;
        self.lines.iter_mut().for_each(|line| line.reset(num_cols));
    }
}
