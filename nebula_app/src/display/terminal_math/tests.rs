use super::*;

fn overlay_with_source_style(flags: Flags, foreground: Rgb, background: Rgb) -> FormulaOverlay {
    let mut overlay =
        scan_grid(&TextGrid::from_rows(&[r"$$E=mc^2$$"])).into_iter().next().expect("test formula");
    overlay.fallback = "$$E=mc^2$$"
        .chars()
        .enumerate()
        .map(|(column, character)| RenderableCell {
            character,
            point: Point::new(0, Column(column)),
            fg: foreground,
            bg: background,
            bg_alpha: 1.0,
            underline: foreground,
            flags,
            extra: None,
        })
        .collect();
    overlay
}

fn compact_prepared(cells: usize) -> Option<PreparedFormula> {
    Some(PreparedFormula {
        fitted_pixel_size: TEST_FONT_PX,
        display_style: false,
        bleed_top: 0.0,
        bleed_bottom: 0.0,
        box_right: 0.0,
        centered: true,
        compact_cells: Some(cells),
    })
}

fn sources(rows: &[&str]) -> Vec<(String, bool)> {
    scan_grid(&TextGrid::from_rows(rows))
        .into_iter()
        .map(|formula| (formula.source.to_string(), formula.display))
        .collect()
}

fn grid_at(rows: &[&str], absolute_top: usize, scrolled_out: usize) -> TextGrid {
    let mut grid = TextGrid::from_rows(rows);
    grid.absolute_top = absolute_top;
    grid.scrolled_out = scrolled_out;
    grid
}

#[test]
fn compact_projection_moves_only_render_coordinates() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let original = grid.rows.clone();
    let overlays = scan_grid(&grid);
    assert_eq!(overlays.len(), 1);
    let span = overlays[0].spans[0];
    let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

    assert!(projection.project_cell(Point::new(0, Column(span.start)), 80).is_none());
    let suffix = projection
        .project_cell(Point::new(0, Column(span.end)), 80)
        .expect("suffix remains visible");
    assert_eq!(suffix.column.0, span.start + 2);
    assert_eq!(grid.rows, original, "projection must not mutate terminal cells");
}

#[test]
fn compact_projection_retains_background_under_visual_formula_box() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let overlays = scan_grid(&grid);
    let span = overlays[0].spans[0];
    let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

    let first = projection
        .project_formula_background(Point::new(0, Column(span.start)), 80)
        .expect("first visual formula cell keeps its background");
    let second = projection
        .project_formula_background(Point::new(0, Column(span.start + 1)), 80)
        .expect("second visual formula cell keeps its background");
    assert_eq!(first.column.0, span.start);
    assert_eq!(second.column.0, span.start + 1);
    assert!(
        projection.project_formula_background(Point::new(0, Column(span.start + 2)), 80).is_none(),
        "source cells beyond the compact formula width must not create a background strip"
    );
}

#[test]
fn disabled_projection_keeps_fixed_columns() {
    let grid = TextGrid::from_rows(&["pre $x^2$                         sidebar"]);
    let overlays = scan_grid(&grid);
    let span = overlays[0].spans[0];
    let prepared = [compact_prepared(2)];
    let mut state = TerminalMathState::default();

    state.update_projection(&overlays, &prepared, false);

    let source = state
        .project_formula_background(Point::new(0, Column(span.start)), 80)
        .expect("formula background remains at its TUI column");
    let sidebar_column = grid.rows[0].iter().position(|cell| *cell == Some('s')).unwrap();
    let sidebar = state
        .project_cell(Point::new(0, Column(sidebar_column)), 80)
        .expect("sidebar remains visible");
    assert_eq!(source.column.0, span.start);
    assert_eq!(sidebar.column.0, sidebar_column);
}

#[test]
fn compact_projection_discards_overlapping_streaming_candidate() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let outer = scan_grid(&grid).remove(0);
    let outer_span = outer.spans[0];
    let mut inner = outer.clone();
    inner.spans[0].start += 1;
    inner.spans[0].end -= 1;
    let mut state = TerminalMathState::default();
    let retained = state.update_projection_with_survivors(
        &[inner, outer],
        &[compact_prepared(1), compact_prepared(2)],
        true,
    );
    let projection = state.projection_snapshot();

    assert_eq!(projection.spans.len(), 1);
    assert_eq!(projection.spans[0].source_start, outer_span.start);
    assert_eq!(projection.spans[0].source_end, outer_span.end);
    assert_eq!(retained, [false, true], "GPUI must drop the same overlapping candidate");
}

#[test]
fn compact_projection_accumulates_multiple_formula_shifts() {
    let grid = TextGrid::from_rows(&["a $x^2$ b $y^2$ c"]);
    let overlays = scan_grid(&grid);
    assert_eq!(overlays.len(), 2);
    let prepared = [compact_prepared(2), compact_prepared(2)];
    let projection = LineProjection::build(&overlays, &prepared);
    let last = overlays[1].spans[0];
    let source_reduction: usize =
        overlays.iter().map(|overlay| overlay.spans[0].end - overlay.spans[0].start - 2).sum();

    let suffix = projection
        .project_cell(Point::new(0, Column(last.end)), 80)
        .expect("suffix remains visible");
    assert_eq!(suffix.column.0, last.end - source_reduction);
}

#[test]
fn formula_hit_testing_returns_source_boundaries() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let overlays = scan_grid(&grid);
    let span = overlays[0].spans[0];
    let projection = LineProjection::build(&overlays, &[compact_prepared(2)]);

    let (left, left_side) =
        projection.source_from_visual(Point::new(0, Column(span.start)), Side::Left);
    let (right, right_side) =
        projection.source_from_visual(Point::new(0, Column(span.start + 1)), Side::Right);
    assert_eq!((left.column.0, left_side), (span.start, Side::Left));
    assert_eq!((right.column.0, right_side), (span.end - 1, Side::Right));
}

#[test]
fn formula_hit_testing_preserves_nonzero_viewport_origin() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let overlays = scan_grid(&grid);
    let span = overlays[0].spans[0];
    let mut state = TerminalMathState::default();
    state.update_projection(&overlays, &[compact_prepared(2)], true);
    let viewport_origin = Line(-7);

    let (left, left_side) = state.source_point(
        Point::new(viewport_origin, Column(span.start)),
        Side::Left,
        viewport_origin,
    );
    let (suffix, suffix_side) = state.source_point(
        Point::new(viewport_origin, Column(span.start + 2)),
        Side::Left,
        viewport_origin,
    );

    assert_eq!((left.line, left.column.0, left_side), (viewport_origin, span.start, Side::Left));
    assert_eq!(
        (suffix.line, suffix.column.0, suffix_side),
        (viewport_origin, span.end, Side::Left)
    );
}

fn remember_visible(state: &mut TerminalMathState, grid: &TextGrid) {
    state.synchronize_grid(grid);
    for overlay in scan_grid(grid) {
        state.remember(grid, &overlay);
    }
}

#[test]
fn completed_formula_replaces_overlapping_streaming_candidate() {
    let grid = TextGrid::from_rows(&["pre $x^2$ suffix"]);
    let outer = scan_grid(&grid).remove(0);
    let mut inner = outer.clone();
    inner.spans[0].start += 1;
    inner.spans[0].end -= 1;

    let mut state = TerminalMathState::default();
    state.remember(&grid, &inner);
    state.remember(&grid, &outer);

    assert_eq!(state.formulas.len(), 1);
    assert!(state.formulas.contains_key(&FormulaAnchor { row: 0, column: outer.spans[0].start }));
}

#[test]
fn recognizes_cli_math_delimiters_and_utf8_prose() {
    assert_eq!(
        sources(&[r"中文 \(x^2+y^2=z^2\) and $\alpha+1$"]),
        vec![("x^2+y^2=z^2".into(), false), (r"\alpha+1".into(), false)]
    );
}

#[test]
fn agent_reasoning_style_is_distinct_from_final_answer_style() {
    let normal = Rgb::new(240, 240, 240);
    let background = Rgb::new(8, 8, 8);

    let codex_reasoning = overlay_with_source_style(Flags::DIM, normal, background);
    assert!(formula_uses_reasoning_style(&codex_reasoning));

    let final_answer = overlay_with_source_style(Flags::empty(), normal, background);
    assert!(!formula_uses_reasoning_style(&final_answer));
}

/// 行内定界符只跨软换行。跨真实换行会把互不相关的两行终端输出合成
/// 一个公式（`$x` / `$y` 两行、WSL bash 两个提示符之间的整段输出），
/// 代价是 Agent TUI 硬换行的**行内**公式恢复不了——那种情况保持原文，
/// 是安全的失败模式。块级公式仍然跨真实换行，见
/// [`display_math_can_cross_hard_terminal_rows`]。
#[test]
fn explicit_inline_formulas_cross_soft_wraps_only() {
    for mut grid in [
        TextGrid::from_rows(&["$e^    ", r"{i\pi}+1=0$"]),
        TextGrid::from_rows(&[r"\(e^    ", r"{i\pi}+1=0\)"]),
    ] {
        grid.wrapped[0] = true;
        let overlays = scan_grid(&grid);
        assert_eq!(overlays.len(), 1, "软换行内要接起来");
        assert!(overlays[0].source.contains("e^"));
        assert!(overlays[0].source.contains(r"{i\pi}+1=0"));

        grid.wrapped[0] = false;
        assert!(scan_grid(&grid).is_empty(), "真实换行必须断开行内公式");
    }
}

