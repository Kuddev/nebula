use super::*;

impl TerminalView {
    pub(in crate::gpui_shell::terminal) fn scrollbar_thumb(
        &self,
        display_offset: usize,
        history: usize,
    ) -> Option<Bounds<Pixels>> {
        let screen = self.rows;
        let total = history + screen;
        if display_offset == 0 || screen == 0 || total <= screen {
            return None;
        }
        let track_top = self.origin.y.as_f32();
        let track_h = self.line_height.as_f32() * screen as f32;
        if track_h <= 1.0 {
            return None;
        }
        let min_thumb = SCROLLBAR_MIN_THUMB.min(track_h);
        let thumb_h = (track_h * screen as f32 / total as f32).clamp(min_thumb, track_h);
        // 视口顶端之上还剩多少行历史：0 = 拉到最顶，history = 贴着底部。
        let above = (history - display_offset) as f32;
        let max_y = (track_h - thumb_h).max(0.0);
        let thumb_y = track_top + (track_h * above / total as f32).clamp(0.0, max_y);
        // 浮在网格右缘（overlay 风格：不占列宽、不画轨道）。
        let grid_right = self.origin.x.as_f32() + self.cell_width.as_f32() * self.cols as f32;
        Some(Bounds::new(
            point(px(grid_right - SCROLLBAR_W), px(thumb_y)),
            gpui::size(px(SCROLLBAR_W), px(thumb_h)),
        ))
    }

    pub(in crate::gpui_shell::terminal) fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    /// 回滚历史行数：拇指高度与拖拽反算的分母来源。
    pub(in crate::gpui_shell::terminal) fn history_size(&self) -> usize {
        self.session.as_ref().map(|s| s.term.lock().history_size()).unwrap_or(0)
    }

    /// 命中滚动条则返回「指针在拇指内的 y 偏移」；落在轨道上（拇指上下方）
    /// 返回半个拇指高＝拇指跳到指针处居中（旧壳 `scrollbar_grab` 同合同）。
    pub(super) fn scrollbar_grab(
        &self,
        position: Point<Pixels>,
        display_offset: usize,
        history: usize,
    ) -> Option<f32> {
        let thumb = self.scrollbar_thumb(display_offset, history)?;
        let x = position.x.as_f32();
        let thumb_x = thumb.origin.x.as_f32();
        if x < thumb_x - SCROLLBAR_SLOP || x > thumb_x + SCROLLBAR_W + SCROLLBAR_SLOP {
            return None;
        }
        let track_top = self.origin.y.as_f32();
        let track_h = self.line_height.as_f32() * self.rows as f32;
        let y = position.y.as_f32();
        if y < track_top || y > track_top + track_h {
            return None;
        }
        let thumb_top = thumb.origin.y.as_f32();
        let thumb_h = thumb.size.height.as_f32();
        if y >= thumb_top && y <= thumb_top + thumb_h {
            Some(y - thumb_top)
        } else {
            Some(thumb_h / 2.0)
        }
    }

    /// 把拖动中的指针 y 反算回 `display_offset`——`scrollbar_thumb` 那套定位
    /// 数学的逆运算（旧壳 `scrollbar_target_offset` 同合同）。
    pub(super) fn scrollbar_target_offset(&self, y: f32, grab: f32, history: usize) -> usize {
        if history == 0 {
            return 0;
        }
        let total = (history + self.rows) as f32;
        let track_top = self.origin.y.as_f32();
        let track_h = (self.line_height.as_f32() * self.rows as f32).max(1.0);
        let above =
            ((y - grab - track_top) / track_h * total).round().clamp(0.0, history as f32) as usize;
        history - above
    }

    /// 一次取锁读出滚动条几何要的两个量：当前回滚位置与历史行数。
    pub(super) fn scroll_state(&self) -> (usize, usize) {
        self.session
            .as_ref()
            .map(|s| {
                let term = s.term.lock();
                (term.grid().display_offset(), term.history_size())
            })
            .unwrap_or((0, 0))
    }

