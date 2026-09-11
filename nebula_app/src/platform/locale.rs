//! Native UI-language preferences, with POSIX environment fallback.

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

#[cfg(target_os = "macos")]
fn native_locale() -> Option<String> {
    use std::ffi::{CStr, c_char, c_void};
    type CFRef = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> CFRef;
        fn CFArrayGetCount(array: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFRef, index: isize) -> CFRef;
        fn CFStringGetLength(string: CFRef) -> isize;
        fn CFStringGetCString(string: CFRef, buffer: *mut c_char, size: isize, encoding: u32)
        -> u8;
        fn CFRelease(value: CFRef);
    }
    struct OwnedLanguages(CFRef);
    impl Drop for OwnedLanguages {
        fn drop(&mut self) {
            // SAFETY: this owns the non-null reference returned by Copy.
            unsafe { CFRelease(self.0) };
        }
    }
    // Finder-launched apps often have no LANG. Read the ordered UI-language
    // preferences directly, without a subprocess or preferences-file parsing.
    let array = unsafe { CFLocaleCopyPreferredLanguages() };
    if array.is_null() {
        return None;
    }
    let languages = OwnedLanguages(array);
    // SAFETY: Apple's API returns a retained CFArray of CFStrings. The strings
    // remain valid while OwnedLanguages lives; returned text is copied to Rust.
    unsafe {
        let count = CFArrayGetCount(languages.0).clamp(0, 64);
        for index in 0..count {
            let string = CFArrayGetValueAtIndex(languages.0, index);
            if string.is_null() {
                continue;
            }
            let length = CFStringGetLength(string);
            if !(1..=256).contains(&length) {
                continue;
            }
            let mut buffer = vec![0_u8; length as usize * 4 + 1];
            const UTF8: u32 = 0x0800_0100;
            if CFStringGetCString(string, buffer.as_mut_ptr().cast(), buffer.len() as isize, UTF8)
                == 0
            {
                continue;
            }
            let Ok(locale) = CStr::from_ptr(buffer.as_ptr().cast()).to_str() else {
                continue;
            };
            if nebula_settings::LanguagePref::from_locale(locale).is_some() {
                return Some(locale.to_owned());
            }
        }
    }
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_locale() -> Option<String> {
    None
}
