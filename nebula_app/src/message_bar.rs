use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use nebula_terminal::grid::Dimensions;

use crate::display::SizeInfo;

pub const CLOSE_BUTTON_TEXT: &str = "[X]";
const CLOSE_BUTTON_PADDING: usize = 1;
const MIN_FREE_LINES: usize = 3;
const TRUNCATED_MESSAGE: &str = "[MESSAGE TRUNCATED]";

/// Window-space message-bar geometry shared by rendering and pointer input.
/// Keeping one pixel rectangle avoids grid-coordinate drift while sidebars,
/// search, or asymmetric padding change the terminal viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MessageBarRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MessageBarRect {
    #[inline]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    #[inline]
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Message background constrained to the terminal content band. In
/// particular, warnings must never paint through the tabs sidebar or drawer.
#[inline]
pub fn message_bar_rect(size_info: &SizeInfo, search_active: bool) -> MessageBarRect {
    let x = size_info.padding_x().clamp(0.0, size_info.width());
    let right = (size_info.width() - size_info.padding_right()).max(x);
    let start_line = size_info.screen_lines() + usize::from(search_active);
    let y = size_info
        .cell_height()
        .mul_add(start_line as f32, size_info.padding_y())
        .clamp(0.0, size_info.height());

    MessageBarRect::new(x, y, right - x, size_info.height() - y)
}

/// Pixel hit target occupied by the visible `[X]` on the first message row.
#[inline]
pub fn message_close_button_rect(
    size_info: &SizeInfo,
    search_active: bool,
) -> Option<MessageBarRect> {
    let button_columns = CLOSE_BUTTON_TEXT.chars().count();
    if size_info.columns() < button_columns {
        return None;
    }

    let bar = message_bar_rect(size_info, search_active);
    let x = size_info.padding_x()
        + (size_info.columns() - button_columns) as f32 * size_info.cell_width();
    Some(MessageBarRect::new(
        x,
        bar.y,
        button_columns as f32 * size_info.cell_width(),
        size_info.cell_height(),
    ))
}

/// Message for display in the MessageBuffer.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Message {
    text: String,
    ty: MessageType,
    target: Option<String>,
}

/// Purpose of the message.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum MessageType {
    /// A message represents an error.
    Error,

    /// A message represents a warning.
    Warning,
}

impl Message {
    /// Create a new message.
    pub fn new(text: String, ty: MessageType) -> Message {
        Message { text, ty, target: None }
    }

    pub fn user_error(error: &crate::ux::UserFacingError) -> Message {
        Message::new(error.message(), MessageType::Error)
    }

    /// Formatted message text lines.
    pub fn text(&self, size_info: &SizeInfo) -> Vec<String> {
        let num_cols = size_info.columns();
        let total_lines = (size_info.height() - size_info.padding_y() - size_info.padding_bottom())
            / size_info.cell_height();
        let max_lines = (total_lines as usize).saturating_sub(MIN_FREE_LINES);
        let button_len = CLOSE_BUTTON_TEXT.chars().count();

        // Split line to fit the screen.
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut line_len: usize = 0;
        for c in self.text.trim().chars() {
            let width = c.width().unwrap_or(0);
            let line_capacity = if lines.is_empty() && num_cols >= button_len {
                // Keep space in first line for button.
                num_cols.saturating_sub(button_len + CLOSE_BUTTON_PADDING)
            } else {
                num_cols
            };
            if c == '\n' || line_len.saturating_add(width) > line_capacity {
                let is_whitespace = c.is_whitespace();

                // Attempt to wrap on word boundaries.
                let mut new_line = String::new();
                if let Some(index) = line.rfind(char::is_whitespace).filter(|_| !is_whitespace) {
                    let whitespace_len = line[index..].chars().next().map_or(0, char::len_utf8);
                    let split = line.split_off(index + whitespace_len);
                    line.truncate(index);
                    new_line = split;
                }

                lines.push(Self::pad_text(line, num_cols));
                line = new_line;
                line_len = line.chars().count();

                // Do not append whitespace at EOL.
                if is_whitespace {
                    continue;
                }
            }

            line.push(c);

            // Reserve extra column for fullwidth characters.
            if width == 2 {
                line.push(' ');
            }

            line_len += width
        }
        lines.push(Self::pad_text(line, num_cols));

        // Truncate output if it's too long.
        if lines.len() > max_lines {
            lines.truncate(max_lines);
            if TRUNCATED_MESSAGE.len() <= num_cols {
                if let Some(line) = lines.iter_mut().last() {
                    *line = Self::pad_text(TRUNCATED_MESSAGE.into(), num_cols);
                }
            }
        }

        // Append close button to first line.
        if button_len <= num_cols {
            if let Some(line) = lines.get_mut(0) {
                // `num_cols` 是字符/单元格数量，不能直接作为 UTF-8 字节下标。
                Self::truncate_chars(line, num_cols - button_len);
                line.push_str(CLOSE_BUTTON_TEXT);
            }
        }

        lines
    }

