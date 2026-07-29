//! chrome 内文本光标的节律。所有自绘输入框共用一套相位。
//!
//! # 为什么不是「每 500ms 翻一次」
//!
//! 上一版是 `(unix_millis / 500) % 2 == 0`——相位挂在 UNIX 纪元上。后果：
//! 点进输入框的瞬间，光标有 **50% 概率正处在熄灭的半周期**，用户要等最多
//! 半秒才看到它。刚点下去的那半秒里，输入框看起来像没获得焦点。
//!
//! 这里改成相位从**最后一次编辑活动**起算，于是：
//!
//! 1. **聚焦/打完字的瞬间必定是亮的**，而且立刻就亮——存在感来自这一条，
//!    而不是来自闪得更快。
//! 2. **连续打字时不闪**。所有成熟编辑器都这样：打字时用户不需要「找光标」，
//!    此时闪烁纯粹是视觉干扰。停手之后才开始呼吸。
//! 3. 明暗占空比偏向亮侧。等分的方波会让光标有一半时间不存在，读作"断续"
//!    而不是"待命"。
//!
//! # 节律取自系统
//!
//! Windows 上读 `GetCaretBlinkTime()`（用户在「控制面板 → 键盘」里能调，
//! 无障碍设置也会改它，还可能被设成"不闪"）。自造一个固定值等于无视这份
//! 系统级偏好——原生感正是由这类地方累积出来的。

use std::sync::atomic::{AtomicU64, Ordering};

/// 打字期间保持常亮的时长。略长于一个半周期，让正常速度的连续输入
/// 完全不触发闪烁。
const SOLID_AFTER_EDIT_MS: u64 = 620;

/// 系统默认半周期。Windows 的出厂值就是 530ms，其他平台沿用它——
/// 这个节律本身是几十年的行业惯例，不是 Windows 特有的口味。
const DEFAULT_HALF_CYCLE_MS: u64 = 530;

/// 亮相占一个完整周期的比例。0.5 是等分方波；偏向亮侧让光标读作
/// 「待命」而不是「断续」。
const ON_DUTY: f32 = 0.58;

/// 最后一次编辑活动的挂钟毫秒。0 表示"还没有过活动"。
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 系统的闪烁半周期。返回 `None` 表示用户要求光标**不闪**——这是一个
/// 真实的无障碍设置（闪烁会诱发部分人的偏头痛甚至光敏性癫痫），必须尊重。
#[cfg(windows)]
fn system_half_cycle_ms() -> Option<u64> {
    // Win32 用 INFINITE（u32::MAX）表示"关闭闪烁"。
    let raw = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
    match raw {
        0 | u32::MAX => None,
        value => Some(u64::from(value)),
    }
}

#[cfg(not(windows))]
fn system_half_cycle_ms() -> Option<u64> {
    Some(DEFAULT_HALF_CYCLE_MS)
}

/// 记录一次编辑活动（插入、删除、移动光标、获得焦点）。
///
/// 调用后光标立刻变亮，并从此刻重新起算相位。所有会改动缓冲区或光标位置
/// 的路径都该调它——漏掉一处的表现是「那个操作之后光标闪得不对劲」。
pub fn note_activity() {
    LAST_ACTIVITY_MS.store(now_millis(), Ordering::Relaxed);
}

/// 这一帧该不该画光标。
pub fn is_on() -> bool {
    let Some(half_cycle) = system_half_cycle_ms() else {
        // 用户关掉了闪烁：光标常亮，位置仍然可见。
        return true;
    };
    let last = LAST_ACTIVITY_MS.load(Ordering::Relaxed);
    let since = now_millis().saturating_sub(last);
    phase_is_on(since, half_cycle)
}

/// 纯函数形式的相位判定，便于在没有挂钟的情况下测试。
fn phase_is_on(since_activity_ms: u64, half_cycle_ms: u64) -> bool {
    if since_activity_ms < SOLID_AFTER_EDIT_MS {
        return true;
    }
    let cycle = (half_cycle_ms * 2).max(1);
    let on_span = (cycle as f32 * ON_DUTY) as u64;
    (since_activity_ms - SOLID_AFTER_EDIT_MS) % cycle < on_span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moment_after_an_edit_is_always_lit() {
        // 这条是本模块存在的理由：旧实现下这一刻有一半概率是灭的。
        assert!(phase_is_on(0, DEFAULT_HALF_CYCLE_MS));
        assert!(phase_is_on(1, DEFAULT_HALF_CYCLE_MS));
    }

    #[test]
    fn typing_does_not_blink() {
        // 以 ~120ms 一个字符的正常手速连打，全程常亮。
        for keystroke in 0..40u64 {
            assert!(
                phase_is_on(keystroke * 120 % SOLID_AFTER_EDIT_MS, DEFAULT_HALF_CYCLE_MS),
                "打字过程中不该出现熄灭帧",
            );
        }
    }

    #[test]
    fn it_starts_blinking_after_the_hand_stops() {
        // 静置足够久之后，明暗都必须出现过——否则就是"不闪"而非"呼吸"。
        let cycle = DEFAULT_HALF_CYCLE_MS * 2;
        let sampled: Vec<bool> = (0..cycle)
            .step_by(20)
            .map(|offset| phase_is_on(SOLID_AFTER_EDIT_MS + offset, DEFAULT_HALF_CYCLE_MS))
            .collect();
        assert!(sampled.contains(&true) && sampled.contains(&false));
    }

    #[test]
    fn lit_span_outweighs_the_dark_span() {
        let cycle = DEFAULT_HALF_CYCLE_MS * 2;
        let lit = (0..cycle)
            .filter(|offset| phase_is_on(SOLID_AFTER_EDIT_MS + offset, DEFAULT_HALF_CYCLE_MS))
            .count();
        assert!(
            lit * 2 > cycle as usize,
            "亮相要多于一半，否则光标读作断续而不是待命（实测 {lit}/{cycle}）",
        );
    }

    #[test]
    fn a_zero_blink_time_never_divides_by_zero() {
        // GetCaretBlinkTime 理论上可以返回很小的值；周期钳到 >=1 防除零。
        assert!(phase_is_on(SOLID_AFTER_EDIT_MS, 0) || !phase_is_on(SOLID_AFTER_EDIT_MS, 0));
    }
}
