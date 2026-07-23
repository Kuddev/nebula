//! Pointer and touch state kept by the application event processor.

use std::cmp::min;
use std::collections::HashSet;
use std::time::Instant;

use ahash::RandomState;
use winit::event::{ElementState, MouseButton, Touch as TouchEvent};

use nebula_terminal::grid::Dimensions;
use nebula_terminal::index::{Column, Point, Side};
use nebula_terminal::term;

use crate::display::SizeInfo;
use crate::input::FONT_SIZE_STEP;

const TOUCH_ZOOM_FACTOR: f32 = 0.01;

#[derive(Default, Debug)]
pub enum TouchPurpose {
    #[default]
    None,
    Select(TouchEvent),
    Scroll(TouchEvent),
    Zoom(TouchZoom),
    ZoomPendingSlot(TouchEvent),
    Tap(TouchEvent),
    Invalid(HashSet<u64, RandomState>),
}

#[derive(Debug)]
pub struct TouchZoom {
    slots: (TouchEvent, TouchEvent),
    fractions: f32,
}

impl TouchZoom {
    pub fn new(slots: (TouchEvent, TouchEvent)) -> Self {
        Self { slots, fractions: 0.0 }
    }

    pub fn font_delta(&mut self, slot: TouchEvent) -> f32 {
        let old_distance = self.distance();
        if slot.id == self.slots.0.id {
            self.slots.0 = slot;
        } else {
            self.slots.1 = slot;
        }
        let delta = (self.distance() - old_distance) * TOUCH_ZOOM_FACTOR + self.fractions;
        let font_delta = (delta.abs() / FONT_SIZE_STEP).floor() * FONT_SIZE_STEP * delta.signum();
        self.fractions = delta - font_delta;
        font_delta
    }

    pub fn slots(&self) -> (TouchEvent, TouchEvent) {
        self.slots
    }

    fn distance(&self) -> f32 {
        let delta_x = self.slots.0.location.x - self.slots.1.location.x;
        let delta_y = self.slots.0.location.y - self.slots.1.location.y;
        delta_x.hypot(delta_y) as f32
    }
}

#[derive(Debug)]
pub struct Mouse {
    pub left_button_state: ElementState,
    pub middle_button_state: ElementState,
    pub right_button_state: ElementState,
    pub last_click_timestamp: Instant,
    pub last_click_button: MouseButton,
    pub last_click_pos: (usize, usize),
    pub click_state: ClickState,
    pub accumulated_scroll: AccumulatedScroll,
    pub cell_side: Side,
    pub block_hint_launcher: bool,
    pub hint_highlight_dirty: bool,
    pub inside_text_area: bool,
    pub drag_origin: Option<(usize, usize)>,
    pub drag_active: bool,
    pub pending_selection: Option<(nebula_terminal::selection::SelectionType, Point, Side)>,
    pub debug_press_id: u64,
    pub debug_selection_updates: u32,
    pub debug_tab_drag_logged: bool,
    pub x: usize,
    pub y: usize,
}

impl Default for Mouse {
    fn default() -> Self {
        Self {
            left_button_state: ElementState::Released,
            middle_button_state: ElementState::Released,
            right_button_state: ElementState::Released,
            last_click_timestamp: Instant::now(),
            last_click_button: MouseButton::Left,
            last_click_pos: (0, 0),
            click_state: ClickState::None,
            accumulated_scroll: AccumulatedScroll::default(),
            cell_side: Side::Left,
            block_hint_launcher: false,
            hint_highlight_dirty: false,
            inside_text_area: false,
            drag_origin: None,
            drag_active: false,
            pending_selection: None,
            debug_press_id: 0,
            debug_selection_updates: 0,
            debug_tab_drag_logged: false,
            x: 0,
            y: 0,
        }
    }
}

impl Mouse {
    #[inline]
    pub fn point(&self, size: &SizeInfo, display_offset: usize) -> Point {
        let col = self.x.saturating_sub(size.padding_x() as usize) / size.cell_width() as usize;
        let col = min(Column(col), size.last_column());
        let line = self.y.saturating_sub(size.padding_y() as usize) / size.cell_height() as usize;
        let line = min(line, size.bottommost_line().0 as usize);
        term::viewport_to_point(display_offset, Point::new(line, col))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClickState {
    None,
    Click,
    DoubleClick,
    TripleClick,
}

#[derive(Default, Debug)]
pub struct AccumulatedScroll {
    pub x: f64,
    pub y: f64,
}