/// Agent TUI 的硬换行：**块级**形态（`\[ \]`、裸 `[`、`$$`）照旧恢复，
/// 行内形态保持原文。
#[test]
fn screenshot_display_formulas_survive_agent_hard_wraps() {
    for rows in [
        vec![r"\[\displaystyle \frac{d}{dx}\sin", r"x=\cos x\]"],
        vec!["$$", r"\lim_{x\to0}\frac{\sin", r"x}{x}=1", "$$"],
    ] {
        let overlays = scan_grid(&TextGrid::from_rows(&rows));
        assert!(!overlays.is_empty(), "expected wrapped formula in {rows:?}");
        for overlay in overlays {
            compile_formula(&overlay.source, true, 18.0, 1.0, DEFAULT_LIMITS).unwrap_or_else(
                |error| panic!("wrapped formula failed: {:?}: {error:?}", overlay.source),
            );
        }
    }
    // 同一批内容的行内形态被硬换行拆开时保持原文，不再跨行拼接。
    assert!(
        scan_grid(&TextGrid::from_rows(&[r"• $\lim_{x\to0}\frac{\sin", r"x}{x}=1$，$PV=nRT$",]))
            .iter()
            .all(|overlay| !overlay.source.contains("lim")),
        "行内公式不得跨真实换行拼接"
    );
}

#[test]
fn display_math_can_cross_hard_terminal_rows() {
    assert_eq!(
        sources(&["answer:", "$$", r"\frac{1}{2} + x^2", "$$", "done"]),
        vec![(r"\frac{1}{2} + x^2".into(), true)]
    );
    assert_eq!(sources(&[r"\[\sum_{i=1}^n i\]"]), vec![(r"\sum_{i=1}^n i".into(), true)]);

    let multiline = sources(&[
        "$$",
        r"\begin{aligned}",
        r"f(x) &= x^2 \\",
        r"g(x) &= x+1",
        r"\end{aligned}",
        "$$",
    ]);
    assert_eq!(multiline.len(), 1);
    assert!(multiline[0].0.contains('\n'));
    assert!(multiline[0].1);
}

#[test]
fn stray_display_opener_in_prose_does_not_swallow_following_formulas() {
    let screen = [
        "行内，$$,\\[等公式都输出一些",
        "",
        "• 行内公式：质能方程 (E = mc^2)，二次方程的求根公式是 (x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a})。",
        "单美元符号行内公式：欧拉公式 $e^{ix}=\\cos x+i\\sin x$。",
        "双美元符号块级公式：",
        "$$",
        "\\int_{-\\infty}^{+\\infty} e^{-x^2},dx=\\sqrt{\\pi}",
        "$$",
        "方括号块级公式：",
        "\\[",
        "\\sum_{n=1}^{\\infty}\\frac{1}{n^2}=\\frac{\\pi^2}{6}",
        "\\]",
        "带编号的公式：",
        "[",
        "\\boxed{\\nabla\\cdot\\mathbf{E}=\\frac{\\rho}{\\varepsilon_0}}",
        "\\tag{1}",
        "]",
    ];
    let expected = vec![
        ("E = mc^2".into(), false),
        (r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}".into(), false),
        (r"e^{ix}=\cos x+i\sin x".into(), false),
        (r"\int_{-\infty}^{+\infty} e^{-x^2},dx=\sqrt{\pi}".into(), true),
        (r"\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}".into(), true),
        (
            // The row break inside the block survives extraction; it is
            // collapsed together with the padding below because TeX treats
            // every run of whitespace alike.
            concat!(r"\boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}}", " ", r"\tag{1}",)
                .into(),
            true,
        ),
    ];

    // `extract` only trims the whole span, so a multi-row source still
    // carries each row's blank padding before the newline. Whitespace is
    // insignificant to TeX, so compare on collapsed runs instead of
    // pinning the terminal width into the expectation.
    let extracted: Vec<(String, bool)> = sources(&screen)
        .into_iter()
        .map(|(source, display)| (source.split_whitespace().collect::<Vec<_>>().join(" "), display))
        .collect();
    assert_eq!(extracted, expected);
    assert!(sources(&screen)[5].0.contains('\n'), "the block keeps its real row break");
    assert!(
        scan_grid(&TextGrid::from_rows(&screen))
            .iter()
            .flat_map(|overlay| overlay.spans.iter())
            .all(|span| span.row != 0),
        "prose on the first row must stay literal",
    );
}

/// `cat math.txt`：块的开头 `$$` 滚出视口顶部之后，视口里第一个 `$$` 是
/// **闭合**。它曾经跟下面那个块的定界符配对，把 27 行正文、三个方括号块
/// 连同末尾的 `$$` 块吞成一条候选——而且编译成功，于是整片区域被一张渲染
/// 图盖掉，中文正文以数学字形出现在图里。
#[test]
fn orphan_display_closer_at_the_viewport_top_stops_at_the_blank_row() {
    let screen = [
        "  $$",
        "",
        "  方括号块级公式：",
        "",
        "  [",
        r"  \sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}",
        "  ]",
        "",
        "  带编号的公式：",
        "",
        "  [",
        r"  \boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}}",
        r"  \tag{1}",
        "  ]",
        "",
        "",
        r"$$(F, D_{\text{few}}) \xrightarrow{\text{Prompting}} \boxed{C}",
        r"  \boxed{(F_{\text{ref}}, F_{\text{neg}})} \xrightarrow{\text{ToT Search}}$$",
    ];
    let expected = [
        r"\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}",
        r"\boxed{\nabla\cdot\mathbf{E}=\frac{\rho}{\varepsilon_0}} \tag{1}",
        concat!(
            r"(F, D_{\text{few}}) \xrightarrow{\text{Prompting}} \boxed{C} ",
            r"\boxed{(F_{\text{ref}}, F_{\text{neg}})} \xrightarrow{\text{ToT Search}}",
        ),
    ];

    let extracted = sources(&screen);
    let collapsed: Vec<String> = extracted
        .iter()
        .map(|(source, _)| source.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(collapsed, expected, "正文和孤立闭合必须保持原文");
    assert!(extracted.iter().all(|(_, display)| *display));

    // 孤立闭合要留成 pending，历史回看才有机会把滚出去的真公式配回来。
    let scan = scan_grid_result(&TextGrid::from_rows(&screen));
    assert!(
        matches!(
            scan.unmatched_display,
            Some((position, DisplayDelimiterKind::Dollars)) if position.row == 0
        ),
        "orphan closer must stay pending, got {:?}",
        scan.unmatched_display
    );
}

/// Codex 的形状，按源码推导而非截图：`codex-rs/tui/src/markdown_render.rs`
/// 只开 strikethrough/tables，不认识 `$$`；块里以 `- ` / `+ ` / `* ` 起行
/// 的公式行被 pulldown-cmark 当成打断段落的列表项，`start_list` 在前面插
/// 一行空行，`start_item` 把记号一律画成 `- `，后续行（含闭合 `$$`）作为
/// 列表项续行缩进两格。「空行终止搜索」的规则会把这些块全判成孤立开头。
/// （09-02 那张截图后来确认是 Claude Code，其形状见
/// `claude_code_display_blocks_from_screenshot_pair_and_compile`。）
#[test]
fn tui_paragraph_gap_inside_a_standalone_display_block_is_bridged() {
    let cases: [(&[&str], &str); 5] = [
        (
            &[
                "$$",
                r"R_{\mu\nu} - \frac{1}{2} R g_{\mu\nu}",
                "",
                r"- \Lambda g_{\mu\nu} = \frac{8\pi G}{c^4} T_{\mu\nu}",
                "  $$",
            ],
            r"R_{\mu\nu} - \frac{1}{2} R g_{\mu\nu} - \Lambda g_{\mu\nu} = \frac{8\pi G}{c^4} T_{\mu\nu}",
        ),
        (
            &[
                "  $$",
                r"  i\hbar \frac{\partial}{\partial t} \Psi",
                "",
                r"  - \frac{\hbar^2}{2m} \nabla^2 \Psi + V \Psi = 0",
                "    $$",
            ],
            r"i\hbar \frac{\partial}{\partial t} \Psi - \frac{\hbar^2}{2m} \nabla^2 \Psi + V \Psi = 0",
        ),
        // 列表项之后还有普通续行：`\end{aligned}` 和闭合都缩进两格。
        (
            &[
                "$$",
                r"\begin{aligned}",
                r"F &= ma \\",
                "",
                r"- \nabla V &= m \ddot{x}",
                r"  \end{aligned}",
                "  $$",
            ],
            r"\begin{aligned} F &= ma \\ - \nabla V &= m \ddot{x} \end{aligned}",
        ),
        // 两次被切：块里有两行以减号开头。
        (
            &["$$", r"x^2", "", r"- 2xy", "", r"- y^2 = (x - y)^2 - 2y^2", "  $$"],
            r"x^2 - 2xy - y^2 = (x - y)^2 - 2y^2",
        ),
        // 没有反斜杠命令也要认：`a^2` 的上标就是证据。
        (&["$$", r"a^2 + b^2", "", r"- c^2 = 0", "  $$"], r"a^2 + b^2 - c^2 = 0"),
    ];

    for (screen, expected) in cases {
        let extracted = sources(screen);
        assert_eq!(extracted.len(), 1, "{screen:?} must yield one display formula");
        let (source, display) = &extracted[0];
        assert!(display);
        assert_eq!(source.split_whitespace().collect::<Vec<_>>().join(" "), expected);
        compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
            .unwrap_or_else(|error| panic!("{expected} must compile: {error:?}"));
        let scan = scan_grid_result(&TextGrid::from_rows(screen));
        assert!(scan.unmatched_display.is_none(), "{screen:?} left a pending delimiter");
    }
}

/// 桥接空行的四道闸，每道各一个反例。松开任何一道，`cat math.txt`
/// 那种孤立闭合就会重新把下面的正文吞进公式。
#[test]
fn display_block_gap_bridging_stops_at_prose_double_gaps_and_inline_closers() {
    // 空行后面是正文：没有数学证据，搜索到此为止。
    let prose: &[&str] = &["$$", r"x^2", "", "所以上式成立。", "$$"];
    // 连续两行空行是真正的分段，不是 TUI 塞的列表间距。
    let double_gap: &[&str] = &["$$", r"x^2", "", "", r"- y^2", "  $$"];
    // 跨过空行之后闭合必须独占一行。
    let inline_closer: &[&str] = &["$$", r"x^2", "", r"- y^2 = z$$", "后文"];
    // 开头不独占一行（单行块被硬折行）不享受桥接。
    let inline_opener: &[&str] = &[r"$$x^2 +", "", r"- y^2", "  $$"];
    // 开头后面紧跟空行：块里还没有内容，这是孤立闭合的形状。
    let empty_head: &[&str] = &["$$", "", r"- y^2", "  $$"];
    for screen in [prose, double_gap, inline_closer, inline_opener, empty_head] {
        let scan = scan_grid_result(&TextGrid::from_rows(screen));
        assert!(
            scan.overlays.is_empty(),
            "{screen:?} must not bridge the gap, got {:?}",
            scan.overlays.iter().map(|overlay| overlay.source.as_ref()).collect::<Vec<_>>()
        );
        assert!(scan.unmatched_display.is_some(), "{screen:?} must stay pending");
    }
}

/// 桥接有行预算：闭合离开头太远就当没有。预算和裸 `[` 块共用同一个数。
#[test]
fn display_block_gap_bridging_has_a_row_budget() {
    // 闭合落在第 DISPLAY_BLOCK_GAP_SEARCH_ROWS + 1 行：超出一行。
    let mut screen: Vec<String> = vec!["$$".into(), r"x^2".into(), String::new()];
    screen.extend((0..DISPLAY_BLOCK_GAP_SEARCH_ROWS - 2).map(|index| format!("- y_{index}")));
    screen.push("  $$".into());
    let rows: Vec<&str> = screen.iter().map(String::as_str).collect();
    assert!(sources(&rows).is_empty(), "closer past the row budget must not pair");

    // 少一行内容，闭合正好落在预算内。
    screen.truncate(screen.len() - 2);
    screen.push("  $$".into());
    let rows: Vec<&str> = screen.iter().map(String::as_str).collect();
    assert_eq!(sources(&rows).len(), 1, "closer inside the row budget pairs");
}

/// 同一个渲染器把 `\[` 吃成 `[`，再用同一手法切块：裸方括号块走同样的桥。
#[test]
fn tui_paragraph_gap_inside_a_bare_bracket_block_is_bridged() {
    let screen = [
        "[",
        r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}",
        "",
        r"- \nabla \times \mathbf{B} = 0",
        "  ]",
    ];
    let extracted = sources(&screen);
    assert_eq!(extracted.len(), 1);
    assert!(extracted[0].1);
    assert_eq!(
        extracted[0].0.split_whitespace().collect::<Vec<_>>().join(" "),
        r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0} - \nabla \times \mathbf{B} = 0",
    );

    // 反例同 `$$`：空行后是正文就停。
    let prose = ["[", r"\nabla \cdot \mathbf{E} = 0", "", "其中 E 是电场。", "  ]"];
    assert!(sources(&prose).is_empty());
}

