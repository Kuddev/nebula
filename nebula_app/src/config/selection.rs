use serde::Serialize;

use nebula_config_derive::ConfigDeserialize;
use nebula_terminal::term::SEMANTIC_ESCAPE_CHARS;

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub semantic_escape_chars: String,
    pub save_to_clipboard: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            semantic_escape_chars: SEMANTIC_ESCAPE_CHARS.to_owned(),
            // copy-on-select 的目标剪贴板。释放鼠标时选区会写入 Selection
            // (X11 primary) 剪贴板；Windows/macOS 没有 primary 选区，写进去等于
            // 无效。所以在这些平台默认同时写系统剪贴板，让"选中即复制"真正生效。
            // Linux/X11 保持 false：primary 选区已可用、中键可粘贴，避免每次选中
            // 都覆盖系统剪贴板（也符合 Linux 习惯）。可由配置覆盖。
            save_to_clipboard: cfg!(any(target_os = "windows", target_os = "macos")),
        }
    }
}
