//! Mouse movement, terminal reporting, selection, wheel, and cursor handling.

use std::cmp::{Ordering, max, min};
use std::time::Duration;

use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase};
use winit::keyboard::ModifiersState;
use winit::window::CursorIcon;

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::index::{Column, Line, Point, Side};
use nebula_terminal::selection::SelectionType;
use nebula_terminal::term::{ClipboardType, TermMode};

use crate::config::{BindingMode, MouseEvent};
use crate::display::hint::HintMatch;
use crate::event::{ClickState, Event, EventType};
use crate::message_bar;
use crate::scheduler::{TimerId, Topic};

use super::{ActionContext, Execute, Processor};

/// Interval for mouse scrolling during selection outside of the boundaries.
const SELECTION_SCROLLING_INTERVAL: Duration = Duration::from_millis(15);

/// Minimum number of pixels at the bottom/top where selection scrolling is performed.
const MIN_SELECTION_SCROLLING_HEIGHT: f64 = 5.;

/// Number of pixels for increasing the selection scrolling speed factor by one.
const SELECTION_SCROLLING_STEP: f64 = 20.;

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    #[inline]
    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let size_info = self.ctx.size_info();
        // Chrome (tabs / title bar / settings) is laid out on the *window*,
        // not the focused pane's viewport; in split mode the two differ.
        let window_size = self.ctx.display().ui_size_info();

        let (x, y) = position.into();

        let lmb_pressed = self.ctx.mouse().left_button_state == ElementState::Pressed;
        let rmb_pressed = self.ctx.mouse().right_button_state == ElementState::Pressed;
        if !self.ctx.selection_is_empty() && (lmb_pressed || rmb_pressed) {
            self.update_selection_scrolling(y);
        }

        let display_offset = self.ctx.terminal().grid().display_offset();
        let old_point = self.ctx.mouse().point(&size_info, display_offset);

        // Clamp to the window, not the pane: chrome to the right/bottom of a
        // split pane must stay hoverable and clickable.
        let x = x.clamp(0, window_size.width() as i32 - 1) as usize;
        let y = y.clamp(0, window_size.height() as i32 - 1) as usize;
        self.ctx.mouse_mut().x = x;
        self.ctx.mouse_mut().y = y;

        if self.ctx.display().nebula_settings_opacity_drag.is_some() {
            if lmb_pressed {
                self.ctx.display().update_settings_opacity_drag(x as f32);
            } else {
                self.ctx.display().finish_settings_opacity_drag();
            }
            self.ctx.window().set_mouse_cursor(CursorIcon::EwResize);
            self.ctx.mark_dirty();
            return;
        }

        // The SSH editor is modal. Its controls own hover/cursor feedback while
        // open, and the closing animation swallows pointer motion until the
        // retained editor snapshot is released.
        if self.ctx.display().nebula_ssh_editor.is_some() {
            self.ctx.display().set_ssh_delete_undo_hover(false);
            let hit = if self.ctx.display().ssh_editor_active() {
                self.ctx.display().ssh_editor_hit(x as f32, y as f32)
            } else {
                crate::display::SshEditorHit::None
            };
            self.ctx.display().set_ssh_editor_hover(hit);
            let cursor = match hit {
                crate::display::SshEditorHit::Destination
                | crate::display::SshEditorHit::Password => CursorIcon::Text,
                crate::display::SshEditorHit::SaveToggleBox
                | crate::display::SshEditorHit::SaveToggleLabel
                | crate::display::SshEditorHit::PasswordToggle
                | crate::display::SshEditorHit::Auth(_)
                | crate::display::SshEditorHit::AddPrivateKey
                | crate::display::SshEditorHit::RemovePrivateKey(_)
                | crate::display::SshEditorHit::Primary
                | crate::display::SshEditorHit::Cancel => CursorIcon::Pointer,
                crate::display::SshEditorHit::None => CursorIcon::Default,
            };
            self.ctx.window().set_mouse_cursor(cursor);
            return;
        }

        // Nebula: while the left button holds a grabbed tab, pointer motion
        // drives the reorder drag instead of hover / text selection.
        if lmb_pressed && self.ctx.display().tab_drag_armed() {
            let active = self.ctx.display().update_tab_drag(x as f32, y as f32);
            if active && !self.ctx.mouse().debug_tab_drag_logged {
                crate::display::nebula_debug_log(format!(
                    "pointer_tab_drag_active id={} xy=({x},{y})",
                    self.ctx.mouse().debug_press_id
                ));
                self.ctx.mouse_mut().debug_tab_drag_logged = true;
            }
            let icon = if active { CursorIcon::Grabbing } else { CursorIcon::Pointer };
            self.ctx.window().set_mouse_cursor(icon);
            if active {
                self.ctx.mark_dirty();
            }
            return;
        }

        // Nebula: while the left button holds the scrollback thumb, pointer
        // motion maps 1:1 onto the display offset (scrollbar drag).
        if let Some(grab) = self.ctx.display().nebula_scrollbar_drag {
            if lmb_pressed {
                let total_lines = self.ctx.terminal().total_lines();
                let display_offset = self.ctx.terminal().grid().display_offset();
                let target = self.ctx.display().scrollbar_target_offset(
                    &size_info,
                    total_lines,
                    y as f32,
                    grab,
                );
                let delta = target as i32 - display_offset as i32;
                if delta != 0 {
                    self.ctx.scroll(Scroll::Delta(delta));
                }
                self.ctx.mark_dirty();
                return;
            }
            // Button no longer held (release happened elsewhere): drop the drag.
            self.ctx.display().nebula_scrollbar_drag = None;
        }

        // Nebula chrome: hover state must be updated on raw pixel movement,
        // not only when the terminal cell changes; otherwise tabs/buttons feel
        // like they have no feedback on high-DPI displays.
        let scale = self.ctx.window().scale_factor as f32;
        let settings_open = self.ctx.display().settings_open();
        let settings_section = self.ctx.display().settings_section();
        let settings_scroll = self.ctx.display().settings_scroll();
        let settings_dropdown = self.ctx.display().nebula_settings_dropdown;
        let shell_picker_count = self.ctx.display().shell_picker_count();
        let font_picker_count = self.ctx.display().font_picker_count();
        let hidden_host_count = self.ctx.display().hidden_ssh_host_count();
        let settings_area = self.ctx.display().terminal_card_rect();
        // A native context menu is a pointer-modal overlay: while it is open,
        // underlying links, tabs and drawer rows must not react to hover.
        if self.ctx.display().context_menu_interactive() {
            self.ctx.display().set_ssh_delete_undo_hover(false);
            let hit = self.ctx.display().context_menu_hover(x as f32, y as f32);
            self.ctx.window().set_mouse_cursor(
                if matches!(hit, crate::display::ContextMenuHit::Action(_)) {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                },
            );
            return;
        }
        let undo_hover = self.ctx.display().nebula_confirm.is_none()
            && !self.ctx.display().command_palette_open()
            && self.ctx.display().ssh_delete_undo_hit(x as f32, y as f32);
        self.ctx.display().set_ssh_delete_undo_hover(undo_hover);
        if undo_hover {
            self.ctx.window().set_mouse_cursor(CursorIcon::Pointer);
            return;
        }
        // Command palette is a pointer-modal overlay: rows light up under the
        // cursor and NOTHING below (settings rows, chrome, links) may react
        // while it is open. This must run BEFORE the settings/chrome hover
        // dispatch — those branches `return`, which is exactly how palette
        // row hover silently died before.
        if self.ctx.display().command_palette_open() {
            let px = x as f32;
            let py = y as f32;
            let layout = crate::display::command_palette::palette_layout(
                window_size.width(),
                window_size.height(),
                scale,
            );
            let (ix, iy, iw, ih) = layout.panel;
            // Hover detection: the pointer must be inside the panel rectangle,
            // AND below the input box (list_y), AND the computed row must be a
            // real item (< visible count). Without the visible-count check, rows
            // beyond the filtered list — empty space inside the panel — also hover.
            let hover_row = if px >= ix && px < ix + iw && py >= layout.list_y && py < iy + ih {
                let row = ((py - layout.list_y) / layout.row_h) as usize;
                let visible_count =
                    self.ctx.display().nebula_palette_visible_count().min(layout.max_rows);
                if row < visible_count { Some(row) } else { None }
            } else {
                None
            };
            if self.ctx.display().palette_hover(hover_row) {
                self.ctx.mark_dirty();
            }
            self.ctx.window().set_mouse_cursor(if hover_row.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
            return;
        }
        let settings_hover = crate::display::settings_hit(
            &window_size,
            scale,
            settings_area,
            x as f32,
            y as f32,
            settings_open,
            settings_section,
            settings_scroll,
            settings_dropdown,
            shell_picker_count,
            font_picker_count,
            hidden_host_count,
        );
        let chrome_hover = if crate::display::in_chrome_bar(&window_size, scale, x as f32, y as f32)
        {
            self.ctx.display().chrome_hit(x as f32, y as f32)
        } else {
            crate::display::ChromeHit::None
        };
        let chrome_hover = if self.ctx.display().nebula_special_tab_active
            && matches!(
                chrome_hover,
                crate::display::ChromeHit::PanelFiles | crate::display::ChromeHit::PanelGit
            ) {
            crate::display::ChromeHit::None
        } else {
            chrome_hover
        };
        self.ctx.display().set_chrome_hover(chrome_hover, settings_hover);

        // Resize cursor on the window border, arrow over the title bar/sidebar,
        // pointer over clickable chrome controls. Chrome/resize geometry is
        // window-level, so hit-test against the full window, not the focused
        // pane's viewport — otherwise a band of the terminal area is mistaken
        // for chrome and swallows link hovers (no underline / no click).
        let resize_enabled = self.ctx.window().allows_drag_resize();
        if let Some(dir) =
            crate::display::resize_edge(&window_size, scale, x as f32, y as f32, resize_enabled)
        {
            use winit::window::ResizeDirection::*;
            let icon = match dir {
                East | West => CursorIcon::EwResize,
                North | South => CursorIcon::NsResize,
                NorthEast | SouthWest => CursorIcon::NeswResize,
                NorthWest | SouthEast => CursorIcon::NwseResize,
            };
            self.ctx.window().set_mouse_cursor(icon);
            return;
        }
        match settings_hover {
            crate::display::SettingsHit::Toggle
            | crate::display::SettingsHit::Nav(_)
            | crate::display::SettingsHit::Theme(_)
            | crate::display::SettingsHit::Language(_)
            | crate::display::SettingsHit::SystemThemeToggle
            | crate::display::SettingsHit::GhostToggle
            | crate::display::SettingsHit::AcceptCycle
            | crate::display::SettingsHit::ShellCycle
            | crate::display::SettingsHit::ShellPickerRow(_)
            | crate::display::SettingsHit::StartupDirectory
            | crate::display::SettingsHit::StartupDirectoryClear
            | crate::display::SettingsHit::FontCycle
            | crate::display::SettingsHit::FontPickerRow(_)
            | crate::display::SettingsHit::RestoreHiddenSsh(_)
            | crate::display::SettingsHit::FetchToggle
            | crate::display::SettingsHit::PowerlineToggle
            | crate::display::SettingsHit::KeepSessionToggle
            | crate::display::SettingsHit::BackgroundColor
            | crate::display::SettingsHit::BackgroundImage
            | crate::display::SettingsHit::BackgroundImageClear
            | crate::display::SettingsHit::BackgroundImageFit
            | crate::display::SettingsHit::BackgroundImageAlignment
            | crate::display::SettingsHit::FitOption(_)
            | crate::display::SettingsHit::AlignOption(_)
            | crate::display::SettingsHit::BackgroundSwatch(_)
            | crate::display::SettingsHit::BackgroundHexInput
            | crate::display::SettingsHit::AcceptOption(_)
            | crate::display::SettingsHit::LanguageDropdown
            | crate::display::SettingsHit::CursorShapeDropdown
            | crate::display::SettingsHit::CursorShapeOption(_)
            | crate::display::SettingsHit::CursorBlinkToggle
            | crate::display::SettingsHit::CopyOnSelectToggle
            | crate::display::SettingsHit::FontSizeUp
            | crate::display::SettingsHit::FontSizeDown
            | crate::display::SettingsHit::BackgroundImageCoverChrome
            | crate::display::SettingsHit::OpenConfigFile
            | crate::display::SettingsHit::Reset => {
                self.ctx.window().set_mouse_cursor(CursorIcon::Pointer);
                return;
            },
            crate::display::SettingsHit::OpacitySlider
            | crate::display::SettingsHit::BackgroundImageOpacitySlider => {
                // WT 风格滑块用普通指针：双向箭头(EwResize)让拖动读起来像
                // 在改窗口大小，是用户报告的"手势不对"。
                self.ctx.window().set_mouse_cursor(CursorIcon::Pointer);
                return;
            },
            crate::display::SettingsHit::Panel
            | crate::display::SettingsHit::BackgroundPopupPanel => {
                self.ctx.window().set_mouse_cursor(CursorIcon::Default);
                return;
            },
            crate::display::SettingsHit::None => {},
        }
        if crate::display::in_chrome_bar(&window_size, scale, x as f32, y as f32) {
            let icon = match self.ctx.display().chrome_hit(x as f32, y as f32) {
                crate::display::ChromeHit::Minimize
                | crate::display::ChromeHit::Maximize
                | crate::display::ChromeHit::Close
                | crate::display::ChromeHit::NewTab
                | crate::display::ChromeHit::NewTabMenu
                | crate::display::ChromeHit::Tab(_)
                | crate::display::ChromeHit::TabClose(_)
                | crate::display::ChromeHit::PanelFiles
                | crate::display::ChromeHit::PanelGit
                | crate::display::ChromeHit::Host(_)
                | crate::display::ChromeHit::TabsSection
                | crate::display::ChromeHit::HostsSection
                | crate::display::ChromeHit::MessageQueue
                | crate::display::ChromeHit::SidebarToggle => CursorIcon::Pointer,
                _ => CursorIcon::Default,
            };
            self.ctx.window().set_mouse_cursor(icon);
            return;
        }

        // Drawer hover: rows / header tabs / action buttons light up, and the
        // pointer picks the matching cursor. The drawer overlays the grid, so
        // while the pointer is on it nothing below may react (no link hover,
        // no beam cursor bleeding through). Skipped while the left button is
        // down — an in-progress text drag-selection sweeping across the drawer
        // must keep updating, not freeze at its edge.
        if self.ctx.display().nebula_side_panel.open
            && self.ctx.display().nebula_sftp_panel.is_some()
            && self.ctx.mouse().left_button_state != ElementState::Pressed
        {
            let hit = self.ctx.display().sftp_hit(x as f32, y as f32);
            if self.ctx.display().sftp_set_hover(hit) {
                self.ctx.mark_dirty();
            }
            if hit != crate::display::sftp_panel::SftpHit::None {
                let cursor = match hit {
                    crate::display::sftp_panel::SftpHit::Path
                    | crate::display::sftp_panel::SftpHit::Filter => CursorIcon::Text,
                    crate::display::sftp_panel::SftpHit::Close
                    | crate::display::sftp_panel::SftpHit::Row(_)
                    | crate::display::sftp_panel::SftpHit::Cancel => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                };
                self.ctx.window().set_mouse_cursor(cursor);
                return;
            }
        }
        if self.ctx.display().nebula_side_panel.open
            && self.ctx.mouse().left_button_state != ElementState::Pressed
        {
            use crate::display::side_panel::{PanelHit, PanelView, panel_interactive_hit};
            let px = x as f32;
            let py = y as f32;
            let layout = self.ctx.display().side_panel_layout();
            let view = self.ctx.display().nebula_side_panel.view;
            let custom_root = self.ctx.display().nebula_side_panel.custom_root_active();
            let has_root = self.ctx.display().nebula_side_panel.root().is_some();
            let hit = panel_interactive_hit(&layout, view, custom_root, has_root, px, py);
            let panel = &mut self.ctx.display().nebula_side_panel;
            if hit != panel.hover || (hit != PanelHit::None && panel.hover_pos != (px, py)) {
                panel.hover = hit;
                panel.hover_pos = (px, py);
                self.ctx.mark_dirty();
            }
            if hit != PanelHit::None {
                let files = self.ctx.display().nebula_side_panel.view == PanelView::Files;
                let icon = match hit {
                    PanelHit::ViewFiles
                    | PanelHit::ViewGit
                    | PanelHit::OpenDirectory
                    | PanelHit::NewTerminalHere
                    | PanelHit::FollowCurrentDirectory => CursorIcon::Pointer,
                    PanelHit::Row(row)
                        if self.ctx.display().nebula_side_panel.view == PanelView::Files
                            || self.ctx.display().nebula_side_panel.git_row_is_file(row) =>
                    {
                        CursorIcon::Pointer
                    },
                    PanelHit::Row(_) => CursorIcon::Default,
                    PanelHit::Search if files => CursorIcon::Text,
                    PanelHit::Search => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                };
                self.ctx.window().set_mouse_cursor(icon);
                return;
            }
        } else if self.ctx.display().nebula_side_panel.hover
            != crate::display::side_panel::PanelHit::None
        {
            self.ctx.display().nebula_side_panel.hover = crate::display::side_panel::PanelHit::None;
            self.ctx.mark_dirty();
        }

        // Command palette hover moved to the TOP of this handler (pointer
        // modal): earlier hover branches `return` and were silently starving
        // the palette of hover updates.

        let inside_text_area = size_info.contains_point(x, y);
        let cell_side = self.cell_side(x);

        // Activate a pending tree-entry drag once the pointer travels; while
        // active, the ghost chip follows the pointer and the copy cursor
        // shows the drop affordance.
        if let Some(drag) = self.ctx.display().nebula_side_panel.drag_file.as_mut() {
            drag.update_position((x as f32, y as f32));
            let active = drag.active;
            if active {
                self.ctx.mark_dirty();
                self.ctx.window().set_mouse_cursor(CursorIcon::Grabbing);
                return;
            }
        }

        let point = self.ctx.mouse().point(&size_info, display_offset);
        let cell_changed = old_point != point;

        // If the mouse hasn't changed cells, do nothing.
        if !cell_changed
            && self.ctx.mouse().cell_side == cell_side
            && self.ctx.mouse().inside_text_area == inside_text_area
        {
            return;
        }

        self.ctx.mouse_mut().inside_text_area = inside_text_area;
        self.ctx.mouse_mut().cell_side = cell_side;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        if (lmb_pressed || rmb_pressed)
            && (self.ctx.modifiers().state().shift_key() || !self.ctx.mouse_mode())
        {
            // Engage drag-selection only past a real drag distance: at least
            // half a cell (and never under 8px). The old 4px threshold was
            // inside ordinary click jitter, so a plain click kept leaving a
            // one-cell selection behind — Windows Terminal only selects once
            // the pointer actually travels, a click never does.
            let dragging = self.ctx.mouse().drag_active
                || self.ctx.mouse().drag_origin.is_some_and(|(ox, oy)| {
                    let scale = self.ctx.window().scale_factor as f32;
                    let tx = (8.0 * scale).max(size_info.cell_width() * 0.5) as f64;
                    let ty = (8.0 * scale).max(size_info.cell_height() * 0.5) as f64;
                    (x as f64 - ox as f64).abs() >= tx || (y as f64 - oy as f64).abs() >= ty
                });
            if dragging {
                let first = !self.ctx.mouse().drag_active;
                self.ctx.mouse_mut().drag_active = true;
                // A real drag is in progress — don't launch hints on release.
                self.ctx.mouse_mut().block_hint_launcher = true;
                // Crossing the threshold is what STARTS the selection (WT
                // model): anchor at the original press cell, not wherever the
                // pointer is by now. Double/triple clicks selected at press
                // and carry no pending entry — they just extend below.
                if first {
                    let (debug_id, drag_origin, pending) = {
                        let mouse = self.ctx.mouse();
                        (
                            mouse.debug_press_id,
                            mouse.drag_origin,
                            format!("{:?}", mouse.pending_selection),
                        )
                    };
                    let tab_drag = self.ctx.display().tab_drag_armed();
                    let selection_empty = self.ctx.selection_is_empty();
                    crate::display::nebula_debug_log(format!(
                        "pointer_drag_threshold id={debug_id} origin={drag_origin:?} xy=({x},{y}) pending={pending} tab_drag={tab_drag} selection_empty={selection_empty}",
                    ));
                    if let Some((ty, anchor, anchor_side)) =
                        self.ctx.mouse_mut().pending_selection.take()
                    {
                        self.ctx.start_selection(ty, anchor, anchor_side);
                    }
                }
                self.ctx.update_selection(point, cell_side);
            }
        } else if cell_changed
            && self.ctx.terminal().mode().intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
        {
            if lmb_pressed {
                self.mouse_report(32, ElementState::Pressed);
            } else if self.ctx.mouse().middle_button_state == ElementState::Pressed {
                self.mouse_report(33, ElementState::Pressed);
            } else if self.ctx.mouse().right_button_state == ElementState::Pressed {
                self.mouse_report(34, ElementState::Pressed);
            } else if self.ctx.terminal().mode().contains(TermMode::MOUSE_MOTION) {
                self.mouse_report(35, ElementState::Pressed);
            }
        }
    }

    /// Check which side of a cell an X coordinate lies on.
    fn cell_side(&self, x: usize) -> Side {
        let size_info = self.ctx.size_info();

        let cell_x =
            x.saturating_sub(size_info.padding_x() as usize) % size_info.cell_width() as usize;
        let half_cell_width = (size_info.cell_width() / 2.0) as usize;

        let additional_padding =
            (size_info.width() - size_info.padding_x() - size_info.padding_right())
                % size_info.cell_width();
        let end_of_grid = size_info.width() - size_info.padding_right() - additional_padding;

        if cell_x > half_cell_width
            // Edge case when mouse leaves the window.
            || x as f32 >= end_of_grid
        {
            Side::Right
        } else {
            Side::Left
        }
    }

    pub(super) fn mouse_report(&mut self, button: u8, state: ElementState) {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        // Assure the mouse point is not in the scrollback.
        if point.line < 0 {
            return;
        }

        // Calculate modifiers value.
        let mut mods = 0;
        let modifiers = self.ctx.modifiers().state();
        if modifiers.shift_key() {
            mods += 4;
        }
        if modifiers.alt_key() {
            mods += 8;
        }
        if modifiers.control_key() {
            mods += 16;
        }

        // Report mouse events.
        if self.ctx.terminal().mode().contains(TermMode::SGR_MOUSE) {
            self.sgr_mouse_report(point, button + mods, state);
        } else if let ElementState::Released = state {
            self.normal_mouse_report(point, 3 + mods);
        } else {
            self.normal_mouse_report(point, button + mods);
        }
    }

    fn normal_mouse_report(&mut self, point: Point, button: u8) {
        let Point { line, column } = point;
        let utf8 = self.ctx.terminal().mode().contains(TermMode::UTF8_MOUSE);

        let max_point = if utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.ctx.write_to_pty(msg);
    }

    fn sgr_mouse_report(&mut self, point: Point, button: u8, state: ElementState) {
        let c = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        let msg = format!("\x1b[<{};{};{}{}", button, point.column + 1, point.line + 1, c);
        self.ctx.write_to_pty(msg.into_bytes());
    }

    /// Approve the pending confirm modal: re-dispatch the gated close (the
    /// pending confirm matches, so the handler clears it and closes for real)
    /// or run the gated paste. Shared by the Enter key and the modal's
    /// primary button.
    pub(super) fn on_left_click(&mut self, point: Point) {
        let side = self.ctx.mouse().cell_side;
        let control = self.ctx.modifiers().state().control_key();

        match self.ctx.mouse().click_state {
            ClickState::Click => {
                let had_selection = !self.ctx.selection_is_empty();
                // Shift+click extends the existing selection to the clicked
                // cell (Windows Terminal / native text field behaviour)
                // instead of clearing it.
                if self.ctx.modifiers().state().shift_key() && had_selection {
                    self.ctx.update_selection(point, side);
                    return;
                }
                let inside_text_area =
                    self.ctx.size_info().contains_point(self.ctx.mouse().x, self.ctx.mouse().y);
                let mut hint_hit = false;
                if inside_text_area {
                    let mods = self.ctx.modifiers().state();
                    // Query the hint with the SAME viewport the hover path uses
                    // (`pane_view`), not `ctx.size_info()`: the two disagree by
                    // the chrome offset, so the press used to look up a hint on
                    // the wrong row and miss links the hover clearly marked.
                    let hint_point = {
                        let view = self.ctx.display().pane_view();
                        let display_offset = self.ctx.terminal().grid().display_offset();
                        self.ctx.mouse().point(&view, display_offset)
                    };
                    if let Some(hint) = crate::display::hint::highlighted_at(
                        self.ctx.terminal(),
                        self.ctx.config(),
                        hint_point,
                        mods,
                    ) {
                        hint_hit = true;
                        self.ctx.display().highlighted_hint = Some(hint);
                        self.ctx.mouse_mut().block_hint_launcher = false;
                        self.ctx.mark_dirty();
                    }
                }
                crate::display::nebula_link_log(format!(
                    "link_press point={point:?} xy=({:.0},{:.0}) had_sel={had_selection} \
                     ctrl={control} inside={inside_text_area} hint_hit={hint_hit}",
                    self.ctx.mouse().x,
                    self.ctx.mouse().y,
                ));

                // Windows Terminal model: a single click never CREATES a
                // selection — it only clears an existing one. The would-be
                // selection is merely armed here; `mouse_moved` starts it for
                // real once the pointer travels past the drag threshold.
                //
                // Don't launch URLs if this click cleared a selection.
                self.ctx.mouse_mut().block_hint_launcher = had_selection && !hint_hit;
                if had_selection {
                    self.ctx.clear_selection();
                }

                // Ctrl+click on a highlighted link is a link-open gesture, not
                // the start of a block selection: hint hit-testing outranks
                // selection arming (WT parity). A plain click over a link
                // still arms — dragging across a URL must select its text.
                if !(control && hint_hit) {
                    let ty = if control { SelectionType::Block } else { SelectionType::Simple };
                    self.ctx.mouse_mut().pending_selection = Some((ty, point, side));
                    crate::display::nebula_debug_log(format!(
                        "pointer_selection_armed id={} type={ty:?} point={point:?} side={side:?} xy=({}, {})",
                        self.ctx.mouse().debug_press_id,
                        self.ctx.mouse().x,
                        self.ctx.mouse().y,
                    ));
                }
            },
            ClickState::DoubleClick if !control => {
                // Double-click selects the word under the pointer — but on an
                // EMPTY cell there is no word, and semantically selecting the
                // blank used to paint a stray one-cell block that read as "a
                // click leaves a cursor behind" (WT selects nothing there).
                let cell_char = self.ctx.terminal().grid()[point].c;
                if cell_char != ' ' && cell_char != '\t' && cell_char != '\0' {
                    self.ctx.mouse_mut().block_hint_launcher = true;
                    self.ctx.start_selection(SelectionType::Semantic, point, side);
                }
            },
            ClickState::TripleClick if !control => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Lines, point, side);
            },
            _ => (),
        };

        // Move vi mode cursor to mouse click position.
        if self.ctx.terminal().mode().contains(TermMode::VI) && !self.ctx.search_active() {
            self.ctx.terminal_mut().vi_mode_cursor.point = point;
            self.ctx.mark_dirty();
        }
    }

    fn on_mouse_release(&mut self, button: MouseButton) {
        // Nebula: finish an in-progress tab-bar reorder drag first, so it works
        // even while a TUI has grabbed the mouse. A plain click (never dragged)
        // returns `None` here and falls through to normal release handling.
        if button == MouseButton::Left {
            if self.ctx.display().finish_settings_opacity_drag() {
                self.ctx.mark_dirty();
                return;
            }
            // Drop a dragged tree entry: released over the terminal (anywhere
            // off the drawer) pastes its full path but never presses Enter.
            // A directory that never crossed the threshold retains its normal
            // expand/collapse click on release.
            if let Some(drag) = self.ctx.display().nebula_side_panel.drag_file.take() {
                if drag.active {
                    let x = self.ctx.mouse().x as f32;
                    let y = self.ctx.mouse().y as f32;
                    let layout = self.ctx.display().side_panel_layout();
                    let over_terminal = crate::display::side_panel::panel_hit(&layout, x, y)
                        == crate::display::side_panel::PanelHit::None;
                    if let Some(text) = drag.terminal_drop_text(over_terminal) {
                        self.ctx.write_to_pty(text);
                    }
                    self.ctx.mark_dirty();
                    return;
                }
                if drag.is_dir {
                    self.ctx.display().nebula_side_panel.click_drag_source(&drag);
                    self.ctx.mark_dirty();
                    return;
                }
            }
            // Let go of the scrollback thumb.
            if self.ctx.display().nebula_scrollbar_drag.take().is_some() {
                self.ctx.mark_dirty();
                return;
            }
            if let Some(action) = self.ctx.display().end_tab_drag() {
                crate::display::nebula_debug_log(format!(
                    "pointer_tab_drag_end id={} action={action:?}",
                    self.ctx.mouse().debug_press_id
                ));
                use crate::display::TabDropAction;
                match action {
                    TabDropAction::Click(index) => {
                        self.ctx.nebula_tab(crate::event::TabRequest::Select(index));
                    },
                    TabDropAction::Reorder { from, to } => {
                        self.ctx.nebula_tab(crate::event::TabRequest::Move { from, to });
                    },
                    TabDropAction::Dock { source, nav } => {
                        self.ctx.nebula_tab(crate::event::TabRequest::DockSplit { source, nav });
                    },
                }
                self.ctx.mark_dirty();
                return;
            }
        }

        if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons.
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };
            self.mouse_report(code, ElementState::Released);
            return;
        }

        // Trigger hints highlighted by the mouse.
        let hint = self.ctx.display().highlighted_hint.take();
        crate::display::nebula_link_log(format!(
            "link_release button={button:?} hint={} block={} sel_empty={}",
            hint.is_some(),
            self.ctx.mouse().block_hint_launcher,
            self.ctx.selection_is_empty()
        ));
        if let Some(hint) = hint.as_ref().filter(|_| button == MouseButton::Left) {
            // The hover highlight is the ground truth the user sees. If this
            // click produced no real selection (no drag, no double-click), a
            // Ctrl+click on a highlighted link opens it — even when the
            // press-side lookup missed, and even though any mouse motion sets
            // `block_hint_launcher` (that flag exists to stop launches after
            // drag-selections, not after ordinary pointer travel).
            //
            // Requiring Ctrl (matching the "Ctrl+点击 打开" hover hint) keeps a
            // plain click free for text selection — a bare click on a link no
            // longer fires the browser/opener by accident.
            let ctrl = self.ctx.modifiers().state().control_key();
            if ctrl && self.ctx.selection_is_empty() {
                self.ctx.mouse_mut().block_hint_launcher = false;
                self.ctx.trigger_hint(hint);
            }
        }
        self.ctx.display().highlighted_hint = hint;

        let timer_id = TimerId::new(Topic::SelectionScrolling, self.ctx.window().id());
        self.ctx.scheduler_mut().unschedule(timer_id);

        if let MouseButton::Left | MouseButton::Right = button {
            // Copy selection on release, to prevent flooding the display server.
            self.ctx.copy_selection(ClipboardType::Selection);
        }
    }

    pub fn mouse_wheel_input(&mut self, delta: MouseScrollDelta, phase: TouchPhase) {
        // Ctrl+wheel zooms the terminal font (Windows Terminal / browser
        // convention). Checked before every scroll consumer so zoom wins over
        // page/drawer/grid scrolling while the modifier is held.
        if self.ctx.modifiers().state().control_key() {
            let steps = match delta {
                MouseScrollDelta::LineDelta(_, lines) => lines,
                MouseScrollDelta::PixelDelta(pos) => pos.y.signum() as f32,
            };
            if steps != 0.0 {
                let scale = self.ctx.window().scale_factor as f32;
                self.ctx.change_font_size(steps.signum() * scale);
            }
            return;
        }

        // The Settings tab captures the wheel: scroll its page instead of the
        // sink terminal used for special-tab input routing.
        if self.ctx.display().settings_open() {
            let px = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    lines * 3.0 * self.ctx.size_info().cell_height()
                },
                MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
            };
            self.ctx.display().settings_scroll_by(-px);
            return;
        }

        let multiplier = self.ctx.config().scrolling.multiplier;

        // The right-side drawer captures the wheel while the pointer hovers it.
        if self.ctx.display().nebula_side_panel.open {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let layout = self.ctx.display().side_panel_layout();
            let sftp_hit = self.ctx.display().sftp_hit(x, y);
            let inside = if self.ctx.display().nebula_sftp_panel.is_some() {
                sftp_hit != crate::display::sftp_panel::SftpHit::None
            } else {
                crate::display::side_panel::panel_hit(&layout, x, y)
                    != crate::display::side_panel::PanelHit::None
            };
            if inside {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines as i32 * 3,
                    MouseScrollDelta::PixelDelta(pos) => {
                        (-pos.y as f32 / layout.row_h.max(1.0)).round() as i32
                    },
                };
                if rows != 0 {
                    let sftp_max_rows = self.ctx.display().sftp_layout().max_rows;
                    if let Some(panel) = self.ctx.display().nebula_sftp_panel.as_mut() {
                        panel.scroll_by(rows, sftp_max_rows);
                    } else {
                        self.ctx.display().nebula_side_panel.scroll_by(rows, layout.max_rows);
                    }
                    self.ctx.mark_dirty();
                }
                return;
            }
        }

        // The left sidebar's sections capture the wheel while hovered: each
        // (TABS / SSH HOSTS) scrolls independently in whole rows.
        {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let rows = match delta {
                MouseScrollDelta::LineDelta(_, lines) => -lines.signum() as i32,
                MouseScrollDelta::PixelDelta(pos) => -(pos.y.signum() as i32),
            };
            if rows != 0 && self.ctx.display().sidebar_wheel(x, y, rows) {
                self.ctx.mark_dirty();
                return;
            }
        }

        // A document-viewer tab owns the remaining wheel: pixel-scroll the
        // document (no grid behind it to scroll).
        if self.ctx.doc_view().is_some() {
            let px = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    -lines * 3.0 * self.ctx.size_info().cell_height()
                },
                MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
            };
            let viewport_h = self.ctx.display().terminal_card_rect().3;
            if let Some(doc) = self.ctx.doc_view() {
                doc.scroll_by(px * multiplier as f32, viewport_h);
            }
            self.ctx.mark_dirty();
            return;
        }

        match delta {
            MouseScrollDelta::LineDelta(columns, lines) => {
                let new_scroll_px_x = columns * self.ctx.size_info().cell_width();
                let new_scroll_px_y = lines * self.ctx.size_info().cell_height();
                self.scroll_terminal(
                    new_scroll_px_x as f64,
                    new_scroll_px_y as f64,
                    multiplier as f64,
                );
            },
            MouseScrollDelta::PixelDelta(mut lpos) => {
                match phase {
                    TouchPhase::Started => {
                        // Reset offset to zero.
                        self.ctx.mouse_mut().accumulated_scroll = Default::default();
                    },
                    TouchPhase::Moved => {
                        // When the angle between (x, 0) and (x, y) is lower than ~25 degrees
                        // (cosine is larger that 0.9) we consider this scrolling as horizontal.
                        if lpos.x.abs() / lpos.x.hypot(lpos.y) > 0.9 {
                            lpos.y = 0.;
                        } else {
                            lpos.x = 0.;
                        }

                        self.scroll_terminal(lpos.x, lpos.y, multiplier as f64);
                    },
                    _ => (),
                }
            },
        }
    }

    pub(super) fn scroll_terminal(
        &mut self,
        new_scroll_x_px: f64,
        new_scroll_y_px: f64,
        multiplier: f64,
    ) {
        const MOUSE_WHEEL_UP: u8 = 64;
        const MOUSE_WHEEL_DOWN: u8 = 65;
        const MOUSE_WHEEL_LEFT: u8 = 66;
        const MOUSE_WHEEL_RIGHT: u8 = 67;

        let width = f64::from(self.ctx.size_info().cell_width());
        let height = f64::from(self.ctx.size_info().cell_height());

        let multiplier = if self.ctx.mouse_mode() { 1. } else { multiplier };

        self.ctx.mouse_mut().accumulated_scroll.x += new_scroll_x_px * multiplier;
        self.ctx.mouse_mut().accumulated_scroll.y += new_scroll_y_px * multiplier;

        let lines = (self.ctx.mouse().accumulated_scroll.y / height).abs() as usize;
        let columns = (self.ctx.mouse().accumulated_scroll.x / width).abs() as usize;

        let is_scroll_up = new_scroll_y_px > 0.;
        let event = if is_scroll_up { MouseEvent::WheelUp } else { MouseEvent::WheelDown };

        if lines != 0 && self.process_mouse_bindings(event) {
            // Repeat for remaining number of lines.
            for _ in 1..lines {
                self.process_mouse_bindings(event);
            }
        } else if self.ctx.mouse_mode() {
            let code = if is_scroll_up { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_DOWN };
            for _ in 0..lines {
                self.mouse_report(code, ElementState::Pressed);
            }

            let code = if new_scroll_x_px > 0. { MOUSE_WHEEL_LEFT } else { MOUSE_WHEEL_RIGHT };
            for _ in 0..columns {
                self.mouse_report(code, ElementState::Pressed);
            }
        } else if self
            .ctx
            .terminal()
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !self.ctx.modifiers().state().shift_key()
        {
            // The chars here are the same as for the respective arrow keys.
            let line_cmd = if is_scroll_up { b'A' } else { b'B' };
            let column_cmd = if new_scroll_x_px > 0. { b'D' } else { b'C' };

            let mut content = Vec::with_capacity(3 * (lines + columns));

            for _ in 0..lines {
                content.push(0x1b);
                content.push(b'O');
                content.push(line_cmd);
            }

            for _ in 0..columns {
                content.push(0x1b);
                content.push(b'O');
                content.push(column_cmd);
            }

            self.ctx.write_to_pty(content);
        } else if lines != 0 {
            let lines = if is_scroll_up { lines as i32 } else { -(lines as i32) };
            self.ctx.scroll(Scroll::Delta(lines));
        }

        self.ctx.mouse_mut().accumulated_scroll.x %= width;
        self.ctx.mouse_mut().accumulated_scroll.y %= height;
    }

    /// Reset mouse cursor based on modifier and terminal state.
    #[inline]
    pub fn reset_mouse_cursor(&mut self) {
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Modifier state change.
    pub fn modifiers_input(&mut self, modifiers: Modifiers) {
        *self.ctx.modifiers() = modifiers;

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    pub fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        match button {
            MouseButton::Left => self.ctx.mouse_mut().left_button_state = state,
            MouseButton::Middle => self.ctx.mouse_mut().middle_button_state = state,
            MouseButton::Right => self.ctx.mouse_mut().right_button_state = state,
            _ => (),
        }

        // Drag-threshold bookkeeping (Windows SM_CXDRAG-style): remember
        // where the left press landed; `mouse_moved` starts a drag-selection
        // only once the pointer travels past the threshold, so a plain click
        // never leaves a stray one-cell selection that eats link clicks.
        if button == MouseButton::Left {
            let origin = (self.ctx.mouse().x, self.ctx.mouse().y);
            match state {
                ElementState::Pressed => {
                    let mouse = self.ctx.mouse_mut();
                    mouse.debug_press_id = mouse.debug_press_id.wrapping_add(1);
                    mouse.debug_selection_updates = 0;
                    mouse.debug_tab_drag_logged = false;
                    mouse.drag_origin = Some(origin);
                    mouse.drag_active = false;
                    mouse.pending_selection = None;
                },
                ElementState::Released => {
                    let (debug_id, x, y, drag_origin, drag_active, pending, selection_updates) = {
                        let mouse = self.ctx.mouse();
                        (
                            mouse.debug_press_id,
                            mouse.x,
                            mouse.y,
                            mouse.drag_origin,
                            mouse.drag_active,
                            format!("{:?}", mouse.pending_selection),
                            mouse.debug_selection_updates,
                        )
                    };
                    let tab_drag = self.ctx.display().tab_drag_armed();
                    let selection_empty = self.ctx.selection_is_empty();
                    crate::display::nebula_debug_log(format!(
                        "pointer_release_raw id={debug_id} xy=({x}, {y}) origin={drag_origin:?} drag_active={drag_active} pending={pending} tab_drag={tab_drag} selection_empty={selection_empty} selection_updates={selection_updates}",
                    ));
                    let mouse = self.ctx.mouse_mut();
                    mouse.drag_origin = None;
                    mouse.drag_active = false;
                    mouse.pending_selection = None;
                },
            }
        }

        // Skip normal mouse events if the message bar has been clicked.
        if self.message_bar_cursor_state() == Some(CursorIcon::Pointer)
            && state == ElementState::Pressed
        {
            let size = self.ctx.size_info();

            let current_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            self.ctx.clear_selection();
            self.ctx.pop_message();

            // Reset cursor when message bar height changed or all messages are gone.
            let new_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            let new_icon = match current_lines.cmp(&new_lines) {
                Ordering::Less => CursorIcon::Default,
                Ordering::Equal => CursorIcon::Pointer,
                Ordering::Greater => {
                    if self.ctx.mouse_mode() {
                        CursorIcon::Default
                    } else {
                        // Nebula: normal arrow over the terminal area.
                        CursorIcon::Default
                    }
                },
            };

            self.ctx.window().set_mouse_cursor(new_icon);
        } else {
            match state {
                ElementState::Pressed => {
                    // Process mouse press before bindings to update the `click_state`.
                    self.on_mouse_press(button);
                    self.process_mouse_bindings(MouseEvent::Button(button));
                },
                ElementState::Released => self.on_mouse_release(button),
            }
        }
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_mouse_bindings(&mut self, event: MouseEvent) -> bool {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mouse_mode = self.ctx.mouse_mode();
        let mods = self.ctx.modifiers().state();
        let mouse_bindings = self.ctx.config().mouse_bindings().to_owned();

        // If mouse mode is active, also look for bindings without shift.
        let fallback_allowed = mouse_mode && mods.contains(ModifiersState::SHIFT);
        let mut match_found: bool = false;

        for binding in &mouse_bindings {
            // Don't trigger normal bindings in mouse mode unless Shift is pressed.
            if binding.is_triggered_by(mode, mods, &event) && (fallback_allowed || !mouse_mode) {
                binding.action.execute(&mut self.ctx);
                match_found = true;
            }
        }

        if fallback_allowed && !match_found {
            let fallback_mods = mods & !ModifiersState::SHIFT;
            for binding in &mouse_bindings {
                if binding.is_triggered_by(mode, fallback_mods, &event) {
                    binding.action.execute(&mut self.ctx);
                    match_found = true;
                }
            }
        }

        match_found
    }

    /// Check mouse icon state in relation to the message bar.
    fn message_bar_cursor_state(&self) -> Option<CursorIcon> {
        let size = self.ctx.size_info();
        let mouse = self.ctx.mouse();
        let search_active = self.ctx.search_active();
        self.ctx.message()?;

        if message_bar::message_close_button_rect(&size, search_active)
            .is_some_and(|rect| rect.contains(mouse.x as f32, mouse.y as f32))
        {
            return Some(CursorIcon::Pointer);
        }

        message_bar::message_bar_rect(&size, search_active)
            .contains(mouse.x as f32, mouse.y as f32)
            .then_some(CursorIcon::Default)
    }

    /// Icon state of the cursor.
    fn cursor_state(&mut self) -> CursorIcon {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let mut point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);
        // `point` is clamped to `size_info`, but we're about to index the grid,
        // whose column/line count can trail `size_info` by one during a resize
        // or sidebar toggle (asymmetric-padding reflow lands a frame later).
        // Clamp to the grid's real bounds so indexing can never panic.
        {
            let grid = self.ctx.terminal().grid();
            let last_col = grid.columns().saturating_sub(1);
            let last_line = grid.screen_lines().saturating_sub(1) as i32;
            if point.column.0 > last_col {
                point.column = Column(last_col);
            }
            if point.line.0 > last_line {
                point.line = Line(last_line);
            }
        }
        let hyperlink = self.ctx.terminal().grid()[point].hyperlink();

        // Function to check if mouse is on top of a hint.
        let hint_highlighted = |hint: &HintMatch| hint.should_highlight(point, hyperlink.as_ref());

        if let Some(mouse_state) = self.message_bar_cursor_state() {
            mouse_state
        } else if self.ctx.display().highlighted_hint.as_ref().is_some_and(hint_highlighted) {
            CursorIcon::Pointer
        } else if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            CursorIcon::Default
        } else {
            // Nebula: keep the normal arrow over the terminal area (no I-beam).
            CursorIcon::Default
        }
    }

    /// Handle automatic scrolling when selecting above/below the window.
    fn update_selection_scrolling(&mut self, mouse_y: i32) {
        let scale_factor = self.ctx.window().scale_factor;
        let size = self.ctx.size_info();
        let window_id = self.ctx.window().id();
        let scheduler = self.ctx.scheduler_mut();

        // Scale constants by DPI.
        let min_height = (MIN_SELECTION_SCROLLING_HEIGHT * scale_factor) as i32;
        let step = (SELECTION_SCROLLING_STEP * scale_factor) as i32;

        // Compute the height of the scrolling areas.
        let end_top = max(min_height, size.padding_y() as i32);
        let text_area_bottom = size.padding_y() + size.screen_lines() as f32 * size.cell_height();
        let start_bottom = min(size.height() as i32 - min_height, text_area_bottom as i32);

        // Get distance from closest window boundary.
        let delta = if mouse_y < end_top {
            end_top - mouse_y + step
        } else if mouse_y >= start_bottom {
            start_bottom - mouse_y - step
        } else {
            scheduler.unschedule(TimerId::new(Topic::SelectionScrolling, window_id));
            return;
        };

        // Scale number of lines scrolled based on distance to boundary.
        let event = Event::new(EventType::Scroll(Scroll::Delta(delta / step)), Some(window_id));

        // Schedule event.
        let timer_id = TimerId::new(Topic::SelectionScrolling, window_id);
        scheduler.unschedule(timer_id);
        scheduler.schedule(event, SELECTION_SCROLLING_INTERVAL, true, timer_id);
    }
}