/// 用户 09-03 截图（图 #10，Claude Code 输出）里七个 `$$` 块在终端网格上的
/// 原样。Claude Code 的 Markdown 渲染器留下两种形状：
/// * 独占一行的 `=` 是 setext 标题下划线，被吃掉后只剩一行空行（傅里叶、
///   Attention）；
/// * 反斜杠转义被消费：`\\` 变成 `\`（cases / pmatrix / aligned 的行尾），
///   `\,` 变成 `,`（高斯、傅里叶）。
/// 前者的 `=` 在网格里已不存在，这里只要求块被完整配对并能编译；后者的
/// 行尾单 `\` 必须仍能编译成换行。
#[test]
fn claude_code_display_blocks_from_screenshot_pair_and_compile() {
    let cases: [(&[&str], &str); 7] = [
        (&["$$", r"e^{i\pi}+1=0", "$$"], r"e^{i\pi}+1=0"),
        (
            &["$$", r"\int_{-\infty}^{+\infty} e^{-x^2},dx=\sqrt{\pi}", "$$"],
            r"\int_{-\infty}^{+\infty} e^{-x^2},dx=\sqrt{\pi}",
        ),
        (
            &[
                "$$",
                r"\widehat{f}(\xi)",
                "",
                r"\int_{-\infty}^{+\infty}",
                r"f(x)e^{-2\pi i x\xi},dx",
                "$$",
            ],
            r"\widehat{f}(\xi) \int_{-\infty}^{+\infty} f(x)e^{-2\pi i x\xi},dx",
        ),
        (
            &[
                "$$",
                r"\operatorname{Attention}(Q,K,V)",
                "",
                r"\operatorname{softmax}\left(",
                r"\frac{QK^{\mathsf T}}{\sqrt{d_k}}",
                r"\right)V",
                "$$",
            ],
            r"\operatorname{Attention}(Q,K,V) \operatorname{softmax}\left( \frac{QK^{\mathsf T}}{\sqrt{d_k}} \right)V",
        ),
        (
            &[
                "$$",
                r"\phi(x)=",
                r"\begin{cases}",
                r"x, & x\ge 0,\",
                r"\alpha(e^x-1), & x<0.",
                r"\end{cases}",
                "$$",
            ],
            r"\phi(x)= \begin{cases} x, & x\ge 0,\ \alpha(e^x-1), & x<0. \end{cases}",
        ),
        (
            &[
                "$$",
                "A=",
                r"\begin{pmatrix}",
                r"a_{11} & a_{12} & \cdots & a_{1n}\",
                r"a_{21} & a_{22} & \cdots & a_{2n}\",
                r"\vdots & \vdots & \ddots & \vdots\",
                r"a_{m1} & a_{m2} & \cdots & a_{mn}",
                r"\end{pmatrix}",
                "$$",
            ],
            concat!(
                r"A= \begin{pmatrix} a_{11} & a_{12} & \cdots & a_{1n}\ ",
                r"a_{21} & a_{22} & \cdots & a_{2n}\ \vdots & \vdots & \ddots & \vdots\ ",
                r"a_{m1} & a_{m2} & \cdots & a_{mn} \end{pmatrix}",
            ),
        ),
        (
            &[
                "$$",
                r"\begin{aligned}",
                r"(a+b)^3",
                r"&=(a+b)(a+b)^2\",
                r"&=(a+b)(a^2+2ab+b^2)\",
                r"&=a^3+3a^2b+3ab^2+b^3",
                r"\end{aligned}",
                "$$",
            ],
            concat!(
                r"\begin{aligned} (a+b)^3 &=(a+b)(a+b)^2\ &=(a+b)(a^2+2ab+b^2)\ ",
                r"&=a^3+3a^2b+3ab^2+b^3 \end{aligned}",
            ),
        ),
    ];

    for (screen, expected) in cases {
        // Claude Code 把回答整体缩进两格；缩进与否都必须配对。
        let indented: Vec<String> = screen.iter().map(|row| format!("  {row}")).collect();
        let indented: Vec<&str> = indented.iter().map(String::as_str).collect();
        for rows in [screen, indented.as_slice()] {
            let extracted = sources(rows);
            assert_eq!(extracted.len(), 1, "{rows:?} must yield one display formula");
            let (source, display) = &extracted[0];
            assert!(display);
            assert_eq!(source.split_whitespace().collect::<Vec<_>>().join(" "), expected);
            compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
                .unwrap_or_else(|error| panic!("{expected} must compile: {error:?}"));
            let scan = scan_grid_result(&TextGrid::from_rows(rows));
            assert!(scan.unmatched_display.is_none(), "{rows:?} left a pending delimiter");
        }
    }
}

#[test]
fn long_display_formula_with_trailing_literal_dollar_remains_renderable() {
    let source = concat!(
        r"(F, D_{few}) \xrightarrow{\text{Prompting}} \boxed{C} ",
        r"\xrightarrow{\text{MultiAgent}} \boxed{(F_{ref}, F_{neg})} ",
        r"\xrightarrow{\text{ToT Search}} ",
    );
    let line = format!("$${source}$$$");

    assert_eq!(sources(&[&line]), vec![(source.trim_end().into(), true)]);
    let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
        .expect("the long display formula must reach native layout");
    assert!(layout.metrics.width > 0.0 && layout.metrics.height > 0.0);
}