    /// Message type.
    #[inline]
    pub fn ty(&self) -> MessageType {
        self.ty
    }

    /// Message target.
    #[inline]
    pub fn target(&self) -> Option<&String> {
        self.target.as_ref()
    }

    /// Update the message target.
    #[inline]
    pub fn set_target(&mut self, target: String) {
        self.target = Some(target);
    }

    /// Right-pad text to fit a specific number of columns.
    #[inline]
    fn pad_text(mut text: String, num_cols: usize) -> String {
        let padding_len = num_cols.saturating_sub(text.chars().count());
        text.extend(vec![' '; padding_len]);
        text
    }

    fn truncate_chars(text: &mut String, max_chars: usize) {
        if let Some((byte_index, _)) = text.char_indices().nth(max_chars) {
            text.truncate(byte_index);
        }
    }
}

/// Storage for message bar.
#[derive(Debug, Default)]
pub struct MessageBuffer {
    messages: VecDeque<Message>,
}

impl MessageBuffer {
    /// Check if there are any messages queued.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Current message.
    #[inline]
    pub fn message(&self) -> Option<&Message> {
        self.messages.front()
    }

    /// Remove the currently visible message.
    #[inline]
    pub fn pop(&mut self) {
        // Remove the message itself.
        let msg = self.messages.pop_front();

        // Remove all duplicates.
        if let Some(msg) = msg {
            self.messages = self.messages.drain(..).filter(|m| m != &msg).collect();
        }
    }

    /// Remove all messages with a specific target.
    #[inline]
    pub fn remove_target(&mut self, target: &str) {
        self.messages = self
            .messages
            .drain(..)
            .filter(|m| m.target().map(String::as_str) != Some(target))
            .collect();
    }

    /// Add a new message to the queue.
    #[inline]
    pub fn push(&mut self, message: Message) {
        self.messages.push_back(message);
    }

    /// Check whether the message is already queued in the message bar.
    #[inline]
    pub fn is_queued(&self, message: &Message) -> bool {
        self.messages.contains(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::display::SizeInfo;

    #[test]
    fn bar_geometry_stays_inside_asymmetric_terminal_content() {
        let size =
            SizeInfo::new_fully_asymmetric(1000.0, 800.0, 10.0, 20.0, 144.0, 96.0, 48.0, 28.0);

        let rect = message_bar_rect(&size, false);

        assert_eq!(rect, MessageBarRect::new(144.0, 768.0, 760.0, 32.0));
    }

    #[test]
    fn close_geometry_matches_first_message_row_and_visible_button_columns() {
        let size =
            SizeInfo::new_fully_asymmetric(1000.0, 800.0, 10.0, 20.0, 144.0, 96.0, 48.0, 28.0);

        let rect = message_close_button_rect(&size, false).expect("visible close button");

        assert_eq!(rect, MessageBarRect::new(874.0, 768.0, 30.0, 20.0));
        assert!(rect.contains(903.0, 787.0));
        assert!(!rect.contains(873.0, 777.0));
        assert!(!rect.contains(200.0, 777.0));
    }

    #[test]
    fn close_geometry_tracks_search_row_offset() {
        let size =
            SizeInfo::new_fully_asymmetric(1000.0, 820.0, 10.0, 20.0, 144.0, 96.0, 48.0, 28.0);
        let without_search = message_close_button_rect(&size, false).expect("close button");
        let with_search = message_close_button_rect(&size, true).expect("close button");

        assert_eq!(with_search.y, without_search.y + size.cell_height());
    }

    #[test]
    fn appends_close_button() {
        let input = "a";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(7., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("a   [X]")]);
    }

    #[test]
    fn multiline_close_button_first_line() {
        let input = "fo\nbar";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(6., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("fo [X]"), String::from("bar   ")]);
    }

