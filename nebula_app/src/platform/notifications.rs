#[cfg(target_os = "macos")]
static MACOS_READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn prepare() {
    #[cfg(target_os = "macos")]
    {
        MACOS_READY.get_or_init(|| {
            let Some(identifier) = objc2_foundation::NSBundle::mainBundle().bundleIdentifier()
            else {
                return false;
            };
            notify_rust::set_application(&identifier.to_string()).is_ok()
        });
    }
}

#[cfg(not(windows))]
pub fn show(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    if !MACOS_READY.get().copied().unwrap_or(false) {
        log::warn!("System notifications require a registered Nebula application bundle");
        return;
    }
    let title = title.to_owned();
    let body = body.to_owned();
    let _ = std::thread::Builder::new().name("nebula-notify".into()).spawn(move || {
        if let Err(error) =
            notify_rust::Notification::new().appname("Nebula").summary(&title).body(&body).show()
        {
            log::warn!("System notification unavailable: {error}");
        }
    });
}
