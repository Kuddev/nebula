//! Nebula's renderer-independent motion runtime.
//!
//! The runtime owns time normalization and motion math only. Rendering code
//! decides how values map to pixels, so OpenGL and wgpu consume identical
//! animation state without either backend becoming a timing source.

use std::time::{Duration, Instant};

const DEFAULT_MAX_DELTA: Duration = Duration::from_millis(50);
const DEFAULT_FRAME_DELTA: Duration = Duration::from_nanos(16_666_667);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotionPolicy {
    #[default]
    Full,
    Reduced,
    Off,
}

impl MotionPolicy {
    pub fn duration(self, duration: Duration) -> Duration {
        match self {
            Self::Full => duration,
            Self::Reduced => duration.min(Duration::from_millis(120)),
            Self::Off => Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub now: Instant,
    pub delta: Duration,
}

impl Frame {
    #[inline]
    pub fn delta_seconds(self) -> f32 {
        self.delta.as_secs_f32()
    }
}

/// One clock should be shared by all motion state belonging to a window.
#[derive(Debug, Clone)]
pub struct MotionClock {
    last_tick: Option<Instant>,
    max_delta: Duration,
}

impl Default for MotionClock {
    fn default() -> Self {
        Self { last_tick: None, max_delta: DEFAULT_MAX_DELTA }
    }
}

impl MotionClock {
    pub fn tick(&mut self) -> Frame {
        self.tick_at(Instant::now())
    }

    pub fn tick_at(&mut self, now: Instant) -> Frame {
        // 窗口从休眠或断点恢复时限制 dt，避免动画一步跳到终点或弹簧爆炸。
        let delta = self
            .last_tick
            .map_or(DEFAULT_FRAME_DELTA, |last| now.saturating_duration_since(last))
            .min(self.max_delta);
        self.last_tick = Some(now);
        Frame { now, delta }
    }

    pub fn reset(&mut self) {
        self.last_tick = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    Linear,
    EaseInQuad,
    EaseOutQuad,
    EaseInOutCubic,
    #[default]
    SwiftOut,
}

/// Product-level intent. Call sites choose why an element moves instead of
/// inventing durations and curves independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionRole {
    Enter,
    Exit,
    Relocate,
    Fade,
    Continuous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionSpec {
    pub duration: Duration,
    pub easing: Easing,
}

impl MotionRole {
    pub const fn spec(self) -> MotionSpec {
        match self {
            // 终端交互首先要求立即响应，因此所有一次性运动都控制在 150ms 内。
            Self::Enter => {
                MotionSpec { duration: Duration::from_millis(120), easing: Easing::SwiftOut }
            },
            Self::Exit => {
                MotionSpec { duration: Duration::from_millis(90), easing: Easing::EaseInQuad }
            },
            Self::Relocate => {
                MotionSpec { duration: Duration::from_millis(140), easing: Easing::EaseInOutCubic }
            },
            Self::Fade => {
                MotionSpec { duration: Duration::from_millis(100), easing: Easing::Linear }
            },
            Self::Continuous => MotionSpec { duration: Duration::ZERO, easing: Easing::Linear },
        }
    }
}

impl Easing {
    pub fn sample(self, progress: f32) -> f32 {
        let t = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseInQuad => t * t,
            Self::EaseOutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOutCubic if t < 0.5 => 4.0 * t * t * t,
            Self::EaseInOutCubic => 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0,
            Self::SwiftOut => 1.0 - (1.0 - t).powi(3),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Tween {
    start: f32,
    value: f32,
    target: f32,
    elapsed: Duration,
    duration: Duration,
    easing: Easing,
    active: bool,
}

impl Tween {
    pub fn new(value: f32) -> Self {
        Self {
            start: value,
            value,
            target: value,
            elapsed: Duration::ZERO,
            duration: Duration::ZERO,
            easing: Easing::default(),
            active: false,
        }
    }

    pub fn animate_to(
        &mut self,
        target: f32,
        duration: Duration,
        easing: Easing,
        policy: MotionPolicy,
    ) {
        let duration = policy.duration(duration);
        if duration.is_zero() || (self.value - target).abs() <= f32::EPSILON {
            self.snap_to(target);
            return;
        }
        self.start = self.value;
        self.target = target;
        self.elapsed = Duration::ZERO;
        self.duration = duration;
        self.easing = easing;
        self.active = true;
    }

    pub fn animate_role(&mut self, target: f32, role: MotionRole, policy: MotionPolicy) {
        let spec = role.spec();
        self.animate_to(target, spec.duration, spec.easing, policy);
    }

    pub fn step(&mut self, frame: Frame) -> bool {
        if !self.active {
            return false;
        }
        self.elapsed = (self.elapsed + frame.delta).min(self.duration);
        let progress = self.elapsed.as_secs_f32() / self.duration.as_secs_f32();
        self.value = lerp(self.start, self.target, self.easing.sample(progress));
        if self.elapsed >= self.duration {
            self.snap_to(self.target);
        }
        self.active
    }

    pub fn snap_to(&mut self, value: f32) {
        self.start = value;
        self.value = value;
        self.target = value;
        self.elapsed = Duration::ZERO;
        self.active = false;
    }

    pub fn value(self) -> f32 {
        self.value
    }

    pub fn is_active(self) -> bool {
        self.active
    }

    pub fn target(self) -> f32 {
        self.target
    }
}

/// Allocation-free, interruption-friendly damped spring for interactive UI.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    response: f32,
    epsilon: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self { value, velocity: 0.0, target: value, response: 0.22, epsilon: 0.001 }
    }

    pub fn with_response(mut self, response: f32) -> Self {
        self.response = response.max(0.05);
        self
    }

    pub fn set_target(&mut self, target: f32, policy: MotionPolicy) {
        if policy == MotionPolicy::Off {
            self.snap_to(target);
        } else {
            self.target = target;
        }
    }

    pub fn step(&mut self, frame: Frame) -> bool {
        if !self.is_active() {
            self.snap_to(self.target);
            return false;
        }

        // 使用临界阻尼解析解而非 Euler；即使掉帧达到 50ms 也不会数值爆炸。
        let omega = std::f32::consts::TAU / self.response;
        let delta = frame.delta_seconds();
        let displacement = self.value - self.target;
        let decay = (-omega * delta).exp();
        let correction = (self.velocity + omega * displacement) * delta;
        self.value = self.target + (displacement + correction) * decay;
        self.velocity = (self.velocity - omega * correction) * decay;
        if !self.is_active() {
            self.snap_to(self.target);
        }
        self.is_active()
    }

    pub fn snap_to(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn value(self) -> f32 {
        self.value
    }

    pub fn target(self) -> f32 {
        self.target
    }

    pub fn is_active(self) -> bool {
        (self.value - self.target).abs() > self.epsilon || self.velocity.abs() > self.epsilon
    }
}

#[inline]
pub fn lerp(start: f32, end: f32, progress: f32) -> f32 {
    start + (end - start) * progress
}

pub trait Interpolate: Copy {
    fn interpolate(self, end: Self, progress: f32) -> Self;
}

impl Interpolate for f32 {
    fn interpolate(self, end: Self, progress: f32) -> Self {
        lerp(self, end, progress)
    }
}

impl<const N: usize> Interpolate for [f32; N] {
    fn interpolate(self, end: Self, progress: f32) -> Self {
        std::array::from_fn(|index| lerp(self[index], end[index], progress))
    }
}

/// Generic allocation-free tween for positions, rectangles and linear color.
#[derive(Debug, Clone, Copy)]
pub struct ValueTween<T: Interpolate> {
    start: T,
    end: T,
    progress: Tween,
}

/// Deterministic phase source for cursor blink, spinners and other loops.
#[derive(Debug, Clone, Copy)]
pub struct Pulse {
    elapsed: Duration,
    period: Duration,
}

impl Pulse {
    pub fn new(period: Duration) -> Self {
        Self { elapsed: Duration::ZERO, period: period.max(Duration::from_millis(1)) }
    }

    pub fn step(&mut self, frame: Frame) {
        self.elapsed += frame.delta;
        while self.elapsed >= self.period {
            self.elapsed -= self.period;
        }
    }

    pub fn phase(self) -> f32 {
        self.elapsed.as_secs_f32() / self.period.as_secs_f32()
    }

    pub fn visible(self, duty_cycle: f32) -> bool {
        self.phase() < duty_cycle.clamp(0.0, 1.0)
    }

    pub fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
    }
}

impl<T: Interpolate> ValueTween<T> {
    pub fn new(value: T) -> Self {
        Self { start: value, end: value, progress: Tween::new(1.0) }
    }

    pub fn animate_to(&mut self, end: T, role: MotionRole, policy: MotionPolicy) {
        self.start = self.value();
        self.end = end;
        self.progress.snap_to(0.0);
        self.progress.animate_role(1.0, role, policy);
    }

    pub fn step(&mut self, frame: Frame) -> bool {
        self.progress.step(frame)
    }

    pub fn value(self) -> T {
        self.start.interpolate(self.end, self.progress.value())
    }

    pub fn is_active(self) -> bool {
        self.progress.is_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_caps_resume_delta() {
        let start = Instant::now();
        let mut clock = MotionClock::default();
        clock.tick_at(start);
        assert_eq!(clock.tick_at(start + Duration::from_secs(2)).delta, DEFAULT_MAX_DELTA);
    }

    #[test]
    fn tween_can_be_interrupted_without_jumping() {
        let now = Instant::now();
        let mut tween = Tween::new(0.0);
        tween.animate_to(1.0, Duration::from_secs(1), Easing::Linear, MotionPolicy::Full);
        tween.step(Frame { now, delta: Duration::from_millis(400) });
        let interrupted_at = tween.value();
        tween.animate_to(0.0, Duration::from_secs(1), Easing::Linear, MotionPolicy::Full);
        assert_eq!(tween.value(), interrupted_at);
    }

    #[test]
    fn motion_off_snaps_immediately() {
        let mut tween = Tween::new(0.0);
        tween.animate_to(1.0, Duration::from_secs(1), Easing::SwiftOut, MotionPolicy::Off);
        assert_eq!(tween.value(), 1.0);
        assert!(!tween.is_active());
    }

    #[test]
    fn semantic_specs_follow_terminal_motion_budget() {
        let enter = MotionRole::Enter.spec();
        let exit = MotionRole::Exit.spec();
        let relocate = MotionRole::Relocate.spec();
        assert_eq!(enter.easing, Easing::SwiftOut);
        assert_eq!(exit.easing, Easing::EaseInQuad);
        assert_eq!(relocate.easing, Easing::EaseInOutCubic);
        assert!(enter.duration <= Duration::from_millis(150));
        assert!(exit.duration < enter.duration);
    }

    #[test]
    fn spring_converges_without_overshooting_when_critically_damped() {
        let now = Instant::now();
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0, MotionPolicy::Full);
        for _ in 0..180 {
            spring.step(Frame { now, delta: DEFAULT_FRAME_DELTA });
            assert!(spring.value() <= 1.001);
        }
        assert!((spring.value() - 1.0).abs() < 0.001);
        assert!(!spring.is_active());
    }

    #[test]
    fn spring_remains_stable_at_capped_frame_delta() {
        let now = Instant::now();
        let mut spring = Spring::new(0.0).with_response(0.14);
        spring.set_target(1.0, MotionPolicy::Full);
        for _ in 0..20 {
            spring.step(Frame { now, delta: DEFAULT_MAX_DELTA });
            assert!(spring.value().is_finite());
            assert!((0.0..=1.001).contains(&spring.value()));
        }
        assert!((spring.value() - 1.0).abs() < 0.001);
    }

    #[test]
    fn value_tween_interpolates_rect_without_allocating() {
        let now = Instant::now();
        let mut motion = ValueTween::new([0.0, 10.0, 20.0, 30.0]);
        motion.animate_to([10.0, 20.0, 40.0, 50.0], MotionRole::Relocate, MotionPolicy::Full);
        motion.step(Frame { now, delta: MotionRole::Relocate.spec().duration / 2 });
        let value = motion.value();
        assert!(value[0] > 0.0 && value[0] < 10.0);
        assert!(value[2] > 20.0 && value[2] < 40.0);
    }

    #[test]
    fn pulse_wraps_without_losing_phase_contract() {
        let now = Instant::now();
        let mut pulse = Pulse::new(Duration::from_millis(1000));
        pulse.step(Frame { now, delta: Duration::from_millis(750) });
        assert!(!pulse.visible(0.5));
        pulse.step(Frame { now, delta: Duration::from_millis(300) });
        assert!(pulse.visible(0.5));
        assert!((pulse.phase() - 0.05).abs() < 0.001);
    }
}
