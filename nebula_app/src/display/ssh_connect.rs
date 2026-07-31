//! SSH 连接卡片：星云轨道 + 粒子流。
//!
//! 画在 pane 内的浮层，**不整页接管**——整页会盖掉 ssh 自己的输出，而
//! host key 警告、banner、`Permission denied` 恰恰是失败时最值钱的信息。
//!
//! 动画承载信息：已完成段的填充宽度就是进度，粒子只在当前段流动。这是它
//! 与"转圈"的根本区别——一眼能看出卡在解析、连接、认证还是开 shell。
//!
//! 有 [`REVEAL_DELAY`] 的延迟门槛：连接比它更快时卡片从不出现。局域网 SSH
//! 常在 200ms 内完成，闪一下的加载页比直接出 prompt 更难受；连接池命中时
//! [`crate::ssh_session::authenticated_session`] 更是瞬时返回。

use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use super::color::Rgb;
use super::i18n::UiLanguage;
use super::ui::icons;
use super::ui::tokens::{radius, space, type_scale};
use super::{NebulaTheme, SizeInfo};
use crate::renderer::ui::{Gradient, Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};
use crate::ssh_session::SshStage;
use nebula_terminal::term::cell::Flags;

/// 低于这个耗时的连接完全不显示卡片。
const REVEAL_DELAY: Duration = Duration::from_millis(350);

/// 粒子绕完当前段一轮的周期。
const PARTICLE_PERIOD: f32 = 1.100;
/// 主粒子数量。
const PARTICLE_COUNT: usize = 5;
/// 每颗粒子回溯的拖尾节数。
const TRAIL_LEN: usize = 5;
/// 拖尾相邻两点的相位差。
const TRAIL_GAP: f32 = 0.032;
/// 主粒子的相位。**刻意不等距**：等距排列读起来像传送带，不等距才像流体
/// 的团簇。
const PARTICLE_PHASE: [f32; PARTICLE_COUNT] = [0.0, 0.17, 0.31, 0.52, 0.63];

/// 卡片宽度（逻辑像素）。
const CARD_W: f32 = 600.0;
/// 轨道节点直径。
const NODE_D: f32 = 10.0;
/// 按钮高度。
const BTN_H: f32 = 30.0;
/// 日志展开区显示的行数。
const LOG_ROWS: usize = 6;

/// 卡片上可点的部位。绘制与命中共用同一份 [`Layout`]，所以图标位置和它的
/// 命中区不可能漂移——这是 `icons.rs` 那条分层约定的延续。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SshConnectHit {
    #[default]
    None,
    /// 右上角的 Logs 折叠开关。
    Logs,
    /// 连接中：放弃这次连接。
    Cancel,
    /// 失败后：关掉这个 pane。
    Close,
}

impl SshConnectHit {
    pub(crate) fn is_none(self) -> bool {
        matches!(self, SshConnectHit::None)
    }
}

/// 一个 pane 的连接进度。
#[derive(Debug, Clone)]
pub(crate) struct SshConnectState {
    /// 用户看到的目标，原样取自 tab 的启动身份。
    destination: String,
    stage: SshStage,
    started: Instant,
    /// 粒子相位（0..1），按帧 delta 推进——与侧栏 spinner 同款做法，不读挂钟。
    phase: f32,
    /// 填充条的当前进度（0..3 的连续值）。向 `stage_index` 插值，避免阶段
    /// 推进时轨道瞬移。
    fill: f32,
    /// 失败原因；`Some` 时卡片停止动画并转为可读的错误态。
    failure: Option<String>,
    /// 当前悬停的部位。
    hover: SshConnectHit,
    /// Logs 区是否展开。
    logs_open: bool,
    /// 连接日志。**不伪造 `ssh -v` 的输出**——我们用 russh 自己实现客户端，
    /// 每一行都对应一个真实发生过的调用，写成 OpenSSH 的样子只会在排障时
    /// 骗人。
    log: Vec<String>,
}

impl SshConnectState {
    pub(crate) fn new(destination: String) -> Self {
        let mut state = Self {
            destination: destination.clone(),
            stage: SshStage::Resolve,
            started: Instant::now(),
            phase: 0.0,
            fill: 0.0,
            failure: None,
            hover: SshConnectHit::None,
            logs_open: false,
            log: Vec::new(),
        };
        state.push_log(format!("resolve   destination = {destination}"));
        state
    }

    fn push_log(&mut self, line: String) {
        let at = self.started.elapsed().as_secs_f32();
        self.log.push(format!("{at:>6.2}s  {line}"));
    }

    /// 阶段推进。`Ready` 由调用方处理成"移除状态"，不会存进来。
    pub(crate) fn set_stage(&mut self, stage: SshStage) {
        // 同一阶段重复上报时不再记一行，否则日志会被刷屏。
        if self.stage != stage {
            match &stage {
                SshStage::Resolve => {},
                SshStage::Connect => self.push_log("connect   tcp handshake + kex".to_owned()),
                SshStage::Authenticate => self.push_log("auth      authenticating".to_owned()),
                SshStage::OpenShell => {
                    self.push_log("shell     channel + pty + shell".to_owned())
                },
                SshStage::Ready => self.push_log("ready     session established".to_owned()),
                SshStage::Failed(message) => self.push_log(format!("error     {message}")),
            }
        }
        if let SshStage::Failed(message) = &stage {
            self.failure = Some(message.clone());
            // 失败的原因是最该被看见的东西，直接把 Logs 展开，不让用户再点
            // 一次才发现真正的线索。
            self.logs_open = true;
        }
        self.stage = stage;
    }

