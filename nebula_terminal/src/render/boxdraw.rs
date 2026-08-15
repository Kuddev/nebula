//! 内建字形几何：框线、块元素与 Powerline 分隔符。
//!
//! 从 nebula_app 的 builtin_font（像素栅格化）移植而来，改为输出纯几何
//! 指令（矩形 + 凸多边形），由前端用自己的图元 API 填充。放进渲染合同的
//! 理由与 `RenderSnapshot` 相同：这些字符必须精确盖满单元格、相邻单元格
//! 必须无缝拼接，字体（尤其 CJK 字体）保证不了这一点；几何是唯一权威，
//! 且任何渲染后端都免费继承同一套字形。
//!
//! 坐标系：单元格局部、逻辑像素、y 向下。内部先换算到设备像素做整数
//! 吸附（与旧渲染器同规则），出口再除回 scale——这是笔画清晰、跨行列
//! 连续的关键。与旧实现的两点已知差异：
//! - Powerline 三角/箭头在窄单元格下不再回退字体，而是在右缘截平
//!   （几何裁剪天然优雅，无需 None 逃逸路径）；
//! - ░▒▓ 以 alpha 表达浓度（64/128/192 ÷ 255，与旧像素灰度一致）。

use std::f32::consts::{FRAC_PI_2, PI};

/// 该字符是否由内建几何绘制。快照据此把单元格路由到
/// [`super::RenderSnapshot::box_glyphs`] 而不是文本段。
pub fn is_builtin(c: char) -> bool {
    matches!(
        c,
        // 框线与块元素。
        '\u{2500}'..='\u{259f}'
        // Legacy Computing 六分块与上部块。
        | '\u{1fb00}'..='\u{1fb3b}'
        | '\u{1fb82}'..='\u{1fb8b}'
        // Powerline 三角/箭头与圆头（e0b5 不在内建集内）。
        | '\u{e0b0}'..='\u{e0b4}'
        | '\u{e0b6}'
    )
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 单个填充图元，单元格局部逻辑像素坐标。
#[derive(Clone, Debug, PartialEq)]
pub enum Primitive {
    /// 轴对齐实心矩形；alpha 仅 ░▒▓ 使用（浓度档），其余为 1。
    Rect { rect: Rect, alpha: f32 },
    /// 凸多边形（对角线、Powerline 笔画、圆弧分段）。顶点保证凸序，
    /// 消费方可安全用首点三角扇填充。
    Poly { points: Vec<[f32; 2]> },
}

/// 生成 `c` 的填充图元。`width`/`height` 是该字形占据的逻辑像素盒
/// （宽字符由调用方先乘 2），`scale` 是设备缩放。非内建字符返回 `None`。
pub fn primitives(c: char, width: f32, height: f32, scale: f32) -> Option<Vec<Primitive>> {
    if !is_builtin(c) {
        return None;
    }
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let mut geom = Geom::new((width * scale).round().max(1.0), (height * scale).round().max(1.0));
    draw(c, &mut geom);
    Some(geom.finish(scale))
}

/// 设备像素坐标下的图元累加器：镜像旧 `Canvas` 的直线/矩形语义
/// （整数截断吸附、画布边缘裁剪），但输出几何而不是像素。
struct Geom {
    w: f32,
    h: f32,
    /// 细笔画宽：单元格宽的 1/8（四舍五入），最小 1 设备像素。
    stroke: f32,
    out: Vec<Primitive>,
}

impl Geom {
    fn new(w: f32, h: f32) -> Self {
        let stroke = (w / 8.0).round().max(1.0);
        Self { w, h, stroke, out: Vec::new() }
    }

    fn x_center(&self) -> f32 {
        self.w / 2.0
    }

    fn y_center(&self) -> f32 {
        self.h / 2.0
    }

    /// 横线在 `y` 处、笔画宽 `stroke` 的上下界（整数吸附，同旧 Canvas）。
    fn h_line_bounds(&self, y: f32, stroke: f32) -> (f32, f32) {
        let top = ((y - stroke / 2.0) as i32).max(0) as f32;
        let bottom = ((y + stroke / 2.0) as i32).min(self.h as i32) as f32;
        (top, bottom)
    }

    fn v_line_bounds(&self, x: f32, stroke: f32) -> (f32, f32) {
        let left = ((x - stroke / 2.0) as i32).max(0) as f32;
        let right = ((x + stroke / 2.0) as i32).min(self.w as i32) as f32;
        (left, right)
    }

    /// 实心矩形，裁剪到单元格（旧 Canvas 在画布边缘截断的对应物）。
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rect_alpha(x, y, w, h, 1.0);
    }

    fn rect_alpha(&mut self, x: f32, y: f32, w: f32, h: f32, alpha: f32) {
        let x0 = x.max(0.0);
        let y0 = y.max(0.0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x1 - x0 > 0.0 && y1 - y0 > 0.0 {
            self.out.push(Primitive::Rect {
                rect: Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 },
                alpha,
            });
        }
    }

    /// 从 (`x`, `y`) 起、长 `size` 的横线；`size` 为 0 或笔画为 0 时无操作。
    fn h_line(&mut self, x: f32, y: f32, size: f32, stroke: f32) {
        let (top, bottom) = self.h_line_bounds(y, stroke);
        self.rect(x, top, size, bottom - top);
    }

    fn v_line(&mut self, x: f32, y: f32, size: f32, stroke: f32) {
        let (left, right) = self.v_line_bounds(x, stroke);
        self.rect(left, y, right - left, size);
    }

    /// 凸多边形，顶点不做单元格裁剪：对角线依赖少量出格实现跨行连续。
    fn poly(&mut self, points: Vec<[f32; 2]>) {
        self.out.push(Primitive::Poly { points });
    }

    /// 设备像素 → 逻辑像素。
    fn finish(self, scale: f32) -> Vec<Primitive> {
        let inv = 1.0 / scale;
        self.out
            .into_iter()
            .map(|p| match p {
                Primitive::Rect { rect, alpha } => Primitive::Rect {
                    rect: Rect {
                        x: rect.x * inv,
                        y: rect.y * inv,
                        w: rect.w * inv,
                        h: rect.h * inv,
                    },
                    alpha,
                },
                Primitive::Poly { points } => Primitive::Poly {
                    points: points.into_iter().map(|[x, y]| [x * inv, y * inv]).collect(),
                },
            })
            .collect()
    }
}