#[test]
fn active_cli_input_logical_line_is_never_persisted_as_math_output() {
    let mut grid =
        TextGrid::from_rows(&["prompt $$lim_x{x}$$", "continued $x^1$", "assistant $$x^2$$"]);
    // The first physical row wraps into the cursor row, so both belong to
    // one live editor buffer and must be excluded together.
    grid.wrapped[0] = true;
    let active_rows = grid.logical_rows_containing(1).expect("cursor logical line");
    assert_eq!(active_rows, 0..=1);

    let mut state = TerminalMathState::default();
    state.synchronize_grid(&grid);
    assert!(state.scan_visible_grid(&grid, Some(&active_rows)).is_none());

    let overlays = state.visible_overlays(&grid);
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].source.as_ref(), "x^2");
}

#[test]
fn unmatched_display_delimiter_on_live_input_does_not_start_history_reconstruction() {
    let grid = TextGrid::from_rows(&["prompt $$"]);
    let active_rows = grid.logical_rows_containing(0).expect("cursor row");
    let mut state = TerminalMathState::default();
    state.synchronize_grid(&grid);

    assert!(state.scan_visible_grid(&grid, Some(&active_rows)).is_none());
    assert!(state.pending_display.is_none());
}

#[test]
fn screenshot_cli_formulas_reach_the_shared_compiler() {
    let samples = [
        sources(&["$$", r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}", "$$"]),
        sources(&[
            "$$",
            r"f(x)=\begin{cases}",
            "x^2,&x\\geq 0\\",
            r"-x,&x<0",
            r"\end{cases}",
            "$$",
        ]),
        sources(&[
            "$$",
            r"A=\begin{pmatrix}",
            r"1&amp;2&amp;3\&amp;nbsp;",
            r"4&amp;5&amp;6\&amp;#160;",
            r"7&amp;8&amp;9",
            r"\end{pmatrix}",
            "$$",
        ]),
    ];

    for extracted in samples {
        let [(source, true)] = extracted.as_slice() else {
            panic!("expected one display formula, got {extracted:?}");
        };
        let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
            .unwrap_or_else(|error| panic!("CLI formula failed: {source:?}: {error:?}"));
        assert!(layout.metrics.width > 0.0 && layout.metrics.height > 0.0);
    }
}

#[test]
fn assistant_aligned_formula_with_row_spacing_reaches_native_layout() {
    let source = r"\begin{aligned}
\Psi(x,t)
&=
\sum_{n=1}^{\infty}
c_n
\sqrt{\frac{2}{L}}
\sin\left(\frac{n\pi x}{L}\right)
\exp\left(-\frac{i n^2\pi^2\hbar}{2mL^2}t\right), \\[6pt]
\int_{0}^{L}\left|\Psi(x,t)\right|^2\,dx
&= 1,
\qquad
E_n=\frac{n^2\pi^2\hbar^2}{2mL^2}, \\[6pt]
\mathbf{A}^{-1}
&=
\frac{1}{ad-bc}
\begin{bmatrix}
d & -b \\
-c & a
\end{bmatrix},
\qquad ad-bc\ne 0, \\[6pt]
\lim_{N\to\infty}
\sum_{k=1}^{N}\frac{(-1)^{k+1}}{k^2}
&=
\frac{\pi^2}{12},
\qquad
\int_{-\infty}^{\infty}e^{-x^2}\,dx=\sqrt{\pi}.
\end{aligned}";

    let layout = compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS)
        .unwrap_or_else(|error| panic!("formula compile failed: {error:?}"));

    assert!(!layout.glyphs.is_empty());
}

#[test]
fn compact_equations_use_the_same_rule_for_every_standard_delimiter() {
    let expected_inline = vec![("E = mc^2".into(), false)];
    let expected_display = vec![("E = mc^2".into(), true)];
    assert_eq!(sources(&["$E = mc^2$"]), expected_inline);
    assert_eq!(sources(&[r"\(E = mc^2\)"]), expected_inline);
    assert_eq!(sources(&["$$E = mc^2$$"]), expected_display);
    assert_eq!(sources(&[r"\[E = mc^2\]"]), expected_display);
}