    /// 更新悬停部位；返回是否需要重绘。
    pub(crate) fn set_hover(&mut self, hit: SshConnectHit) -> bool {
        let changed = self.hover != hit;
        self.hover = hit;
        changed
    }

    pub(crate) fn toggle_logs(&mut self) {
        self.logs_open = !self.logs_open;
    }

    pub(crate) fn destination(&self) -> &str {
        &self.destination
    }

    pub(crate) fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// 卡片是否该出现。失败态无视门槛立刻显示——用户已经在等结果了。
    pub(crate) fn visible(&self) -> bool {
        self.failed() || self.started.elapsed() >= REVEAL_DELAY
    }

    /// 当前阶段在四格轨道上的下标。
    fn stage_index(&self) -> usize {
        match self.stage {
            SshStage::Resolve => 0,
            SshStage::Connect => 1,
            SshStage::Authenticate => 2,
            SshStage::OpenShell | SshStage::Ready => 3,
            // 失败停在它失败的那一格；解析阶段就失败的情况归到第 0 格。
            SshStage::Failed(_) => self.fill.round().clamp(0.0, 3.0) as usize,
        }
    }

    /// 推进动画。`delta` 来自共享的 motion frame，所以慢帧不会让粒子跳步。
    pub(crate) fn step(&mut self, delta: Duration) {
        if self.failed() {
            return;
        }
        let dt = delta.as_secs_f32();
        self.phase = (self.phase + dt / PARTICLE_PERIOD).fract();
        // 填充向目标阶段插值：指数趋近，320ms 量级的观感与原型一致。
        let target = self.stage_index() as f32;
        let k = 1.0 - (-dt * 8.0).exp();
        self.fill += (target - self.fill) * k;
    }

    fn elapsed_text(&self) -> String {
        format!("{:.1}s", self.started.elapsed().as_secs_f32())
    }
}

/// 卡片的像素布局。所有矩形都在窗口像素坐标系里，与文字共用同一套坐标 ——
/// resize HUD 那次事故（框按窗口居中、文字按终端网格居中）的教训。
struct Layout {
    card: (f32, f32, f32, f32),
    /// 主机图标方块。
    icon: (f32, f32, f32, f32),
    /// 身份文字的左边缘与两行基线。
    name_x: f32,
    name_y: f32,
    meta_y: f32,
    /// 右上角 Logs 折叠开关。
    logs_btn: (f32, f32, f32, f32),
    /// 轨道：左端节点中心、右端节点中心、纵向中心。
    rail_x0: f32,
    rail_x1: f32,
    rail_cy: f32,
    /// 阶段标签基线。
    labels_y: f32,
    /// 状态行基线。
    status_y: f32,
    /// 失败详情基线（失败态才用）。
    detail_y: f32,
    /// 日志区外框（展开时才有）。
    logs_area: Option<(f32, f32, f32, f32)>,
    /// 底部按钮：矩形、语义、是否主按钮。绘制与命中同源。
    buttons: Vec<((f32, f32, f32, f32), SshConnectHit, bool)>,
    pad: f32,
}

/// 底部按钮的文案与语义，随状态切换：连接中只能放弃，失败后只剩收尾。
///
/// 这里**没有**「重试」——它要在同一个 tab 位上原子地重连，而 `Close` +
/// `NewSsh` 两个事件按序执行会先把最后一个 tab 连窗口一起关掉。宁可少一个
/// 按钮，也不放一个按下去会出事的。
fn button_specs(state: &SshConnectState, language: UiLanguage) -> Vec<(String, SshConnectHit, bool)> {
    if state.failed() {
        vec![(language.pick("关闭", "Close").to_owned(), SshConnectHit::Close, true)]
    } else {
        vec![(language.pick("取消", "Cancel").to_owned(), SshConnectHit::Cancel, false)]
    }
}

