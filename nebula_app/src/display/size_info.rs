//! Terminal viewport geometry and PTY size conversion.

use std::cmp;

use serde::{Deserialize, Serialize};

use nebula_terminal::event::WindowSize;
use nebula_terminal::grid::Dimensions;
use nebula_terminal::term::{MIN_COLUMNS, MIN_SCREEN_LINES};

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct SizeInfo<T = f32> {
    pub(super) width: T,
    pub(super) height: T,
    pub(super) cell_width: T,
    pub(super) cell_height: T,
    /// Grid origin on the X axis. A left sidebar is represented here rather
    /// than as a renderer offset so cursor, IME, mouse, and damage agree.
    pub(super) padding_x: T,
    pub(super) padding_right: T,
    pub(super) padding_y: T,
    pub(super) padding_bottom: T,
    pub(super) screen_lines: usize,
    pub(super) columns: usize,
}

impl From<SizeInfo<f32>> for SizeInfo<u32> {
    fn from(size_info: SizeInfo<f32>) -> Self {
        Self {
            width: size_info.width as u32,
            height: size_info.height as u32,
            cell_width: size_info.cell_width as u32,
            cell_height: size_info.cell_height as u32,
            padding_x: size_info.padding_x as u32,
            padding_right: size_info.padding_right as u32,
            padding_y: size_info.padding_y as u32,
            padding_bottom: size_info.padding_bottom as u32,
            screen_lines: size_info.screen_lines,
            columns: size_info.columns,
        }
    }
}

impl From<SizeInfo<f32>> for WindowSize {
    fn from(size_info: SizeInfo<f32>) -> Self {
        Self {
            num_cols: size_info.columns() as u16,
            num_lines: size_info.screen_lines() as u16,
            cell_width: size_info.cell_width() as u16,
            cell_height: size_info.cell_height() as u16,
        }
    }
}

impl<T: Clone + Copy> SizeInfo<T> {
    #[inline]
    pub fn width(&self) -> T {
        self.width
    }

    #[inline]
    pub fn height(&self) -> T {
        self.height
    }

    #[inline]
    pub fn cell_width(&self) -> T {
        self.cell_width
    }

    #[inline]
    pub fn cell_height(&self) -> T {
        self.cell_height
    }

    #[inline]
    pub fn padding_x(&self) -> T {
        self.padding_x
    }

    #[inline]
    pub fn padding_right(&self) -> T {
        self.padding_right
    }

    #[inline]
    pub fn padding_y(&self) -> T {
        self.padding_y
    }

    #[inline]
    pub fn padding_bottom(&self) -> T {
        self.padding_bottom
    }
}

impl SizeInfo<f32> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        mut padding_x: f32,
        mut padding_y: f32,
        dynamic_padding: bool,
    ) -> SizeInfo {
        let padding_right = padding_x;
        if dynamic_padding {
            padding_x = Self::dynamic_padding(padding_x.floor(), width, cell_width);
            padding_y = Self::dynamic_padding(padding_y.floor(), height, cell_height);
        }

        Self::assemble(
            width,
            height,
            cell_width,
            cell_height,
            padding_x,
            padding_right,
            padding_y,
            padding_y,
        )
    }

    /// Build a grid whose left and right padding differ.
    #[allow(clippy::too_many_arguments)]
    pub fn new_asymmetric(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        padding_x: f32,
        padding_right: f32,
        padding_y: f32,
    ) -> SizeInfo {
        Self::assemble(
            width,
            height,
            cell_width,
            cell_height,
            padding_x,
            padding_right,
            padding_y,
            padding_y,
        )
    }

    /// Build a grid with independent padding on all four sides.
    #[allow(clippy::too_many_arguments)]
    pub fn new_fully_asymmetric(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        padding_x: f32,
        padding_right: f32,
        padding_y: f32,
        padding_bottom: f32,
    ) -> SizeInfo {
        Self::assemble(
            width,
            height,
            cell_width,
            cell_height,
            padding_x,
            padding_right,
            padding_y,
            padding_bottom,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        padding_x: f32,
        padding_right: f32,
        padding_y: f32,
        padding_bottom: f32,
    ) -> SizeInfo {
        let lines = (height - padding_y - padding_bottom) / cell_height;
        let screen_lines = cmp::max(lines as usize, MIN_SCREEN_LINES);
        let columns = (width - padding_x - padding_right) / cell_width;
        let columns = cmp::max(columns as usize, MIN_COLUMNS);

        SizeInfo {
            width,
            height,
            cell_width,
            cell_height,
            padding_x: padding_x.floor(),
            padding_right: padding_right.floor(),
            padding_y: padding_y.floor(),
            padding_bottom: padding_bottom.floor(),
            screen_lines,
            columns,
        }
    }

    #[inline]
    pub fn reserve_lines(&mut self, count: usize) {
        self.screen_lines = cmp::max(self.screen_lines.saturating_sub(count), MIN_SCREEN_LINES);
    }

    /// Check whether physical coordinates fall inside the terminal grid.
    #[inline]
    pub fn contains_point(&self, x: usize, y: usize) -> bool {
        x <= (self.padding_x + self.columns as f32 * self.cell_width) as usize
            && x > self.padding_x as usize
            && y <= (self.padding_y + self.screen_lines as f32 * self.cell_height) as usize
            && y > self.padding_y as usize
    }

    #[inline]
    fn dynamic_padding(padding: f32, dimension: f32, cell_dimension: f32) -> f32 {
        padding + ((dimension - 2. * padding) % cell_dimension) / 2.
    }
}

impl Dimensions for SizeInfo {
    #[inline]
    fn columns(&self) -> usize {
        self.columns
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
}