#[test]
fn markdown_unescaped_bare_brackets_render_as_display_math() {
    // 部分 AI CLI 的 markdown 渲染会吃掉 `\[` `\]` `\,` 的反斜杠
    // （它们是 markdown 标点转义），`\int`/`\frac` 不是转义所以幸存，
    // 屏幕上只剩裸 `[` 块——2026-08-17 截图的真实形态。
    assert_eq!(
        sources(&["[", r"\int_0^1 x^2,dx = \frac{1}{3}", "]"]),
        vec![(r"\int_0^1 x^2,dx = \frac{1}{3}".into(), true)]
    );
    assert_eq!(
        sources(&["[", r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}", "]"]),
        vec![(r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}".into(), true)]
    );
    assert_eq!(
        sources(&[r"[ \lim_{x\to 0}\frac{\sin x}{x}=1 ]"]),
        vec![(r"\lim_{x\to 0}\frac{\sin x}{x}=1".into(), true)]
    );
    // Codex CLI under WSL renders `\[ E = mc^2 \]` as three plain rows
    // because its Markdown layer consumes the bracket escapes.
    assert_eq!(sources(&["[", "E = mc^2", "]"]), vec![("E = mc^2".into(), true)]);
    // Codex's list renderer keeps the bullet on the opening delimiter.
    assert_eq!(
        sources(&["• [", r"  \int_{-\infty}^{+\infty} e^{-x^2}\,dx=\sqrt{\pi}", "  ]"]),
        vec![(r"\int_{-\infty}^{+\infty} e^{-x^2}\,dx=\sqrt{\pi}".into(), true)]
    );
}

#[test]
fn bare_bracket_interval_inside_formula_does_not_close_early() {
    // 内容行里的 `[0, 1]` 区间：其 `]` 不独占一行，没有闭合资格。
    assert_eq!(sources(&["[", r"x \in [0, 1]", "]"]), vec![(r"x \in [0, 1]".into(), true)]);
}

#[test]
fn bare_brackets_without_tex_evidence_stay_literal() {
    assert!(sources(&["[", "  1, 2, 3,", "]"]).is_empty());
    assert!(sources(&[r#"["alpha", "beta"]"#]).is_empty());
    assert!(sources(&["[INFO] server started"]).is_empty());
    assert!(sources(&["[x^2]"]).is_empty());
    assert!(sources(&["result [ x^2 ] done"]).is_empty());
    assert!(sources(&["[", r"C:\temp\integral.txt", "]"]).is_empty());
}

#[test]
fn markdown_unescaped_bare_parens_render_inline() {
    assert_eq!(
        sources(&[r"也可以使用 (\sqrt{x^2+y^2}) 表示行内公式"]),
        vec![(r"\sqrt{x^2+y^2}".into(), false)]
    );
    assert_eq!(sources(&["• 质能方程为 (E=mc^2)。"]), vec![("E=mc^2".into(), false)]);
    let sequence = r"a_n=a_1+(n-1)d";
    assert_eq!(sources(&[&format!("• ({sequence})，done")]), vec![(sequence.into(), false)]);
    compile_formula(sequence, false, 18.0, 1.0, DEFAULT_LIMITS)
        .expect("the recovered sequence formula must compile");
    // 括号深度配对：`\sin(x)` 的内层 `)` 不提前截断。
    assert_eq!(sources(&[r"值 (\sin(x)) 收敛"]), vec![(r"\sin(x)".into(), false)]);
}

#[test]
fn codex_line_with_multiple_inline_formulas_keeps_bare_paren_math() {
    let source = r"$E = mc^2$，$a^2 + b^2 = c^2$，$e^{i\pi}+1=0$，(\displaystyle \int_0^1 x^2,dx=\frac13)，$\sum_{n=1}^{\infty}\frac1{n^2}=\frac{\pi^2}{6}$";
    let formulas = sources(&[source]);
    assert_eq!(formulas.len(), 5);
    assert_eq!(formulas[3], (r"\displaystyle \int_0^1 x^2,dx=\frac13".into(), false));
    compile_formula(&formulas[3].0, false, 18.0, 1.0, DEFAULT_LIMITS)
        .expect("Codex's unbraced fraction arguments must compile");
}

#[test]
fn implicit_product_equations_use_the_standard_delimiter_rule() {
    let expected = vec![("F=ma".into(), false), ("PV=nRT".into(), false)];
    assert_eq!(sources(&[r"\(F=ma\), \(PV=nRT\)"]), expected);
    assert_eq!(sources(&["$F=ma$, $PV=nRT$"]), expected);
    assert_eq!(
        sources(&[r"(\displaystyle F=ma), (\displaystyle PV=nRT)"]),
        vec![(r"\displaystyle F=ma".into(), false), (r"\displaystyle PV=nRT".into(), false),]
    );

    assert_eq!(sources(&[r"\(key=value\)"]), Vec::new());
    assert_eq!(sources(&["$key=value$"]), Vec::new());
    assert!(sources(&["setting (key=value)"]).is_empty());
}

#[test]
fn bare_parens_reject_regex_prose_and_single_letter_escapes() {
    assert!(sources(&[r"grep -E (\d+) input.txt"]).is_empty());
    assert!(sources(&[r"match (\w*) here"]).is_empty());
    assert!(sources(&["plain (normal prose) text"]).is_empty());
    assert!(sources(&["setting (key=value)"]).is_empty());
    assert!(sources(&[r"case (\n) newline"]).is_empty());
    assert!(sources(&[r"path (C:\temp\frac) oops"]).is_empty());
}

/// 定界符表明意图，内容提供证据——**两者都要**。只看定界符会把终端里
/// 满地的 shell sigil、提示符和价格全渲染成公式，所以这些形状必须保持
/// 原文。它们不是理论候选：`HOME`、`npm install`、`PATH=/tmp` 在
/// pulldown-latex 里都能成功解析成公式事件，判定放过就是真替换。
#[test]
fn explicit_delimiters_still_require_content_evidence() {
    assert!(sources(&[r"escaped \$x$"]).is_empty());
    // 货币、全大写环境变量、散文、配置串：噪音否决层与证据层各管一段。
    assert!(sources(&["price $5$ and $12.50$"]).is_empty());
    assert!(sources(&["env $LONG_VARIABLE$"]).is_empty());
    assert!(sources(&["quote $USD 20$ today"]).is_empty());
    assert!(sources(&["literal $hello$ text"]).is_empty());
    assert!(sources(&["config $PATH=/tmp$"]).is_empty());
    assert!(sources(&[r"plain \(normal prose\)"]).is_empty());
    assert!(sources(&["$ $", r"\(  \)", "$$  $$"]).is_empty());
    // 路径不是公式：`explicit_operand` 把标识符卡在一个字母，`foo/bar`
    // 的两侧都不合格，所以 `/` 不构成紧凑运算符证据。
    assert!(sources(&["path $foo/bar$"]).is_empty());
    // 真数学照旧：display 块的 lax 证据。
    assert_eq!(sources(&["$$E = mc^2$$"]), vec![("E = mc^2".into(), true)]);
    assert_eq!(sources(&["$a/b$"]), vec![("a/b".into(), false)]);
}

/// 普通 shell 里真实出现过的形状，一个都不许变成公式。
/// 每一行都对应实测过的误报（见 PR #55 review）。
#[test]
fn ordinary_shell_output_stays_literal() {
    // 变量 sigil 成对出现，中间那截会被当成源码。
    assert!(sources(&["echo $HOME $USER"]).is_empty());
    assert!(sources(&["echo $HOME"]).is_empty());
    // sh 家族提示符以 `$ ` 结尾——`$` 后紧跟空白直接否决。
    assert!(sources(&["$ npm install", "$ npm test"]).is_empty());
    assert!(sources(&["user@host:~$ echo hello", "hello", "user@host:~$ ls"]).is_empty());
    // 行内定界符不跨真实换行，两行各自的 `$` 不配对。
    assert!(sources(&["$x", "$y"]).is_empty());
    assert!(sources(&["export $FOO=1", "export $BAR=2"]).is_empty());
    // 价格与提交信息。
    assert!(sources(&["cost $5 vs $7 today"]).is_empty());
    assert!(sources(&["fix: charge $9.99", "feat: refund $9.99"]).is_empty());
    // BRE 捕获组：`\(…\)` 撞 sed/grep 的日常写法。
    assert!(sources(&[r"sed 's/\(abc\)/x/' input.txt"]).is_empty());
    assert!(sources(&[r"grep '\(foo\|bar\)' log.txt"]).is_empty());
}

/// 软换行仍要跨——这是行内公式在窄窗口里的正常形态；跨的是软换行，
/// 不是真实换行。
#[test]
fn inline_formula_crosses_soft_wrap_but_not_a_real_newline() {
    let mut wrapped = TextGrid::from_rows(&["prefix $x^2", "+ y^2$ tail"]);
    wrapped.wrapped[0] = true;
    assert_eq!(scan_grid(&wrapped).len(), 1, "soft wrap 内的行内公式要接起来");

    let hard = TextGrid::from_rows(&["prefix $x^2", "+ y^2$ tail"]);
    assert!(scan_grid(&hard).is_empty(), "真实换行必须断开行内公式");
}

/// display 定界符照旧跨真实换行：`$$` / `\[ \]` 占住它们之间的整块，
/// Agent TUI 硬换行的块级公式靠这条恢复。
#[test]
fn display_formula_still_crosses_a_real_newline() {
    assert_eq!(sources(&["$$", r"\frac{1}{2}", "$$"]).len(), 1);
    assert_eq!(sources(&[r"\[", r"\sum_{i=1}^n i", r"\]"]).len(), 1);
}

/// 基线证据层漏掉的两个真公式。`explicit_operand` 把标识符卡在一个字母，
/// 物理里省略乘号的 `mc^2` / `nRT` 全被误杀；阶乘则压根没有证据项。
#[test]
fn implicit_products_and_factorials_are_math() {
    assert_eq!(sources(&["$E=mc^2$"]), vec![("E=mc^2".into(), false)]);
    assert_eq!(sources(&["$E = mc^2$"]), vec![("E = mc^2".into(), false)]);
    assert_eq!(sources(&["$PV=nRT$"]), vec![("PV=nRT".into(), false)]);
    assert_eq!(sources(&["$F=ma$"]), vec![("F=ma".into(), false)]);
    assert_eq!(sources(&["$n!$"]), vec![("n!".into(), false)]);
    assert_eq!(sources(&[r"\(E=mc^2\)"]), vec![("E=mc^2".into(), false)]);
    // 放宽到三个字母不等于放开散文：这些仍然保持原文。
    assert!(sources(&["$key=value$"]).is_empty());
    assert!(sources(&["$PATH=/tmp$"]).is_empty());
    assert!(sources(&["$npm install$"]).is_empty());
    assert!(sources(&["$Hello!$"]).is_empty());
}

/// 失败候选必须有预算：`MAX_VISIBLE_FORMULAS` 只限成功数，不限失败数。
/// 满屏未闭合的 `(` 曾让单帧扫描从 ~2ms 涨到 ~322ms（50×200 debug）。
#[test]
fn unclosed_bare_delimiters_stay_within_budget() {
    let dense: String = std::iter::repeat("(x ").take(66).collect();
    let rows: Vec<&str> = std::iter::repeat(dense.as_str()).take(50).collect();
    let grid = TextGrid::from_rows(&rows);
    let plain: String = std::iter::repeat("plain text ").take(18).collect();
    let plain_rows: Vec<&str> = std::iter::repeat(plain.as_str()).take(50).collect();
    let baseline_grid = TextGrid::from_rows(&plain_rows);

    let start = std::time::Instant::now();
    assert!(scan_grid(&grid).is_empty(), "未闭合的裸括号不产生公式");
    let dense_cost = start.elapsed();
    let start = std::time::Instant::now();
    assert!(scan_grid(&baseline_grid).is_empty());
    let baseline_cost = start.elapsed();

    // 预算把每个候选的搜索钉在常数行数上，于是总成本跟着格子数走而不是
    // 格子数的平方。放宽到 12 倍是给 CI 抖动留的余量，回归会是三位数倍。
    assert!(
        dense_cost < baseline_cost * 12,
        "未闭合括号扫描 {dense_cost:?} 相对基准 {baseline_cost:?} 退化成二次复杂度"
    );
}

#[test]
fn single_dollar_accepts_only_explicit_math_shapes() {
    assert_eq!(sources(&["$x$ $x_1$ $2+2$ $a/b$ $x=y$ $f(x)$ $f(x)=0$"]).len(), 7);
    assert_eq!(sources(&[r"$\frac{1}{2}$ $\sin x$ $x^2$"]).len(), 3);
}

#[test]
fn single_dollar_uses_the_standard_formula_rule_without_agent_context() {
    assert_eq!(sources(&["plain $x$ text"]), vec![("x".into(), false)]);
    assert_eq!(
        sources(&["质能方程 $E = mc^2$，其中 c 为光速。"]),
        vec![("E = mc^2".into(), false)]
    );
    assert_eq!(
        sources(&[
            r"二次方程：$x = \dfrac{-b \pm \sqrt{b^2 - 4ac}}{2a}$",
            r"斯特林公式：$n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n$",
        ],),
        vec![
            (r"x = \dfrac{-b \pm \sqrt{b^2 - 4ac}}{2a}".into(), false),
            (r"n! \approx \sqrt{2\pi n}\left(\dfrac{n}{e}\right)^n".into(), false),
        ]
    );
}

#[test]
fn persisted_formula_survives_partial_scroll_without_rescaling_its_bounds() {
    let mut state = TerminalMathState::default();
    let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
    remember_visible(&mut state, &initial);
    assert_eq!(state.formulas.len(), 1);

    let scrolled = grid_at(&["x^2  ", "$$   ", "tail ", "next "], 41, 0);
    state.synchronize_grid(&scrolled);
    assert!(scan_grid(&scrolled).is_empty());
    let overlays = state.visible_overlays(&scrolled);

    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].source.as_ref(), "x^2");
    assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-1));
    assert_eq!(overlays[0].spans.last().map(|span| span.row), Some(1));
}