fn layout(
    state: &SshConnectState,
    size: &SizeInfo,
    view: (f32, f32, f32, f32),
    scale: f32,
    language: UiLanguage,
) -> Layout {
    let s = |v: f32| v * scale;
    let pad = s(space::L);
    let ch = size.cell_height();
    let cell_w = size.cell_width();
    let card_w = s(CARD_W).min(view.2 - s(space::L) * 2.0);

    // 内容从上往下堆，卡片高度由内容决定（内容自适应，不留空洞）。
    let icon_h = s(40.0);
    let mut h = pad;
    let icon_top = h;
    h += icon_h + s(space::L);
    let rail_top = h;
    h += s(NODE_D) + s(space::XS) + ch * type_scale::SECTION_CAPTION + s(space::L);
    let status_top = h;
    h += ch;

    let detail_top = if state.failed() {
        let top = h + s(space::XS);
        h = top + ch * type_scale::SUPPORTING * 2.0;
        top
    } else {
        0.0
    };

    let logs_h = if state.logs_open {
        // 高度跟着真实行数走，最多 LOG_ROWS 行。固定高度会在只有三行日志时
        // 留下一大块空框，读起来像"还有内容没加载出来"。
        let rows = state.log.len().clamp(1, LOG_ROWS);
        let inner = ch * type_scale::SUPPORTING * rows as f32 + s(space::S) * 2.0;
        h += s(space::M) + inner;
        Some(inner)
    } else {
        None
    };

    h += s(space::L);
    let btn_top = h;
    h += s(BTN_H) + pad;

    let card_x = (view.0 + (view.2 - card_w) * 0.5).round();
    let card_y = (view.1 + (view.3 - h) * 0.5).round();

    let icon_x = card_x + pad;
    let text_x = icon_x + icon_h + s(space::M);
    // 两行身份文字在图标高度内垂直居中。
    let name_line = ch * type_scale::BODY;
    let meta_line = ch * type_scale::SUPPORTING;
    let text_block = name_line + s(2.0) + meta_line;
    let text_top = card_y + icon_top + (icon_h - text_block) * 0.5;

    // Logs 按钮贴卡片右内缘，与图标同一条水平中线。
    let logs_label = language.pick("Logs", "Logs");
    let logs_w = text_w(logs_label, cell_w, type_scale::SUPPORTING) + s(space::M) * 2.0 + s(10.0);
    let logs_btn = (
        (card_x + card_w - pad - logs_w).round(),
        (card_y + icon_top + (icon_h - s(BTN_H)) * 0.5).round(),
        logs_w.round(),
        s(BTN_H),
    );

    // 底部按钮从右往左排，主按钮在左——与原型一致（提交 / 取消）。
    let mut buttons = Vec::new();
    let mut right = card_x + card_w - pad;
    for (label, hit, primary) in button_specs(state, language).into_iter().rev() {
        let w = (text_w(&label, cell_w, type_scale::BODY) + s(space::M) * 2.0).max(s(76.0));
        right -= w;
        buttons.push((
            ((right).round(), (card_y + btn_top).round(), w.round(), s(BTN_H)),
            hit,
            primary,
        ));
        right -= s(space::S);
    }

    Layout {
        card: (card_x, card_y, card_w, h),
        icon: (icon_x, card_y + icon_top, icon_h, icon_h),
        name_x: text_x,
        name_y: text_top.round(),
        meta_y: (text_top + name_line + s(2.0)).round(),
        logs_btn,
        rail_x0: card_x + pad + s(NODE_D) * 0.5,
        rail_x1: card_x + card_w - pad - s(NODE_D) * 0.5,
        rail_cy: (card_y + rail_top + s(NODE_D) * 0.5).round(),
        labels_y: (card_y + rail_top + s(NODE_D) + s(space::XS)).round(),
        status_y: (card_y + status_top).round(),
        detail_y: (card_y + detail_top).round(),
        logs_area: logs_h.map(|inner| {
            let top = card_y + btn_top - s(space::L) - inner;
            ((card_x + pad).round(), top.round(), (card_w - pad * 2.0).round(), inner)
        }),
        buttons,
        pad,
    }
}

