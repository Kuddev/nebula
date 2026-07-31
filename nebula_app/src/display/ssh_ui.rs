//! SSH editor and reversible-deletion state types.
//!
//! Keeping these out of `display::mod` makes the security-sensitive lifetime
//! rule explicit: a pending deletion owns credential cleanup and only Undo may
//! disarm it.

use super::ui::text_field::TextCursor;
use crate::ssh_profiles::SshAuthMode;
use std::path::PathBuf;

/// How long a destructive SSH action stays reversible in the in-app bar.
pub const SSH_DELETE_UNDO_DURATION: std::time::Duration = std::time::Duration::from_secs(8);

/// 密码框的掩码字符。渲染和点击命中共用它：命中要按**看得见的那串**算列，
/// 否则密码里有一个全角字符，光标就会落到与手指差好几格的地方。
pub const PASSWORD_MASK: &str = "•";

pub fn auth_sections(mode: SshAuthMode) -> (bool, bool) {
    match mode {
        SshAuthMode::Auto => (true, true),
        SshAuthMode::Password => (true, false),
        SshAuthMode::PublicKey => (false, true),
        SshAuthMode::KeyboardInteractive => (false, false),
    }
}

pub fn push_private_key(keys: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    let normalized = path.to_string_lossy();
    if keys.iter().any(|existing| existing.to_string_lossy().eq_ignore_ascii_case(&normalized)) {
        false
    } else {
        keys.push(path);
        true
    }
}

/// 把一个地址串拆成「不含端口的地址」和「端口」，喂给编辑器的两个输入框。
///
/// 端口是 22 或根本没写时返回空串——空的端口框读作"用默认值"，比预填一个
/// `22` 好：预填会让人以为这是自己设的，删掉反而不知道会发生什么。
///
/// 端口位置写了非数字（`host:abc`）时整串原样留在地址里，交给地址校验去报
/// 错。悄悄吞掉那一截，用户会看着自己粘进来的东西凭空少了一半。
pub fn split_destination_port(destination: &str) -> (String, String) {
    let trimmed = destination.trim();
    let (scheme, rest) = match trimmed.strip_prefix("ssh://") {
        Some(rest) => ("ssh://", rest),
        None => ("", trimmed),
    };
    let (user, host_port) = match rest.rsplit_once('@') {
        Some((user, host_port)) => (Some(user), host_port),
        None => (None, rest),
    };

    let (host, port) = if let Some(after) = host_port.strip_prefix('[') {
        // IPv6 字面量：`[::1]:22`，中括号里的冒号属于地址。
        match after.split_once(']') {
            Some((host, suffix)) => {
                (format!("[{host}]"), suffix.strip_prefix(':').unwrap_or_default())
            },
            None => (host_port.to_owned(), ""),
        }
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        // 没加中括号的裸 IPv6 不能按 host:port 拆——最后一段是地址的一部分。
        if host.contains(':') { (host_port.to_owned(), "") } else { (host.to_owned(), port) }
    } else {
        (host_port.to_owned(), "")
    };

    if !port.is_empty() && port.parse::<u16>().is_err() {
        return (trimmed.to_owned(), String::new());
    }
    let mut address = String::from(scheme);
    if let Some(user) = user {
        address.push_str(user);
        address.push('@');
    }
    address.push_str(&host);
    (address, if port == "22" { String::new() } else { port.to_owned() })
}

/// 拼回存盘用的地址串。地址框里自带的端口优先——用户刚粘进去的那个才是意图，
/// 端口框里的可能是上一台主机的残留。
pub fn join_destination_port(address: &str, port: &str) -> String {
    let (base, embedded) = split_destination_port(address);
    let port = if embedded.is_empty() { port.trim() } else { embedded.as_str() };
    // 默认端口不写进地址：给每台主机都缀上 `:22` 只是噪音，而且会让同一台
    // 机器出现 `host` 和 `host:22` 两条记录、各自一份凭据。
    if port.is_empty() || port == "22" { base } else { format!("{base}:{port}") }
}

#[derive(Debug)]
pub(super) struct SshDeleteUndo {
    pub(super) host: String,
    pub(super) saved_index: Option<usize>,
    pub(super) pinned_index: Option<usize>,
    pub(super) was_hidden: bool,
    pub(super) from_config: bool,
    pub(super) started_at: std::time::Instant,
    pub(super) delete_credential_on_drop: bool,
}