#[test]
fn streamed_display_formula_completes_after_opening_scrolls_into_history() {
    let mut state = TerminalMathState::default();
    let opening = grid_at(
        &[
            "$$                      ",
            r"\begin{aligned}         ",
            r"f(x) &= x^2 \\          ",
            "h(x) &= x^3            ",
        ],
        40,
        0,
    );
    state.synchronize_grid(&opening);
    assert!(state.scan_visible_grid(&opening, None).is_none());

    let visible_tail = grid_at(
        &[
            r"g(x) &= x+1            ",
            r"\end{aligned}           ",
            "$$                      ",
            "tail                    ",
        ],
        44,
        0,
    );
    state.synchronize_grid(&visible_tail);
    let history_anchor = state
        .scan_visible_grid(&visible_tail, None)
        .expect("closing delimiter requests the pending opening from history");
    assert_eq!(history_anchor, FormulaAnchor { row: 40, column: 0 });

    let history = grid_at(
        &[
            "$$                      ",
            r"\begin{aligned}         ",
            r"f(x) &= x^2 \\          ",
            "h(x) &= x^3            ",
            "g(x) &= x+1            ",
            r"\end{aligned}           ",
            "$$                      ",
            "tail                    ",
        ],
        40,
        0,
    );
    assert!(state.complete_pending_from_history(&history));

    let overlays = state.visible_overlays(&visible_tail);
    assert_eq!(overlays.len(), 1);
    assert!(overlays[0].source.contains("f(x)"));
    assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-4));
    assert_eq!(overlays[0].spans.last().map(|span| span.row), Some(2));
}

#[test]
fn visible_content_mismatch_drops_a_persisted_formula() {
    let mut state = TerminalMathState::default();
    let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
    remember_visible(&mut state, &initial);

    let changed = grid_at(&["y^2  ", "$$   ", "tail ", "next "], 41, 0);
    state.synchronize_grid(&changed);
    assert!(state.visible_overlays(&changed).is_empty());
    assert!(state.formulas.is_empty());
}

#[test]
fn tui_redraw_blank_interim_frame_keeps_the_persisted_formula() {
    let mut state = TerminalMathState::default();
    let initial = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 40, 0);
    remember_visible(&mut state, &initial);
    assert_eq!(state.formulas.len(), 1);

    // A TUI clears a line before repainting it: a partially blanked frame
    // must not evict the formula (this was the input-time flicker).
    let interim = grid_at(&["$$   ", "     ", "$$   ", "tail "], 40, 0);
    state.synchronize_grid(&interim);
    assert_eq!(state.visible_overlays(&interim).len(), 1);
    assert_eq!(state.formulas.len(), 1);

    // Rewritten with different visible content -> genuinely gone.
    let changed = grid_at(&["other", "words", "here ", "tail "], 40, 0);
    state.synchronize_grid(&changed);
    assert!(state.visible_overlays(&changed).is_empty());
}

#[test]
fn orphan_closing_delimiter_recovers_the_formula_from_history() {
    let mut state = TerminalMathState::default();
    // Viewport starts below the formula opener: only the closing `$$` is
    // visible (e.g. the persisted copy was evicted by a redraw glitch).
    let visible = grid_at(&["$$   ", "tail "], 45, 0);
    state.synchronize_grid(&visible);
    assert!(state.scan_visible_grid(&visible, None).is_none());

    let anchor = state
        .scan_visible_grid(&visible, None)
        .expect("stable orphan delimiter requests one history pass");
    assert_eq!(anchor, FormulaAnchor { row: 45, column: 0 });

    let history = grid_at(&["$$   ", "x^2  ", "$$   ", "tail "], 43, 0);
    assert!(state.complete_pending_from_history(&history));

    let overlays = state.visible_overlays(&visible);
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].source.as_ref(), "x^2");
    assert_eq!(overlays[0].spans.first().map(|span| span.row), Some(-2));

    // The attempt is consumed: the same orphan never re-triggers, even
    // though the scanner keeps reporting it as unmatched every frame.
    assert!(state.scan_visible_grid(&visible, None).is_none());
    assert!(state.scan_visible_grid(&visible, None).is_none());
}

#[test]
fn reflow_history_pruning_and_memory_limits_invalidate_bounded_state() {
    let mut state = TerminalMathState::default();
    for index in 0..MAX_PERSISTED_FORMULAS + 32 {
        let absolute_top = index * 3;
        let grid = grid_at(&["$$ ", "x^2", "$$ "], absolute_top, 0);
        remember_visible(&mut state, &grid);
    }
    assert!(state.formulas.len() <= MAX_PERSISTED_FORMULAS);
    assert!(state.persisted_bytes <= PERSISTED_FORMULA_BUDGET);

    let reflowed = grid_at(&["$$  ", "x^2 ", "$$  "], 0, 0);
    state.synchronize_grid(&reflowed);
    assert!(state.formulas.is_empty());

    let initial = grid_at(&["$$ ", "x^2", "$$ "], 90, 0);
    remember_visible(&mut state, &initial);
    let pruned = grid_at(&["text"], 93, 93);
    state.synchronize_grid(&pruned);
    assert!(state.formulas.is_empty());
}

/// Cell geometry proportional to a real terminal font: monospace advance
/// is ~0.6 em and line height ~1.3 em, so budgets in these tests mean the
/// same thing they mean on screen.
fn test_size(columns: f32, lines: f32) -> SizeInfo {
    SizeInfo::new(columns * 12.0, lines * 26.0, 12.0, 26.0, 0.0, 0.0, false)
}

/// Nominal terminal font size behind [`test_size`].
const TEST_FONT_PX: f32 = 20.0;

fn fitted_size(rows: &[&str]) -> f32 {
    let grid = TextGrid::from_rows(rows);
    let overlays = scan_grid_with_hints(&grid);
    assert_eq!(overlays.len(), 1, "expected exactly one formula in {rows:?}");
    let size = test_size(grid.columns as f32, grid.rows.len() as f32);
    let mut state = TerminalMathState::default();
    let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
    prepared[0].expect("formula must render").fitted_pixel_size
}

#[test]
fn display_formula_with_blank_neighbours_keeps_the_terminal_font_size() {
    let fitted = fitted_size(&[
        "                    ",
        "$$                  ",
        r"\sum_{i=1}^{n} i^2  ",
        "$$                  ",
        "                    ",
    ]);
    assert_eq!(fitted, TEST_FONT_PX, "blank-neighbour display math must not shrink");
}

/// The whole point of the blank-row budget: the same source keeps one size
/// whether the emitter put it on one line or spread it over three.
#[test]
fn display_math_size_does_not_depend_on_how_many_rows_the_source_used() {
    let one_line = fitted_size(&[
        "                              ",
        r"$$\sum_{i=1}^{n} i^2$$        ",
        "                              ",
    ]);
    let three_lines = fitted_size(&[
        "                              ",
        "$$                            ",
        r"\sum_{i=1}^{n} i^2            ",
        "$$                            ",
        "                              ",
    ]);
    assert_eq!(one_line, three_lines);
    assert_eq!(one_line, TEST_FONT_PX);
}

/// A row hemmed in on both sides by prose that reaches under the formula
/// is the one case where the budget really is a single line gap. The
/// formula must then trade layout style — and only as a last resort a few
/// percent of size — for staying inside it, because the clip crops exactly
/// what the fit could not absorb.
#[test]
fn tall_formula_hemmed_in_by_prose_stays_inside_the_line_gap_budget() {
    let rows = &[
        "prose above that runs under the formula",
        r"$$\frac{x^2+1}{y-1}$$                  ",
        "prose below that runs under the formula",
    ];
    let grid = TextGrid::from_rows(rows);
    let overlays = scan_grid_with_hints(&grid);
    assert_eq!(overlays.len(), 1);
    let size = test_size(grid.columns as f32, 8.0);

    let mut state = TerminalMathState::default();
    let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
    let prepared = prepared[0].expect("formula must render");
    assert_eq!(
        prepared.bleed_top,
        size.cell_height() * DISPLAY_BLEED_INTO_PROSE,
        "display math should leave a small prose clearance above",
    );
    assert_eq!(
        prepared.bleed_bottom,
        size.cell_height() * DISPLAY_BLEED_INTO_PROSE,
        "display math should leave a small prose clearance below",
    );
    assert!(!prepared.display_style, "the compact style must be tried before any size is given up",);
    assert!(
        prepared.fitted_pixel_size >= TEST_FONT_PX * 0.9,
        "compact style should cost at most a few percent, got {}",
        prepared.fitted_pixel_size,
    );

    // The invariant behind the whole scheme: re-laid-out ink stays inside
    // bounds plus the prose-side budgets and the deliberate overrun
    // tolerance, so the clip only ever trims an antialiasing edge.
    let layout = state
        .layout(
            overlays[0].formula_id,
            &overlays[0].source,
            prepared.fitted_pixel_size,
            1.0,
            prepared.display_style,
        )
        .expect("fitted layout");
    let budget = size.cell_height()
        * (1.0 + DISPLAY_BLEED_INTO_PROSE * 2.0)
        * (1.0 + HEIGHT_OVERRUN_TOLERANCE);
    assert!(
        layout.metrics.height + layout.metrics.depth <= budget + 0.5,
        "fitted ink {} must fit the prose budget {}",
        layout.metrics.height + layout.metrics.depth,
        budget,
    );
}

