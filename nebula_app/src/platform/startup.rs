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