    #[test]
    fn splits_on_newline() {
        let input = "a\nb";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(6., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn splits_on_length() {
        let input = "foobar1";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(6., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn empty_with_shortterm() {
        let input = "foobar";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(6., 0., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn truncates_long_messages() {
        let input = "hahahahahahahahahahaha truncate this because it's too long for the term";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(22., (MIN_FREE_LINES + 2) as f32, 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(
            lines,
            vec![String::from("hahahahahahahahaha [X]"), String::from("[MESSAGE TRUNCATED]   ")]
        );
    }

    #[test]
    fn hide_button_when_too_narrow() {
        let input = "ha";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(2., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("ha")]);
    }

    #[test]
    fn hide_truncated_when_too_narrow() {
        let input = "hahahahahahahahaha";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(2., (MIN_FREE_LINES + 2) as f32, 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("ha"), String::from("ha")]);
    }

    #[test]
    fn add_newline_for_button() {
        let input = "test";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(5., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("t [X]"), String::from("est  ")]);
    }

    #[test]
    fn remove_target() {
        let mut message_buffer = MessageBuffer::default();
        for i in 0..10 {
            let mut msg = Message::new(i.to_string(), MessageType::Error);
            if i % 2 == 0 && i < 5 {
                msg.set_target("target".into());
            }
            message_buffer.push(msg);
        }

        message_buffer.remove_target("target");

        // Count number of messages.
        let mut num_messages = 0;
        while message_buffer.message().is_some() {
            num_messages += 1;
            message_buffer.pop();
        }

        assert_eq!(num_messages, 7);
    }

    #[test]
    fn pop() {
        let mut message_buffer = MessageBuffer::default();
        let one = Message::new(String::from("one"), MessageType::Error);
        message_buffer.push(one.clone());
        let two = Message::new(String::from("two"), MessageType::Warning);
        message_buffer.push(two.clone());

        assert_eq!(message_buffer.message(), Some(&one));

        message_buffer.pop();

        assert_eq!(message_buffer.message(), Some(&two));
    }

    #[test]
    fn wrap_on_words() {
        let input = "a\nbc defg";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(5., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(
            lines,
            vec![String::from("a [X]"), String::from("bc   "), String::from("defg ")]
        );
    }

    #[test]
    fn wrap_with_unicode() {
        let input = "ab\nc 👩d fgh";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(7., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(
            lines,
            vec![String::from("ab  [X]"), String::from("c 👩 d  "), String::from("fgh    ")]
        );
    }

    #[test]
    fn wraps_cjk_before_appending_close_button() {
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new("配置加载失败".into(), MessageType::Error));
        let size = SizeInfo::new(7., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(
            lines,
            vec![String::from("配   [X]"), String::from("置 加 载  "), String::from("失 败    ")]
        );
    }

    #[test]
    fn strip_whitespace_at_linebreak() {
        let input = "\n0 1 2 3";
        let mut message_buffer = MessageBuffer::default();
        message_buffer.push(Message::new(input.into(), MessageType::Error));
        let size = SizeInfo::new(3., 10., 1., 1., 0., 0., false);

        let lines = message_buffer.message().unwrap().text(&size);

        assert_eq!(lines, vec![String::from("[X]"), String::from("0 1"), String::from("2 3"),]);
    }

    #[test]
    fn remove_duplicates() {
        let mut message_buffer = MessageBuffer::default();
        for _ in 0..10 {
            let msg = Message::new(String::from("test"), MessageType::Error);
            message_buffer.push(msg);
        }
        message_buffer.push(Message::new(String::from("other"), MessageType::Error));
        message_buffer.push(Message::new(String::from("test"), MessageType::Warning));
        let _ = message_buffer.message();

        message_buffer.pop();

        // Count number of messages.
        let mut num_messages = 0;
        while message_buffer.message().is_some() {
            num_messages += 1;
            message_buffer.pop();
        }

        assert_eq!(num_messages, 2);
    }
}