#[test]
fn display_math_uses_a_smaller_prose_bleed_than_inline_math() {
    let display_grid = TextGrid::from_rows(&["above", "$$x^2$$", "below"]);
    let display_overlays = scan_grid_with_hints(&display_grid);
    let display_size = test_size(display_grid.columns as f32, display_grid.rows.len() as f32);
    let display_overlay = &display_overlays[0];
    let display_bounds = display_overlay.bounds(&display_size).expect("display bounds");
    let display_bleed = display_overlay.vertical_bleed(&display_size, (0, 0));

    let inline_grid = TextGrid::from_rows(&["value $x^2$ grows"]);
    let inline_overlays = scan_grid_with_hints(&inline_grid);
    let inline_size = test_size(inline_grid.columns as f32, inline_grid.rows.len() as f32);
    let inline_overlay = &inline_overlays[0];
    let inline_bleed = inline_overlay.vertical_bleed(&inline_size, (0, 0));

    assert!(display_overlay.display);
    assert!(!inline_overlay.display);
    assert!(display_bleed.0 < inline_bleed.0);
    assert!(display_bleed.1 < inline_bleed.1);
    assert_eq!(display_bleed.0, display_bounds.height() * DISPLAY_BLEED_INTO_PROSE);
}

/// Prose that stops well before the formula's columns is not in the way:
/// the ink of a centred block lands where those rows are empty, so it may
/// use their height and keep both the display style and the font size.
#[test]
fn short_prose_neighbours_do_not_shrink_a_centred_block() {
    let rows = &[
        "其中：                                  ",
        r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$",
        "下面继续说明。                            ",
    ];
    let grid = TextGrid::from_rows(rows);
    let overlays = scan_grid_with_hints(&grid);
    assert_eq!(overlays.len(), 1);

    let mut state = TerminalMathState::default();
    let size = test_size(grid.columns as f32, 8.0);
    let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
    let prepared = prepared[0].expect("formula must render");
    assert_eq!(prepared.fitted_pixel_size, TEST_FONT_PX);
    assert!(prepared.display_style, "there is room for the block style here");
}

#[test]
fn inline_formula_between_prose_keeps_the_terminal_font_size() {
    let fitted = fitted_size(&["value $x^2$ grows   "]);
    assert_eq!(
        fitted, TEST_FONT_PX,
        "a superscript must fit the line-gap budget without shrinking",
    );
}

#[test]
fn adjacent_formula_rows_do_not_share_vertical_clip_budget() {
    let rows = [
        r"left $\frac{a_1+b^2}{c_i-d^3}+x_i^2$",
        r"mid $\int_0^\infty e^{-x^2}dx+\sum_{k=1}^n k^2$",
        r"right $\frac{\sum_{i=1}^n x_i}{\sqrt{1+x_{j-1}^2}}+y_{m+1}$",
    ];
    let grid = TextGrid::from_rows(&rows);
    let overlays = scan_grid_with_hints(&grid);
    assert_eq!(overlays.len(), 3);
    let size = test_size(grid.columns as f32, grid.rows.len() as f32);
    let mut state = TerminalMathState::default();
    let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);

    for (index, overlay) in overlays.iter().enumerate() {
        let bounds = overlay.bounds(&size).expect("formula bounds");
        let metrics = TerminalMathState::default()
            .layout(overlay.formula_id, &overlay.source, TEST_FONT_PX, 1.0, false)
            .expect("formula layout")
            .metrics;
        let columns = ink_columns(overlay, &size, bounds, bounds.right, metrics.width);
        let absorbed = overlay.absorbable_rows(columns);
        let (above, below) = overlay.vertical_bleed(&size, absorbed);
        if index > 0 {
            assert_eq!(above, 0.0, "formula row {index} must not bleed into row above");
        }
        if index + 1 < overlays.len() {
            assert_eq!(below, 0.0, "formula row {index} must not bleed into row below");
        }

        let fitted = prepared[index].expect("dense formula must render");
        let layout = state
            .layout(
                overlay.formula_id,
                &overlay.source,
                fitted.fitted_pixel_size,
                1.0,
                fitted.display_style,
            )
            .expect("fitted formula layout");
        let available_height = bounds.height() + fitted.bleed_top + fitted.bleed_bottom;
        assert!(
            layout.metrics.height + layout.metrics.depth <= available_height + 0.5,
            "formula row {index} ink {} exceeds its clip budget {}",
            layout.metrics.height + layout.metrics.depth,
            available_height,
        );
    }
}

/// The acceptance rule: formulas with the same delimiter should read at one size;
/// at one size. Groups may differ from each other — an inline fraction is
/// meant to be more compact than a block one — but within a group a reader
/// scanning an answer must not see one formula shrunk against the others.
#[test]
fn formulas_of_the_same_kind_render_at_one_size() {
    let rows = &[
        "核心想法：与其硬选一个 prompt，不如软组合一堆 prompt 组件。      ",
        "                                                              ",
        "- 用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：   ",
        r"$$P_i = \sum_j a_{i,j} \cdot c_j$$                            ",
        "- 这样每个任务的 prompt 是所有组件的连续加权组合。              ",
        "                                                              ",
        "单行块紧贴正文：                                                ",
        r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$                       ",
        "下面继续说明。                                                  ",
        "                                                              ",
        r"高斯积分：$$\int_{-\infty}^{\infty} e^{-x^2}dx = \sqrt{\pi}$$   ",
        "                                                              ",
        "$$                                                            ",
        r"A = \begin{pmatrix}1 & 2 \\ 3 & 4\end{pmatrix}                ",
        "$$                                                            ",
        "                                                              ",
        r"内联的分式 $\frac{a+b}{c-d}$ 与根号 $\sqrt{x^2+y^2}$ 混排收尾。 ",
        r"行内的求和 $\sum_{i=1}^{n} i$ 和上标 $x^2$ 也在同一段里。       ",
    ];
    let grid = TextGrid::from_rows(rows);
    let overlays = scan_grid_with_hints(&grid);
    let size = test_size(grid.columns as f32, grid.rows.len() as f32);
    let mut state = TerminalMathState::default();
    let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);

    let mut sizes: BTreeMap<bool, Vec<(String, f32)>> = BTreeMap::new();
    for (overlay, prepared) in overlays.iter().zip(&prepared) {
        let prepared = prepared.expect("every formula in the sample must render");
        sizes
            .entry(overlay.display)
            .or_default()
            .push((overlay.source.to_string(), prepared.fitted_pixel_size));
    }
    assert_eq!(sizes.len(), 2, "sample must exercise both kinds");
    for (display, group) in sizes {
        assert!(group.len() >= 4, "each kind needs several samples, got {group:?}");
        // A hard formula boundary may require a small local reduction to
        // keep a script intact; unconstrained neighbours remain at the
        // terminal size and no formula is allowed to become unreadably small.
        assert!(
            group.iter().all(|(_, size)| *size >= TEST_FONT_PX * 0.9),
            "{} formulas must stay readable: {group:?}",
            if display { "block" } else { "inline" },
        );
    }
}

/// The ceiling behind the raised script sizes: a formula sharing its row
/// with prose may grow past one row, but never past two — beyond that it
/// stops being ink in a line gap and starts being ink on the neighbours.
#[test]
fn inline_math_never_grows_past_two_rows() {
    let sources = [
        r"$\frac{a+b}{c-d}$",
        r"$\int_0^\infty e^{-x^2}dx$",
        r"$\sqrt{\frac{a+b}{c+d}}$",
        r"$x_{i+1}^2+y_{j-1}^2$",
        r"$\sum_{i=1}^{n} i$",
    ];
    let size = test_size(60.0, 4.0);
    for source in sources {
        let row = format!("值 {source} 收敛                        ");
        let grid = TextGrid::from_rows(&[&row]);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 1, "{source}");

        let mut state = TerminalMathState::default();
        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
        let prepared = prepared[0].expect("formula must render");
        let layout = state
            .layout(
                overlays[0].formula_id,
                &overlays[0].source,
                prepared.fitted_pixel_size,
                1.0,
                prepared.display_style,
            )
            .expect("fitted layout");
        let rows = (layout.metrics.height + layout.metrics.depth) / size.cell_height();
        assert!(rows <= 2.0, "{source} rendered {rows} rows tall");
    }
}

#[test]
fn display_math_mask_only_covers_the_formula_source() {
    let grid = TextGrid::from_rows(&[
        "     explanation in the main pane                                  $0.00 spent      ",
        "     $$E = mc^2$$                                                                    ",
    ]);
    let overlays = scan_grid_with_hints(&grid);
    let coverage = CoverageMask::build(&overlays, &[Some(())]);

    assert!(coverage.covers(Point::new(1, Column(5))), "formula source is hidden");
    assert!(coverage.covers(Point::new(1, Column(16))), "formula source is hidden");
    assert!(
        !coverage.covers(Point::new(1, Column(17))),
        "centering space keeps its TUI background"
    );
    assert!(!coverage.covers(Point::new(1, Column(41))), "trailing TUI background is preserved");
}

#[test]
fn display_math_without_a_right_pane_uses_the_viewport() {
    let grid = TextGrid::from_rows(&[
        "     ordinary prose across one terminal row                                      ",
        "     $$E = mc^2$$                                                                  ",
    ]);
    let overlays = scan_grid_with_hints(&grid);

    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].widen_right_to, Some(grid.columns));
}

