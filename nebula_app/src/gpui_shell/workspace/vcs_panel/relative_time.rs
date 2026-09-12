//! Localized Git history timestamps, independent of the renderer and system clock.

use crate::i18n::{Message, UiLanguage};

pub(super) fn git_relative_time_at(timestamp: i64, now: i64, language: UiLanguage) -> String {
    let seconds = now.saturating_sub(timestamp).max(0);
    let (count, singular, plural) = match seconds {
        0..=59 => return language.text(Message::VcsTimeNow).to_owned(),
        60..=3599 => (seconds / 60, Message::VcsTimeMinute, Message::VcsTimeMinutes),
        3600..=86_399 => (seconds / 3600, Message::VcsTimeHour, Message::VcsTimeHours),
        86_400..=2_592_000 => (seconds / 86_400, Message::VcsTimeDay, Message::VcsTimeDays),
        2_592_001..=31_536_000 => {
            (seconds / 2_592_000, Message::VcsTimeMonth, Message::VcsTimeMonths)
        },
        _ => (seconds / 31_536_000, Message::VcsTimeYear, Message::VcsTimeYears),
    };
    if count == 1 {
        language.text(singular).to_owned()
    } else {
        language.format(plural, &[("count", &count.to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_history_relative_time_uses_stable_boundaries() {
        let now = 100_000_000;
        assert_eq!(git_relative_time_at(now + 1, now, UiLanguage::ZhCn), "刚刚");
        assert_eq!(git_relative_time_at(now - 59, now, UiLanguage::ZhCn), "刚刚");
        assert_eq!(git_relative_time_at(now - 60, now, UiLanguage::ZhCn), "1 分钟前");
        assert_eq!(git_relative_time_at(now - 3_600, now, UiLanguage::ZhCn), "1 小时前");
        assert_eq!(git_relative_time_at(now - 86_400, now, UiLanguage::ZhCn), "1 天前");
        assert_eq!(git_relative_time_at(now - 2_592_001, now, UiLanguage::ZhCn), "1 个月前");
        assert_eq!(git_relative_time_at(now - 31_536_001, now, UiLanguage::ZhCn), "1 年前");
    }

    #[test]
    fn git_history_time_uses_the_active_language_and_english_fallback() {
        let now = 100_000_000;
        for (seconds, expected) in [
            (-1, "just now"),
            (59, "just now"),
            (60, "1 minute ago"),
            (120, "2 minutes ago"),
            (3_600, "1 hour ago"),
            (7_200, "2 hours ago"),
            (86_400, "1 day ago"),
            (172_800, "2 days ago"),
            (2_592_001, "1 month ago"),
            (5_184_000, "2 months ago"),
            (31_536_001, "1 year ago"),
            (63_072_000, "2 years ago"),
        ] {
            assert_eq!(git_relative_time_at(now - seconds, now, UiLanguage::EnUs), expected);
            assert_eq!(git_relative_time_at(now - seconds, now, UiLanguage::FrFr), expected);
        }
    }
}
