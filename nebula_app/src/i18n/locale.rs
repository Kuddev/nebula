pub fn system_locale() -> Option<String> {
    native_locale().or_else(|| {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
    })
}

#[cfg(windows)]
fn native_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    let mut buffer = [0_u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 1).then(|| String::from_utf16(&buffer[..length as usize - 1]).ok()).flatten()
}

#[cfg(not(windows))]
fn native_locale() -> Option<String> {
    None
}