/// 诊断用：打印各场景的缩放比例，`cargo test diagnose -- --nocapture`。
#[test]
fn diagnose_sizes() {
    let blank = " ".repeat(80);
    let blank = blank.as_str();
    let long = r"\pm\quad \mp\quad \times\quad \div\quad \cdot\quad \ast\quad \circ\quad \bullet\quad \oplus\quad \otimes";
    let cases: Vec<(&str, Vec<&str>, bool)> = vec![
        ("display 3行 sum", vec![blank, "$$", r"\sum_{i=1}^{n} i^2", "$$", blank], false),
        ("display 1行 sum", vec![blank, r"$$\sum_{i=1}^{n} i^2$$", blank], false),
        ("display 1行 frac", vec![blank, r"$$\frac{x^2+1}{y-1}$$", blank], false),
        (
            "display 1行 矩阵",
            vec![blank, r"$$A=\begin{pmatrix}1&2\\3&4\end{pmatrix}$$", blank],
            false,
        ),
        (
            "display 3行 矩阵",
            vec![blank, "$$", r"A=\begin{pmatrix}1&2\\3&4\end{pmatrix}", "$$", blank],
            false,
        ),
        (
            "display 3行 大矩阵",
            vec![blank, "$$", r"A=\begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix}", "$$", blank],
            false,
        ),
        ("display 3行 长串", vec![blank, "$$", long, "$$", blank], false),
        ("display 3行 紧贴正文", vec!["前文", "$$", r"\frac{x^2+1}{y-1}", "$$", "后文"], false),
        (
            "display 1行 紧贴正文",
            vec![
                "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                r"$$P_i = \sum_j attention_{i,j} \cdot component_j$$",
                "- 这样每个任务的 prompt 不是从池子里挑一个，而是所有组件的连续加权组合。",
            ],
            false,
        ),
        (
            "display 1行 sum 夹住",
            vec!["前面的说明文字：", r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$", "下面继续说明。"],
            false,
        ),
        // 用户 08-05 截图的真实排版：上下都是跑满整行的长正文，公式的
        // 墨迹列被两侧的字覆盖，一行的高度就是全部预算。
        (
            "display 长正文夹住 sum",
            vec![
                "单行块紧贴正文：- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                r"$$\sum_{i=1}^{n} i = \frac{n(n+1)}{2}$$",
                "下面继续说明。- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
            ],
            false,
        ),
        (
            "display 长正文夹住 int",
            vec![
                "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
                r"高斯积分：$$\int_{-\infty}^{\infty} e^{-x^2}dx = \sqrt{\pi}$$",
                "- 来一张图片，用 query 对所有组件算注意力权重，然后加权求和出一组 prompt：",
            ],
            false,
        ),
        (
            "display 1行 矩阵夹住",
            vec!["前面的说明：", r"$$A=\begin{pmatrix}1&2\\3&4\end{pmatrix}$$", "下面继续。"],
            false,
        ),
        ("inline x^2", vec!["value $x^2$ grows"], true),
        ("inline frac", vec![r"value $\frac{a}{b}$ grows"], true),
        ("inline 大 frac", vec![r"value $\frac{x^2+1}{y-1}$ grows"], true),
        ("inline sqrt", vec![r"value $\sqrt{x^2+y^2}$ grows"], true),
        ("inline sum", vec![r"value $\sum_{i=1}^{n} i$ grows"], true),
        ("inline int", vec![r"value $\int_0^\infty e^{-x^2}dx$ ok"], true),
        ("inline 短源码", vec![r"值 $x_{i+1}^2+y_{j-1}^2$ 收敛"], true),
    ];
    println!(
        "\n{:<22} {:>6} {:>6} {:>16} {:>16}",
        "场景", "比例", "样式", "墨迹 宽×高", "预算 宽×高"
    );
    for (name, rows, expected_inline) in cases {
        let grid = TextGrid::from_rows(&rows);
        let overlays = scan_grid_with_hints(&grid);
        assert_eq!(overlays.len(), 1, "{name}: expected one formula");
        let overlay = &overlays[0];
        assert_eq!(!overlay.display, expected_inline, "{name}: unexpected formula style");
        let size = test_size(grid.columns as f32, grid.rows.len() as f32);
        let mut state = TerminalMathState::default();

        let bounds = overlay.bounds(&size).expect("bounds");
        let right = overlay.widen_right_to.map_or(bounds.right, |column| {
            (size.padding_x() + column as f32 * size.cell_width()).max(bounds.right)
        });
        let base = state
            .layout(overlay.formula_id, &overlay.source, TEST_FONT_PX, 1.0, overlay.display)
            .expect("layout")
            .metrics;
        let absorbed =
            overlay.absorbable_rows(ink_columns(overlay, &size, bounds, right, base.width));
        let (bleed_top, bleed_bottom) = overlay.vertical_bleed(&size, absorbed);
        let budget_width = right - bounds.left - FORMULA_INSET * 2.0;
        let budget_height = bounds.height() + bleed_top + bleed_bottom;

        let prepared = prepare_overlays(&mut state, &overlays, &size, TEST_FONT_PX, 1.0);
        let prepared = prepared[0].expect("formula must render");
        println!(
            "{name:<22} {:>5.0}% {:>6} {:>7.0}×{:<8.0} {:>7.0}×{:<8.0}",
            prepared.fitted_pixel_size / TEST_FONT_PX * 100.0,
            if prepared.display_style { "块级" } else { "行内" },
            base.width,
            base.height + base.depth,
            budget_width,
            budget_height,
        );
    }
}

/// 每帧扫描与冷编译的量尺，不是断言。跑法：
/// `cargo test -p nebula --bin nebula terminal_math::tests::measure_scan -- --ignored --nocapture`
#[test]
#[ignore = "timing probe, run by hand"]
fn measure_scan_and_compile_cost() {
    use std::time::Instant;

    // 50×200 的视口，塞满带 `$`、`[`、`(` 的正文：每个候选都触发一次搜索
    // 却没有一个能成公式，是扫描器最贵的形状。
    let prose = "price is $5 and $HOME/bin (see [1]) costs 3 (x) dollars $ USD [a, b] ";
    let row: String = prose.repeat(200 / prose.chars().count() + 1).chars().take(200).collect();
    let rows: Vec<&str> = (0..50).map(|_| row.as_str()).collect();
    let grid = TextGrid::from_rows(&rows);
    let iterations = 200;
    let start = Instant::now();
    for _ in 0..iterations {
        let scan = scan_grid_result(&grid);
        assert!(scan.overlays.is_empty());
    }
    let per_scan = start.elapsed() / iterations;
    println!("scan 50x200 prose (worst case, 0 formulas): {per_scan:?} per frame");

    // 同样尺寸，塞满能命中的行内公式：每帧的公式上限是 MAX_VISIBLE_FORMULAS。
    let formula_row = "the energy $E = mc^2$ and $\\int_0^1 x\\,dx = \\frac{1}{2}$ hold; ";
    let row: String =
        formula_row.repeat(200 / formula_row.chars().count() + 1).chars().take(200).collect();
    let rows: Vec<&str> = (0..50).map(|_| row.as_str()).collect();
    let grid = TextGrid::from_rows(&rows);
    let start = Instant::now();
    for _ in 0..iterations {
        let scan = scan_grid_result(&grid);
        assert_eq!(scan.overlays.len(), MAX_VISIBLE_FORMULAS);
    }
    let per_scan = start.elapsed() / iterations;
    println!(
        "scan 50x200 inline formulas (capped at {MAX_VISIBLE_FORMULAS}): {per_scan:?} per frame"
    );

    let screenshot: [&str; 7] = [
        r"e^{i\pi}+1=0",
        r"\int_{-\infty}^{+\infty} e^{-x^2},dx=\sqrt{\pi}",
        "\\widehat{f}(\\xi)\n\n\\int_{-\\infty}^{+\\infty}\nf(x)e^{-2\\pi i x\\xi},dx",
        "\\operatorname{Attention}(Q,K,V)\n\n\\operatorname{softmax}\\left(\n\\frac{QK^{\\mathsf T}}{\\sqrt{d_k}}\n\\right)V",
        "\\phi(x)=\n\\begin{cases}\nx, & x\\ge 0,\\\n\\alpha(e^x-1), & x<0.\n\\end{cases}",
        "A=\n\\begin{pmatrix}\na_{11} & a_{12} & \\cdots & a_{1n}\\\na_{21} & a_{22} & \\cdots & a_{2n}\\\n\\vdots & \\vdots & \\ddots & \\vdots\\\na_{m1} & a_{m2} & \\cdots & a_{mn}\n\\end{pmatrix}",
        "\\begin{aligned}\n(a+b)^3\n&=(a+b)(a+b)^2\\\n&=(a+b)(a^2+2ab+b^2)\\\n&=a^3+3a^2b+3ab^2+b^3\n\\end{aligned}",
    ];
    for source in screenshot {
        let start = Instant::now();
        for _ in 0..iterations {
            compile_formula(source, true, 18.0, 1.0, DEFAULT_LIMITS).expect("compiles");
        }
        let per_compile = start.elapsed() / iterations;
        let head: String =
            source.chars().take(28).map(|c| if c == '\n' { ' ' } else { c }).collect();
        println!("compile {head:<30} {per_compile:?} cold (cache hit is a map lookup)");
    }
}
