use crate::display::UiLanguage;

#[derive(Clone, Copy)]
pub(super) struct SettingHelp {
    pub summary: &'static str,
    pub details: Option<&'static str>,
}

impl From<&'static str> for SettingHelp {
    fn from(summary: &'static str) -> Self {
        Self { summary, details: None }
    }
}

pub(super) fn help(key: &str, language: UiLanguage) -> SettingHelp {
    let (summary, details) = match key {
        "background" => (
            language.pick("只更改终端底色，设置页仍跟随主题。", "Changes only the terminal background; Settings keeps the theme colors."),
            None,
        ),
        "background_image" => (
            language.pick("在终端文字后方显示图片。", "Shows an image behind terminal text."),
            Some(language.pick("图片不改变文字配色；看不清时可降低背景图像不透明度。", "Text colors still come from the theme. Lower the image opacity if text is hard to read.")),
        ),
        "background_image_fit" => (
            language.pick("调整图片的铺放方式。", "Chooses how the background image fills the terminal."),
            Some(language.pick("拉伸会改变比例；适应和填充保持比例；原始尺寸不缩放。", "Fill stretches the image. Uniform and Uniform to fill preserve its aspect ratio. Original size does not scale it.")),
        ),
        "background_image_alignment" => (
            language.pick("设置图片在终端中的位置。", "Positions the image within the terminal."),
            None,
        ),
        "background_image_opacity" => (
            language.pick("数值越低，图片越淡。", "Lower values make the image less prominent."),
            None,
        ),
        "background_image_cover_chrome" => (
            language.pick("关闭后，图片只显示在终端区域。", "When disabled, the image appears only behind the terminal."),
            None,
        ),
        "cursor_shape" => ("", None),
        "cursor_blink" => (
            language.pick("关闭后，光标保持常亮。", "When disabled, the cursor stays lit."),
            None,
        ),
        "tab_close_visible" => (
            language.pick("隐藏后，仍可用鼠标中键关闭标签。", "Middle-click still closes tabs when their close buttons are hidden."),
            None,
        ),
        "language" => (
            language.pick("只更改应用界面的语言。", "Changes only the application interface language."),
            Some(language.pick("终端内程序的输出语言由各自的环境设置决定。", "Programs inside the terminal choose their output language from their own environment.")),
        ),
        "density" => (
            language.pick("调整界面留白，不改变终端行距。", "Adjusts interface spacing without changing terminal line spacing."),
            None,
        ),
        "opacity" => (
            language.pick("100% 为完全不透明，不影响设置页。", "100% is fully opaque. The Settings page is unaffected."),
            None,
        ),
        "blur" => (
            language.pick("选择窗口背景的模糊效果。", "Chooses the window background effect."),
            Some(language.pick("Mica 系列开销较低；Aero 和 Acrylic 实时模糊窗口后方的内容，开销较高。", "Mica effects use fewer resources. Aero and Acrylic blur live content behind the window and cost more.")),
        ),
        "cell_width_mode" => (
            language.pick("调整终端字符的列宽。", "Adjusts terminal character cell width."),
            Some(language.pick("紧凑向下取整；宽松向上补齐，避免小数字宽的字体显得拥挤。只影响终端网格。", "Compact rounds down. Relaxed rounds up to avoid squeezing fonts with fractional widths. Only the terminal grid is affected.")),
        ),
        "fetch" => (
            language.pick("新会话启动时显示系统信息。", "Shows system information when a new session starts."),
            Some(language.pick("通过 fastfetch 显示系统信息；关闭可减少新标签的启动等待。", "Uses fastfetch to display system information. Disabling it reduces startup work for new tabs.")),
        ),
        "powerline" => (
            language.pick("显示分段提示符，需字体支持。", "Shows a segmented prompt. Requires a compatible font."),
            Some(language.pick("字体须包含 Powerline 字形，否则分段箭头可能显示为方框。", "The font must include Powerline glyphs or the segment arrows may appear as boxes.")),
        ),
        "shell" => (
            language.pick("用于新标签，不影响已打开的终端。", "Used for new tabs. Existing terminals are unaffected."),
            None,
        ),
        "startup_directory" => (
            language.pick("未设置时，继承应用的启动目录。", "When unset, inherits the directory used to launch the application."),
            Some(language.pick("从资源管理器的文件夹中启动时，默认进入该文件夹。", "Launching from a File Explorer folder uses that folder by default.")),
        ),
        "bell" => (
            language.pick("程序提示时，选择声音或闪烁提醒。", "Chooses sound or visual feedback for terminal alerts."),
            None,
        ),
        "font_family" => (
            language.pick("按字体组顺序查找可用字形。", "Uses the font group in order to find available glyphs."),
            Some(language.pick("前面的字体缺少字形时，会继续尝试后面的字体。", "When a font lacks a glyph, the next font in the group is tried.")),
        ),
        "ghost" => (
            language.pick("根据历史命令给出建议，确认后才填入。", "Suggests commands from history and inserts them only after acceptance."),
            None,
        ),
        "accept" => (
            language.pick("选择填入建议时使用的按键。", "Chooses the key used to accept a suggestion."),
            Some(language.pick("如果 Tab 与 Shell 自带补全冲突，可改用右方向键。", "Use Right arrow if Tab conflicts with the shell's own completion.")),
        ),
        "completion_style" => (
            language.pick("选择行内建议或候选列表。", "Chooses inline suggestions or a list of candidates."),
            Some(language.pick("行内建议不遮挡下方输出；候选列表能同时显示多项建议。", "Inline suggestions leave output visible. A popup list shows several candidates at once.")),
        ),
        "copy_on_select" => (
            language.pick("松开鼠标自动复制，右键直接粘贴。", "Copies on mouse release and pastes on right-click."),
            Some(language.pick("关闭后，选中不会自动复制；右键改为打开复制与粘贴菜单。", "When disabled, selection does not copy automatically and right-click opens the copy/paste menu.")),
        ),
        "multiline_paste_confirm" => (
            language.pick("在普通 shell 中粘贴多行或高风险内容时先确认。", "Asks before multiline or risky pastes in a plain shell."),
            Some(language.pick("检查换行、提权命令和控制字符。Bracketed paste 模式与全屏程序不受此项保护；关闭后直接粘贴。", "Checks line breaks, privileged commands and control characters. Bracketed paste and full-screen apps are unaffected; disabling this pastes directly.")),
        ),
        "panel_resize" => (
            language.pick("关闭后锁定分界线，避免误拖。", "Locks the divider when disabled to prevent accidental resizing."),
            None,
        ),
        "cjk_bold_regular" => (
            language.pick("用提亮代替加粗，让密集笔画更清楚。", "Brightens dense glyphs instead of thickening their strokes."),
            Some(language.pick("只影响中日韩（CJK）粗体字形，拉丁字母仍使用真正的粗体。", "Only CJK bold glyphs are affected. Latin letters still use true bold.")),
        ),
        "tabs_position" => ("", None),
        "tab_reveal" => (
            language.pick("新标签滑动出现，或立即显示。", "Slides new tabs into view or shows them immediately."),
            None,
        ),
        "new_tab_position" => (
            language.pick("只影响新标签，不改变恢复时的顺序。", "Affects new tabs only, not the order of restored tabs."),
            None,
        ),
        "windowing_behavior" => (
            language.pick("再次启动应用时，新开窗口或加入已有窗口。", "Opens a new window or joins an existing one when launched again."),
            None,
        ),
        "vcs_display" => (
            language.pick("选择侧栏显示 Git 还是 SVN 状态。", "Chooses whether the sidebar shows Git or SVN status."),
            Some(language.pick("自动检测会识别 .git 和 .svn；同目录存在两者时可手动指定。", "Auto detect checks for .git and .svn. Choose manually when both are present.")),
        ),
        "keep_session" => (
            language.pick("关窗后终端继续运行；关闭此项会结束会话。", "Keeps terminals running after closing the window; disabling it ends sessions."),
            Some(language.pick("关闭此项后，关窗会终止 Shell，未保存的工作可能丢失。开启后可重新附着到后台会话。", "When disabled, closing the window terminates its shells and may lose unsaved work. When enabled, background sessions can be reattached.")),
        ),
        "restore_session" => (
            language.pick("恢复标签、布局和目录，不恢复原进程。", "Restores tabs, layout and directories, not the original processes."),
            Some(language.pick("恢复的标签会启动新的 Shell，不会让已结束的程序继续运行。", "Restored tabs start new shells; terminated programs are not resumed.")),
        ),
        "resume_ai" => (
            language.pick("恢复 AI 标签时，继续上次的对话。", "Continues the previous conversation when restoring an AI tab."),
            None,
        ),
        "tray" => (
            language.pick("在通知区域查看 AI 会话状态。", "Shows AI session status in the notification area."),
            None,
        ),
        _ => unreachable!("unknown settings help key: {key}"),
    };
    SettingHelp { summary, details }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_stay_short_in_both_interface_languages() {
        let keys = [
            "background",
            "background_image",
            "background_image_fit",
            "background_image_alignment",
            "background_image_opacity",
            "background_image_cover_chrome",
            "cursor_shape",
            "cursor_blink",
            "tab_close_visible",
            "language",
            "density",
            "opacity",
            "blur",
            "cell_width_mode",
            "fetch",
            "powerline",
            "shell",
            "startup_directory",
            "bell",
            "font_family",
            "ghost",
            "accept",
            "completion_style",
            "copy_on_select",
            "multiline_paste_confirm",
            "panel_resize",
            "cjk_bold_regular",
            "tabs_position",
            "tab_reveal",
            "new_tab_position",
            "windowing_behavior",
            "vcs_display",
            "keep_session",
            "restore_session",
            "resume_ai",
            "tray",
        ];
        for key in keys {
            for language in [UiLanguage::ZhCn, UiLanguage::EnUs] {
                let description = help(key, language);
                let limit = if language == UiLanguage::ZhCn { 32 } else { 100 };
                assert!(description.summary.chars().count() <= limit, "long summary: {key}");
                assert!(description.details.is_none_or(|details| !details.is_empty()));
            }
        }
    }

    #[test]
    fn critical_limits_remain_in_the_short_description() {
        let language = UiLanguage::ZhCn;
        assert!(help("keep_session", language).summary.contains("结束会话"));
        assert!(help("restore_session", language).summary.contains("不恢复原进程"));
        assert!(help("multiline_paste_confirm", language).details.unwrap().contains("全屏程序"));
    }
}
