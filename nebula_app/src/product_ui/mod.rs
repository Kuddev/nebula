//! GPUI 正式产品复用的 UI/终端领域模型。
//!
//! 旧壳过去把模型和 OpenGL 绘制都放在 `display` 下；本门面只接入 GPUI
//! 当前需要的无窗口、无 GL、无 crossfont 部分。后续模型继续从旧目录迁出时，
//! 对外路径保持稳定，正式产品也不会重新依赖旧渲染器。

#[path = "../display/background_color_model.rs"]
mod background_color_model;
#[path = "../display/color.rs"]
pub mod color;
#[path = "../display/command_completion.rs"]
mod command_completion;
#[path = "../display/command_palette.rs"]
pub(crate) mod command_palette;
#[path = "../display/content.rs"]
pub(crate) mod content;
#[path = "../display/context_menu_model.rs"]
pub(crate) mod context_menu;
#[path = "../display/document_model.rs"]
mod document_model;
#[path = "../display/file_dialog.rs"]
pub(crate) mod file_dialog;
#[path = "../display/file_operations.rs"]
mod file_operations;
#[path = "../display/hint.rs"]
pub(crate) mod hint;
#[path = "../display/image_viewer.rs"]
pub mod image_viewer;
#[path = "../display/input_state.rs"]
mod input_state;
#[path = "../display/keymap.rs"]
pub(crate) mod keymap;
#[path = "../display/network_proxy_model.rs"]
mod network_proxy_model;
#[path = "../display/program_identity.rs"]
mod program_identity;
#[path = "../display/side_panel/mod.rs"]
pub(crate) mod side_panel;
#[path = "../display/size_info.rs"]
mod size_info;
#[path = "../display/ssh_connect.rs"]
pub(crate) mod ssh_connect;
#[path = "../display/ssh_ui.rs"]
mod ssh_ui;
#[path = "../display/state.rs"]
pub(crate) mod state;
#[path = "../display/suggest_engine.rs"]
pub(crate) mod suggest_engine;
#[path = "../display/terminal_color.rs"]
pub(crate) mod terminal_color;
#[path = "../display/terminal_math.rs"]
pub(crate) mod terminal_math;
#[path = "../display/text_input.rs"]
mod text_input;
#[path = "../display/text_path_model.rs"]
mod text_path_model;
#[path = "../display/ui/toast_kind.rs"]
mod toast_kind;
pub mod ui;

pub use crate::i18n::{LanguagePreference, UiLanguage};
pub use program_identity::AiLogo;
pub use size_info::SizeInfo;
pub(crate) use ssh_ui::merge_ssh_hosts;
pub use ssh_ui::{
    auth_sections, join_destination_port, join_destination_user, push_private_key,
    split_destination_port, split_destination_user,
};
pub use state::{
    AcceptKey, AiSessionIdentity, CompletionStyle, NebulaCompletionItem, NebulaCompletionKind,
    NebulaConfirm, NebulaInlineImage, NebulaPaneState, NebulaShell, SplitDirection, SplitNav,
};
pub use suggest_engine::SuggestEnv;
pub use toast_kind::ToastKind;
pub use ui::theme::NebulaTheme;

pub mod markdown_view {
    pub use super::document_model::viewable_file;
}

pub use background_color_model::BgPickerPart;
pub(crate) use background_color_model::{BACKGROUND_SWATCHES, hsv_to_rgb, rgb_to_hsv};

pub(crate) use command_completion::{
    NEBULA_GHOST_MAX, extract_program, nebula_command_hint, nebula_command_hints,
    nebula_commands_handle, nebula_is_command_position, nebula_path_wants_directory,
};
pub(crate) use file_operations::send_to_recycle_bin;
pub(crate) use input_state::{
    nebula_clear_line, nebula_input_backspace, nebula_input_char, nebula_input_delete_word,
    nebula_input_text, nebula_shell_prompt_restored_from_raw_grid,
};
#[cfg(windows)]
pub(crate) use input_state::{nebula_input_from_raw_grid, nebula_prompt_line_from_raw_grid};
pub(crate) use network_proxy_model::{
    MANUAL_PROXY_PROTOCOL_OPTIONS, ManualProxyProtocol, ProxyTestStatus, manual_proxy_parts,
    manual_proxy_value,
};
pub(crate) use program_identity::{ai_logo_for_program, prepare_ai_logo_texture, program_icon};
pub(crate) use text_path_model::{fit_tail, strip_file_scheme, truncate_tab_label};

pub(crate) const NEBULA_UNFOCUSED_SPLIT_DIM: f32 = 0.30;
pub(crate) const UI_CORNER_RADIUS_LOGICAL: f32 = ui::tokens::radius::OVERLAY;

pub(crate) fn nebula_data_dir() -> std::path::PathBuf {
    crate::platform::dirs::data_dir().to_path_buf()
}

pub(crate) use crate::logging::debug_log as nebula_debug_log;