fn draw(c: char, g: &mut Geom) {
    let stroke = g.stroke;
    let heavy = stroke * 2.0;
    let (w, h) = (g.w, g.h);
    match c {
        // 对角线 '╱','╲','╳'：中心线贯穿两个对角，竖直方向厚 stroke，
        // 端点上下各出格半个笔画以在相邻行间连续（旧实现用放大画布）。
        '\u{2571}'..='\u{2573}' => {
            let half = stroke / 2.0;
            if c != '\u{2572}' {
                // '╱'：左下 → 右上。
                g.poly(vec![[0.0, h - half], [0.0, h + half], [w, half], [w, -half]]);
            }
            if c != '\u{2571}' {
                // '╲'：左上 → 右下。
                g.poly(vec![[0.0, -half], [0.0, half], [w, h + half], [w, h - half]]);
            }
        },
        // 横向虚线 '┄','┅','┈','┉','╌','╍'。
        '\u{2504}' | '\u{2505}' | '\u{2508}' | '\u{2509}' | '\u{254c}' | '\u{254d}' => {
            let (num_gaps, stroke) = match c {
                '\u{2504}' => (2, stroke),
                '\u{2505}' => (2, heavy),
                '\u{2508}' => (3, stroke),
                '\u{2509}' => (3, heavy),
                '\u{254c}' => (1, stroke),
                '\u{254d}' => (1, heavy),
                _ => unreachable!(),
            };
            let gap_len = (w / 8.0).floor().max(1.0);
            let dash_len = (((w - gap_len * num_gaps as f32).max(0.0)) / (num_gaps + 1) as f32)
                .floor()
                .max(1.0);
            let y = g.y_center();
            for gap in 0..=num_gaps {
                let x = (gap as f32 * (dash_len + gap_len)).min(w);
                g.h_line(x, y, dash_len, stroke);
            }
        },
        // 纵向虚线 '┆','┇','┊','┋','╎','╏'。
        '\u{2506}' | '\u{2507}' | '\u{250a}' | '\u{250b}' | '\u{254e}' | '\u{254f}' => {
            let (num_gaps, stroke) = match c {
                '\u{2506}' => (2, stroke),
                '\u{2507}' => (2, heavy),
                '\u{250a}' => (3, stroke),
                '\u{250b}' => (3, heavy),
                '\u{254e}' => (1, stroke),
                '\u{254f}' => (1, heavy),
                _ => unreachable!(),
            };
            let gap_len = (h / 8.0).floor().max(1.0);
            let dash_len = (((h - gap_len * num_gaps as f32).max(0.0)) / (num_gaps + 1) as f32)
                .floor()
                .max(1.0);
            let x = g.x_center();
            for gap in 0..=num_gaps {
                let y = (gap as f32 * (dash_len + gap_len)).min(h);
                g.v_line(x, y, dash_len, stroke);
            }
        },
        // 单/重线箱形组件：横竖直线、角、T 形、十字与轻重混合过渡。
        // 分解为左右上下四条臂，各自笔画为 0/细/重（表与旧实现逐字一致）。
        '\u{2500}'..='\u{2503}' | '\u{250c}'..='\u{254b}' | '\u{2574}'..='\u{257f}' => {
            // 左臂。
            let stroke_h1 = match c {
                '\u{2500}' | '\u{2510}' | '\u{2512}' | '\u{2518}' | '\u{251a}' | '\u{2524}'
                | '\u{2526}' | '\u{2527}' | '\u{2528}' | '\u{252c}' | '\u{252e}' | '\u{2530}'
                | '\u{2532}' | '\u{2534}' | '\u{2536}' | '\u{2538}' | '\u{253a}' | '\u{253c}'
                | '\u{253e}' | '\u{2540}' | '\u{2541}' | '\u{2542}' | '\u{2544}' | '\u{2546}'
                | '\u{254a}' | '\u{2574}' | '\u{257c}' => stroke,
                '\u{2501}' | '\u{2511}' | '\u{2513}' | '\u{2519}' | '\u{251b}' | '\u{2525}'
                | '\u{2529}' | '\u{252a}' | '\u{252b}' | '\u{252d}' | '\u{252f}' | '\u{2531}'
                | '\u{2533}' | '\u{2535}' | '\u{2537}' | '\u{2539}' | '\u{253b}' | '\u{253d}'
                | '\u{253f}' | '\u{2543}' | '\u{2545}' | '\u{2547}' | '\u{2548}' | '\u{2549}'
                | '\u{254b}' | '\u{2578}' | '\u{257e}' => heavy,
                _ => 0.0,
            };
            // 右臂。
            let stroke_h2 = match c {
                '\u{2500}' | '\u{250c}' | '\u{250e}' | '\u{2514}' | '\u{2516}' | '\u{251c}'
                | '\u{251e}' | '\u{251f}' | '\u{2520}' | '\u{252c}' | '\u{252d}' | '\u{2530}'
                | '\u{2531}' | '\u{2534}' | '\u{2535}' | '\u{2538}' | '\u{2539}' | '\u{253c}'
                | '\u{253d}' | '\u{2540}' | '\u{2541}' | '\u{2542}' | '\u{2543}' | '\u{2545}'
                | '\u{2549}' | '\u{2576}' | '\u{257e}' => stroke,
                '\u{2501}' | '\u{250d}' | '\u{250f}' | '\u{2515}' | '\u{2517}' | '\u{251d}'
                | '\u{2521}' | '\u{2522}' | '\u{2523}' | '\u{252e}' | '\u{252f}' | '\u{2532}'
                | '\u{2533}' | '\u{2536}' | '\u{2537}' | '\u{253a}' | '\u{253b}' | '\u{253e}'
                | '\u{253f}' | '\u{2544}' | '\u{2546}' | '\u{2547}' | '\u{2548}' | '\u{254a}'
                | '\u{254b}' | '\u{257a}' | '\u{257c}' => heavy,
                _ => 0.0,
            };
            // 上臂。
            let stroke_v1 = match c {
                '\u{2502}' | '\u{2514}' | '\u{2515}' | '\u{2518}' | '\u{2519}' | '\u{251c}'
                | '\u{251d}' | '\u{251f}' | '\u{2522}' | '\u{2524}' | '\u{2525}' | '\u{2527}'
                | '\u{252a}' | '\u{2534}' | '\u{2535}' | '\u{2536}' | '\u{2537}' | '\u{253c}'
                | '\u{253d}' | '\u{253e}' | '\u{253f}' | '\u{2541}' | '\u{2545}' | '\u{2546}'
                | '\u{2548}' | '\u{2575}' | '\u{257d}' => stroke,
                '\u{2503}' | '\u{2516}' | '\u{2517}' | '\u{251a}' | '\u{251b}' | '\u{251e}'
                | '\u{2520}' | '\u{2521}' | '\u{2523}' | '\u{2526}' | '\u{2528}' | '\u{2529}'
                | '\u{252b}' | '\u{2538}' | '\u{2539}' | '\u{253a}' | '\u{253b}' | '\u{2540}'
                | '\u{2542}' | '\u{2543}' | '\u{2544}' | '\u{2547}' | '\u{2549}' | '\u{254a}'
                | '\u{254b}' | '\u{2579}' | '\u{257f}' => heavy,
                _ => 0.0,
            };
            // 下臂。
            let stroke_v2 = match c {
                '\u{2502}' | '\u{250c}' | '\u{250d}' | '\u{2510}' | '\u{2511}' | '\u{251c}'
                | '\u{251d}' | '\u{251e}' | '\u{2521}' | '\u{2524}' | '\u{2525}' | '\u{2526}'
                | '\u{2529}' | '\u{252c}' | '\u{252d}' | '\u{252e}' | '\u{252f}' | '\u{253c}'
                | '\u{253d}' | '\u{253e}' | '\u{253f}' | '\u{2540}' | '\u{2543}' | '\u{2544}'
                | '\u{2547}' | '\u{2577}' | '\u{257f}' => stroke,
                '\u{2503}' | '\u{250e}' | '\u{250f}' | '\u{2512}' | '\u{2513}' | '\u{251f}'
                | '\u{2520}' | '\u{2522}' | '\u{2523}' | '\u{2527}' | '\u{2528}' | '\u{252a}'
                | '\u{252b}' | '\u{2530}' | '\u{2531}' | '\u{2532}' | '\u{2533}' | '\u{2541}'
                | '\u{2542}' | '\u{2545}' | '\u{2546}' | '\u{2548}' | '\u{2549}' | '\u{254a}'
                | '\u{254b}' | '\u{257b}' | '\u{257d}' => heavy,
                _ => 0.0,
            };

            let x_v = g.x_center();
            let y_h = g.y_center();

            let v_top = g.v_line_bounds(x_v, stroke_v1);
            let v_bot = g.v_line_bounds(x_v, stroke_v2);
            let h_left = g.h_line_bounds(y_h, stroke_h1);
            let h_right = g.h_line_bounds(y_h, stroke_h2);

            // 臂长延伸到对向臂的笔画边界，保证交点处无缺口。
            let size_h1 = v_top.1.max(v_bot.1);
            let x_h = v_top.0.min(v_bot.0);
            let size_h2 = w - x_h;
            let size_v1 = h_left.1.max(h_right.1);
            let y_v = h_left.0.min(h_right.0);
            let size_v2 = h - y_v;

            g.h_line(0.0, y_h, size_h1, stroke_h1);
            g.h_line(x_h, y_h, size_h2, stroke_h2);
            g.v_line(x_v, 0.0, size_v1, stroke_v1);
            g.v_line(x_v, y_v, size_v2, stroke_v2);
        },
        // 单/双线组合 '═','║','╒'..'╬'：双线是围绕单线边界外扩 1 设备像素
        // 的两条平行细线；各分支的截止点表与旧实现逐字一致。
        '\u{2550}'..='\u{256c}' => {
            let v_lines = match c {
                '\u{2552}' | '\u{2555}' | '\u{2558}' | '\u{255b}' | '\u{255e}' | '\u{2561}'
                | '\u{2564}' | '\u{2567}' | '\u{256a}' => (g.x_center(), g.x_center()),
                _ => {
                    let bounds = g.v_line_bounds(g.x_center(), stroke);
                    let left = (bounds.0 as i32 - 1).max(0) as f32;
                    let right = (bounds.1 as i32 + 1).min(w as i32) as f32;
                    (left, right)
                },
            };
            let h_lines = match c {
                '\u{2553}' | '\u{2556}' | '\u{2559}' | '\u{255c}' | '\u{255f}' | '\u{2562}'
                | '\u{2565}' | '\u{2568}' | '\u{256b}' => (g.y_center(), g.y_center()),
                _ => {
                    let bounds = g.h_line_bounds(g.y_center(), stroke);
                    let top = (bounds.0 as i32 - 1).max(0) as f32;
                    let bottom = (bounds.1 as i32 + 1).min(h as i32) as f32;
                    (top, bottom)
                },
            };

            let v_left_bounds = g.v_line_bounds(v_lines.0, stroke);
            let v_right_bounds = g.v_line_bounds(v_lines.1, stroke);
            let h_top_bounds = g.h_line_bounds(h_lines.0, stroke);
            let h_bot_bounds = g.h_line_bounds(h_lines.1, stroke);

            // 左半横线（上、下两条）。
            let (top_left_size, bot_left_size) = match c {
                '\u{2550}' | '\u{256b}' => (g.x_center(), g.x_center()),
                '\u{2555}'..='\u{2557}' => (v_right_bounds.1, v_left_bounds.1),
                '\u{255b}'..='\u{255d}' => (v_left_bounds.1, v_right_bounds.1),
                '\u{2561}'..='\u{2563}' | '\u{256a}' | '\u{256c}' => {
                    (v_left_bounds.1, v_left_bounds.1)
                },
                '\u{2564}'..='\u{2568}' => (g.x_center(), v_left_bounds.1),
                '\u{2569}' => (v_left_bounds.1, g.x_center()),
                _ => (0.0, 0.0),
            };
            // 右半横线。
            let (top_right_x, bot_right_x, right_size) = match c {
                '\u{2550}' | '\u{2565}' | '\u{256b}' => (g.x_center(), g.x_center(), w),
                '\u{2552}'..='\u{2554}' | '\u{2568}' => (v_left_bounds.0, v_right_bounds.0, w),
                '\u{2558}'..='\u{255a}' => (v_right_bounds.0, v_left_bounds.0, w),
                '\u{255e}'..='\u{2560}' | '\u{256a}' | '\u{256c}' => {
                    (v_right_bounds.0, v_right_bounds.0, w)
                },
                '\u{2564}' | '\u{2566}' => (g.x_center(), v_right_bounds.0, w),
                '\u{2567}' | '\u{2569}' => (v_right_bounds.0, g.x_center(), w),
                _ => (0.0, 0.0, 0.0),
            };
            // 上半竖线（左、右两条）。
            let (left_top_size, right_top_size) = match c {
                '\u{2551}' | '\u{256a}' => (g.y_center(), g.y_center()),
                '\u{2558}'..='\u{255c}' | '\u{2568}' => (h_bot_bounds.1, h_top_bounds.1),
                '\u{255d}' => (h_top_bounds.1, h_bot_bounds.1),
                '\u{255e}'..='\u{2560}' => (g.y_center(), h_top_bounds.1),
                '\u{2561}'..='\u{2563}' => (h_top_bounds.1, g.y_center()),
                '\u{2567}' | '\u{2569}' | '\u{256b}' | '\u{256c}' => {
                    (h_top_bounds.1, h_top_bounds.1)
                },
                _ => (0.0, 0.0),
            };
            // 下半竖线。
            let (left_bot_y, right_bot_y, bottom_size) = match c {
                '\u{2551}' | '\u{256a}' => (g.y_center(), g.y_center(), h),
                '\u{2552}'..='\u{2554}' => (h_top_bounds.0, h_bot_bounds.0, h),
                '\u{2555}'..='\u{2557}' => (h_bot_bounds.0, h_top_bounds.0, h),
                '\u{255e}'..='\u{2560}' => (g.y_center(), h_bot_bounds.0, h),
                '\u{2561}'..='\u{2563}' => (h_bot_bounds.0, g.y_center(), h),
                '\u{2564}'..='\u{2566}' | '\u{256b}' | '\u{256c}' => {
                    (h_bot_bounds.0, h_bot_bounds.0, h)
                },
                _ => (0.0, 0.0, 0.0),
            };

            g.h_line(0.0, h_lines.0, top_left_size, stroke);
            g.h_line(0.0, h_lines.1, bot_left_size, stroke);
            g.h_line(top_right_x, h_lines.0, right_size, stroke);
            g.h_line(bot_right_x, h_lines.1, right_size, stroke);
            g.v_line(v_lines.0, 0.0, left_top_size, stroke);
            g.v_line(v_lines.1, 0.0, right_top_size, stroke);
            g.v_line(v_lines.0, left_bot_y, bottom_size, stroke);
            g.v_line(v_lines.1, right_bot_y, bottom_size, stroke);
        },
        // 圆角 '╭','╮','╯','╰'：两段直臂 + 四分之一圆环（凸四边形分段）。
        '\u{256d}'..='\u{2570}' => {
            // 臂方向：sh=+1 右臂，sv=+1 下臂。
            let (sh, sv): (f32, f32) = match c {
                '\u{256d}' => (1.0, 1.0),
                '\u{256e}' => (-1.0, 1.0),
                '\u{256f}' => (-1.0, -1.0),
                '\u{2570}' => (1.0, -1.0),
                _ => unreachable!(),
            };
            let t = stroke;
            // 外半径与旧实现一致；圆心落在两臂中线的交汇几何位上。
            let outer = (w.min(h) + t) / 2.0;
            let inner = outer - t;
            let d = outer - t / 2.0;
            let (kx, ky) = (g.x_center() + sh * d, g.y_center() + sv * d);

            // 直臂：竖臂从上/下边缘到圆心高度，横臂从左/右边缘到圆心横位。
            let (vx0, vx1) = g.v_line_bounds(g.x_center(), t);
            let (hy0, hy1) = g.h_line_bounds(g.y_center(), t);
            if sv > 0.0 {
                g.rect(vx0, ky, vx1 - vx0, h - ky);
            } else {
                g.rect(vx0, 0.0, vx1 - vx0, ky);
            }
            if sh > 0.0 {
                g.rect(kx, hy0, w - kx, hy1 - hy0);
            } else {
                g.rect(0.0, hy0, kx, hy1 - hy0);
            }

            // 圆弧朝单元格中心一侧弯曲：象限方向 (-sh, -sv)。
            let (u, v) = (-sh, -sv);
            const SEGMENTS: usize = 8;
            let at = |angle: f32, radius: f32| {
                [kx + u * radius * angle.cos(), ky + v * radius * angle.sin()]
            };
            for i in 0..SEGMENTS {
                let a0 = FRAC_PI_2 * i as f32 / SEGMENTS as f32;
                let a1 = FRAC_PI_2 * (i + 1) as f32 / SEGMENTS as f32;
                g.poly(vec![at(a0, outer), at(a1, outer), at(a1, inner), at(a0, inner)]);
            }
        },
        // 局部块：上/下 n/8、左/右 n/8（含 Legacy Computing 上部块）。
        '\u{2580}'..='\u{2587}'
        | '\u{2589}'..='\u{2590}'
        | '\u{2594}'
        | '\u{2595}'
        | '\u{1fb82}'..='\u{1fb8b}' => {
            let mut rect_width = match c {
                '\u{2589}' | '\u{1fb8b}' => w * 7.0 / 8.0,
                '\u{258a}' | '\u{1fb8a}' => w * 6.0 / 8.0,
                '\u{258b}' | '\u{1fb89}' => w * 5.0 / 8.0,
                '\u{258c}' => w * 4.0 / 8.0,
                '\u{258d}' | '\u{1fb88}' => w * 3.0 / 8.0,
                '\u{258e}' | '\u{1fb87}' => w * 2.0 / 8.0,
                '\u{258f}' => w * 1.0 / 8.0,
                '\u{2590}' => w * 4.0 / 8.0,
                '\u{2595}' => w * 1.0 / 8.0,
                _ => w,
            };
            let (mut rect_height, y) = match c {
                '\u{2580}' => (h * 4.0 / 8.0, h),
                '\u{2581}' => (h * 1.0 / 8.0, h * 1.0 / 8.0),
                '\u{2582}' => (h * 2.0 / 8.0, h * 2.0 / 8.0),
                '\u{2583}' => (h * 3.0 / 8.0, h * 3.0 / 8.0),
                '\u{2584}' => (h * 4.0 / 8.0, h * 4.0 / 8.0),
                '\u{2585}' => (h * 5.0 / 8.0, h * 5.0 / 8.0),
                '\u{2586}' => (h * 6.0 / 8.0, h * 6.0 / 8.0),
                '\u{2587}' => (h * 7.0 / 8.0, h * 7.0 / 8.0),
                '\u{2594}' => (h * 1.0 / 8.0, h),
                '\u{1fb82}' => (h * 2.0 / 8.0, h),
                '\u{1fb83}' => (h * 3.0 / 8.0, h),
                '\u{1fb84}' => (h * 5.0 / 8.0, h),
                '\u{1fb85}' => (h * 6.0 / 8.0, h),
                '\u{1fb86}' => (h * 7.0 / 8.0, h),
                _ => (h, h),
            };
            let y = (h - y).round();
            rect_width = rect_width.round().max(1.0);
            rect_height = rect_height.round().max(1.0);
            let x = match c {
                '\u{2590}' => g.x_center(),
                '\u{2595}' | '\u{1fb87}'..='\u{1fb8b}' => w - rect_width,
                _ => 0.0,
            };
            g.rect(x, y, rect_width, rect_height);
        },
        // 整块与浓度块 '█','░','▒','▓'（alpha 与旧灰度 64/128/192 一致）。
        '\u{2588}' | '\u{2591}' | '\u{2592}' | '\u{2593}' => {
            let alpha = match c {
                '\u{2588}' => 1.0,
                '\u{2591}' => 64.0 / 255.0,
                '\u{2592}' => 128.0 / 255.0,
                '\u{2593}' => 192.0 / 255.0,
                _ => unreachable!(),
            };
            g.rect_alpha(0.0, 0.0, w, h, alpha);
        },
        // 象限块 '▖'..'▟'。
        '\u{2596}'..='\u{259f}' => {
            let x_center = g.x_center().round().max(1.0);
            let y_center = g.y_center().round().max(1.0);

            // 左上象限。
            let (w_second, h_second) = match c {
                '\u{2598}' | '\u{2599}' | '\u{259a}' | '\u{259b}' | '\u{259c}' => {
                    (x_center, y_center)
                },
                _ => (0.0, 0.0),
            };
            // 右上象限。
            let (w_first, h_first) = match c {
                '\u{259b}' | '\u{259c}' | '\u{259d}' | '\u{259e}' | '\u{259f}' => {
                    (x_center, y_center)
                },
                _ => (0.0, 0.0),
            };
            // 左下象限。
            let (w_third, h_third) = match c {
                '\u{2596}' | '\u{2599}' | '\u{259b}' | '\u{259e}' | '\u{259f}' => {
                    (x_center, y_center)
                },
                _ => (0.0, 0.0),
            };
            // 右下象限。
            let (w_fourth, h_fourth) = match c {
                '\u{2597}' | '\u{2599}' | '\u{259a}' | '\u{259c}' | '\u{259f}' => {
                    (x_center, y_center)
                },
                _ => (0.0, 0.0),
            };

            g.rect(0.0, 0.0, w_second, h_second);
            g.rect(x_center, 0.0, w_first, h_first);
            g.rect(0.0, y_center, w_third, h_third);
            g.rect(x_center, y_center, w_fourth, h_fourth);
        },
        // 六分块 '🬀'..'🬻'（2×3 网格，表与旧实现逐字一致）。
        '\u{1fb00}'..='\u{1fb3b}' => {
            let x_center = g.x_center().round().max(1.0);
            let y_third = (h / 3.0).round().max(1.0);
            let y_last_third = h - 2.0 * y_third;

            let (w_top_left, h_top_left) = match c {
                '\u{1fb00}' | '\u{1fb02}' | '\u{1fb04}' | '\u{1fb06}' | '\u{1fb08}'
                | '\u{1fb0a}' | '\u{1fb0c}' | '\u{1fb0e}' | '\u{1fb10}' | '\u{1fb12}'
                | '\u{1fb15}' | '\u{1fb17}' | '\u{1fb19}' | '\u{1fb1b}' | '\u{1fb1d}'
                | '\u{1fb1f}' | '\u{1fb21}' | '\u{1fb23}' | '\u{1fb25}' | '\u{1fb27}'
                | '\u{1fb28}' | '\u{1fb2a}' | '\u{1fb2c}' | '\u{1fb2e}' | '\u{1fb30}'
                | '\u{1fb32}' | '\u{1fb34}' | '\u{1fb36}' | '\u{1fb38}' | '\u{1fb3a}' => {
                    (x_center, y_third)
                },
                _ => (0.0, 0.0),
            };
            let (w_top_right, h_top_right) = match c {
                '\u{1fb01}' | '\u{1fb02}' | '\u{1fb05}' | '\u{1fb06}' | '\u{1fb09}'
                | '\u{1fb0a}' | '\u{1fb0d}' | '\u{1fb0e}' | '\u{1fb11}' | '\u{1fb12}'
                | '\u{1fb14}' | '\u{1fb15}' | '\u{1fb18}' | '\u{1fb19}' | '\u{1fb1c}'
                | '\u{1fb1d}' | '\u{1fb20}' | '\u{1fb21}' | '\u{1fb24}' | '\u{1fb25}'
                | '\u{1fb28}' | '\u{1fb2b}' | '\u{1fb2c}' | '\u{1fb2f}' | '\u{1fb30}'
                | '\u{1fb33}' | '\u{1fb34}' | '\u{1fb37}' | '\u{1fb38}' | '\u{1fb3b}' => {
                    (x_center, y_third)
                },
                _ => (0.0, 0.0),
            };
            let (w_mid_left, h_mid_left) = match c {
                '\u{1fb03}' | '\u{1fb04}' | '\u{1fb05}' | '\u{1fb06}' | '\u{1fb0b}'
                | '\u{1fb0c}' | '\u{1fb0d}' | '\u{1fb0e}' | '\u{1fb13}' | '\u{1fb14}'
                | '\u{1fb15}' | '\u{1fb1a}' | '\u{1fb1b}' | '\u{1fb1c}' | '\u{1fb1d}'
                | '\u{1fb22}' | '\u{1fb23}' | '\u{1fb24}' | '\u{1fb25}' | '\u{1fb29}'
                | '\u{1fb2a}' | '\u{1fb2b}' | '\u{1fb2c}' | '\u{1fb31}' | '\u{1fb32}'
                | '\u{1fb33}' | '\u{1fb34}' | '\u{1fb39}' | '\u{1fb3a}' | '\u{1fb3b}' => {
                    (x_center, y_third)
                },
                _ => (0.0, 0.0),
            };
            let (w_mid_right, h_mid_right) = match c {
                '\u{1fb07}' | '\u{1fb08}' | '\u{1fb09}' | '\u{1fb0a}' | '\u{1fb0b}'
                | '\u{1fb0c}' | '\u{1fb0d}' | '\u{1fb0e}' | '\u{1fb16}' | '\u{1fb17}'
                | '\u{1fb18}' | '\u{1fb19}' | '\u{1fb1a}' | '\u{1fb1b}' | '\u{1fb1c}'
                | '\u{1fb1d}' | '\u{1fb26}' | '\u{1fb27}' | '\u{1fb28}' | '\u{1fb29}'
                | '\u{1fb2a}' | '\u{1fb2b}' | '\u{1fb2c}' | '\u{1fb35}' | '\u{1fb36}'
                | '\u{1fb37}' | '\u{1fb38}' | '\u{1fb39}' | '\u{1fb3a}' | '\u{1fb3b}' => {
                    (x_center, y_third)
                },
                _ => (0.0, 0.0),
            };
            let (w_bottom_left, h_bottom_left) = match c {
                '\u{1fb0f}' | '\u{1fb10}' | '\u{1fb11}' | '\u{1fb12}' | '\u{1fb13}'
                | '\u{1fb14}' | '\u{1fb15}' | '\u{1fb16}' | '\u{1fb17}' | '\u{1fb18}'
                | '\u{1fb19}' | '\u{1fb1a}' | '\u{1fb1b}' | '\u{1fb1c}' | '\u{1fb1d}'
                | '\u{1fb2d}' | '\u{1fb2e}' | '\u{1fb2f}' | '\u{1fb30}' | '\u{1fb31}'
                | '\u{1fb32}' | '\u{1fb33}' | '\u{1fb34}' | '\u{1fb35}' | '\u{1fb36}'
                | '\u{1fb37}' | '\u{1fb38}' | '\u{1fb39}' | '\u{1fb3a}' | '\u{1fb3b}' => {
                    (x_center, y_last_third)
                },
                _ => (0.0, 0.0),
            };
            let (w_bottom_right, h_bottom_right) = match c {
                '\u{1fb1e}' | '\u{1fb1f}' | '\u{1fb20}' | '\u{1fb21}' | '\u{1fb22}'
                | '\u{1fb23}' | '\u{1fb24}' | '\u{1fb25}' | '\u{1fb26}' | '\u{1fb27}'
                | '\u{1fb28}' | '\u{1fb29}' | '\u{1fb2a}' | '\u{1fb2b}' | '\u{1fb2c}'
                | '\u{1fb2d}' | '\u{1fb2e}' | '\u{1fb2f}' | '\u{1fb30}' | '\u{1fb31}'
                | '\u{1fb32}' | '\u{1fb33}' | '\u{1fb34}' | '\u{1fb35}' | '\u{1fb36}'
                | '\u{1fb37}' | '\u{1fb38}' | '\u{1fb39}' | '\u{1fb3a}' | '\u{1fb3b}' => {
                    (x_center, y_last_third)
                },
                _ => (0.0, 0.0),
            };

            g.rect(0.0, 0.0, w_top_left, h_top_left);
            g.rect(x_center, 0.0, w_top_right, h_top_right);
            g.rect(0.0, y_third, w_mid_left, h_mid_left);
            g.rect(x_center, y_third, w_mid_right, h_mid_right);
            g.rect(0.0, y_third * 2.0, w_bottom_left, h_bottom_left);
            g.rect(x_center, y_third * 2.0, w_bottom_right, h_bottom_right);
        },
        // Powerline 实心三角 '',''：上下各留 1 设备像素（同旧实现），
        // 斜率 1；尖端放不下时在边缘截平成梯形。
        '\u{e0b0}' | '\u{e0b2}' => {
            let top = 1.0;
            let bottom = h - 1.0;
            let apex_x = (h - 2.0) / 2.0;
            let mut points = if apex_x <= w {
                vec![[0.0, top], [apex_x, h / 2.0], [0.0, bottom]]
            } else {
                vec![[0.0, top], [w, top + w], [w, bottom - w], [0.0, bottom]]
            };
            if c == '\u{e0b2}' {
                for p in &mut points {
                    p[0] = w - p[0];
                }
            }
            g.poly(points);
        },
        // Powerline 箭头 '',''：两道竖向厚度 = stroke 的斜笔画；
        // 尖端被截平时补一段竖桥封口（同旧实现的封口行为）。
        '\u{e0b1}' | '\u{e0b3}' => {
            let t = stroke;
            let top = 1.0;
            let bottom = h - 1.0;
            let apex_x = ((h - 2.0) / 2.0).min(w);
            let apex_top_y = top + apex_x;
            let apex_bot_y = bottom - apex_x;
            let mut polys = vec![
                vec![[0.0, top], [apex_x, apex_top_y], [apex_x, apex_top_y + t], [0.0, top + t]],
                vec![
                    [0.0, bottom],
                    [apex_x, apex_bot_y],
                    [apex_x, apex_bot_y - t],
                    [0.0, bottom - t],
                ],
            ];
            if apex_x >= w && apex_top_y + t < apex_bot_y - t {
                polys.push(vec![
                    [apex_x - t, apex_top_y],
                    [apex_x, apex_top_y],
                    [apex_x, apex_bot_y],
                    [apex_x - t, apex_bot_y],
                ]);
            }
            for mut points in polys {
                if c == '\u{e0b3}' {
                    for p in &mut points {
                        p[0] = w - p[0];
                    }
                }
                g.poly(points);
            }
        },
        // Powerline 圆头 '',''：半椭圆（凸），折线近似。
        '\u{e0b4}' | '\u{e0b6}' => {
            let (flat_x, dir) = if c == '\u{e0b4}' { (0.0, 1.0) } else { (w, -1.0) };
            let (rx, ry) = (w, h / 2.0);
            const SEGMENTS: usize = 16;
            let mut points = Vec::with_capacity(SEGMENTS + 1);
            for i in 0..=SEGMENTS {
                let a = -FRAC_PI_2 + PI * i as f32 / SEGMENTS as f32;
                points.push([flat_x + dir * rx * a.cos(), h / 2.0 + ry * a.sin()]);
            }
            g.poly(points);
        },
        _ => unreachable!("is_builtin 与 draw 的覆盖必须一致: {c:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 9.0;
    const H: f32 = 20.0;

    fn prims(c: char) -> Vec<Primitive> {
        primitives(c, W, H, 1.0).expect("内建字符")
    }

    fn rects(c: char) -> Vec<(Rect, f32)> {
        prims(c)
            .into_iter()
            .filter_map(|p| match p {
                Primitive::Rect { rect, alpha } => Some((rect, alpha)),
                Primitive::Poly { .. } => None,
            })
            .collect()
    }

    /// 覆盖面与旧 builtin_font 一致；邻近区间不越界。
    #[test]
    fn coverage_matches_legacy_builtin_font() {
        let covered = ('\u{2500}'..='\u{259f}')
            .chain('\u{1fb00}'..='\u{1fb3b}')
            .chain('\u{1fb82}'..='\u{1fb8b}')
            .chain('\u{e0b0}'..='\u{e0b4}')
            .chain(std::iter::once('\u{e0b6}'));
        for c in covered {
            let prims = primitives(c, W, H, 1.0).unwrap_or_else(|| panic!("{c:?} 应为内建"));
            assert!(!prims.is_empty(), "{c:?} 不应产生空图元");
        }

        let outside = ('\u{2450}'..'\u{2500}')
            .chain('\u{25a0}'..'\u{2600}')
            .chain('\u{1fb3c}'..'\u{1fb82}')
            .chain('\u{1fb8c}'..'\u{1fba0}')
            .chain('\u{e0a0}'..'\u{e0b0}')
            .chain(std::iter::once('\u{e0b5}'))
            .chain('\u{e0b7}'..'\u{e0c0}');
        for c in outside {
            assert!(primitives(c, W, H, 1.0).is_none(), "{c:?} 不应内建");
            assert!(!is_builtin(c));
        }
    }

    /// '─' 精确盖满整格宽、垂直居中，笔画 = round(w/8) = 1。
    #[test]
    fn light_horizontal_spans_full_width() {
        let rects = rects('\u{2500}');
        let min_x = rects.iter().map(|(r, _)| r.x).fold(f32::MAX, f32::min);
        let max_x = rects.iter().map(|(r, _)| r.x + r.w).fold(f32::MIN, f32::max);
        assert_eq!((min_x, max_x), (0.0, W));
        for (rect, alpha) in &rects {
            // y_center = 10，stroke 1：(10 - 0.5) as i32 = 9，高 1。
            assert_eq!((rect.y, rect.h, *alpha), (9.0, 1.0, 1.0));
        }
    }

    /// 重笔画 '━' 是细笔画的两倍厚。
    #[test]
    fn heavy_stroke_doubles_light() {
        let light: f32 = rects('\u{2500}').iter().map(|(r, _)| r.h).fold(0.0, f32::max);
        let heavy: f32 = rects('\u{2501}').iter().map(|(r, _)| r.h).fold(0.0, f32::max);
        assert_eq!((light, heavy), (1.0, 2.0));
    }

    /// '█' 单矩形盖满；░▒▓ 按 64/128/192 灰度的 alpha 递进。
    #[test]
    fn full_block_and_shades() {
        let full = rects('\u{2588}');
        assert_eq!(full, vec![(Rect { x: 0.0, y: 0.0, w: W, h: H }, 1.0)]);
        let alpha_of = |c: char| rects(c)[0].1;
        assert!((alpha_of('\u{2591}') - 64.0 / 255.0).abs() < 1e-6);
        assert!((alpha_of('\u{2592}') - 128.0 / 255.0).abs() < 1e-6);
        assert!((alpha_of('\u{2593}') - 192.0 / 255.0).abs() < 1e-6);
    }

    /// '▀' 上半块、'▂' 下二八分、'▘' 左上象限：与旧实现同一分割。
    #[test]
    fn partial_blocks_and_quadrants() {
        assert_eq!(rects('\u{2580}'), vec![(Rect { x: 0.0, y: 0.0, w: W, h: 10.0 }, 1.0)]);
        assert_eq!(rects('\u{2582}'), vec![(Rect { x: 0.0, y: 15.0, w: W, h: 5.0 }, 1.0)]);
        // x_center.round() = 5（4.5 away-from-zero），y_center = 10。
        assert_eq!(rects('\u{2598}'), vec![(Rect { x: 0.0, y: 0.0, w: 5.0, h: 10.0 }, 1.0)]);
    }

    /// '═' 双横线：两条独立的细线带，中间留缝。
    #[test]
    fn double_horizontal_has_two_separated_bands() {
        let rects = rects('\u{2550}');
        let mut bands: Vec<f32> = rects.iter().map(|(r, _)| r.y).collect();
        bands.sort_by(f32::total_cmp);
        bands.dedup();
        assert_eq!(bands.len(), 2, "应有两条平行带");
        let (top, bottom) = (bands[0], bands[1]);
        let top_h = rects.iter().find(|(r, _)| r.y == top).unwrap().0.h;
        assert!(bottom > top + top_h, "两带之间必须留缝");
        // 每条带都盖满整格宽（左右两半拼接无缝）。
        for band in [top, bottom] {
            let xs: Vec<_> = rects.iter().filter(|(r, _)| r.y == band).collect();
            let min_x = xs.iter().map(|(r, _)| r.x).fold(f32::MAX, f32::min);
            let max_x = xs.iter().map(|(r, _)| r.x + r.w).fold(f32::MIN, f32::max);
            assert_eq!((min_x, max_x), (0.0, W), "band y={band}");
        }
    }

    /// 四臂交点无缺口：'┌' 的横臂起点必须盖到竖臂带、竖臂起点盖到横臂带。
    #[test]
    fn corner_arms_meet_at_joint() {
        let rects = rects('\u{250c}');
        // 一条横带（右臂）+ 一条竖带（下臂）。
        let horizontal = rects.iter().find(|(r, _)| r.w > r.h).map(|(r, _)| *r).expect("横臂");
        let vertical = rects.iter().find(|(r, _)| r.h > r.w).map(|(r, _)| *r).expect("竖臂");
        assert!(horizontal.x <= vertical.x && horizontal.x + horizontal.w == W);
        assert!(vertical.y <= horizontal.y && vertical.y + vertical.h == H);
    }

    /// Powerline 三角 ''：单个凸三角形，左缘全高、尖端指右。
    #[test]
    fn powerline_triangle_shape() {
        let prims = prims('\u{e0b0}');
        assert_eq!(prims.len(), 1);
        let Primitive::Poly { points } = &prims[0] else { panic!("应为多边形") };
        assert_eq!(points.len(), 3);
        assert_eq!(points[0], [0.0, 1.0]);
        assert_eq!(points[1], [9.0, 10.0]);
        assert_eq!(points[2], [0.0, 19.0]);
    }

    /// 缩放下所有矩形边界落在设备像素网格上（清晰度前提）。
    #[test]
    fn rect_bounds_snap_to_device_pixels() {
        let scale = 1.5;
        for c in ['\u{2500}', '\u{2503}', '\u{2550}', '\u{2580}'] {
            for prim in primitives(c, W, H, scale).unwrap() {
                if let Primitive::Rect { rect, .. } = prim {
                    for v in [rect.y * scale, (rect.y + rect.h) * scale] {
                        assert!((v - v.round()).abs() < 1e-4, "{c:?} 的 y 边界 {v} 未吸附设备像素");
                    }
                }
            }
        }
    }

    /// 圆角 '╭' 输出直臂 + 圆环分段，全部落在单元格附近（允许亚像素越界）。
    #[test]
    fn rounded_corner_stays_near_cell() {
        let prims = prims('\u{256d}');
        let polys = prims.iter().filter(|p| matches!(p, Primitive::Poly { .. })).count();
        assert_eq!(polys, 8, "四分之一圆环按 8 段近似");
        for prim in &prims {
            if let Primitive::Poly { points } = prim {
                for [x, y] in points {
                    assert!((-1.0..=W + 1.0).contains(x), "x={x}");
                    assert!((-1.0..=H + 1.0).contains(y), "y={y}");
                }
            }
        }
    }
}
