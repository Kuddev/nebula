//! Native character data discarded by the GPUI keyboard adapter.

/// Recover Return's native character without changing keyboard composition state.
/// GPUI filters control text out of `key_char`, including Ctrl+Enter's LF.
pub(crate) fn enter_character(scan_code: u32, shift: bool, control: bool, alt: bool) -> u16 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        ToUnicode, VK_CONTROL, VK_MENU, VK_RETURN, VK_SHIFT,
    };

    let mut state = [0_u8; 256];
    state[VK_SHIFT as usize] = if shift { 0x80 } else { 0 };
    state[VK_CONTROL as usize] = if control { 0x80 } else { 0 };
    state[VK_MENU as usize] = if alt { 0x80 } else { 0 };
    let mut text = [0_u16; 2];
    // SAFETY: both buffers have the lengths required by ToUnicode. Flag 0x4
    // prevents dead-key state mutation; 0x1 selects menu translation semantics.
    let count = unsafe {
        ToUnicode(
            u32::from(VK_RETURN),
            scan_code,
            state.as_ptr(),
            text.as_mut_ptr(),
            text.len() as i32,
            0x5,
        )
    };
    // Zero is a valid absence of text. Never fabricate a partial UTF-16 unit.
    if count == 1 { text[0] } else { 0 }
}
