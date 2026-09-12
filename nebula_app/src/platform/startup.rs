/// Surface a pre-logger failure for a GUI launch; the caller also returns it on stderr.
pub(crate) fn report_error(error: &dyn std::fmt::Display, gui_launch: bool) {
    #[cfg(windows)]
    crate::panic::report_startup_error(error, gui_launch);
    #[cfg(not(windows))]
    let _ = (error, gui_launch);
}

pub fn prepare_gui() {
    #[cfg(target_os = "macos")]
    {
        crate::macos::locale::set_locale_environment();
        crate::macos::disable_autofill();
        if std::env::current_dir().ok().as_deref() == Some(std::path::Path::new("/")) {
            if let Some(home) = home::home_dir() {
                if let Err(error) = std::env::set_current_dir(home) {
                    eprintln!("Could not use the home directory: {error}");
                }
            }
        }
    }
    super::notifications::prepare();
}

pub(crate) fn start_hidden(settings: &nebula_settings::RuntimeSettings) -> bool {
    super::CAPABILITIES.hide_window_on_close && settings.silent_start && settings.tray
}

/// The installer and Settings manage the same per-user Startup shortcut.
pub(crate) fn launch_at_login() -> bool {
    #[cfg(windows)]
    return startup_shortcut().is_ok_and(|path| path.is_file());
    #[cfg(not(windows))]
    false
}

pub(crate) fn set_launch_at_login(enabled: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize, IPersistFile,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{HSTRING, Interface, w};

        let path = startup_shortcut()?;
        if !enabled {
            return match std::fs::remove_file(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            };
        }
        let executable = std::env::current_exe()?;
        // SAFETY: COM calls stay on this thread; interfaces are dropped before
        // balancing the successful initialization, including on save errors.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(std::io::Error::other)?;
            let result = (|| -> windows::core::Result<()> {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(executable.as_os_str()))?;
                link.SetArguments(w!("--gpui"))?;
                if let Some(home) = super::dirs::home_dir() {
                    link.SetWorkingDirectory(&HSTRING::from(home.as_os_str()))?;
                }
                link.cast::<IPersistFile>()?.Save(&HSTRING::from(path.as_os_str()), true)
            })();
            CoUninitialize();
            result.map_err(std::io::Error::other)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

#[cfg(windows)]
fn startup_shortcut() -> std::io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Startup, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // SAFETY: the known-folder buffer is copied before its COM allocation is freed.
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None)
            .map_err(std::io::Error::other)?;
        let directory = std::ffi::OsString::from_wide(path.as_wide());
        CoTaskMemFree(Some(path.0.cast()));
        Ok(std::path::PathBuf::from(directory).join("Pebrel.lnk"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn silent_start_requires_a_tray_and_native_window_hiding() {
        use nebula_settings::{RawSettings, RuntimeSettings};

        for (text, expected) in [
            ("", false),
            ("silent_start=1\ntray=0", false),
            ("silent_start=0\ntray=1", false),
            ("silent_start=1\ntray=1", super::super::CAPABILITIES.hide_window_on_close),
        ] {
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(text));
            assert_eq!(super::start_hidden(&settings), expected);
        }
    }
}