fn contains(rect: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

/// 命中测试。卡片是模态浮层：**落在卡片内但不在任何控件上，也要吞掉事件**，
/// 否则点击会漏进下面的终端去拖选区——侧栏拖拽残影那个 bug 就是这么来的。
pub(super) fn hit_test(
    state: &SshConnectState,
    size: &SizeInfo,
    view: (f32, f32, f32, f32),
    scale: f32,
    language: UiLanguage,
    x: f32,
    y: f32,
) -> SshConnectHit {
    let l = layout(state, size, view, scale, language);
    if contains(l.logs_btn, x, y) {
        return SshConnectHit::Logs;
    }
    for (rect, hit, _) in &l.buttons {
        if contains(*rect, x, y) {
            return *hit;
        }
    }
    SshConnectHit::None
}

/// 遮罩覆盖整个 pane，所以连接期间任何落在 pane 内的点击都不该穿透到终端。
pub(super) fn covers(view: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    contains(view, x, y)
}

/// 节点中心的 x 坐标。
fn node_x(l: &Layout, index: usize) -> f32 {
    l.rail_x0 + (l.rail_x1 - l.rail_x0) * index as f32 / 3.0
}

/// 速度场：两端慢、中段快，与线性混合以保留端点的基础流速。
///
/// 拖尾位置也过这条曲线，于是快段自动拉长、慢段自动压缩——流体感就来自
/// 这里，不是靠额外的装饰。
fn ease(s: f32) -> f32 {
    0.35 * s + 0.65 * (s * s * (3.0 - 2.0 * s))
}

fn lerp_rgb(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_quads(
    state: &SshConnectState,
    theme: &NebulaTheme,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    view: (f32, f32, f32, f32),
    scale: f32,
    language: UiLanguage,
    backdrop: Rgba,
) {
    let l = layout(state, size, view, scale, language);
    let s = |v: f32| v * scale;
    let sk = theme.skin();
    let palette = theme.palette();
    let light = sk.is_light;
    // 品牌色对：Nebula 主题自带的星云紫→青。浅色主题下压暗才够对比。
    let brand_l = if light { lerp_rgb(rgb_of(palette.edge_l), Rgb::new(0, 0, 0), 0.28) } else { rgb_of(palette.edge_l) };
    let brand_r = if light { lerp_rgb(rgb_of(palette.edge_r), Rgb::new(0, 0, 0), 0.28) } else { rgb_of(palette.edge_r) };

    // ── 遮罩 ────────────────────────────────────────────────────
    // 连接期间 pane 里没有任何值得看的东西（grid 是空的，只有一个在 blink
    // 的光标），却会跟着我们的持续重绘一起闪。用 pane 底色整块盖住：卡片
    // 由此成为真正的第一层，会话就绪后遮罩和卡片一起退场，露出真实终端。
    // ssh 自己的输出不会因此丢失——它们进了 Logs 区。
    quads.push(UiQuad::solid(view.0, view.1, view.2, view.3, 0.0, backdrop).pixel_snapped());

    let (cx, cy, cw, chh) = l.card;

    // 阴影 + 面板：一个要求用户等待的浮层必须有真实高度。
    let shadow_alpha = if light { 26 } else { 56 };
    quads.push(
        UiQuad::shadow(
            cx,
            cy,
            cw,
            chh,
            s(radius::OVERLAY),
            s(20.0),
            s(7.0),
            Rgba::new(0, 0, 0, shadow_alpha),
        )
        .pixel_snapped(),
    );
    quads.push(UiQuad::solid(cx, cy, cw, chh, s(radius::OVERLAY), sk.panel).pixel_snapped());
    // hairline 描边：与阴影二选一会显薄，浮层这一层两者都要（阴影给高度，
    // 描边给边界），但描边只有 1px 且极淡。
    quads.push(
        UiQuad::solid(cx, cy, cw, chh, s(radius::OVERLAY), sk.hairline).pixel_snapped(),
    );
    quads.push(
        UiQuad::solid(
            cx + s(1.0),
            cy + s(1.0),
            cw - s(2.0),
            chh - s(2.0),
            s(radius::OVERLAY) - s(1.0),
            sk.panel,
        )
        .pixel_snapped(),
    );

    // 主机图标底：card 底 + hairline，不用彩色块。Termius 那种高饱和彩色
    // 方块排一屏时眼睛不知道看哪，而且颜色不承载任何可操作信息。
    let (ix, iy, iw, ih) = l.icon;
    // 卡片底和图标底都带 alpha，直接拿它们当"挖空色"盖不住下面的墨迹——
    // 这正是 icons.rs 里 blend_over 存在的理由。先算出真实的不透明底色。
    let solid_panel = icons::blend_over(backdrop, sk.panel);
    let solid_card = icons::blend_over(solid_panel, sk.card);
    quads.push(UiQuad::solid(ix, iy, iw, ih, s(radius::CONTROL), solid_card).pixel_snapped());
    // 服务器机架墨迹：两层机箱 + 各一颗指示灯。字体私用区字形在分数 DPI 下
    // 会漂，所以和 icons.rs 一样走矢量，不借字形。
    {
        let icx = ix + iw * 0.5;
        let icy = iy + ih * 0.5;
        let ink = Rgba::opaque(sk.ink_dim);
        let bw = s(18.0);
        let bh = s(7.5);
        let gap = s(3.0);
        let stroke = (s(1.4)).max(1.0);
        let r = s(2.0);
        for row in [-1.0f32, 1.0] {
            let by = icy + row * (bh + gap) * 0.5 - bh * 0.5;
            let bx = icx - bw * 0.5;
            // 无 stroke 图元：ink 板 + 挖回底色的内芯。
            quads.push(UiQuad::solid(bx, by, bw, bh, r, ink).pixel_snapped());
            quads.push(
                UiQuad::solid(
                    bx + stroke,
                    by + stroke,
                    bw - stroke * 2.0,
                    bh - stroke * 2.0,
                    (r - stroke).max(0.0),
                    solid_card,
                )
                .pixel_snapped(),
            );
            // 指示灯：靠左，与机箱同一条中线。
            let d = s(2.4);
            quads.push(UiQuad::solid(
                bx + stroke + s(2.2),
                by + bh * 0.5 - d * 0.5,
                d,
                d,
                d * 0.5,
                ink,
            ));
        }
    }

    // ── 星云轨道 ────────────────────────────────────────────────
    let rail_h = s(2.0);
    let rail_y = (l.rail_cy - rail_h * 0.5).round();
    // 底轨：未完成部分。
    quads.push(
        UiQuad::solid(l.rail_x0, rail_y, l.rail_x1 - l.rail_x0, rail_h, rail_h * 0.5, sk.hairline)
            .pixel_snapped(),
    );

    let fill_to = node_x(&l, 0) + (l.rail_x1 - l.rail_x0) * (state.fill / 3.0);
    let fill_w = (fill_to - l.rail_x0).max(0.0);
    if fill_w > 0.5 {
        // 已完成段：品牌紫→青，宽度即进度。这是全屏唯一一处渐变。
        let end = lerp_rgb(brand_l, brand_r, state.fill / 3.0);
        if state.failed() {
            quads.push(
                UiQuad::solid(l.rail_x0, rail_y, fill_w, rail_h, rail_h * 0.5, sk.danger)
                    .pixel_snapped(),
            );
        } else {
            // 深色下给填充段极淡外溢；浅色主题 glow 一律关闭（theme.rs 把
            // 浅色 glow alpha 硬编码为 0：低 alpha 径向渐变在浅底上只落在
            // 极少数 8-bit 台阶，量化轮廓会读成模糊灰线）。
            if !light {
                // brand-glow: 进度轨道的能量外溢是星云品牌位本身的画面。
                quads.push(UiQuad::glow(
                    l.rail_x0,
                    rail_y - s(3.0),
                    fill_w,
                    rail_h + s(6.0),
                    Rgba::opaque(end).with_alpha(0.20),
                ));
            }
            quads.push(
                UiQuad::gradient(
                    l.rail_x0,
                    rail_y,
                    fill_w,
                    rail_h,
                    rail_h * 0.5,
                    Rgba::opaque(brand_l),
                    Rgba::opaque(end),
                    Gradient::Horizontal,
                )
                .pixel_snapped(),
            );
        }
    }

    // 粒子：只在当前段流动。失败后不画（动画停止）。
    if !state.failed() {
        let seg = state.stage_index().min(2);
        let from = node_x(&l, seg);
        let to = node_x(&l, seg + 1);
        let span = l.rail_x1 - l.rail_x0;
        for i in 0..PARTICLE_COUNT {
            let head = (state.phase + PARTICLE_PHASE[i]).fract();
            // 从尾画到头，头压在最上。
            for k in (0..=TRAIL_LEN).rev() {
                let t = head - k as f32 * TRAIL_GAP;
                if t < 0.0 || t > 1.0 {
                    continue;
                }
                let x = from + (to - from) * ease(t);
                let fade = 1.0 - k as f32 / (TRAIL_LEN as f32 + 1.2);
                // 进出两端淡入淡出，避免粒子在节点上凭空出现或撞停。
                let edge = (t / 0.14).min((1.0 - t) / 0.14).min(1.0);
                let alpha = fade * fade * edge * if light { 0.55 } else { 0.9 };
                if alpha < 0.012 {
                    continue;
                }
                // 颜色按整条轨道的位置从品牌紫渐变到品牌青。
                let color = lerp_rgb(brand_l, brand_r, (x - l.rail_x0) / span.max(1.0));
                if !light {
                    let rad = if k == 0 { s(5.4) } else { s(4.4) * fade + s(1.2) };
                    // brand-glow: 拖尾粒子就是发光体，不是在垫层级。
                    quads.push(UiQuad::glow(
                        x - rad,
                        l.rail_cy - rad,
                        rad * 2.0,
                        rad * 2.0,
                        Rgba::opaque(color).with_alpha(alpha * 0.5),
                    ));
                }
                let cr = if k == 0 { s(1.55) } else { s(1.3) * fade + s(0.2) };
                quads.push(UiQuad::solid(
                    x - cr,
                    l.rail_cy - cr,
                    cr * 2.0,
                    cr * 2.0,
                    cr,
                    Rgba::opaque(color).with_alpha(alpha * if k == 0 { 1.0 } else { 0.78 }),
                ));
            }
        }
    }

    // 节点：三态。空心（未到）→ accent 实心带光环（正在做）→ 品牌青实心。
    let done_upto = state.fill.floor() as usize;
    let active = state.stage_index();
    for i in 0..4 {
        let d = s(NODE_D);
        let x = node_x(&l, i) - d * 0.5;
        let y = l.rail_cy - d * 0.5;
        let failed_here = state.failed() && i == active;
        if failed_here {
            quads.push(UiQuad::solid(x, y, d, d, d * 0.5, sk.danger).pixel_snapped());
        } else if i <= done_upto && i < active {
            quads.push(UiQuad::solid(x, y, d, d, d * 0.5, Rgba::opaque(brand_r)).pixel_snapped());
        } else if i == active {
            if !light {
                // brand-glow: 当前阶段节点的脉冲，和轨道粒子是同一套语言。
                quads.push(UiQuad::glow(
                    x - s(3.0),
                    y - s(3.0),
                    d + s(6.0),
                    d + s(6.0),
                    Rgba::opaque(sk.accent).with_alpha(0.45),
                ));
            }
            quads.push(UiQuad::solid(x, y, d, d, d * 0.5, Rgba::opaque(sk.accent)).pixel_snapped());
        } else {
            // 空心：外圈 hairline + 内芯挖回面板色。
            quads.push(UiQuad::solid(x, y, d, d, d * 0.5, sk.hairline).pixel_snapped());
            let inset = s(1.5);
            quads.push(
                UiQuad::solid(
                    x + inset,
                    y + inset,
                    d - inset * 2.0,
                    d - inset * 2.0,
                    (d - inset * 2.0) * 0.5,
                    sk.panel,
                )
                .pixel_snapped(),
            );
        }
    }

    // ── Logs 折叠开关 ───────────────────────────────────────────
    // Tabby 那张连接图唯一真正值钱的东西就是它，所以它必须在失败之前就在
    // 场，而不是失败后才冒出来。
    push_button_frame(quads, l.logs_btn, s(radius::CONTROL), false, state.hover == SshConnectHit::Logs, sk);
    {
        let (bx, by, bw, bh) = l.logs_btn;
        let cxx = bx + bw - s(space::M) - s(3.0);
        let cyy = by + bh * 0.5;
        let arm = s(3.2);
        let stroke = (s(1.3)).max(1.0);
        let ink = Rgba::opaque(sk.ink_dim);
        // 展开时朝上，收起时朝下——方向指向"点下去会发生什么"。
        let dir = if state.logs_open { -1.0 } else { 1.0 };
        icons::push_segment(
            quads,
            (cxx - arm, cyy - arm * 0.5 * dir),
            (cxx, cyy + arm * 0.5 * dir),
            stroke,
            ink,
        );
        icons::push_segment(
            quads,
            (cxx, cyy + arm * 0.5 * dir),
            (cxx + arm, cyy - arm * 0.5 * dir),
            stroke,
            ink,
        );
    }

    // ── 日志区 ─────────────────────────────────────────────────
    if let Some(area) = l.logs_area {
        quads.push(
            UiQuad::solid(area.0, area.1, area.2, area.3, s(radius::CONTROL), sk.hairline)
                .pixel_snapped(),
        );
        let inset = s(1.0);
        quads.push(
            UiQuad::solid(
                area.0 + inset,
                area.1 + inset,
                area.2 - inset * 2.0,
                area.3 - inset * 2.0,
                s(radius::CONTROL) - inset,
                sk.card,
            )
            .pixel_snapped(),
        );
    }

    // ── 底部按钮 ───────────────────────────────────────────────
    for (rect, hit, primary) in &l.buttons {
        push_button_frame(quads, *rect, s(radius::CONTROL), *primary, state.hover == *hit, sk);
    }
}

/// 按钮底：主按钮实心 accent，次按钮 card 底 + hairline 描边。hover 只改
/// 亮度，不改尺寸——动效纪律里"不加装饰动效"的直接后果。
fn push_button_frame(
    quads: &mut Vec<UiQuad>,
    rect: (f32, f32, f32, f32),
    radius: f32,
    primary: bool,
    hovered: bool,
    sk: crate::display::ui::theme::Skin,
) {
    let (x, y, w, h) = rect;
    let lift = |c: Rgba| {
        if !hovered {
            return c;
        }
        // 深色底往上提亮，浅色底往下压暗，两边都是"更靠近手指"的方向。
        let top = if sk.is_light {
            Rgba::new(0, 0, 0, 20)
        } else {
            Rgba::new(255, 255, 255, 26)
        };
        icons::blend_over(c, top)
    };
    if primary {
        quads.push(UiQuad::solid(x, y, w, h, radius, lift(Rgba::opaque(sk.accent))).pixel_snapped());
    } else {
        quads.push(UiQuad::solid(x, y, w, h, radius, sk.hairline).pixel_snapped());
        let inset = (radius * 0.0 + 1.0).max(1.0);
        quads.push(
            UiQuad::solid(
                x + inset,
                y + inset,
                w - inset * 2.0,
                h - inset * 2.0,
                (radius - inset).max(0.0),
                lift(sk.card),
            )
            .pixel_snapped(),
        );
    }
}

pub(super) fn rgb_of(c: Rgba) -> Rgb {
    Rgb::new(c.r, c.g, c.b)
}

/// 文字宽度（像素）。等宽字体下按列宽求和是精确的。
fn text_w(text: &str, cell_w: f32, scale: f32) -> f32 {
    let cols: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    cols as f32 * cell_w * scale
}

/// 估算宽度与实际渲染宽度之间的安全系数。
///
/// UI 文字走 UI 字体，中文字形的实际推进宽度略大于"列数 × cell_w"的估算，
/// 按估算贴边排版会溢出卡片——失败详情第一次就是这么冲出右边界的。
const TEXT_SAFETY: f32 = 0.94;

/// 一块宽度能放下多少列。
pub(super) fn cols_that_fit(width: f32, cell_w: f32, scale: f32) -> usize {
    ((width * TEXT_SAFETY / (cell_w * scale)).floor() as usize).max(4)
}

/// 按列宽截断，超出部分收成省略号。日志行里那条中文系统错误消息不截断就
/// 会一路画到窗口外面去。
pub(super) fn truncate_cols(text: &str, max_cols: usize) -> String {
    let total: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max_cols {
        return text.to_owned();
    }
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut cols = 0usize;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if cols + w > budget {
            break;
        }
        out.push(c);
        cols += w;
    }
    out.push('…');
    out
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_text(
    state: &SshConnectState,
    theme: &NebulaTheme,
    language: UiLanguage,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    view: (f32, f32, f32, f32),
    scale: f32,
) {
    let l = layout(state, size, view, scale, language);
    let sk = theme.skin();
    let cell_w = size.cell_width();

    // 身份：标签用主墨，技术地址降一级——列表和卡片同一套读法。
    let name = short_name(&state.destination);
    r.draw_ui_text(
        size,
        l.name_x,
        l.name_y,
        type_scale::BODY,
        sk.ink_strong,
        Flags::empty(),
        &name,
        gc,
    );
    let meta = format!("SSH · {}", state.destination);
    r.draw_ui_text(
        size,
        l.name_x,
        l.meta_y,
        type_scale::SUPPORTING,
        sk.ink_dim,
        Flags::empty(),
        &meta,
        gc,
    );

    // 阶段标签：已完成 ink_dim、当前 ink、未到 ink_faint。
    let labels = stage_labels(language);
    let active = state.stage_index();
    for (i, label) in labels.iter().enumerate() {
        let ink = if state.failed() && i == active {
            rgb_of(sk.danger)
        } else if i == active {
            sk.ink
        } else if i < active {
            sk.ink_dim
        } else {
            sk.ink_faint
        };
        let w = text_w(label, cell_w, type_scale::SECTION_CAPTION);
        // 首尾贴齐轨道两端，中间的以节点为中心——标签是节点的名字，不是
        // 均分的四列。
        let cx = node_x(&l, i);
        let x = match i {
            0 => l.rail_x0 - s_half(scale),
            3 => l.rail_x1 + s_half(scale) - w,
            _ => cx - w * 0.5,
        };
        r.draw_ui_text(
            size,
            x.round(),
            l.labels_y,
            type_scale::SECTION_CAPTION,
            ink,
            Flags::empty(),
            label,
            gc,
        );
    }

    // 状态行：左边当前动作，右边计时（失败后计时停在失败时刻）。
    let (msg, msg_ink) = match &state.failure {
        Some(_) => (failure_headline(language), rgb_of(sk.danger)),
        None => (stage_message(&state.stage, language), sk.ink),
    };
    r.draw_ui_text(
        size,
        l.card.0 + l.pad,
        l.status_y,
        type_scale::BODY,
        msg_ink,
        Flags::empty(),
        &msg,
        gc,
    );
    if !state.failed() {
        let elapsed = state.elapsed_text();
        let w = text_w(&elapsed, cell_w, type_scale::BODY);
        r.draw_ui_text(
            size,
            (l.card.0 + l.card.2 - l.pad - w).round(),
            l.status_y,
            type_scale::BODY,
            sk.ink_faint,
            Flags::empty(),
            &elapsed,
            gc,
        );
    }

    // 失败详情：指到位置的原因，最多两行，末行溢出收成省略号。
    if let Some(reason) = &state.failure {
        let avail = l.card.2 - l.pad * 2.0;
        let per_line = cols_that_fit(avail, cell_w, type_scale::SUPPORTING);
        let ch = size.cell_height();
        for (i, line) in wrap(reason, per_line).into_iter().take(2).enumerate() {
            r.draw_ui_text(
                size,
                l.card.0 + l.pad,
                l.detail_y + i as f32 * ch * type_scale::SUPPORTING * 1.35,
                type_scale::SUPPORTING,
                sk.ink_dim,
                Flags::empty(),
                &truncate_cols(&line, per_line),
                gc,
            );
        }
    }

    // Logs 开关的标签（箭头是矢量，在 push_quads 里）。
    r.draw_ui_text(
        size,
        (l.logs_btn.0 + space::M * scale).round(),
        (l.logs_btn.1 + (l.logs_btn.3 - size.cell_height() * type_scale::SUPPORTING) * 0.5).round(),
        type_scale::SUPPORTING,
        sk.ink_dim,
        Flags::empty(),
        "Logs",
        gc,
    );

    // 日志：只显示末尾若干行，最新的在最下——和终端一样往下长。每行按日志
    // 区宽度截断，否则那条中文系统错误会一路画到窗口外面。
    if let Some(area) = l.logs_area {
        let line_h = size.cell_height() * type_scale::SUPPORTING;
        let inner_pad = space::S * scale;
        let max_cols = cols_that_fit(area.2 - inner_pad * 2.0, cell_w, type_scale::SUPPORTING);
        let start = state.log.len().saturating_sub(LOG_ROWS);
        for (i, line) in state.log[start..].iter().enumerate() {
            let y = area.1 + inner_pad + i as f32 * line_h;
            if y + line_h > area.1 + area.3 {
                break;
            }
            let ink = if line.contains("error") { rgb_of(sk.danger) } else { sk.ink_dim };
            r.draw_ui_text(
                size,
                (area.0 + inner_pad).round(),
                y.round(),
                type_scale::SUPPORTING,
                ink,
                Flags::empty(),
                &truncate_cols(line, max_cols),
                gc,
            );
        }
    }

    // 底部按钮文字：居中。主按钮的文字色按 accent 亮度选黑或白，不能写死。
    for (rect, hit, primary) in &l.buttons {
        let label = button_specs(state, language)
            .into_iter()
            .find(|(_, h, _)| h == hit)
            .map(|(label, _, _)| label)
            .unwrap_or_default();
        let w = text_w(&label, cell_w, type_scale::BODY);
        let ink = if *primary { on_accent(sk.accent) } else { sk.ink };
        r.draw_ui_text(
            size,
            (rect.0 + (rect.2 - w) * 0.5).round(),
            (rect.1 + (rect.3 - size.cell_height() * type_scale::BODY) * 0.5).round(),
            type_scale::BODY,
            ink,
            Flags::empty(),
            &label,
            gc,
        );
    }
}

/// accent 上的文字色。accent 在浅色主题里是深靛蓝、在深色主题里是亮蓝，
/// 写死黑或白都会在另一边糊掉。
fn on_accent(accent: Rgb) -> Rgb {
    let lum = 0.299 * accent.r as f32 + 0.587 * accent.g as f32 + 0.114 * accent.b as f32;
    if lum > 150.0 { Rgb::new(12, 14, 18) } else { Rgb::new(255, 255, 255) }
}

fn s_half(scale: f32) -> f32 {
    NODE_D * 0.5 * scale
}

/// 列表和卡片共用的短名：有别名就用别名，否则去掉 user@ 只留主机。
fn short_name(destination: &str) -> String {
    let host = destination.rsplit('@').next().unwrap_or(destination);
    host.to_owned()
}

fn stage_labels(language: UiLanguage) -> [String; 4] {
    [
        language.pick("解析", "Resolve").to_owned(),
        language.pick("连接", "Connect").to_owned(),
        language.pick("认证", "Auth").to_owned(),
        language.pick("会话", "Shell").to_owned(),
    ]
}

fn stage_message(stage: &SshStage, language: UiLanguage) -> String {
    match stage {
        SshStage::Resolve => language.pick("正在解析主机…", "Resolving host…"),
        SshStage::Connect => language.pick("正在建立连接…", "Connecting…"),
        SshStage::Authenticate => language.pick("正在认证…", "Authenticating…"),
        SshStage::OpenShell => language.pick("正在打开会话…", "Opening shell…"),
        SshStage::Ready => language.pick("已连接", "Connected"),
        SshStage::Failed(_) => language.pick("连接失败", "Connection failed"),
    }
    .to_owned()
}

fn failure_headline(language: UiLanguage) -> String {
    language.pick("连接失败", "Connection failed").to_owned()
}

/// 按列宽换行。中日韩宽字符按 2 列计，所以不能按 char 数切。
fn wrap(text: &str, per_line: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cols = 0usize;
    for c in text.chars() {
        if c == '\n' {
            lines.push(std::mem::take(&mut line));
            cols = 0;
            continue;
        }
        let w = c.width().unwrap_or(0);
        if cols + w > per_line && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            cols = 0;
        }
        line.push(c);
        cols += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_stays_hidden_until_the_reveal_delay() {
        let state = SshConnectState::new("root@example.com".into());
        assert!(!state.visible(), "快连接不该闪一下加载页");
    }

    #[test]
    fn failure_shows_immediately_regardless_of_delay() {
        let mut state = SshConnectState::new("root@example.com".into());
        state.set_stage(SshStage::Failed("Permission denied".into()));
        assert!(state.visible());
        assert!(state.failed());
    }

    #[test]
    fn stage_index_maps_every_stage_into_the_four_slot_rail() {
        let mut state = SshConnectState::new("h".into());
        for (stage, want) in [
            (SshStage::Resolve, 0),
            (SshStage::Connect, 1),
            (SshStage::Authenticate, 2),
            (SshStage::OpenShell, 3),
            (SshStage::Ready, 3),
        ] {
            state.set_stage(stage);
            assert_eq!(state.stage_index(), want);
        }
    }

    #[test]
    fn fill_interpolates_toward_the_stage_without_overshooting() {
        let mut state = SshConnectState::new("h".into());
        state.set_stage(SshStage::Authenticate);
        for _ in 0..240 {
            state.step(Duration::from_millis(16));
        }
        assert!((state.fill - 2.0).abs() < 0.05, "fill={}", state.fill);
        assert!(state.fill <= 2.0 + f32::EPSILON, "插值不该越过目标");
    }

    #[test]
    fn particle_phase_wraps_and_never_leaves_unit_range() {
        let mut state = SshConnectState::new("h".into());
        for _ in 0..500 {
            state.step(Duration::from_millis(33));
            assert!((0.0..1.0).contains(&state.phase), "phase={}", state.phase);
        }
    }

    #[test]
    fn failed_state_freezes_the_animation() {
        let mut state = SshConnectState::new("h".into());
        state.step(Duration::from_millis(16));
        let phase = state.phase;
        state.set_stage(SshStage::Failed("boom".into()));
        state.step(Duration::from_millis(100));
        assert_eq!(state.phase, phase, "失败后不该继续跑粒子");
    }

    #[test]
    fn speed_field_is_monotonic_and_pins_both_ends() {
        assert!((ease(0.0)).abs() < 1e-6);
        assert!((ease(1.0) - 1.0).abs() < 1e-6);
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = ease(i as f32 / 100.0);
            assert!(v > prev, "速度场必须单调递增");
            prev = v;
        }
        // 中段比端点快：这正是拖尾被拉长的原因。
        let mid = ease(0.55) - ease(0.45);
        let head = ease(0.10) - ease(0.0);
        assert!(mid > head, "中段速度应高于起步段");
    }

    #[test]
    fn wrap_counts_wide_characters_as_two_columns() {
        let lines = wrap("认证失败拒绝", 4);
        assert_eq!(lines, vec!["认证".to_owned(), "失败".to_owned(), "拒绝".to_owned()]);
    }

    #[test]
    fn short_name_drops_the_user_prefix() {
        assert_eq!(short_name("root@192.168.200.150"), "192.168.200.150");
        assert_eq!(short_name("web-01"), "web-01");
    }

    fn probe_size() -> SizeInfo {
        SizeInfo::new_fully_asymmetric(1000.0, 1000.0, 10.0, 20.0, 0.0, 0.0, 64.0, 16.0)
    }

    const VIEW: (f32, f32, f32, f32) = (0.0, 0.0, 1000.0, 800.0);

    #[test]
    fn failure_opens_the_log_so_the_reason_is_not_one_click_away() {
        let mut state = SshConnectState::new("root@h".into());
        assert!(!state.logs_open);
        state.set_stage(SshStage::Failed("Permission denied".into()));
        assert!(state.logs_open, "失败原因是最该被看见的东西");
        assert!(
            state.log.iter().any(|line| line.contains("Permission denied")),
            "失败原因必须进日志: {:?}",
            state.log
        );
    }

    #[test]
    fn repeated_stage_reports_do_not_flood_the_log() {
        let mut state = SshConnectState::new("root@h".into());
        let base = state.log.len();
        for _ in 0..20 {
            state.set_stage(SshStage::Connect);
        }
        assert_eq!(state.log.len(), base + 1, "同一阶段重复上报只该记一行");
    }

    #[test]
    fn buttons_and_logs_toggle_hit_where_they_are_drawn() {
        let size = probe_size();
        let lang = UiLanguage::EnUs;
        let state = SshConnectState::new("root@h".into());
        let l = layout(&state, &size, VIEW, 1.0, lang);

        let (bx, by, bw, bh) = l.logs_btn;
        assert_eq!(
            hit_test(&state, &size, VIEW, 1.0, lang, bx + bw * 0.5, by + bh * 0.5),
            SshConnectHit::Logs
        );

        let (rect, hit, _) = l.buttons[0];
        assert_eq!(hit, SshConnectHit::Cancel, "连接中只该有取消");
        assert_eq!(
            hit_test(&state, &size, VIEW, 1.0, lang, rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5),
            SshConnectHit::Cancel
        );
    }

    #[test]
    fn failed_card_offers_close_instead_of_cancel() {
        let size = probe_size();
        let lang = UiLanguage::EnUs;
        let mut state = SshConnectState::new("root@h".into());
        state.set_stage(SshStage::Failed("boom".into()));
        let l = layout(&state, &size, VIEW, 1.0, lang);
        let (rect, hit, primary) = l.buttons[0];
        assert_eq!(hit, SshConnectHit::Close);
        assert!(primary, "唯一动作应当是主按钮");
        assert_eq!(
            hit_test(&state, &size, VIEW, 1.0, lang, rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5),
            SshConnectHit::Close
        );
    }

    #[test]
    fn the_mask_swallows_clicks_anywhere_in_the_pane() {
        // 卡片之外的空白也必须被吞掉，否则按压会漏进终端起拖选。
        assert!(covers(VIEW, 5.0, 5.0));
        assert!(covers(VIEW, 999.0, 799.0));
        assert!(!covers(VIEW, 1001.0, 400.0));
    }

    #[test]
    fn opening_the_log_makes_the_card_taller_not_narrower() {
        let size = probe_size();
        let lang = UiLanguage::EnUs;
        let mut state = SshConnectState::new("root@h".into());
        let before = layout(&state, &size, VIEW, 1.0, lang).card;
        state.toggle_logs();
        let after = layout(&state, &size, VIEW, 1.0, lang).card;
        assert!(after.3 > before.3, "展开日志应当长高");
        assert_eq!(after.2, before.2, "宽度不该跟着变，否则读起来像在抖");
        assert!(layout(&state, &size, VIEW, 1.0, lang).logs_area.is_some());
    }
}
