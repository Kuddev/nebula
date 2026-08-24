use super::*;

impl NebulaWorkspace {
    /// 侧栏等宽标签的 cell 宽：与终端元素同一套度量法（塑形一个 "M" 取
    /// advance），列数换算与省略号都建立在它上面。字体缺失时回落 0.6em，
    /// 只影响截断位置、不会画错。
    fn sidebar_cell_width(&self, window: &mut Window, family: &SharedString, size_px: f32) -> f32 {
        let shaped = window.text_system().shape_line(
            SharedString::new_static("M"),
            px(size_px),
            &[gpui::TextRun {
                len: 1,
                font: gpui::font(family.clone()),
                color: gpui::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let width = f32::from(shaped.width);
        if width > 0.5 { width } else { size_px * 0.6 }
    }

    /// 旧壳 `icons::push_spinner` 的 canvas 复刻：暗轨道 + 绕行亮弧（占
    /// 整圈 1/3），半径 5.5、笔画 0.30r，中性灰（spinner 表达「还在跑」，
    /// 不抢品牌色）。phase 由 render 侧的帧循环推进。
    pub(super) fn spinner(phase: f32, track: gpui::Rgba, head: gpui::Rgba) -> impl IntoElement {
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let side = f32::from(bounds.size.width);
                let (cx, cy) = (ox + side * 0.5, oy + side * 0.5);
                let radius = 5.5_f32;
                let stroke = (radius * 0.30).max(1.0);
                // 点铺在轨道中线上：外缘正好落在 radius 上。
                let mid = radius - stroke * 0.5;
                // 与旧壳 `push_spinner` 完全同式：相邻圆点约重叠 50%，既不
                // 留珠链缝，也不以过密叠加制造额外模糊。
                let steps =
                    ((mid * std::f32::consts::TAU / (stroke * 0.5)).ceil() as usize).clamp(24, 96);
                const ARC: f32 = 0.34;
                for step in 0..steps {
                    let at = step as f32 / steps as f32;
                    let angle = at * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                    let behind = (phase - at).rem_euclid(1.0);
                    let t = (1.0 - behind / ARC).clamp(0.0, 1.0);
                    // 旧壳在 RGB 域插值，不在 HSL 域绕色相；两端均已预合成
                    // 为不透明色，圆点相交处不会积累 alpha。
                    let mix = |a: f32, b: f32| a + (b - a) * t;
                    let c: gpui::Hsla = gpui::Rgba {
                        r: mix(track.r, head.r),
                        g: mix(track.g, head.g),
                        b: mix(track.b, head.b),
                        a: 1.0,
                    }
                    .into();
                    let x0 = cx + mid * angle.cos() - stroke * 0.5;
                    let y0 = cy + mid * angle.sin() - stroke * 0.5;
                    // 旧壳不对组成圆环的每个点单独做像素吸附。圆周点本来就落
                    // 在连续坐标上，强制取整会让相邻点忽近忽远、半径跳变，低
                    // DPI 下尤其容易显成锯齿珠链；交给 GPUI 统一抗锯齿才与
                    // `icons::push_spinner` 的几何一致。
                    window.paint_quad(
                        gpui::fill(
                            Bounds::new(gpui::point(px(x0), px(y0)), size(px(stroke), px(stroke))),
                            c,
                        )
                        .corner_radii(px(stroke * 0.5)),
                    );
                }
            },
        )
        .size(px(11.0))
    }

    pub(super) fn tab_presentation(&self, ix: usize, cx: &App, dark: bool) -> TabPresentation {
        let active = ix == self.active;
        let title = self.tab_title(ix, cx);
        let is_settings = self.tabs[ix].is_settings();
        let is_terminal = self.tabs[ix].is_terminal();
        let pane_count = match &self.tabs[ix] {
            WorkspaceTab::Terminal { panes, .. } => panes.len(),
            _ => 0,
        };
        let (program, activity) = self.tabs[ix]
            .focused_view()
            .map(|entity| {
                let view = entity.read(cx);
                let program = view
                    .running_program
                    .clone()
                    .or_else(|| view.ai_session.as_ref().map(|identity| identity.source.clone()))
                    .or_else(|| view.ssh_destination.as_ref().map(|_| "ssh".to_owned()));
                (program, view.sidebar_activity())
            })
            .unwrap_or((None, SidebarActivity::Idle));
        let activity = if !active && self.meta(ix).has_bell && activity == SidebarActivity::Idle {
            SidebarActivity::Done
        } else {
            activity
        };
        let logo_image = program
            .as_deref()
            .and_then(crate::display::ai_logo_for_program)
            .and_then(|logo| self.sidebar_logo_images.get(&(logo, dark)).cloned());
        let program_glyph = program
            .as_deref()
            .filter(|_| logo_image.is_none())
            .map(crate::display::program_icon)
            .or_else(|| match &self.tabs[ix] {
                WorkspaceTab::Document { .. } => Some("\u{eb1d}"),
                WorkspaceTab::Code { view } => {
                    Some(crate::display::side_panel::file_type_icon(&view.read(cx).title))
                },
                WorkspaceTab::Image { view } => {
                    Some(crate::display::side_panel::file_type_icon(&view.read(cx).title))
                },
                _ => None,
            });
        let meta = self.meta(ix);
        // 分屏 tab 不画 shell 短标：一个 tab 里的 N 个 pane 完全可能跑着不同
        // 的 shell，只贴其中一个（聚焦那个）是误导；数量胶囊「这是一组」才是
        // 此时该占这个槽位的信息。顺带把 28px 让回标题——顶栏挤到 120px 时，
        // 图标+胶囊+短标三样一起上，标题只剩两三个字符。
        let shell_tag = (is_terminal && activity == SidebarActivity::Idle && pane_count <= 1)
            .then_some(meta.shell_tag.clone())
            .flatten()
            .filter(|tag| !tag.is_empty());
        let renaming = self
            .tab_rename
            .as_ref()
            .filter(|rename| rename.ix == ix)
            .map(|rename| rename.input.clone());
        TabPresentation {
            title,
            is_settings,
            activity,
            logo_image,
            program_glyph,
            shell_tag,
            color: meta.color,
            renaming,
            pane_count,
        }
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // 数量 chip 数字：旧壳独用 ink_faint（比 ink_dim 再暗一档），chip 背
        // 景 surface 洗色不变——数字不该和标题抢层级。
        let faint = crate::gpui_shell::theme::faint_ink(cx);
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = theme.list_hover;
        let dark = theme.is_dark();
        // 运行程序图标是 Nerd Font 字位，chrome 的 UI 字体没有——用终端等
        // 宽字体渲染（旧壳 chrome 同理，其内置字体本就带全部图标字位）。
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let mono_family: SharedString = settings
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Cascadia Mono"))
            .into();
        // 旧壳合同（display/mod.rs `ui_font_px`）：chrome 锚定**配置字号**
        // （nebula.toml `font.size` 默认 11.25pt = 15px）。终端的持久化缩放
        // （`font_size=` 键）只影响终端网格，侧栏不得跟着变粗/变大；
        // 固定 14px 的旧毛病（比旧壳小一号）也不能回潮。
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        // 等宽 advance 的唯一事实源：与终端元素同款——塑形一个 "M" 量出来，
        // 而不是按 0.6em 猜。列数换算和省略号都建立在这个数上。
        let cell_w = self.sidebar_cell_width(window, &mono_family, label_px);
        // 旧壳标题是同一个 tracked run：`"{chevron}  TABS"`。T 的起点
        // 因而位于箭头起点之后三个完整字位（箭头自身 + 两个空格），不是
        // Tailwind `gap_2` 的固定 8px。按标题字号实测 advance，再补回旧壳
        // 0.65px/字的 tracking，字号和 DPI 改变时呼吸位仍保持同一比例。
        let section_title_cell_w =
            self.sidebar_cell_width(window, &mono_family, label_px * SIDEBAR_TITLE_SCALE);
        let tabs_disclosure_slot_w = (section_title_cell_w + 0.65) * 3.0;

        // 受约束拖拽的渲染参数：激活后被拖行骑指针位移，落点槽位由位移换算。
        let drag = self
            .tab_drag
            .as_ref()
            .filter(|d| d.active && d.axis == TabDragAxis::Vertical)
            .map(|d| (d.source, Self::drag_slot(d, self.tabs.len()), d.offset));

        // 本次渲染里是否有「运行中」行（spinner 帧循环的开关）。
        let items_running = std::cell::Cell::new(false);
        // 折叠只裁剪槽位，不改窗口算法：旧壳 `tabs_avail` 与 `tabs_open`
        // 分开——折起来时行矩形为零，但可用高度仍按面板剩余算。
        let (tabs_scroll, tabs_show) = self.tabs_visible_window();
        // 行的确定宽度：侧栏宽 − 侧栏 p_2 两边 − 列表右侧滚动条留白。
        // 与下面 `label_avail` 同一份减法口径，两者不能各算一套。
        let row_w = (self.sidebar_width - 16.0 - tab_scroll::TAB_SCROLL_GUTTER).max(1.0);
        let items = (0..self.tabs.len())
            .filter(|&ix| tab_scroll::index_visible(ix, tabs_scroll, tabs_show))
            .map(|ix| {
                let active = ix == self.active;
                let TabPresentation {
                    title,
                    is_settings,
                    activity,
                    logo_image,
                    program_glyph,
                    shell_tag,
                    color: tab_color,
                    renaming,
                    pane_count,
                } = self.tab_presentation(ix, cx, dark);
                let hover_group: SharedString = format!("sidebar-tab-hover-{ix}").into();
                // 可用列数 = （行宽 − 行内 px_2 − 行内 gap − 状态槽 − 行首图标槽）
                // ÷ cell 宽。基准取上面的 `row_w`（已扣掉侧栏 p_2 与滚动条留白），
                // 与行的实际宽度同源——否则算出的列数会比行能容纳的多出一格，
                // 截断后的标题反过来把行撑开。省略号由旧壳同一份
                // `truncate_tab_label` 追加，两壳的裁切位置因此一致。
                // 行首图标槽只有一个：身份图标（设置 / AI logo / 程序字位）优先，
                // 都没有时分屏标记才补位。三者互斥，所以扣一份宽即可。
                let has_program_glyph = program_glyph.is_some();
                let has_icon =
                    is_settings || logo_image.is_some() || has_program_glyph || pane_count > 1;
                let label_avail = row_w
                    - 16.0
                    - TAB_STATUS_SLOT_W
                    - 8.0
                    - if has_icon { TAB_LABEL_ICON_W + 8.0 } else { 0.0 }
                    - if pane_count > 1 { pane_header::split_badge_slot_w(label_px) } else { 0.0 };
                let label_cols = (label_avail / cell_w).floor().max(1.0) as usize;
                let title: SharedString =
                    crate::display::truncate_tab_label(&title, label_cols).into();
                let cross_window_drag = self.cross_window_drag_payload(ix, cx);
                // 用户明确设置过的标签色：行左侧一条竖光条（旧壳 strip，位置与
                // 尺寸同源：左内缩 4、上下各留 7、宽 2.5）。默认标签不占这层
                // 视觉层级。
                let strip = tab_color.map(|color| gpui::Rgba {
                    r: color.r as f32 / 255.0,
                    g: color.g as f32 / 255.0,
                    b: color.b as f32 / 255.0,
                    a: 1.0,
                });
                let status_color = if active { active_fg } else { muted };
                let resting_status: Option<gpui::AnyElement> = match activity {
                    SidebarActivity::Running => {
                        items_running.set(true);
                        let (track, head) =
                            crate::gpui_shell::theme::sidebar_spinner_colors(cx, active);
                        Some(Self::spinner(self.spinner_phase, track, head).into_any_element())
                    },
                    // 回合完成、等下一条指令：旧壳蓝点语义——不转圈，留一个
                    // 「有结果没看」的痕迹。
                    SidebarActivity::Done => Some(
                        div().size(px(6.0)).rounded_full().bg(theme.primary).into_any_element(),
                    ),
                    // 停在授权/提问上：比「完成」更强，必须换形状而不是换色
                    // （旧壳教训：两态共用圆点在界面上根本分不出来）。
                    SidebarActivity::Attention => Some(
                        Icon::new(IconName::TriangleAlert)
                            .xsmall()
                            .text_color(theme.warning)
                            .into_any_element(),
                    ),
                    SidebarActivity::Failed => Some(
                        Icon::new(IconName::CircleX)
                            .xsmall()
                            .text_color(theme.danger)
                            .into_any_element(),
                    ),
                    SidebarActivity::Idle => shell_tag.map(|tag| {
                        div()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px * SIDEBAR_TAG_SCALE))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(status_color)
                            .child(tag)
                            .into_any_element()
                    }),
                };
                // 三类行位移（旧壳 tab_drag_draw_y 的语义）：被拖行骑指针，
                // 源与落点之间的行向反方向让一个槽位，其余不动。存储顺序在
                // 拖拽期间不变，释放时一次性提交。
                let (dragged, shift) = match drag {
                    Some((src, _, _)) if ix == src => (true, 0.0),
                    Some((src, tgt, _)) if src < tgt && ix > src && ix <= tgt => {
                        (false, -TAB_ROW_PITCH)
                    },
                    Some((src, tgt, _)) if src > tgt && ix >= tgt && ix < src => {
                        (false, TAB_ROW_PITCH)
                    },
                    _ => (false, 0.0),
                };
                let row = h_flex()
                .id(("sidebar-tab", ix))
                .group(hover_group.clone())
                .relative()
                // 旧壳 `layout.tabs[i]` 的命中矩形覆盖整条可见行。
                //
                // 这里必须是**显式像素宽**，不能用 `w_full`：行在
                // `tab_scroll::wrap_tabs_scroll_list` 的 overflow_hidden 容器
                // 里，百分比宽度到不了这一层，会回落成 shrink-to-fit——表现
                // 就是行宽跟着文件名长短变，带图标的行还整体右移一个图标宽
                // （用户 08-19 报的侧栏 tab 宽度乱跳）。双击重命名"看起来正常"
                // 只是因为 Input 恰好把容器撑满，不是宽度真的对了。
                .w(px(row_w))
                // 内容再宽也不许把行撑开：截断后的标题若比测量值宽一两像素，
                // 撑开的行会重新引入上面那个症状。
                .overflow_hidden()
                .gap_2()
                .px_2()
                .h(px(TAB_ROW_H))
                .items_center()
                // 旧壳 pill 圆角 = UI_CORNER_RADIUS_LOGICAL(8)，rounded_md(6)
                // 偏小一圈，选中水洗的轮廓形状会不一样。
                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
                // GPUI 默认文本样式可能把侧栏整行带到中等/粗体；旧壳
                // tab chrome 使用终端 Regular face，所有子文本从这里继承常规字重。
                .font_weight(FontWeight::NORMAL)
                .cursor_pointer()
                .when(active, |item| item.bg(active_bg).text_color(active_fg))
                .when(!active && !dragged, |item| {
                    item.text_color(muted).hover(|style| style.bg(hover_bg))
                })
                .when(!active && dragged, |item| item.text_color(muted).bg(hover_bg))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_tab(ix, window, cx);
                }))
                // 受约束拖拽：按下待命（源下标 + 按点 Y），移动阈值与让位
                // 由 update_tab_drag 驱动；激活后的指针独占见 render 根部
                // 的透明罩层。
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.tab_drag = Some(TabDrag {
                            source: ix,
                            press_x: f32::from(event.position.x),
                            press_y: f32::from(event.position.y),
                            axis: TabDragAxis::Vertical,
                            pitch: TAB_ROW_PITCH,
                            offset: 0.0,
                            active: false,
                            dock: None,
                        });
                    }),
                )
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close_tab(ix, window, cx);
                    }),
                )
                .on_double_click(cx.listener(move |this, _, window, cx| {
                    // 旧壳 `ChromeHit::Tab` + DoubleClick → BeginRename。
                    cx.stop_propagation();
                    this.begin_rename(ix, window, cx);
                }))
                .when_some(strip, |row, color| {
                    row.child(
                        div()
                            .absolute()
                            .left(px(4.0))
                            .top(px(7.0))
                            .w(px(2.5))
                            .h(px(TAB_ROW_H - 14.0))
                            .rounded_full()
                            .bg(color),
                    )
                })
                // 行首图标的优先级：**先身份、后形态**。AI 品牌图 / 程序字位
                // 表达「这个 tab 里在跑什么」，它必须跟随聚焦 pane（旧壳与
                // Netcatty 同行为）；2×2 分屏标记只在没有身份可显示时补位。
                // 反过来（分屏就一律画 grid）会让 claude 分屏之后标签上再也
                // 看不出跑着 claude——用户 08-23 报的「侧边 tab 不跟随激活
                // tab 变化」就是这个。数量由尾部胶囊表达，不必和图标抢槽位。
                .when(is_settings, |row| {
                    row.child(
                        div().w(px(TAB_LABEL_ICON_W)).flex_shrink_0().flex().justify_center().child(
                            Icon::new(IconName::Settings)
                                .small()
                                .text_color(if active { active_fg } else { muted }),
                        ),
                    )
                })
                .when_some(logo_image.clone(), |row, image| {
                    row.child(
                        img(image)
                            .size(px(TAB_LABEL_ICON_SIZE))
                            .flex_shrink_0()
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when_some(program_glyph, |row, glyph| {
                    row.child(
                        div()
                            .w(px(TAB_LABEL_ICON_W))
                            .flex_shrink_0()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(if active { active_fg } else { muted })
                            .child(glyph),
                    )
                })
                .when(
                    pane_count > 1 && !is_settings && logo_image.is_none() && !has_program_glyph,
                    |row| {
                        row.child(
                            div()
                                .w(px(TAB_LABEL_ICON_W))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(pane_header::split_glyph(
                                    label_px * 0.78,
                                    if active { active_fg } else { muted },
                                )),
                        )
                    },
                )
                // 标签走终端字体 + 终端字号（旧壳 chrome 同源）。文本已按列
                // 截断，这里只需要不换行；再叠一层 `truncate()` 会把省略号
                // 自己裁掉（用户报的"直接截断"）。重命名中的那一行原地换成
                // 输入框：点进去不该触发选中/拖拽，所以自己吃掉 mouse_down。
                .child(match renaming {
                    Some(input) => div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.cancel_rename(window, cx);
                            }
                        }))
                        .child(
                            Input::new(&input)
                                .w_full()
                                .text_size(px(label_px))
                                .font_family(mono_family.clone()),
                        )
                        .into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono_family.clone())
                        .text_size(px(label_px))
                        // 标签标题本身使用 Light；活动态只换前景/背景色，
                        // 不再靠更粗字重强调，避免选中行看起来突然加粗。
                        .font_weight(FontWeight::LIGHT)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(title)
                        .into_any_element(),
                })
                .when(pane_count > 1, |row| {
                    row.child(
                        div()
                            .id(("sidebar-pane-count", ix))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.cycle_pane_focus(ix, window, cx);
                            }))
                            .child(pane_header::split_badge(
                                pane_count,
                                label_px,
                                if active { active_fg } else { muted },
                                if active { active_bg } else { theme.muted },
                            )),
                    )
                })
                .child(
                    div()
                        .relative()
                        .w(px(TAB_STATUS_SLOT_W))
                        .h_full()
                        .flex_shrink_0()
                        .when_some(resting_status, |slot, status| {
                            slot.child(
                                h_flex()
                                    .absolute()
                                    .inset_0()
                                    .justify_end()
                                    .items_center()
                                    .group_hover(hover_group.clone(), |item| item.invisible())
                                    .child(status),
                            )
                        })
                        .child(
                            // 关闭按钮跟状态徽章共用同一个居中槽位：位置由
                            // flex 给出，不再硬写 top 偏移。
                            h_flex()
                                .absolute()
                                .inset_0()
                                .justify_end()
                                .items_center()
                                .invisible()
                                .group_hover(hover_group, |slot| slot.visible())
                                .child(
                                    Button::new(("close-tab", ix))
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.request_close_tab(ix, window, cx);
                                        })),
                                ),
                        ),
                )
                // 右键只记锚点：菜单由 workspace 根上唯一一份宿主画。挂
                // `.context_menu()` 会让每个标签行都渲染同一个 PopupMenu，
                // 阴影按标签数叠厚（见 `tab_menu.rs` 模块头）。
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_tab_context_menu(ix, event.position, window, cx);
                    }),
                )
                .when_some(cross_window_drag, |row, payload| {
                    row.on_drag(payload, |payload, _, _, cx| {
                        NebulaWorkspace::cross_window_drag_preview(payload, cx)
                    })
                });
                if dragged {
                    // 骑指针 + 提到最上层画（deferred 只延后绘制、不动布局），
                    // 阴影给"拿起来"的抬升感。
                    gpui::deferred(
                        row.top(px(drag.map(|(_, _, off)| off).unwrap_or(0.0))).shadow_md(),
                    )
                    .into_any_element()
                } else if shift != 0.0 {
                    // 让位滑动：进位方向 ease-out 滑入（旧壳是双向弹簧；回位
                    // 这里先直落，违和再补逐帧插值）。设置「标签动画=立即」时
                    // 直接落位（旧壳 TabRevealMotion::Instant 的 Snap 语义）。
                    if nebula_settings::RuntimeSettings::load().tab_reveal
                        == nebula_settings::TabRevealName::Instant
                    {
                        row.top(px(shift)).into_any_element()
                    } else {
                        row.with_animation(
                            ("tab-make-way", ix),
                            Animation::new(Duration::from_millis(120))
                                .with_easing(ease_out_quint()),
                            move |row, t| row.top(px(shift * t)),
                        )
                        .into_any_element()
                    }
                } else {
                    row.into_any_element()
                }
            })
            .collect::<Vec<_>>();

        let header_group: SharedString = "sidebar-tabs-header-hover".into();
        let count: SharedString = self.tabs.len().to_string().into();

        let sidebar = v_flex()
            .w(px(self.sidebar_width))
            .h_full()
            .flex_shrink_0()
            // workspace 根保持透明，侧栏自己只铺一层壳色；否则终端卡会
            // 叠到根底色上，把 Acrylic 的目标透明度二次增浓。
            .bg(theme.background)
            .p_2()
            .gap_2()
            // 待命阶段（未过阈值）的指针跟踪；激活后由根部罩层独占接管。
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.update_tab_drag(event, window, cx);
            }))
            .child(
                h_flex()
                    .id("sidebar-tabs-toggle")
                    .group(header_group.clone())
                    .w_full()
                    .h(px(34.0))
                    .pb_1()
                    // 旧壳标题文字从 panel_x + 16px 起；侧栏根已有 8px
                    // padding，这里再补 8px，箭头不会贴住左边缘。
                    .pl_2()
                    .items_center()
                    .cursor_pointer()
                    // `ChromeHit::TabsSection` 命中整条 tabs_header。监听必须
                    // 挂在标题行根节点，右侧空白区同样可以折叠，而不是只有
                    // 箭头、标题和数量这一小段能点。
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_tabs_section(cx);
                    }))
                    .child(
                        // 箭头、TABS 和计数仍作为一个排版段；折叠命中已经
                        // 提升到外层整行。右侧 +/⋯ 自己停止冒泡。
                        h_flex()
                            .h_full()
                            .items_center()
                            .pr_1()
                            .child(
                                // 折叠三角用组件库线性 Chevron（lucide 细线），
                                // 不再用 Nerd Font 实心字位——后者在侧栏标题
                                // 上偏重、和右侧 +/⋯ 的 SVG 不一套语言。
                                h_flex()
                                    .w(px(tabs_disclosure_slot_w))
                                    .h_full()
                                    .flex_shrink_0()
                                    .items_center()
                                    .child(
                                        Icon::new(if self.tabs_section_collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        })
                                        .xsmall()
                                        .text_color(muted),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(muted)
                                    .child("TABS"),
                            )
                            .child(
                                // chip 尺寸贴旧壳公式：h = max(cell_h×0.82×1.18, 11)
                                // ≈ 18-19px（不是 22），1 位数宽 max(adv+8, h×1.25)。
                                // 数字 ink_faint。
                                h_flex()
                                    .ml_2()
                                    .h(px((label_px * 1.22 * SIDEBAR_TITLE_SCALE * 1.18).max(11.0)))
                                    .min_w(px(label_px * 0.62 + 8.0))
                                    .px_2()
                                    .justify_center()
                                    .items_center()
                                    .rounded_full()
                                    .bg(theme.muted)
                                    .text_size(px(label_px * SIDEBAR_TITLE_SCALE))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(faint)
                                    .child(count),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(2.0))
                            .child(
                                // 旧壳 `ChromeHit::NewTab`：直接开设置里的默认
                                // shell，不经过选择器。三点才是 NewTabMenu。
                                h_flex()
                                    .id("sidebar-new-tab")
                                    .size(px(SIDEBAR_PLUS_SIZE))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .invisible()
                                    .group_hover(header_group, |button| button.visible())
                                    .hover(|button| button.bg(hover_bg).text_color(theme.foreground))
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new(
                                            "新建终端 (Ctrl+Shift+T)",
                                        )
                                        .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.add_terminal(window, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::Plus).with_size(px(SIDEBAR_HEADER_ICON)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .id("sidebar-tabs-menu")
                                    .w(px(SIDEBAR_MENU_W))
                                    .h(px(SIDEBAR_PLUS_SIZE))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(|button| button.bg(hover_bg).text_color(theme.foreground))
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new("新建终端 (Ctrl+K)")
                                            .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_shell_palette(window, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::EllipsisVertical)
                                            .with_size(px(SIDEBAR_HEADER_ICON)),
                                    ),
                            ),
                    ),
            )
            .child(self.render_tabs_section(items, cx));
        // spinner 帧循环（旧壳 motion frame 的对应物）：有任何行在
        // 「运行中」时推进相位；notify → 下一次 render 再续帧。
        if items_running.get() {
            cx.on_next_frame(window, |this, _, cx| {
                let now = std::time::Instant::now();
                let dt = now - this.spinner_last;
                this.spinner_last = now;
                this.spinner_phase = (this.spinner_phase + dt.as_secs_f32() / 0.8).rem_euclid(1.0);
                cx.notify();
            });
        }
        sidebar
    }

    /// 与旧壳 `nebula_tabs_section_open` 同义：点标题整行折叠/展开。
    /// 卷帘只裁剪槽位，视口高度在动画期间冻结，避免量到裁剪高后把溢出
    /// 列表锁成一行滚动区。
    fn toggle_tabs_section(&mut self, cx: &mut Context<Self>) {
        self.tabs_section_collapsed = !self.tabs_section_collapsed;
        // 卷帘一动，菜单锚定的那一行就不在原处了。
        self.tab_menu = None;
        self.tabs_fold_armed = true;
        self.tabs_fold_seq = self.tabs_fold_seq.wrapping_add(1).max(1);
        let seq = self.tabs_fold_seq;
        self.tabs_fold_frozen = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update(cx, |this, cx| {
                if this.tabs_fold_seq == seq {
                    this.tabs_fold_frozen = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Tab 列表槽位。展开时列表是侧栏 `v_flex` 的 `flex_1` 子项，视口等于
    /// 面板剩余高度（旧壳 `tabs_avail`）。折叠动画只按**上次量到的剩余
    /// 高度**卷帘，绝不按全部行高，也不把裁剪高度写回窗口。
    fn render_tabs_section<I>(&self, items: I, cx: &mut Context<Self>) -> gpui::AnyElement
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        let collapsed = self.tabs_section_collapsed;
        let list =
            self.wrap_tabs_scroll_list(items.into_iter().map(|item| item.into_any_element()), cx);
        if collapsed && !self.tabs_fold_frozen {
            return div().into_any_element();
        }
        if !self.tabs_fold_armed || !self.tabs_fold_frozen {
            return if collapsed { div().into_any_element() } else { list };
        }
        let slot_h = self.tabs_viewport_h;
        let (from, to) = if collapsed { (slot_h, 0.0) } else { (0.0, slot_h) };
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(list)
            .with_animation(
                ("tabs-fold", collapsed as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| {
                    let height = from + (to - from) * t;
                    if !collapsed && t >= 1.0 { slot } else { slot.max_h(px(height)) }
                },
            )
            .into_any_element()
    }

    /// 侧栏槽位：宽度在 0..持久化宽度间以 ease-out 滑动，近似旧壳
    /// response=0.14 的 swift-out 弹簧；内容保持固定宽、由槽位裁剪，
    /// 终端卡随 flex 布局自然滑移收编空间（对齐旧壳"卡骑在折叠动画上"
    /// 的观感）。动画按方向换 key 重启，端点随运行时设置变化。
    pub(super) fn render_sidebar_slot(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let collapsed = self.sidebar_collapsed;
        if !self.sidebar_fold_armed {
            return if collapsed {
                div().into_any_element()
            } else {
                self.render_sidebar(window, cx).into_any_element()
            };
        }
        let width = self.sidebar_width;
        let (from, to) = if collapsed { (width, 0.0) } else { (0.0, width) };
        div()
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .child(self.render_sidebar(window, cx))
            .with_animation(
                ("sidebar-fold", collapsed as usize),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                move |slot, t| slot.w(px(from + (to - from) * t)),
            )
            .into_any_element()
    }

    /// 侧栏模式的标题栏：左边侧栏开关 + 齿轮，右边目录树 + Git，中间在侧栏
    /// 折叠时顶上活动 tab 的名字。与顶部 tab 模式的 [`Self::render_top_title_bar`]
    /// 对称——两种布局各自持有自己那条标题带的全部内容。
    pub(super) fn render_sidebar_title_bar(
        &self,
        files_active: bool,
        git_active: bool,
        settings_active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // 颜色先取出来：`cx.theme()` 不可变借着 cx，后面每个 `cx.listener`
        // 都要可变借，混在一个表达式里借用检查过不去。
        let secondary = cx.theme().secondary;
        h_flex()
            .size_full()
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    // 旧壳两枚 32px 命中块之间固定留 8px；默认 Button 正好是
                    // 32px，`.small()` 会把热区缩成 24px。
                    .gap_2()
                    .items_center()
                    .occlude()
                    .child(
                        Button::new("toggle-sidebar")
                            .icon(IconName::PanelLeft)
                            .ghost()
                            // 侧栏是开关而非一次性动作：展开期间必须持续显示
                            // 选中底，和旧壳 `left_sidebar_visible()` 同义。
                            .selected(!self.sidebar_collapsed)
                            // Ghost 的全局 selected 使用 hover_strong，静态底比
                            // 旧壳亮一档；仅此按钮覆写回旧壳 surface。
                            .when(!self.sidebar_collapsed, |button| button.bg(secondary))
                            .tooltip("折叠/展开侧边栏 (Ctrl+Shift+B)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_collapsed = !this.sidebar_collapsed;
                                this.sidebar_fold_armed = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("open-settings")
                            .icon(IconName::Settings)
                            .ghost()
                            .selected(settings_active)
                            .tooltip("设置 (Ctrl+,)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(window, cx);
                            })),
                    ),
            )
            .child(self.render_collapsed_tab_title(cx))
            .child(
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_2()
                    .occlude()
                    .child(
                        Button::new("toggle-file-tree")
                            .icon(if files_active {
                                IconName::FolderOpen
                            } else {
                                IconName::FolderClosed
                            })
                            .ghost()
                            .selected(files_active)
                            .tooltip("目录树 (Ctrl+Shift+F)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_file_tree(cx);
                            })),
                    )
                    .child(
                        Button::new("toggle-git-tree")
                            .icon(IconName::GitHub)
                            .ghost()
                            .selected(git_active)
                            .tooltip("Git 状态 (Ctrl+Shift+G)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_git_tree(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// 折叠态的活动 tab 名（旧壳 `chrome.rs` ~2129：侧栏收起后顶栏居中画活动
    /// tab 名，"没有侧栏也知道自己在哪"）。
    ///
    /// 弹性容器 + `absolute` 覆盖层的组合是刻意的：容器用 `flex_1` 抢下两侧
    /// 工具之间的空档，文字走覆盖层不参与布局——既不会把左右按钮挤歪，也不
    /// 吃鼠标事件，标题栏本身的拖窗和双击最大化照旧。
    fn render_collapsed_tab_title(&self, cx: &mut Context<Self>) -> gpui::Div {
        let slot = div().relative().flex_1().min_w_0().h_full();
        if !self.sidebar_collapsed || self.tabs.is_empty() {
            return slot;
        }
        let theme = cx.theme();
        let (ink, dim, badge_fill) = (theme.foreground, theme.muted_foreground, theme.muted);
        let dark = theme.is_dark();
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let mono_family: SharedString = settings
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Cascadia Mono"))
            .into();
        let label_px = settings.map(|settings| settings.base_font_size_px).unwrap_or(15.0);
        let TabPresentation { title, logo_image, program_glyph, pane_count, .. } =
            self.tab_presentation(self.active, cx, dark);
        slot.child(
            h_flex()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .gap_2()
                // 两侧工具靠 flex 天然让位，这点内缩只是别让长标题贴到按钮上。
                .px_4()
                .when_some(logo_image, |row, image| {
                    row.child(
                        img(image)
                            .size(px(TAB_LABEL_ICON_SIZE))
                            .flex_shrink_0()
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when_some(program_glyph, |row, glyph| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .font_family(mono_family.clone())
                            .text_size(px(label_px))
                            .text_color(dim)
                            .child(glyph),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(mono_family)
                        .text_size(px(label_px))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(ink)
                        .child(title),
                )
                // 折叠态没有侧栏行可看，分屏数量只能挂在这里；> 1 才画。
                .when(pane_count > 1, |row| {
                    row.child(pane_header::split_badge(pane_count, label_px, dim, badge_fill))
                }),
        )
    }
}
