//! 终端视图：持有会话、处理输入与 IME、驱动重绘。

use gpui::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, Font, FontStyle,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollWheelEvent,
    Size, Styled as _, UTF16Selection, Window, div, point, px,
};
use gpui_component::PixelsExt as _;
use nebula_terminal::event::{Event as TermEvent, Notify as _, OnResize as _, WindowSize};
use nebula_terminal::event_loop::Msg;
use nebula_terminal::grid::Scroll;
use nebula_terminal::index::{Column, Point as TermPoint, Side};
use nebula_terminal::render::{CellMetrics, ViewportTracker};
use nebula_terminal::selection::{Selection, SelectionType};
use nebula_terminal::term::TermMode;
use nebula_terminal::term::viewport_to_point;

use std::sync::Arc;

use super::colors::Palette;
use super::element::TerminalElement;
use super::keymap;
use super::mouse_protocol;
use super::session::{self, GridSize, TerminalSession};
use crate::gpui_shell::config::Settings;

use futures::StreamExt as _;

/// 终端视图对宿主（Panel/Workspace）暴露的状态变化。
pub enum TerminalViewEvent {
    /// OSC 标题变化，宿主应刷新 Tab 标题。
    TitleChanged,
    /// 会话结束（子进程退出或 PTY 故障），只发一次。
    Exited,
}

pub struct TerminalView {
    pub session: Option<TerminalSession>,
    pub focus_handle: FocusHandle,
    pub font: Font,
    pub font_bold: Font,
    pub font_italic: Font,
    pub font_bold_italic: Font,
    pub font_size: Pixels,
    pub line_height_mul: f32,
    pub palette: Arc<Palette>,
    pub marked_text: Option<String>,
    pub ime_bounds: Bounds<Pixels>,
    pub title: String,
    error: Option<String>,
    exited: Option<String>,
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    cols: usize,
    rows: usize,
    window_size: WindowSize,
    viewports: ViewportTracker,
    scroll_px: f32,
    selecting: bool,
    /// 选中即复制（旧壳 `copy_on_select`）；关闭时复制交给右键路径。
    copy_on_select: bool,
    /// 鼠标模式下最后上报的单元格：move 事件按"进入新单元格"去重。
    last_report_point: Option<TermPoint>,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // 字体、调色板与终端启动配置来自用户配置（nebula.toml +
        // nebula_settings.txt，bootstrap 时装载为全局 Settings）。
        let (families, font_size, palette, term_config, copy_on_select) =
            match cx.try_global::<Settings>() {
                Some(settings) => (
                    [
                        settings.font_family.clone(),
                        settings.font_bold_family.clone(),
                        settings.font_italic_family.clone(),
                        settings.font_bold_italic_family.clone(),
                    ],
                    px(settings.font_size_px),
                    Arc::new(settings.palette.clone()),
                    settings.term_config(),
                    settings.copy_on_select,
                ),
                None => (
                    std::array::from_fn(|_| String::from("Cascadia Mono")),
                    px(15.0),
                    Arc::new(Palette::default()),
                    nebula_terminal::term::Config::default(),
                    // 旧壳的出厂默认即开。
                    true,
                ),
            };
        // to_string：SharedString 的 &str 转换要求 'static，字体名是运行时值。
        let mono = |family: &str, weight: FontWeight, style: FontStyle| Font {
            weight,
            style,
            ..gpui::font(family.to_string())
        };

