//! Nebula chrome, modal, tab, drawer, and context-menu pointer dispatch.

use std::time::{Duration, Instant};

use winit::event::{ElementState, MouseButton};

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::term::ClipboardType;

use crate::event::{ClickState, Event, EventType};
use crate::scheduler::{TimerId, Topic};

use super::{ActionContext, Processor};

/// Fallback double/triple-click interval where the OS setting isn't
/// available; Windows uses the user's control-panel value instead.
#[cfg(not(windows))]
const CLICK_THRESHOLD: Duration = Duration::from_millis(400);

/// Multi-click interval: the user's system double-click time on Windows.
#[cfg(windows)]
pub(super) fn multi_click_time() -> Duration {
    let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    Duration::from_millis(u64::from(ms))
}

#[cfg(not(windows))]
pub(super) fn multi_click_time() -> Duration {
    CLICK_THRESHOLD
}

/// Half the system double-click rectangle: how far apart two presses may
/// land (per axis) and still count as one multi-click sequence.
#[cfg(windows)]
fn double_click_slop() -> (f32, f32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
    };
    let half = |v: i32| (v.max(4) as f32) / 2.0;
    unsafe { (half(GetSystemMetrics(SM_CXDOUBLECLK)), half(GetSystemMetrics(SM_CYDOUBLECLK))) }
}