impl Drop for SshDeleteUndo {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.delete_credential_on_drop {
            let _ = crate::ssh_credentials::forget_password(&self.host);
            let path = super::nebula_data_dir().join("ssh_profiles.json");
            if let Ok(mut profiles) = crate::ssh_profiles::SshProfiles::load(&path) {
                profiles.remove(&self.host);
                let _ = profiles.save(&path);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshEditorField {
    Destination,
    /// 端口。数据上仍然属于 destination（`host:2222`），拆成独立字段只是
    /// 因为把端口埋在地址串里，用户既看不见默认值也不知道能改。
    Port,
    /// 列表里显示的名字。空则回落到地址本身。
    Label,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshEditorHit {
    None,
    Close,
    Destination,
    Port,
    Label,
    Password,
    PasswordToggle,
    Auth(SshAuthMode),
    AddPrivateKey,
    RemovePrivateKey(usize),
    SaveToggleBox,
    SaveToggleLabel,
    Test,
    TestStatus,
    Primary,
    Cancel,
}

/// 页脚「测试连接」的四态状态小字（稿一）。结果只对发起时的草稿有效——
/// 任何字段一改就回 [`Idle`]，绝不让旧结果背书新配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SshTestState {
    #[default]
    Idle,
    Running {
        request_id: u64,
    },
    Ok {
        elapsed_ms: u64,
    },
    Failed {
        summary: String,
    },
}

#[derive(Debug, Clone)]
pub struct SshHostEditor {
    /// Destination before editing, when this modal was opened from a row.
    pub original_destination: Option<String>,
    pub destination: String,
    /// 端口输入框的内容。空 = 用默认 22，保存时不写进地址串。
    pub port: String,
    /// 列表显示名。空 = 显示地址。
    pub label: String,
    pub password: String,
    pub save_password: bool,
    pub show_password: bool,
    pub auth: SshAuthMode,
    pub private_keys: Vec<PathBuf>,
    pub field: SshEditorField,
    pub focus: crate::ux::FocusIndex,
    pub error: Option<String>,
    /// 「测试连接」当前状态；见 [`SshTestState`]。
    pub test: SshTestState,
    pub(super) destination_cursor: TextCursor,
    pub(super) port_cursor: TextCursor,
    pub(super) label_cursor: TextCursor,
    pub(super) password_cursor: TextCursor,
}

impl SshHostEditor {
    /// 某个字段的文本与光标。插入、退格、全选、点击定位对四个字段的处理完全
    /// 一样，集中到这里之后，再加字段只需要补这一个 match，而不是把同样的分支
    /// 在五个方法里各抄一遍——那种写法漏一处就是"某个框打不出字"。
    pub(super) fn field_mut(&mut self, field: SshEditorField) -> (&mut String, &mut TextCursor) {
        match field {
            SshEditorField::Destination => (&mut self.destination, &mut self.destination_cursor),
            SshEditorField::Port => (&mut self.port, &mut self.port_cursor),
            SshEditorField::Label => (&mut self.label, &mut self.label_cursor),
            SshEditorField::Password => (&mut self.password, &mut self.password_cursor),
        }
    }

    /// 只读版本，给渲染与命中用。
    pub(super) fn field_view(&self, field: SshEditorField) -> (&str, &TextCursor) {
        match field {
            SshEditorField::Destination => (&self.destination, &self.destination_cursor),
            SshEditorField::Port => (&self.port, &self.port_cursor),
            SshEditorField::Label => (&self.label, &self.label_cursor),
            SshEditorField::Password => (&self.password, &self.password_cursor),
        }
    }

    pub(super) fn active_field(&mut self) -> (&mut String, &mut TextCursor) {
        self.field_mut(self.field)
    }

    pub(super) fn active_text(&self) -> &str {
        self.field_view(self.field).0
    }

    /// 清掉全部四个字段的选区，但保留各自的光标位置。切字段时用：选区是
    /// "此刻正在操作的一段"，跨字段留着它只会让人以为那边还选着东西。
    pub(super) fn clear_selections(&mut self) {
        self.destination_cursor.clear_selection();
        self.port_cursor.clear_selection();
        self.label_cursor.clear_selection();
        self.password_cursor.clear_selection();
    }

    /// 距文字起点 `offset_x` 像素处落在第几个字符的缝隙上。
    pub(super) fn index_at(&self, field: SshEditorField, offset_x: f32, cell_w: f32) -> usize {
        let text = self.field_view(field).0;
        if field == SshEditorField::Password && !self.show_password {
            let masked = PASSWORD_MASK.repeat(text.chars().count());
            return super::ui::text_field::index_at(&masked, offset_x, cell_w);
        }
        super::ui::text_field::index_at(text, offset_x, cell_w)
    }

    /// 实际画在框里的那串字，供渲染和光标定位共用。空串时返回空——占位文案
    /// 不参与光标定位，否则光标会跑到"留空则连接时询问"的末尾去。
    pub(super) fn display_text(&self, field: SshEditorField) -> String {
        let text = self.field_view(field).0;
        if field == SshEditorField::Password && !self.show_password {
            return PASSWORD_MASK.repeat(text.chars().count());
        }
        text.to_owned()
    }
}

/// 输入框里文字的排布，由渲染写入、命中读取。
///
/// 光标要落在字符缝隙上，就必须和文字用同一个起点和同一个格宽。两边各算一次
/// 的写法在全角字符和居中字段上必然漂移，而漂移几像素恰恰是最难被报告、也
/// 最伤手感的那类问题——所以这里让绘制把算好的值交出来，命中直接用。
#[derive(Debug, Clone, Copy, Default)]
pub struct SshFieldMetrics {
    pub destination_x: f32,
    pub port_x: f32,
    pub label_x: f32,
    pub password_x: f32,
    /// 一列的推进宽度（物理像素）。
    pub cell_w: f32,
}

impl SshFieldMetrics {
    pub fn origin(&self, field: SshEditorField) -> f32 {
        match field {
            SshEditorField::Destination => self.destination_x,
            SshEditorField::Port => self.port_x,
            SshEditorField::Label => self.label_x,
            SshEditorField::Password => self.password_x,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SshEditorRects {
    pub close: (f32, f32, f32, f32),
    pub destination: (f32, f32, f32, f32),
    pub port: (f32, f32, f32, f32),
    pub label: (f32, f32, f32, f32),
    pub password: (f32, f32, f32, f32),
    pub password_toggle: (f32, f32, f32, f32),
    pub auth: [(SshAuthMode, (f32, f32, f32, f32)); 4],
    pub add_private_key: (f32, f32, f32, f32),
    pub private_key_rows: Vec<(usize, (f32, f32, f32, f32))>,
    pub save_checkbox: (f32, f32, f32, f32),
    pub save_toggle: (f32, f32, f32, f32),
    pub test: (f32, f32, f32, f32),
    pub test_status: (f32, f32, f32, f32),
    pub primary: (f32, f32, f32, f32),
    pub cancel: (f32, f32, f32, f32),
    pub metrics: SshFieldMetrics,
}

impl SshEditorRects {
    /// 输入框矩形 + 文字起点，供点击定位用。返回 `None` 表示这个 hit 不是
    /// 可编辑字段。
    pub fn field_of(&self, hit: SshEditorHit) -> Option<(SshEditorField, (f32, f32, f32, f32))> {
        match hit {
            SshEditorHit::Destination => Some((SshEditorField::Destination, self.destination)),
            SshEditorHit::Port => Some((SshEditorField::Port, self.port)),
            SshEditorHit::Label => Some((SshEditorField::Label, self.label)),
            SshEditorHit::Password => Some((SshEditorField::Password, self.password)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{auth_sections, join_destination_port, push_private_key, split_destination_port};
    use crate::ssh_profiles::SshAuthMode;
    use std::path::PathBuf;

    #[test]
    fn auth_modes_show_the_expected_editor_sections() {
        assert_eq!(auth_sections(SshAuthMode::Auto), (true, true));
        assert_eq!(auth_sections(SshAuthMode::Password), (true, false));
        assert_eq!(auth_sections(SshAuthMode::PublicKey), (false, true));
        assert_eq!(auth_sections(SshAuthMode::KeyboardInteractive), (false, false));
    }

    #[test]
    fn private_key_list_keeps_order_and_deduplicates_windows_paths() {
        let mut keys = Vec::new();
        assert!(push_private_key(&mut keys, PathBuf::from(r"C:\Keys\first")));
        assert!(!push_private_key(&mut keys, PathBuf::from(r"c:\keys\FIRST")));
        assert!(push_private_key(&mut keys, PathBuf::from(r"C:\Keys\second")));
        assert_eq!(keys, vec![PathBuf::from(r"C:\Keys\first"), PathBuf::from(r"C:\Keys\second")]);
    }

    #[test]
    fn destination_splits_into_address_and_port() {
        assert_eq!(
            split_destination_port("dev@example.com:2222"),
            ("dev@example.com".to_owned(), "2222".to_owned())
        );
        // 默认端口不回填到端口框。
        assert_eq!(
            split_destination_port("dev@example.com:22"),
            ("dev@example.com".to_owned(), String::new())
        );
        assert_eq!(
            split_destination_port("ssh://dev@example.com:2222"),
            ("ssh://dev@example.com".to_owned(), "2222".to_owned())
        );
        assert_eq!(
            split_destination_port("example.com"),
            ("example.com".to_owned(), String::new())
        );
    }

    #[test]
    fn destination_split_keeps_ipv6_and_bad_ports_intact() {
        assert_eq!(
            split_destination_port("dev@[fe80::1]:2222"),
            ("dev@[fe80::1]".to_owned(), "2222".to_owned())
        );
        // 裸 IPv6 的末段是地址，不是端口。
        assert_eq!(split_destination_port("fe80::1"), ("fe80::1".to_owned(), String::new()));
        // 端口位置不是数字：整串留在地址里让校验去报错，不能悄悄截掉。
        assert_eq!(
            split_destination_port("dev@example.com:abc"),
            ("dev@example.com:abc".to_owned(), String::new())
        );
    }

    #[test]
    fn destination_join_prefers_the_port_typed_into_the_address() {
        assert_eq!(join_destination_port("dev@example.com", "2222"), "dev@example.com:2222");
        assert_eq!(join_destination_port("dev@example.com", ""), "dev@example.com");
        assert_eq!(join_destination_port("dev@example.com", "22"), "dev@example.com");
        // 地址框里粘进来的端口压过端口框里的旧值。
        assert_eq!(join_destination_port("dev@example.com:2022", "2222"), "dev@example.com:2022");
    }
}