        let initial = WindowSize { num_lines: 30, num_cols: 100, cell_width: 9, cell_height: 18 };
        let (session, error) = match session::spawn(initial, term_config) {
            Ok((session, mut rx)) => {
                cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        // 合并同一批到达的事件，避免每个 Wakeup 都独立触发一帧。
                        let mut batch = vec![event];
                        while batch.len() < 128 {
                            match rx.try_recv() {
                                Ok(event) => batch.push(event),
                                _ => break,
                            }
                        }
                        let done = batch.iter().any(|e| matches!(e, TermEvent::Exit));
                        if this
                            .update(cx, |view: &mut Self, cx| {
                                for event in batch {
                                    view.process_event(event, cx);
                                }
                            })
                            .is_err()
                            || done
                        {
                            break;
                        }
                    }
                })
                .detach();
                (Some(session), None)
            },
            Err(err) => (None, Some(format!("PTY 启动失败: {err}"))),
        };

        Self {
            session,
            focus_handle: cx.focus_handle(),
            font: mono(&families[0], FontWeight::NORMAL, FontStyle::Normal),
            font_bold: mono(&families[1], FontWeight::BOLD, FontStyle::Normal),
            font_italic: mono(&families[2], FontWeight::NORMAL, FontStyle::Italic),
            font_bold_italic: mono(&families[3], FontWeight::BOLD, FontStyle::Italic),
            font_size,
            line_height_mul: 1.25,
            palette,
            marked_text: None,
            ime_bounds: Bounds::default(),
            title: String::from("shell"),
            error,
            exited: None,
            origin: point(px(0.0), px(0.0)),
            cell_width: px(8.0),
            line_height: px(18.0),
            cols: initial.num_cols as usize,
            rows: initial.num_lines as usize,
            window_size: initial,
            viewports: ViewportTracker::default(),
            scroll_px: 0.0,
            selecting: false,
            copy_on_select,
            last_report_point: None,
        }
    }

    fn process_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange => {
                cx.notify();
            },
            TermEvent::Title(title) => {
                self.title = title;
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            },
            TermEvent::ResetTitle => {
                self.title = String::from("shell");
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            },
            TermEvent::PtyWrite(text) => self.write_bytes(text.into_bytes()),
            TermEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            },
            TermEvent::ClipboardLoad(_, formatter) => {
                let text =
                    cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                self.write_bytes(formatter(&text).into_bytes());
            },
            TermEvent::ColorRequest(index, formatter) => {
                let reply = self.session.as_ref().map(|session| {
                    let term = session.term.lock();
                    self.palette.query_reply(index, term.colors())
                });
                if let Some(rgb) = reply {
                    self.write_bytes(formatter(rgb).into_bytes());
                }
            },
            TermEvent::TextAreaSizeRequest(formatter) => {
                self.write_bytes(formatter(self.window_size).into_bytes());
            },
            TermEvent::ChildExit(code) => {
                self.mark_exited(format!("进程已退出（{code:?}）"), cx);
            },
            TermEvent::PtyFailure(reason) => {
                self.mark_exited(format!("PTY 故障：{reason}"), cx);
            },
            TermEvent::Exit => {
                self.mark_exited(String::from("会话已结束"), cx);
            },
            // 垂直切片不处理：铃声、cwd 上报、内联图片、OSC133、AI hook 等。
            _ => {},
        }
    }

    /// `Exited` 只对宿主发一次；重复的退出信号（ChildExit 之后必然跟 Exit）只更新文案。
    fn mark_exited(&mut self, message: String, cx: &mut Context<Self>) {
        if self.exited.is_none() {
            self.exited = Some(message);
            cx.emit(TerminalViewEvent::Exited);
        }
        cx.notify();
    }

    /// 让 EventLoop 退出并回收 ConPTY/子进程。幂等：重复调用只会得到发送失败。
    pub fn shutdown(&self) {
        if let Some(session) = &self.session {
            let _ = session.notifier.0.send(Msg::Shutdown);
        }
    }

    fn write_bytes(&self, bytes: Vec<u8>) {
        if let Some(session) = &self.session {
            session.notifier.notify(bytes);
        }
    }

    /// 输入后回到底部并请求重绘。
    fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            {
                let mut term = session.term.lock();
                term.scroll_display(Scroll::Bottom);
                term.selection = None;
            }
            session.notifier.notify(bytes);
        }
        cx.notify();
    }

    /// 元素 prepaint 回写布局：内容矩形与度量交给渲染合同裁定网格。
    /// 网格变化时同步 Term 与 ConPTY；行列不变但像素口径变化也上报 PTY
    /// （Ghostty 规则）；稳态帧 observe 返回 None，零额外开销。
    pub fn set_layout(
        &mut self,
        origin: Point<Pixels>,
        cell_width: Pixels,
        line_height: Pixels,
        content: Size<Pixels>,
        scale: f32,
    ) {
        self.origin = origin;
        self.cell_width = cell_width;
        self.line_height = line_height;
        let metrics = CellMetrics {
            cell_width: cell_width.as_f32(),
            cell_height: line_height.as_f32(),
            scale,
        };
        let Some(change) =
            self.viewports.observe(content.width.as_f32(), content.height.as_f32(), &metrics)
        else {
            return;
        };
        let viewport = change.viewport;
        self.cols = viewport.cols as usize;
        self.rows = viewport.rows as usize;
        self.window_size = viewport.window_size();
        if let Some(session) = &self.session {
            if change.grid_changed {
                session
                    .term
                    .lock()
                    .resize(GridSize { columns: self.cols, screen_lines: self.rows });
            }
            let mut notifier = nebula_terminal::event_loop::Notifier(session.notifier.0.clone());
            notifier.on_resize(viewport.window_size());
        }
    }

    pub fn grid_rows(&self) -> usize {
        self.rows
    }

    pub fn grid_cols(&self) -> usize {
        self.cols
    }

    fn term_mode(&self) -> TermMode {
        self.session.as_ref().map(|s| *s.term.lock().mode()).unwrap_or_default()
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            let text = session.term.lock().selection_to_string();
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        let bytes = if self.term_mode().contains(TermMode::BRACKETED_PASTE) {
            let mut b = b"\x1b[200~".to_vec();
            // 过滤粘贴内容里的收尾哨兵，防止注入截断。
            b.extend_from_slice(normalized.replace("\x1b[201~", "").as_bytes());
            b.extend_from_slice(b"\x1b[201~");
            b
        } else {
            normalized.into_bytes()
        };
        self.write_input(bytes, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.exited.is_some() {
            return;
        }
        let ks = &event.keystroke;
        let mods = &ks.modifiers;

        // 终端惯例快捷键优先于编码器。
        if mods.control && mods.shift {
            match ks.key.as_str() {
                "c" => {
                    self.copy_selection(cx);
                    cx.stop_propagation();
                    return;
                },
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                    return;
                },
                // 应用级快捷键（新建/关闭 Tab）：不编码、不拦截，
                // 让按键冒泡到 workspace 的 action 绑定。
                "t" | "w" => return,
                _ => {},
            }
        }

        let mode = self.term_mode();

        // 回滚快捷键（对齐旧壳默认绑定，仅主屏）：Shift+PageUp/PageDown
        // 翻页、Shift+Home/End 到顶/到底；备用屏下不拦截，交给编码器
        // 把带修饰的键序发给应用（less 自己处理 Shift+PageUp）。
        if mods.shift && !mods.control && !mods.alt && !mode.contains(TermMode::ALT_SCREEN) {
            let scroll = match ks.key.as_str() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                if let Some(session) = &self.session {
                    session.term.lock().scroll_display(scroll);
                }
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }

        if let Some(bytes) = keymap::encode(ks, &mode) {
            self.write_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    /// 像素坐标 → 网格坐标（含滚动偏移）与半格判定。
    fn grid_point(&self, position: Point<Pixels>) -> (TermPoint, Side) {
        let rel_x = (position.x - self.origin.x).as_f32() / self.cell_width.as_f32().max(1.0);
        let rel_y = (position.y - self.origin.y).as_f32() / self.line_height.as_f32().max(1.0);
        let col = (rel_x.floor().max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (rel_y.floor().max(0.0) as usize).min(self.rows.saturating_sub(1));
        let side = if rel_x.fract() > 0.5 { Side::Right } else { Side::Left };
        let display_offset =
            self.session.as_ref().map(|s| s.term.lock().grid().display_offset()).unwrap_or(0);
        (viewport_to_point(display_offset, TermPoint::new(row, Column(col))), side)
    }

    /// 应用是否接管了鼠标（vim/htop 等）。Shift 按住时强制旁路——这是
    /// 终端的通用逃生门：应用吃鼠标时用户仍能选择/复制。
    fn mouse_mode_active(&self, mods: &gpui::Modifiers) -> bool {
        !mods.shift && self.term_mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 把一次鼠标事件按当前协议（SGR/normal/UTF-8）编码上报给应用。
    fn send_mouse_report(
        &mut self,
        position: Point<Pixels>,
        button: u8,
        pressed: bool,
        mods: &gpui::Modifiers,
    ) {
        let (point, _) = self.grid_point(position);
        self.last_report_point = Some(point);
        let mode = self.term_mode();
        let mods =
            mouse_protocol::ReportMods { shift: mods.shift, alt: mods.alt, control: mods.control };
        if let Some(bytes) = mouse_protocol::report(&mode, point, button, pressed, mods) {
            self.write_bytes(bytes);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if self.session.is_none() {
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_LEFT,
                true,
                &event.modifiers,
            );
            return;
        }
        let (point, side) = self.grid_point(event.position);
        let ty = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        if let Some(session) = &self.session {
            let mut term = session.term.lock();
            // Shift+单击扩展既有选区到点击处（原生文本框行为，对齐旧壳）；
            // 其余情况都开新选区。
            let extend = event.modifiers.shift
                && event.click_count == 1
                && term.selection.as_ref().is_some_and(|s| !s.is_empty());
            match term.selection.as_mut() {
                Some(selection) if extend => selection.update(point, side),
                _ => term.selection = Some(Selection::new(ty, point, side)),
            }
        }
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (point, side) = self.grid_point(event.position);
            if let Some(session) = &self.session {
                if let Some(selection) = session.term.lock().selection.as_mut() {
                    selection.update(point, side);
                }
            }
            cx.notify();
            return;
        }
        if !self.mouse_mode_active(&event.modifiers) {
            return;
        }
        // 鼠标模式的移动上报：拖动 = 按钮码+32（需 DRAG 或 MOTION 任一），
        // 无按键纯移动 = 35（仅 MOTION）；同一单元格内的移动不重报。
        let mode = self.term_mode();
        if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
            return;
        }
        let button = match event.pressed_button {
            Some(MouseButton::Left) => mouse_protocol::BUTTON_LEFT + mouse_protocol::DRAG_OFFSET,
            Some(MouseButton::Middle) => {
                mouse_protocol::BUTTON_MIDDLE + mouse_protocol::DRAG_OFFSET
            },
            Some(MouseButton::Right) => mouse_protocol::BUTTON_RIGHT + mouse_protocol::DRAG_OFFSET,
            _ => {
                if !mode.contains(TermMode::MOUSE_MOTION) {
                    return;
                }
                mouse_protocol::MOTION_ONLY
            },
        };
        let (point, _) = self.grid_point(event.position);
        if self.last_report_point == Some(point) {
            return;
        }
        self.send_mouse_report(event.position, button, true, &event.modifiers);
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting && self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_LEFT,
                false,
                &event.modifiers,
            );
            return;
        }
        self.selecting = false;
        // 选中即复制：在抬手时一次性入剪贴板（旧壳同样在 release 复制，
        // 避免拖动过程刷爆剪贴板）。空选区在 copy_selection 内自然短路。
        if self.copy_on_select {
            self.copy_selection(cx);
        }
        cx.notify();
    }

    /// 右键（旧壳 Windows 惯例）：有选区 → 复制并清除；无选区 → 粘贴。
    /// 应用接管鼠标时上报给应用。
    fn on_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if self.session.is_none() {
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_RIGHT,
                true,
                &event.modifiers,
            );
            return;
        }
        let has_selection = self
            .session
            .as_ref()
            .is_some_and(|s| s.term.lock().selection.as_ref().is_some_and(|sel| !sel.is_empty()));
        if has_selection {
            self.copy_selection(cx);
            if let Some(session) = &self.session {
                session.term.lock().selection = None;
            }
            cx.notify();
        } else {
            self.paste(cx);
        }
    }

    fn on_right_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_RIGHT,
                false,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    /// 中键只在鼠标模式下有意义（上报给应用）；旧壳在 Windows 上没有
    /// 中键粘贴路径，这里保持一致。
    fn on_middle_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_MIDDLE,
                true,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    fn on_middle_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_mode_active(&event.modifiers) {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_MIDDLE,
                false,
                &event.modifiers,
            );
        }
        cx.notify();
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_y = event.delta.pixel_delta(self.line_height).y.as_f32();
        self.scroll_px += delta_y;
        let lines = (self.scroll_px / self.line_height.as_f32().max(1.0)).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * self.line_height.as_f32();
        let mode = self.term_mode();
        // 应用接管鼠标时滚轮也归应用（htop 列表滚动）；Shift 旁路回本地回滚。
        if !event.modifiers.shift && mode.intersects(TermMode::MOUSE_MODE) {
            let code =
                if lines > 0 { mouse_protocol::WHEEL_UP } else { mouse_protocol::WHEEL_DOWN };
            for _ in 0..lines.unsigned_abs() {
                self.send_mouse_report(event.position, code, true, &event.modifiers);
            }
            cx.notify();
            return;
        }
        let Some(session) = &self.session else { return };
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // 备用屏（less/vim）：滚轮翻译成方向键。
            let seq: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                if lines > 0 { b"\x1bOA" } else { b"\x1bOB" }
            } else if lines > 0 {
                b"\x1b[A"
            } else {
                b"\x1b[B"
            };
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs() {
                bytes.extend_from_slice(seq);
            }
            session.notifier.notify(bytes);
        } else {
            session.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }

    fn marked_utf16_len(&self) -> usize {
        self.marked_text.as_deref().map(|t| t.encode_utf16().count()).unwrap_or(0)
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // 兜底清理：无论视图以何种路径销毁（关 Tab、关窗口、退出应用），
        // 都保证 PTY 线程和子进程被回收。
        self.shutdown();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // 终端没有文本框语义的选区；返回折叠选区让 IME 把组合文本插在光标处。
        let len = self.marked_utf16_len();
        Some(UTF16Selection { range: len..len, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked_text.as_ref().map(|_| 0..self.marked_utf16_len())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() && self.exited.is_none() {
            self.write_input(text.as_bytes().to_vec(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = if new_text.is_empty() { None } else { Some(new_text.to_string()) };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.ime_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .id("nebula-terminal")
            .size_full()
            .overflow_hidden()
            .bg(self.palette.background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_right_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_middle_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll));

        if let Some(error) = &self.error {
            root = root.child(div().p_4().text_color(gpui::red()).child(error.clone()));
        } else {
            root = root.child(TerminalElement::new(cx.entity()));
            if let Some(exited) = &self.exited {
                root = root.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(gpui::opaque_grey(0.2, 0.9))
                        .text_sm()
                        .child(exited.clone()),
                );
            }
        }
        root
    }
}