#[cfg(not(windows))]
fn double_click_slop() -> (f32, f32) {
    (4.0, 4.0)
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    pub fn nebula_confirm_accept(&mut self, confirm: crate::display::NebulaConfirm) {
        use crate::display::NebulaConfirm;
        match confirm {
            NebulaConfirm::ClosePane { .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::Close);
            },
            NebulaConfirm::CloseTab { index, .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::CloseIndex(index));
            },
            NebulaConfirm::CloseWindow { .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::CloseWindow);
            },
            NebulaConfirm::Paste { text, bracketed, .. } => {
                self.ctx.display().nebula_confirm = None;
                self.ctx.paste_now(&text, bracketed);
            },
            NebulaConfirm::DeleteSsh { host, .. } => {
                self.ctx.display().nebula_confirm = None;
                if self.ctx.display().confirm_delete_ssh_host(&host) {
                    self.schedule_ssh_delete_undo_expiry();
                }
            },
            NebulaConfirm::DeleteSftp { entry } => {
                self.ctx.display().sftp_confirm_delete(entry);
            },
            NebulaConfirm::InstallRequiredFont { directory } => {
                self.ctx.display().nebula_confirm = None;
                self.ctx.open_path(&directory);
            },
        }
    }

    /// Dismiss a confirmation without taking its primary action.
    pub fn nebula_confirm_cancel(&mut self, confirm: crate::display::NebulaConfirm) {
        if confirm.can_dismiss() {
            self.ctx.display().nebula_confirm = None;
        }
    }

    /// Keep timer creation and cancellation identical across keyboard, mouse,
    /// and confirm-dialog paths. The delayed event owns only window routing;
    /// sensitive credential state stays inside `Display`.
    fn schedule_ssh_delete_undo_expiry(&mut self) {
        let window_id = self.ctx.window().id();
        let timer_id = TimerId::new(Topic::SshDeleteUndo, window_id);
        let event = Event::new(EventType::SshDeleteUndoExpired, window_id);
        let scheduler = self.ctx.scheduler_mut();
        scheduler.unschedule(timer_id);
        scheduler.schedule(event, crate::display::SSH_DELETE_UNDO_DURATION, false, timer_id);
    }

    /// Cancel expiry first so a late event cannot immediately dispose a host
    /// that the user has just restored.
    pub(super) fn undo_ssh_delete(&mut self) -> bool {
        let window_id = self.ctx.window().id();
        self.ctx.scheduler_mut().unschedule(TimerId::new(Topic::SshDeleteUndo, window_id));
        self.ctx.display().undo_delete_ssh_host()
    }

    /// Advance the multi-click state machine for this press — exactly ONCE
    /// per press, at the top of `on_mouse_press` (WT's `_numberOfClicks`
    /// model). A press upgrades Click→Double→Triple only when it is the same
    /// button, within the system double-click time AND within half a cell of
    /// the previous press; anything else resets to a plain Click. The
    /// distance gate keeps "click somewhere, then immediately click-drag
    /// elsewhere" from being misread as a word/line drag.
    fn advance_click_state(&mut self, button: MouseButton) -> ClickState {
        let now = Instant::now();
        let mouse = self.ctx.mouse();
        let elapsed = now - mouse.last_click_timestamp;
        let (last_x, last_y) = mouse.last_click_pos;
        let (slop_x, slop_y) = double_click_slop();
        let near = (mouse.x as f32 - last_x as f32).abs() <= slop_x
            && (mouse.y as f32 - last_y as f32).abs() <= slop_y;
        let state = match mouse.click_state {
            _ if button != mouse.last_click_button || !near || elapsed >= multi_click_time() => {
                ClickState::Click
            },
            ClickState::Click => ClickState::DoubleClick,
            ClickState::DoubleClick => ClickState::TripleClick,
            _ => ClickState::Click,
        };
        let pos = (self.ctx.mouse().x, self.ctx.mouse().y);
        let mouse = self.ctx.mouse_mut();
        mouse.last_click_timestamp = now;
        mouse.last_click_button = button;
        mouse.last_click_pos = pos;
        mouse.click_state = state;
        state
    }

    fn run_context_menu_action(&mut self, action: crate::display::ContextMenuAction) {
        use crate::display::ContextMenuAction::*;
        match action {
            DuplicateTab(index) => {
                self.ctx.nebula_tab(crate::event::TabRequest::Duplicate(index));
            },
            SplitTabRight(index) => {
                self.ctx.nebula_tab(crate::event::TabRequest::SplitIndex {
                    index,
                    direction: crate::display::SplitDirection::LeftRight,
                });
            },
            SplitTabDown(index) => {
                self.ctx.nebula_tab(crate::event::TabRequest::SplitIndex {
                    index,
                    direction: crate::display::SplitDirection::TopBottom,
                });
            },
            RenameTab(index) => {
                self.ctx.nebula_tab(crate::event::TabRequest::BeginRename(index));
            },
            CloseTab(index) => {
                self.ctx.nebula_tab(crate::event::TabRequest::CloseIndex(index));
            },
            SetTabColor { index, color } => {
                self.ctx.nebula_tab(crate::event::TabRequest::SetColor { index, color });
            },
            ConnectSsh(index) => {
                let host = self.ctx.display().nebula_ssh_hosts.get(index).cloned();
                if let Some(host) = host {
                    self.ctx.nebula_tab(crate::event::TabRequest::NewSsh(host));
                }
            },
            OpenSftp(index) => {
                let host = self.ctx.display().nebula_ssh_hosts.get(index).cloned();
                if let Some(host) = host {
                    self.ctx.nebula_open_sftp(host);
                }
            },
            CopySshAddress(index) => {
                let host = self.ctx.display().nebula_ssh_hosts.get(index).cloned();
                if let Some(host) = host {
                    self.ctx.clipboard_mut().store(ClipboardType::Clipboard, host);
                }
            },
            EditSsh(index) => self.ctx.display().edit_ssh_host(index),
            DeleteSsh(index) => self.ctx.display().request_delete_ssh_host(index),
            DownloadSftp(index) => self.ctx.display().sftp_download_row(index),
            RenameSftp(index) => self.ctx.display().sftp_begin_rename_row(index),
            DeleteSftp(index) => self.ctx.display().sftp_request_delete_row(index),
            RefreshSftp => self.ctx.display().sftp_refresh(),
            UploadFilesSftp => self.ctx.display().sftp_pick_upload_files(),
            UploadDirectorySftp => self.ctx.display().sftp_pick_upload_directory(),
            NewDirectorySftp => self.ctx.display().sftp_begin_create_directory(),
        }
        self.ctx.mark_dirty();
    }

    pub(super) fn on_mouse_press(&mut self, button: MouseButton) {
        // Multi-click bookkeeping happens here and nowhere else. It used to
        // be advanced both by the chrome block and the terminal block below;
        // the second advance saw elapsed≈0 and upgraded EVERY terminal click
        // to a double/triple — plain drags selected by word or whole line.
        self.advance_click_state(button);

        let debug_id = self.ctx.mouse().debug_press_id;
        let debug_x = self.ctx.mouse().x as f32;
        let debug_y = self.ctx.mouse().y as f32;
        if self.ctx.nebula_chrome_active() {
            let window_size = self.ctx.display().size_info;
            let pane_size = self.ctx.size_info();
            let scale = self.ctx.window().scale_factor as f32;
            let chrome_hit = self.ctx.display().chrome_hit(debug_x, debug_y);
            let in_chrome = crate::display::in_chrome_bar(&window_size, scale, debug_x, debug_y);
            let tab_drag = self.ctx.display().tab_drag_armed();
            let selection_empty = self.ctx.selection_is_empty();
            crate::display::nebula_debug_log(format!(
                "pointer_press id={debug_id} button={button:?} xy=({debug_x:.0},{debug_y:.0}) scale={scale:.3} window={}x{} pane={}x{} pad=({:.0},{:.0},{:.0},{:.0}) chrome_hit={chrome_hit:?} in_chrome={in_chrome} tab_drag={tab_drag} selection_empty={selection_empty} click={:?}",
                window_size.width(),
                window_size.height(),
                pane_size.width(),
                pane_size.height(),
                window_size.padding_x(),
                window_size.padding_right(),
                window_size.padding_y(),
                window_size.padding_bottom(),
                self.ctx.mouse().click_state,
            ));
        }

        // Window chrome is deliberately optional at the input boundary. The
        // terminal's click-state machine is useful and testable without a GPU
        // window; real application contexts keep the default `true` value.
        if self.ctx.nebula_chrome_active() {
            // Non-primary presses also release lightweight menus. Right-click on
            // a context menu is handled below so it can naturally retarget.
            if button != MouseButton::Left {
                if button != MouseButton::Right && self.ctx.display().context_menu_interactive() {
                    self.ctx.display().close_context_menu();
                    self.ctx.mark_dirty();
                    return;
                }
                if self.ctx.display().command_palette_picker_open() {
                    self.ctx.display().close_command_palette();
                    self.ctx.mark_dirty();
                    return;
                }
                if self.ctx.display().nebula_shell_picker_open {
                    self.ctx.display().close_shell_picker();
                    self.ctx.mark_dirty();
                    return;
                }
                if self.ctx.display().nebula_font_picker_open {
                    self.ctx.display().close_font_picker();
                    self.ctx.mark_dirty();
                    return;
                }
            }

            if button == MouseButton::Left && self.ctx.display().context_menu_interactive() {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                if let crate::display::ContextMenuHit::Action(action) =
                    self.ctx.display().context_menu_click(x, y)
                {
                    self.run_context_menu_action(action);
                }
                self.ctx.mark_dirty();
                return;
            }

            // A left press anywhere OUTSIDE the rename box ends the edit
            // (canceling, like Esc) — the click itself still lands wherever it
            // was aimed. Clicking inside the box is caret placement (below).
            if button == MouseButton::Left {
                if let Some((idx, _)) = self.ctx.display().nebula_tab_rename.clone() {
                    let x = self.ctx.mouse().x as f32;
                    let y = self.ctx.mouse().y as f32;
                    if self.ctx.display().chrome_hit(x, y) != crate::display::ChromeHit::Tab(idx) {
                        self.ctx.nebula_tab(crate::event::TabRequest::CancelRename);
                    }
                }
            }

            if button == MouseButton::Left && self.ctx.display().nebula_ssh_editor.is_some() {
                if self.ctx.display().ssh_editor_active() {
                    let x = self.ctx.mouse().x as f32;
                    let y = self.ctx.mouse().y as f32;
                    self.ctx.display().ssh_editor_click(x, y);
                }
                self.ctx.mark_dirty();
                return;
            }

            // Nebula command palette: clicking a row runs it, clicking outside
            // dismisses — same modal semantics as the keyboard path.
            if button == MouseButton::Left && self.ctx.display().command_palette_open() {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                let size = self.ctx.display().size_info;
                let scale = self.ctx.window().scale_factor as f32;
                let layout = crate::display::command_palette::palette_layout(
                    size.width(),
                    size.height(),
                    scale,
                );
                let (px, py, pw, ph) = layout.panel;
                if x >= px && x < px + pw && y >= py && y < py + ph {
                    if y >= layout.list_y {
                        let row = ((y - layout.list_y) / layout.row_h) as usize;
                        if let Some(action) = self.ctx.display().palette_click(row, layout.max_rows)
                        {
                            self.run_palette_action(action);
                        }
                    }
                } else {
                    self.ctx.display().close_command_palette();
                }
                self.ctx.mark_dirty();
                return;
            }

            // The reversible-action bar floats above drawer/chrome content. True
            // modals still retain pointer ownership and therefore block this path.
            let undo_pointer = (self.ctx.mouse().x as f32, self.ctx.mouse().y as f32);
            if button == MouseButton::Left
                && self.ctx.display().nebula_confirm.is_none()
                && self.ctx.display().ssh_delete_undo_hit(undo_pointer.0, undo_pointer.1)
            {
                self.undo_ssh_delete();
                self.ctx.mark_dirty();
                return;
            }

            // Right-side drawer (directory tree / git): header tabs switch views,
            // directory rows expand/collapse. Sits under the modal layers, so
            // only when no modal owns the pointer.
            if button == MouseButton::Left
                && self.ctx.display().nebula_sftp_panel.is_some()
                && !self.ctx.display().settings_open()
                && self.ctx.display().nebula_confirm.is_none()
            {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                let hit = self.ctx.display().sftp_hit(x, y);
                if hit != crate::display::sftp_panel::SftpHit::None {
                    self.ctx.display().sftp_click(hit);
                    self.ctx.mark_dirty();
                    return;
                }
            }
            if button == MouseButton::Left
                && self.ctx.display().nebula_side_panel.open
                && self.ctx.display().nebula_sftp_panel.is_none()
                && !self.ctx.display().settings_open()
                && self.ctx.display().nebula_confirm.is_none()
            {
                use crate::display::side_panel::{PanelHit, PanelView, panel_interactive_hit};
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                let layout = self.ctx.display().side_panel_layout();
                let view = self.ctx.display().nebula_side_panel.view;
                let custom_root = self.ctx.display().nebula_side_panel.custom_root_active();
                let has_root = self.ctx.display().nebula_side_panel.root().is_some();
                match panel_interactive_hit(&layout, view, custom_root, has_root, x, y) {
                    PanelHit::None => {
                        // Clicking anywhere outside the drawer drops search focus
                        // and the persistent file selection.
                        let panel = &mut self.ctx.display().nebula_side_panel;
                        if panel.search_focus || panel.selected.is_some() {
                            panel.search_unfocus(false);
                            panel.selected = None;
                            self.ctx.mark_dirty();
                        }
                    },
                    hit => {
                        match hit {
                            PanelHit::ViewFiles => {
                                self.ctx.display().toggle_side_panel(PanelView::Files)
                            },
                            PanelHit::ViewGit => {
                                self.ctx.display().toggle_side_panel(PanelView::Git)
                            },
                            PanelHit::OpenDirectory => {
                                self.ctx.display().choose_side_panel_directory();
                            },
                            PanelHit::NewTerminalHere => {
                                let root = self
                                    .ctx
                                    .display()
                                    .nebula_side_panel
                                    .root()
                                    .map(std::path::Path::to_path_buf);
                                if let Some(root) = root {
                                    self.ctx
                                        .nebula_tab(crate::event::TabRequest::NewAtDirectory(root));
                                }
                            },
                            PanelHit::FollowCurrentDirectory => {
                                self.ctx.display().follow_focused_directory();
                            },
                            PanelHit::Search => {
                                let files =
                                    self.ctx.display().nebula_side_panel.view == PanelView::Files;
                                if files {
                                    // The Files view's filter box takes focus.
                                    self.ctx.display().nebula_side_panel.search_focus = true;
                                } else {
                                    // Git view: that strip is the 暂存/提交/推送
                                    // button row (or the commit-message input,
                                    // which the keyboard owns — clicks are inert).
                                    if !self.ctx.display().nebula_side_panel.commit_focus {
                                        let (sx, _, sw, _) = layout.search;
                                        let gap = 6.0 * self.ctx.window().scale_factor as f32;
                                        let rects = crate::display::side_panel::git_button_rects(
                                            sx, sw, gap,
                                        );
                                        let panel = &mut self.ctx.display().nebula_side_panel;
                                        let action =
                                            rects.iter().position(|(button_x, button_w)| {
                                                x >= *button_x && x < *button_x + *button_w
                                            });
                                        match action {
                                            Some(0) => panel.git_stage_all(),
                                            Some(1) => panel.git_begin_commit(),
                                            Some(2) => panel.git_pull(),
                                            Some(3) => panel.git_push(),
                                            _ => {},
                                        }
                                    }
                                }
                            },
                            PanelHit::Row(row) => {
                                self.ctx.display().nebula_side_panel.search_unfocus(false);
                                let info = self
                                    .ctx
                                    .display()
                                    .nebula_side_panel
                                    .visible_row(row)
                                    .map(|r| (r.path.clone(), r.is_dir, r.is_parent));
                                match info {
                                    None => {
                                        self.ctx.display().nebula_side_panel.click_row(row);
                                    },
                                    // `..` 是导航项而非可拖拽目录，按下时立即完成；
                                    // 这样鼠标松开阶段不会误展开切换后的新根目录。
                                    Some((_, _, true)) => {
                                        self.ctx.display().nebula_side_panel.click_row(row);
                                    },
                                    // Directory clicks are deferred to mouse-up:
                                    // crossing the threshold turns them into a
                                    // path drag without first changing the tree.
                                    Some((path, true, false)) => {
                                        use crate::display::side_panel::FileDrag;
                                        let name = path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_default();
                                        self.ctx.display().nebula_side_panel.drag_file =
                                            Some(FileDrag::new(path, name, true, row, (x, y)));
                                    },
                                    // Files: double-click opens with the system
                                    // handler; a single press arms a drag toward
                                    // the terminal (drop pastes the path).
                                    Some((path, false, false)) => {
                                        use crate::display::side_panel::FileDrag;
                                        let now = std::time::Instant::now();
                                        let dbl = {
                                            let panel = &mut self.ctx.display().nebula_side_panel;
                                            // Click = persistent selection (until
                                            // clicking off the panel / closing it).
                                            panel.selected = Some(path.clone());
                                            let dbl = panel.last_file_click.as_ref().is_some_and(
                                                |(p, t)| {
                                                    *p == path
                                                        && t.elapsed()
                                                            < std::time::Duration::from_millis(400)
                                                },
                                            );
                                            if dbl {
                                                panel.last_file_click = None;
                                                panel.drag_file = None;
                                            } else {
                                                panel.last_file_click = Some((path.clone(), now));
                                                let name = path
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().into_owned())
                                                    .unwrap_or_default();
                                                panel.drag_file = Some(FileDrag::new(
                                                    path.clone(),
                                                    name,
                                                    false,
                                                    row,
                                                    (x, y),
                                                ));
                                            }
                                            dbl
                                        };
                                        if dbl {
                                            // Readable text files open in an
                                            // in-app viewer tab; everything else
                                            // goes to the system handler.
                                            if crate::display::markdown_view::viewable_file(&path) {
                                                self.ctx.nebula_tab(
                                                    crate::event::TabRequest::OpenDoc(path),
                                                );
                                            } else {
                                                self.ctx.open_path(&path);
                                            }
                                        }
                                    },
                                }
                            },
                            _ => {
                                self.ctx.display().nebula_side_panel.search_unfocus(false);
                            },
                        }
                        self.ctx.mark_dirty();
                        return;
                    },
                }
            }

            // Right-clicking the sidebar "+" opens the quick-launch profile menu
            // (Windows Terminal's profile dropdown); left-click keeps opening the
            // default shell. Tab and SSH context menus will replace the old
            // reorder/pin shortcuts; until that menu lands, SSH right-click is
            // consumed without changing the saved-host order.
            if button == MouseButton::Right {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                let menu_was_open = self.ctx.display().context_menu_interactive();
                if menu_was_open
                    && !matches!(
                        self.ctx.display().context_menu_hit(x, y),
                        crate::display::ContextMenuHit::Outside
                    )
                {
                    return;
                }
                match self.ctx.display().sftp_hit(x, y) {
                    crate::display::sftp_panel::SftpHit::Row(index) => {
                        let entry = self
                            .ctx
                            .display()
                            .nebula_sftp_panel
                            .as_ref()
                            .and_then(|panel| panel.visible_entry(index));
                        match entry {
                            Some(entry) if !entry.is_parent => {
                                self.ctx.display().open_sftp_context_menu(index, x, y);
                            },
                            Some(_) => {},
                            None => self.ctx.display().open_sftp_panel_context_menu(x, y),
                        }
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::sftp_panel::SftpHit::Inside => {
                        self.ctx.display().open_sftp_panel_context_menu(x, y);
                        self.ctx.mark_dirty();
                        return;
                    },
                    _ => {},
                }
                match self.ctx.display().chrome_hit(x, y) {
                    crate::display::ChromeHit::NewTab => {
                        let profiles: Vec<String> =
                            self.ctx.config().profiles.iter().map(|p| p.name.clone()).collect();
                        // Detected shells fill the menu even with no config
                        // profiles, so always open it.
                        self.ctx.display().open_shell_menu(&profiles);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::Tab(index)
                    | crate::display::ChromeHit::TabClose(index) => {
                        self.ctx.display().open_tab_context_menu(index, x, y);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::Host(index) => {
                        self.ctx.display().open_ssh_context_menu(index, x, y);
                        self.ctx.mark_dirty();
                        return;
                    },
                    _ if menu_was_open => {
                        self.ctx.display().close_context_menu();
                        self.ctx.mark_dirty();
                        return;
                    },
                    _ => {},
                }
            }

            // Nebula chrome: intercept clicks on the custom title bar and window
            // controls before any terminal handling.
            if button == MouseButton::Left {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                // Chrome geometry is window-relative; the pane view would misplace
                // every hit rect in split mode (unclickable gear, wrong tabs).
                let size = self.ctx.display().size_info;
                let scale = self.ctx.window().scale_factor as f32;
                // Window border resize takes priority over the chrome controls.
                let resize_enabled = self.ctx.window().allows_drag_resize();
                if let Some(dir) = crate::display::resize_edge(&size, scale, x, y, resize_enabled) {
                    self.ctx.window().drag_resize(dir);
                    return;
                }
                // The confirm modal owns the pointer while it shows: its two
                // buttons dispatch, any other click is swallowed (modal
                // semantics — nothing may reach the UI behind the veil).
                if let Some(confirm) = self.ctx.display().nebula_confirm.clone() {
                    if let Some((primary, cancel)) = self.ctx.display().nebula_confirm_buttons {
                        let hit = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
                            x >= rx && x < rx + rw && y >= ry && y < ry + rh
                        };
                        if hit(primary) {
                            self.nebula_confirm_accept(confirm);
                        } else if hit(cancel) {
                            self.nebula_confirm_cancel(confirm);
                        }
                    }
                    self.ctx.mark_dirty();
                    return;
                }
                let settings_open = self.ctx.display().settings_open();
                let settings_section = self.ctx.display().settings_section();
                let settings_scroll = self.ctx.display().settings_scroll();
                let shell_picker_open = self.ctx.display().nebula_shell_picker_open;
                let shell_picker_count = self.ctx.display().shell_picker_count();
                let font_picker_open = self.ctx.display().nebula_font_picker_open;
                let font_picker_count = self.ctx.display().font_picker_count();
                let hidden_host_count = self.ctx.display().hidden_ssh_host_count();
                let settings_area = self.ctx.display().terminal_card_rect();
                let settings_hit = crate::display::settings_hit(
                    &size,
                    scale,
                    settings_area,
                    x,
                    y,
                    settings_open,
                    settings_section,
                    settings_scroll,
                    shell_picker_open,
                    shell_picker_count,
                    font_picker_open,
                    font_picker_count,
                    hidden_host_count,
                );
                if shell_picker_open
                    && !matches!(
                        settings_hit,
                        crate::display::SettingsHit::ShellCycle
                            | crate::display::SettingsHit::ShellPickerRow(_)
                    )
                {
                    // First outside click dismisses the picker without activating
                    // an unrelated control hidden behind its temporary focus scope.
                    self.ctx.display().close_shell_picker();
                    self.ctx.mark_dirty();
                    return;
                }
                if font_picker_open
                    && !matches!(
                        settings_hit,
                        crate::display::SettingsHit::FontCycle
                            | crate::display::SettingsHit::FontPickerRow(_)
                    )
                {
                    self.ctx.display().close_font_picker();
                    self.ctx.mark_dirty();
                    return;
                }
                match settings_hit {
                    crate::display::SettingsHit::Toggle => {
                        self.ctx.nebula_tab(crate::event::TabRequest::OpenSettings);
                        return;
                    },
                    crate::display::SettingsHit::Nav(section) => {
                        self.ctx.display().select_settings_section(section);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::Theme(theme) => {
                        self.ctx.display().select_nebula_theme(theme);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::Language(language) => {
                        self.ctx.display().set_ui_language(language);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::GhostToggle => {
                        self.ctx.display().toggle_ghost();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::AcceptCycle => {
                        self.ctx.display().cycle_accept();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::ShellCycle => {
                        // Toggle inline shell picker (expand/collapse the list).
                        self.ctx.display().toggle_shell_picker();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::ShellPickerRow(index) => {
                        self.ctx.display().set_default_shell_by_index(index);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::FontCycle => {
                        self.ctx.display().toggle_font_picker();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::FontPickerRow(index) => {
                        let base_font = self.ctx.config().font.clone();
                        self.ctx.display().set_terminal_font_by_index(index, &base_font);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::SystemThemeToggle => {
                        self.ctx.display().toggle_system_theme_following();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::RestoreHiddenSsh(index) => {
                        self.ctx.display().restore_hidden_ssh_host(index);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::FetchToggle => {
                        self.ctx.display().toggle_fetch();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::PowerlineToggle => {
                        self.ctx.display().toggle_powerline();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::KeepSessionToggle => {
                        self.ctx.display().toggle_keep_session();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::OpacityDown => {
                        self.ctx.display().adjust_window_opacity(-0.05);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::OpacityUp => {
                        self.ctx.display().adjust_window_opacity(0.05);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::BackgroundColor => {
                        self.ctx.display().cycle_background_color();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::BackgroundImage => {
                        self.ctx.display().pick_background_image();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::OpenConfigFile => {
                        self.ctx.display().open_user_config_file();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::Reset => {
                        self.ctx.display().reset_appearance_settings();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::SettingsHit::Panel => return,
                    crate::display::SettingsHit::None => {},
                }
                let chrome_hit = self.ctx.display().chrome_hit(x, y);
                if self.ctx.display().nebula_special_tab_active
                    && matches!(
                        chrome_hit,
                        crate::display::ChromeHit::PanelFiles | crate::display::ChromeHit::PanelGit
                    )
                {
                    return;
                }
                // Multi-click state was advanced once at the top of this
                // function; read it for the tab double-click rename below.
                let state = self.ctx.mouse().click_state;
                match chrome_hit {
                    crate::display::ChromeHit::NewTab => {
                        self.ctx.nebula_tab(crate::event::TabRequest::New);
                        return;
                    },
                    crate::display::ChromeHit::NewTabMenu => {
                        // The chevron opens the shell dropdown (detected shells +
                        // config profiles), like Windows Terminal's profile menu.
                        let profiles: Vec<String> =
                            self.ctx.config().profiles.iter().map(|p| p.name.clone()).collect();
                        self.ctx.display().open_shell_menu(&profiles);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::AddSshHost => {
                        self.ctx.display().open_ssh_editor();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::TabClose(index) => {
                        self.ctx.nebula_tab(crate::event::TabRequest::CloseIndex(index));
                        return;
                    },
                    crate::display::ChromeHit::Tab(index) => {
                        // Clicking inside the rename box places the caret there
                        // (real text-field behaviour) instead of starting a drag.
                        if self
                            .ctx
                            .display()
                            .nebula_tab_rename
                            .as_ref()
                            .is_some_and(|(i, _)| *i == index)
                        {
                            self.ctx.display().tab_rename_click(x);
                            self.ctx.mark_dirty();
                            return;
                        }
                        // Double-click a tab to start renaming (Windows Terminal style).
                        if state == ClickState::DoubleClick {
                            self.ctx.nebula_tab(crate::event::TabRequest::BeginRename(index));
                            return;
                        }
                        // Selection is deferred to release (a plain click becomes
                        // TabDropAction::Click): the terminal area must keep
                        // showing the ACTIVE tab while another tab is dragged over
                        // it toward a dock zone.
                        self.ctx.display().arm_tab_drag(index, x, y);
                        return;
                    },
                    crate::display::ChromeHit::SidebarToggle => {
                        self.ctx.display().toggle_sidebar();
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::Host(index) => {
                        // Open a new tab connected to this ~/.ssh/config host.
                        let host = self.ctx.display().nebula_ssh_hosts.get(index).cloned();
                        if let Some(host) = host {
                            self.ctx.nebula_tab(crate::event::TabRequest::NewSsh(host));
                        }
                        return;
                    },
                    crate::display::ChromeHit::TabsSection => {
                        self.ctx.display().toggle_sidebar_section(false);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::HostsSection => {
                        self.ctx.display().toggle_sidebar_section(true);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::PanelFiles => {
                        if let Some(destination) =
                            self.ctx.nebula_ssh_destination().map(str::to_owned)
                        {
                            self.ctx.nebula_open_sftp(destination);
                        } else {
                            self.ctx
                                .display()
                                .toggle_side_panel(crate::display::side_panel::PanelView::Files);
                        }
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::PanelGit => {
                        self.ctx
                            .display()
                            .toggle_side_panel(crate::display::side_panel::PanelView::Git);
                        self.ctx.mark_dirty();
                        return;
                    },
                    crate::display::ChromeHit::Close => {
                        self.ctx.nebula_tab(crate::event::TabRequest::CloseWindow);
                        return;
                    },
                    crate::display::ChromeHit::Minimize => {
                        self.ctx.window().set_minimized(true);
                        return;
                    },
                    crate::display::ChromeHit::Maximize => {
                        self.ctx.window().toggle_maximized();
                        return;
                    },
                    crate::display::ChromeHit::TitleBar => {
                        self.ctx.window().drag_window();
                        return;
                    },
                    crate::display::ChromeHit::None => {},
                }

                // A press inside the chrome band (top bar / tab sidebar) that
                // hit no control ends here. Falling through used to reach the
                // terminal's selection arming, which clamps the point into the
                // nearest grid cell — dragging from the sidebar then painted a
                // stray selection across the pane (the "drag ghost").
                let window_size = self.ctx.display().size_info;
                let scale = self.ctx.window().scale_factor as f32;
                if crate::display::in_chrome_bar(&window_size, scale, x, y) {
                    crate::display::nebula_debug_log(format!(
                        "pointer_route id={} route=chrome-unclaimed-consumed xy=({x:.0},{y:.0})",
                        self.ctx.mouse().debug_press_id
                    ));
                    return;
                }

                // Nebula: grab the scrollback thumb (or jump on a track press).
                // Only live while scrolled into history, since the bar auto-hides.
                let view = self.ctx.size_info();
                let display_offset = self.ctx.terminal().grid().display_offset();
                let total_lines = self.ctx.terminal().total_lines();
                if let Some(grab) =
                    self.ctx.display().scrollbar_grab(&view, display_offset, total_lines, x, y)
                {
                    self.ctx.display().nebula_scrollbar_drag = Some(grab);
                    let target =
                        self.ctx.display().scrollbar_target_offset(&view, total_lines, y, grab);
                    let delta = target as i32 - display_offset as i32;
                    if delta != 0 {
                        self.ctx.scroll(Scroll::Delta(delta));
                    }
                    self.ctx.mark_dirty();
                    return;
                }
            }
        }

        if button == MouseButton::Left {
            crate::display::nebula_debug_log(format!(
                "pointer_route id={} route=terminal-fallthrough xy=({}, {})",
                self.ctx.mouse().debug_press_id,
                self.ctx.mouse().x,
                self.ctx.mouse().y,
            ));
        }

        // Nebula: right-click copies the selection, or pastes when there is
        // none (Windows Terminal-style), unless the app is in mouse mode.
        if button == MouseButton::Right
            && !self.ctx.modifiers().state().shift_key()
            && !self.ctx.mouse_mode()
        {
            if self.ctx.selection_is_empty() {
                let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                self.ctx.paste(&text, true);
            } else {
                self.ctx.copy_selection(ClipboardType::Clipboard);
                self.ctx.clear_selection();
            }
            return;
        }

        // Handle mouse mode.
        if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            self.ctx.mouse_mut().click_state = ClickState::None;

            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons..
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };

            self.mouse_report(code, ElementState::Pressed);
        } else {
            // Multi-click state was advanced once at the top of this function.
            // Load mouse point, treating message bar and padding as the closest cell.
            let display_offset = self.ctx.terminal().grid().display_offset();
            let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

            if let MouseButton::Left = button {
                self.on_left_click(point)
            }
        }
    }
}