    /// 把回滚位置落到 `target`（滚动条拖拽的落点提交）。
    pub(super) fn scroll_to_offset(&self, target: usize, current: usize) {
        if target == current {
            return;
        }
        if let Some(session) = &self.session {
            session.term.lock().scroll_display(Scroll::Delta(target as i32 - current as i32));
        }
    }

    /// 像素坐标 → 网格坐标（含滚动偏移）与半格判定。
    pub(super) fn grid_point(&self, position: Point<Pixels>) -> (TermPoint, Side) {
        let rel_x = (position.x - self.origin.x).as_f32() / self.cell_width.as_f32().max(1.0);
        let rel_y = (position.y - self.origin.y).as_f32() / self.line_height.as_f32().max(1.0);
        let col = (rel_x.floor().max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (rel_y.floor().max(0.0) as usize).min(self.rows.saturating_sub(1));
        let side = if rel_x.fract() > 0.5 { Side::Right } else { Side::Left };
        let Some(session) = self.session.as_ref() else {
            return (TermPoint::new(Line(row as i32), Column(col)), side);
        };
        let term = session.term.lock();
        let viewport_origin = term.viewport_origin_for(self.rows);
        let point = term.visual_viewport_to_point(self.rows, TermPoint::new(row, Column(col)));
        self.math.source_point(point, side, viewport_origin)
    }

    pub(super) fn selection_is_empty(&self) -> bool {
        self.session.as_ref().is_none_or(|session| {
            session.term.lock().selection.as_ref().is_none_or(|selection| selection.is_empty())
        })
    }

    pub(super) fn update_link_hover(
        &mut self,
        position: Point<Pixels>,
        mods: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let next = if self.selecting {
            None
        } else {
            self.session.as_ref().and_then(|session| {
                let (point, _) = self.grid_point(position);
                let term = session.term.lock();
                super::super::osc_links::highlighted_at(&term, &self.hint_config, point, mods)
                    .and_then(|hint| {
                        super::super::osc_links::hover_from_hint(&term, hint, self.rows, self.cols)
                    })
            })
        };
        let changed = match (&self.link_hover, &next) {
            (None, None) => false,
            (Some(prev), Some(new)) => {
                prev.preview != new.preview
                    || prev.anchor_row != new.anchor_row
                    || prev.anchor_col != new.anchor_col
                    || prev.hint != new.hint
            },
            _ => true,
        };
        if changed {
            self.link_hover = next;
            cx.notify();
        }
    }

    pub(super) fn clear_link_hover(&mut self, cx: &mut Context<Self>) {
        if self.link_hover.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn try_open_hovered_link(&self, cx: &Context<Self>) {
        let Some(hover) = self.link_hover.as_ref() else { return };
        let Some(session) = self.session.as_ref() else { return };
        let term = session.term.lock();
        super::super::osc_links::open_hint(&hover.hint, &term, cx);
    }

    /// 应用是否接管了鼠标（vim/htop 等）。Shift 按住时强制旁路——这是
    /// 终端的通用逃生门：应用吃鼠标时用户仍能选择/复制。
    pub(super) fn mouse_mode_active(&self, mods: &gpui::Modifiers) -> bool {
        !mods.shift && self.term_mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 把一次鼠标事件按当前协议（SGR/normal/UTF-8）编码上报给应用。
    pub(super) fn send_mouse_report(
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

    /// 像素命中与绘制共用同一份候选几何，截短列表或上翻时也不会点错行。
    pub(super) fn completion_popup_geometry(
        &self,
    ) -> Option<super::super::element::CompletionPopupLayout> {
        if !suggest::popup_active(&self.suggest) {
            return None;
        }
        let (cursor_row, cursor_col) = self.suggest_anchor?;
        completion_popup_layout(
            &self.suggest.completion_items,
            self.suggest.completion_selected,
            self.completion_viewport.offset,
            cursor_row,
            cursor_col,
            self.rows,
            self.cols,
            self.cell_width,
            self.line_height,
        )
    }

    pub(super) fn completion_popup_hit(&self, position: Point<Pixels>) -> Option<usize> {
        let popup = self.completion_popup_geometry()?;
        let content = popup.content_bounds(self.origin);
        if !content.contains(&position) {
            return None;
        }
        let row = ((position.y - content.origin.y).as_f32() / popup.row_height.as_f32()) as usize;
        let index = popup.offset + row;
        (index < self.suggest.completion_items.len()).then_some(index)
    }

    fn completion_popup_contains(&self, position: Point<Pixels>) -> bool {
        self.completion_popup_geometry()
            .is_some_and(|popup| popup.panel_bounds(self.origin).contains(&position))
    }

    fn completion_popup_scrollbar_grab(&self, position: Point<Pixels>) -> Option<f32> {
        let popup = self.completion_popup_geometry()?;
        let (track, thumb) =
            popup.scrollbar_bounds(self.origin, self.suggest.completion_items.len())?;
        let content = popup.content_bounds(self.origin);
        if position.x < content.right()
            || position.x >= content.right() + px(5.0)
            || position.y < track.origin.y
            || position.y >= track.bottom()
        {
            return None;
        }
        Some(if position.y >= thumb.origin.y && position.y < thumb.bottom() {
            (position.y - thumb.origin.y).as_f32()
        } else {
            thumb.size.height.as_f32() * 0.5
        })
    }

    fn scroll_completion_popup_to(&mut self, position: Point<Pixels>, released: bool) -> bool {
        if self.completion_viewport.scrollbar_grab.is_none() {
            return false;
        }
        let Some(popup) = self.completion_popup_geometry() else {
            self.completion_viewport.scrollbar_grab = None;
            return true;
        };
        let count = self.suggest.completion_items.len();
        let Some((track, thumb)) = popup.scrollbar_bounds(self.origin, count) else {
            self.completion_viewport.scrollbar_grab = None;
            return true;
        };
        self.completion_viewport.drag_scrollbar(
            (position.y - track.origin.y).as_f32(),
            (track.size.height - thumb.size.height).as_f32(),
            count,
            popup.rows,
            released,
        )
    }

    pub(in crate::gpui_shell::terminal) fn move_completion_popup_scrollbar(
        &mut self,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let handled = if event.pressed_button == Some(MouseButton::Left) {
            self.scroll_completion_popup_to(event.position, false)
        } else {
            self.completion_viewport.scrollbar_grab.take().is_some()
        };
        if handled {
            cx.notify();
        }
        handled
    }

    pub(in crate::gpui_shell::terminal) fn finish_completion_popup_scrollbar(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let handled = self.scroll_completion_popup_to(position, true);
        if handled {
            cx.notify();
        }
        handled
    }

    /// 接受当前候选时只写入补全余量。即使余量为空也算已处理，Enter 不能
    /// 继续透传成一次命令执行。
    pub(super) fn accept_completion_popup(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(insert) = suggest::popup_take(&mut self.suggest) else { return false };
        self.completion_viewport.clear();
        let before = if self.suggest.screen_line.is_empty() {
            self.suggest.line_buf.clone()
        } else {
            self.suggest.screen_line.clone()
        };
        for c in insert.chars() {
            crate::display::nebula_input_char(&mut self.suggest, c);
        }
        self.suggest.completion_suppressed_line = Some(format!("{before}{insert}"));
        if !insert.is_empty() {
            self.write_user_text(insert.clone(), false, insert.into_bytes(), cx);
        }
        true
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if self.session.is_none() {
            return;
        }
        if event.button == MouseButton::Left
            && let Some(grab) = self.completion_popup_scrollbar_grab(event.position)
        {
            self.completion_viewport.scrollbar_grab = Some(grab);
            self.scroll_completion_popup_to(event.position, false);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Left
            && let Some(index) = self.completion_popup_hit(event.position)
        {
            self.suggest.completion_selected = Some(index);
            if self.accept_completion_popup(cx) {
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }
        if self.completion_popup_contains(event.position) {
            cx.stop_propagation();
            return;
        }
        // 面板外第一次点击只负责关闭浮层，避免同一击又在终端里开选区或触发
        // TUI 鼠标协议；缓存当前行，行不变时浮层不会下一帧原地复活。
        if suggest::popup_dismiss(&mut self.suggest) {
            self.completion_viewport.clear();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // 滚动条是壳的控件，命中优先于选区和鼠标上报——否则在开了鼠标追踪的
        // TUI 里（codex/vim）根本抓不住条。贴底时拇指为 None，正常操作零影响。
        let (display_offset, history) = self.scroll_state();
        if let Some(grab) = self.scrollbar_grab(event.position, display_offset, history) {
            self.scrollbar_drag = Some(grab);
            let target = self.scrollbar_target_offset(event.position.y.as_f32(), grab, history);
            self.scroll_to_offset(target, display_offset);
            cx.notify();
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
        if event.modifiers.control && event.click_count == 1 {
            let (point, _) = self.grid_point(event.position);
            let hit = self.session.as_ref().is_some_and(|session| {
                let term = session.term.lock();
                super::super::osc_links::highlighted_at(
                    &term,
                    &self.hint_config,
                    point,
                    &event.modifiers,
                )
                .is_some()
            });
            if hit {
                if let Some(session) = &self.session {
                    session.term.lock().selection = None;
                }
                self.selecting = false;
                self.pending_link_open = true;
                self.update_link_hover(event.position, &event.modifiers, cx);
                return;
            }
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
        // 自动回滚在这里就上弦，而不是等第一次「拖出网格」的移动事件：指针
        // 一旦离开元素 hitbox 就不再有 move 送进来，一甩到顶的拖法最后一个
        // 元素内事件往往还在网格中间，等 move 判定＝永远等不到（用户报的
        // 「甩到顶不动，往下再往上才滚」）。tick 自己看指针位置，网格内是
        // 一次比较就返回的空转。
        self.start_selection_scroll(window, cx);
        cx.notify();
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.move_completion_popup_scrollbar(event, cx) {
            cx.stop_propagation();
            return;
        }
        if event.pressed_button.is_none() {
            let index = self.completion_popup_hit(event.position);
            if self
                .completion_viewport
                .hover((event.position.x.as_f32(), event.position.y.as_f32()), index)
            {
                cx.notify();
            }
            if self.completion_popup_contains(event.position) {
                self.clear_link_hover(cx);
                cx.stop_propagation();
                return;
            }
        }
        // 拖条中：指针的整段轨迹都归滚动条，不进选区也不上报。
        if let Some(grab) = self.scrollbar_drag {
            if event.pressed_button != Some(MouseButton::Left) {
                self.scrollbar_drag = None;
                return;
            }
            let (display_offset, history) = self.scroll_state();
            let target = self.scrollbar_target_offset(event.position.y.as_f32(), grab, history);
            self.scroll_to_offset(target, display_offset);
            cx.notify();
            return;
        }
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
            // 越界回滚不在这里判：链在按下时就起了（见 `start_selection_scroll`），
            // 指针是否出了网格由 tick 自己看——一甩到顶的拖法最后一个元素内
            // 事件往往还在网格中间，靠 move 判定就永远起不来。
            cx.notify();
            return;
        }
        if self.mouse_mode_active(&event.modifiers) {
            self.clear_link_hover(cx);
            // 鼠标模式的移动上报：拖动 = 按钮码+32（需 DRAG 或 MOTION 任一），
            // 无按键纯移动 = 35（仅 MOTION）；同一单元格内的移动不重报。
            let mode = self.term_mode();
            if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                return;
            }
            let button = match event.pressed_button {
                Some(MouseButton::Left) => {
                    mouse_protocol::BUTTON_LEFT + mouse_protocol::DRAG_OFFSET
                },
                Some(MouseButton::Middle) => {
                    mouse_protocol::BUTTON_MIDDLE + mouse_protocol::DRAG_OFFSET
                },
                Some(MouseButton::Right) => {
                    mouse_protocol::BUTTON_RIGHT + mouse_protocol::DRAG_OFFSET
                },
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
            return;
        }
        self.update_link_hover(event.position, &event.modifiers, cx);
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.finish_completion_popup_scrollbar(event.position, cx) {
            cx.stop_propagation();
            return;
        }
        self.stop_selection_scroll();
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
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
        let open_link = (self.pending_link_open || event.modifiers.control)
            && self.selection_is_empty()
            && self.link_hover.is_some();
        if open_link {
            self.try_open_hovered_link(cx);
        } else if self.copy_on_select {
            self.copy_selection(false, window, cx);
        }
        self.pending_link_open = false;
        cx.notify();
    }

    /// 指针在终端之外松手（拖到 tab 栏、别的 pane，或按下后甩出窗口——
    /// Windows 平台按下即 `SetCapture`，事件仍会送达）。GPUI 的 `on_mouse_up`
    /// 只在 hitbox 命中时派发，没有这条兜底，`selecting` 会一直停在按下态：
    /// 自动回滚停不下来，下一次移动还会继续改选区。
    pub(super) fn on_mouse_up_out(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.finish_completion_popup_scrollbar(event.position, cx) {
            cx.stop_propagation();
            return;
        }
        self.stop_selection_scroll();
        let dragging_scrollbar = self.scrollbar_drag.take().is_some();
        if !self.selecting {
            if dragging_scrollbar {
                cx.notify();
            }
            return;
        }
        self.selecting = false;
        self.pending_link_open = false;
        // 指针不在终端上，链接打开不该发生；只补选中即复制这一条收尾。
        if self.copy_on_select {
            self.copy_selection(false, window, cx);
        }
        cx.notify();
    }

    /// 网格的上下边界（窗口坐标），自动回滚的触发线。旧壳用的是
    /// `padding_y` 与文本区底边，这里同义：卡内 8px 呼吸边距之内就已经
    /// 算「出了网格」。
    pub(super) fn grid_vertical_bounds(&self) -> (f32, f32) {
        let top = self.origin.y.as_f32();
        (top, top + self.line_height.as_f32() * self.rows as f32)
    }

    pub(super) fn selection_scroll_lines_at(&self, y: f32) -> i32 {
        let (top, bottom) = self.grid_vertical_bounds();
        selection_scroll_lines(y, top, bottom, self.rows as i32)
    }

    /// 选区拖拽全程常驻的自动回滚链：按下即起，松手才停。
    ///
    /// 不用「移动越界才起链」是因为那条判据依赖 move 事件送得到——指针离开
    /// 元素 hitbox 后 GPUI 就不再派发，一甩到顶的拖法最后一个元素内事件通常
    /// 还在网格中间，链于是永远起不来。常驻链把「是否越界」挪进 tick，指针
    /// 在网格内时它只是一次比较加返回，不重绘也不取 Term 锁。
    pub(super) fn start_selection_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.selection_scroll_active {
            return;
        }
        self.selection_scroll_active = true;
        self.selection_scroll_epoch = self.selection_scroll_epoch.wrapping_add(1);
        let epoch = self.selection_scroll_epoch;
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                executor.timer(SELECTION_SCROLL_INTERVAL).await;
                let keep = this.update_in(cx, |view, window, cx| {
                    view.selection_scroll_tick(epoch, window, cx)
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn stop_selection_scroll(&mut self) {
        if !self.selection_scroll_active {
            return;
        }
        self.selection_scroll_active = false;
        self.selection_scroll_epoch = self.selection_scroll_epoch.wrapping_add(1);
    }

    /// 一次自动回滚 tick：越界就滚 N 行，并把选区末端跟到指针**此刻**所在
    /// 的格；没越界就什么都不做，等下一次 tick。
    ///
    /// 位置每 tick 从 `window.mouse_position()` 现取而不是沿用最后一次 move：
    /// 指针停在元素外（tab 栏、下方的 pane）时根本不会再有 move 事件，沿用
    /// 旧值就等于速度被钉死在贴边那一档，旧壳「越拖越快」的手感会丢。
    ///
    /// 返回 `false` 表示这条定时器链该结束（松手、换会话，或被新链顶掉）。
    pub(super) fn selection_scroll_tick(
        &mut self,
        epoch: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if epoch != self.selection_scroll_epoch || !self.selection_scroll_active {
            return false;
        }
        if !self.selecting || self.session.is_none() {
            self.stop_selection_scroll();
            return false;
        }
        // 按下但还没拖出一个格：旧壳同样要求选区非空才滚，否则在卡片边距上
        // 按一下不动就会开始翻历史。
        if self.selection_is_empty() {
            return true;
        }
        let position = window.mouse_position();
        let lines = self.selection_scroll_lines_at(position.y.as_f32());
        if lines == 0 {
            return true;
        }
        if let Some(session) = &self.session {
            session.term.lock().scroll_display(Scroll::Delta(lines));
        }
        // 滚动换了视口到绝对行的映射，选区末端必须按滚动后的网格重算——
        // 否则视口滑过去了，选区还钉在原来那几行。两段各自取一次锁：
        // `grid_point` 内部还要读 Term，握着锁进去会自锁。
        let (point, side) = self.grid_point(position);
        if let Some(session) = &self.session
            && let Some(selection) = session.term.lock().selection.as_mut()
        {
            selection.update(point, side);
        }
        cx.notify();
        true
    }

    /// 右键行为：有选区直接复制，无选区直接粘贴。
    /// Ctrl+右键保留选区菜单，供显式调用 Send to Chat。
    pub(super) fn on_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if suggest::popup_dismiss(&mut self.suggest) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.session.is_none() {
            return;
        }
        if self.mouse_mode_active(&event.modifiers) && !event.modifiers.control {
            self.send_mouse_report(
                event.position,
                mouse_protocol::BUTTON_RIGHT,
                true,
                &event.modifiers,
            );
            return;
        }
        let selected_text = self
            .session
            .as_ref()
            .and_then(|session| session.term.lock().selection_to_string())
            .filter(|text| !text.is_empty());
        if let Some(text) = selected_text {
            if event.modifiers.control {
                cx.emit(TerminalViewEvent::SelectionContextMenuRequested {
                    position: event.position,
                    text,
                });
            } else {
                self.copy_selection(true, window, cx);
            }
            cx.stop_propagation();
        } else {
            self.paste(window, cx);
            cx.stop_propagation();
        }
    }

    pub(super) fn on_right_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mouse_mode_active(&event.modifiers) && !event.modifiers.control {
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
    pub(super) fn on_middle_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        cx.emit(TerminalViewEvent::FocusRequested);
        if suggest::popup_dismiss(&mut self.suggest) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
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

    pub(super) fn on_middle_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_y = event.delta.pixel_delta(self.line_height).y.as_f32();
        // 旧壳 `mouse_wheel_input`：Ctrl+滚轮先于一切滚动消费者，一步 1
        // 逻辑像素，钳在 4–64。设置页步进会写盘；这里同样写 `font_size=`，
        // 让下次启动跟上（旧壳只在其它设置落盘时顺便带走当前字号）。
        if event.modifiers.control && !event.modifiers.alt && delta_y != 0.0 {
            let step = if delta_y > 0.0 { 1.0 } else { -1.0 };
            self.zoom_font_size(step, cx);
            cx.stop_propagation();
            return;
        }
        if self.completion_popup_contains(event.position)
            && let Some(popup) = self.completion_popup_geometry()
        {
            let delta = event.delta.pixel_delta(popup.row_height).y.as_f32();
            if self.completion_viewport.scroll(
                delta,
                popup.row_height.as_f32(),
                self.suggest.completion_items.len(),
                popup.rows,
            ) {
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
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
}
