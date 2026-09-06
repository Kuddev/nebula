//! Native notification registration, delivery, and activation callbacks.
//! Attention policy and throttling belong to `crate::notify`.

pub(crate) type ToastActivation = std::sync::Arc<dyn Fn() + Send + Sync>;

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
        if let Err(error) = notify_rust::Notification::new()
            .appname(crate::brand::NAME)
            .summary(&title)
            .body(&body)
            .show()
        {
            log::warn!("System notification unavailable: {error}");
        }
    });
}

/// [`toast`], optionally wired for click-to-focus: activating the banner (or
/// its Action Center entry) surfaces `window` and, when a pane is named, its
/// tab. Uses the in-process WinRT Activated handler — no COM server, no
/// protocol registration. The one trade-off: clicks after Nebula exited do
/// nothing, which is exactly right (there is nothing left to focus).
#[cfg(windows)]
pub(crate) fn toast_clickable(title: &str, body: &str, activation: Option<ToastActivation>) {
    use tauri_winrt_notification::{IconCrop, Toast};

    // Attribute the toast to the Nebula AUMID so it reads "Nebula" instead of
    // "Windows PowerShell". One registry write, cached per process.
    win::ensure_aumid();

    let mut toast = Toast::new(win::AUMID)
        .title(title)
        .text1(body)
        .duration(tauri_winrt_notification::Duration::Short);
    // Belt and braces: besides the AUMID IconUri (which some Windows builds
    // cache stale), embed the logo per-toast as appLogoOverride so the banner
    // always carries the Nebula mark next to the message.
    if let Some(icon) = win::icon_path() {
        toast = toast.icon(&icon, IconCrop::Square, crate::brand::NAME);
    }
    if let Some(activation) = activation {
        toast = toast.on_activated(move |_action| {
            activation();
            Ok(())
        });
    }

    match toast.show() {
        Ok(()) => log::debug!("notify: toast shown"),
        Err(err) => log::warn!("notify: toast failed: {err}"),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn toast_clickable(title: &str, body: &str, activation: Option<ToastActivation>) {
    #[cfg(target_os = "macos")]
    {
        if objc2_foundation::NSBundle::mainBundle().bundleIdentifier().is_none() {
            log::warn!("System notifications require a registered Nebula application bundle");
            return;
        }
        crate::platform::notifications::prepare();
    }
    let mut notification = notify_rust::Notification::new();
    notification.appname(crate::brand::NAME).summary(title).body(body);
    if activation.is_some() {
        let language = crate::i18n::LanguagePreference::from(
            nebula_settings::RuntimeSettings::load().language,
        )
        .resolved();
        notification.action("default", language.text(crate::i18n::Message::CommonOpen));
    }
    match notification.show() {
        Ok(handle) => {
            if let Some(activation) = activation {
                let result = handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
                    if response.is_default_action()
                        || matches!(response, notify_rust::NotificationResponse::Action(action) if action == "default")
                    {
                        activation();
                    }
                });
                if let Err(error) = result {
                    log::warn!("notify: activation listener failed: {error}");
                }
            }
        },
        Err(error) => log::warn!("notify: toast failed: {error}"),
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) fn toast_clickable(title: &str, body: &str, _activation: Option<ToastActivation>) {
    crate::platform::notifications::show(title, body);
}

/// `nebula notify-test` entrypoint: run the full toast pipeline synchronously
/// (registration + show), printing per-step diagnostics to the console. Skips
/// the focus policy and throttle on purpose — the tester is looking at the
/// screen. Returns a process exit code.
#[cfg(windows)]
pub fn notify_test() -> i32 {
    println!("[1/2] Registering AUMID '{}' ...", win::AUMID);
    match win::register_aumid() {
        Ok(()) => {
            println!(r"      OK  (HKCU\Software\Classes\AppUserModelId\{})", win::AUMID);
            match win::icon_path() {
                Some(path) => println!("      icon: {}", path.display()),
                None => println!("      icon: unavailable (banner will have no logo)"),
            }
        },
        Err(err) => {
            eprintln!("      FAILED: {err}");
            return 1;
        },
    }

    println!("[2/2] Showing toast ...");
    let mut toast = tauri_winrt_notification::Toast::new(win::AUMID)
        .title(crate::brand::NAME)
        .text1("通知链路正常：nebula notify-test")
        .duration(tauri_winrt_notification::Duration::Short);
    if let Some(icon) = win::icon_path() {
        toast = toast.icon(&icon, tauri_winrt_notification::IconCrop::Square, crate::brand::NAME);
    }
    match toast.show() {
        Ok(()) => {
            println!("      OK  — a toast should be on screen now.");
            println!();
            println!("If nothing appeared, check Windows Settings > System > Notifications:");
            println!(
                "the global toggle, Do Not Disturb / Focus Assist, and the {} entry.",
                crate::brand::NAME
            );
            0
        },
        Err(err) => {
            eprintln!("      FAILED: {err}");
            1
        },
    }
}

#[cfg(not(windows))]
pub fn notify_test() -> i32 {
    println!("notify-test: system toasts are only implemented on Windows.");
    0
}

/// Windows-only: the Nebula AppUserModelID and its registration.
///
/// A WinRT toast must be attributed to an AUMID that Windows can resolve to
/// an app identity, or it silently refuses to show. For an unpackaged app the
/// documented lightweight route is a registry key —
/// `HKCU\Software\Classes\AppUserModelId\<AUMID>` with a `DisplayName` value
/// (what Microsoft's own ToastNotificationManagerCompat writes). Per-user, no
/// admin rights, no COM, no Start-menu shortcut, and idempotent: rewriting
/// the same value is a cheap no-op, so a broken key self-heals on next run.
#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};

    /// AppUserModelID for Nebula. Toast notifications fire under this identity
    /// so the system shows "Nebula" instead of "PowerShell" / "cmd.exe".
    pub const AUMID: &str = "com.nebula.terminal";

    /// Ensure the AUMID is registered. Best-effort, cached per process: the
    /// write itself is a few syscalls, there is just no point repeating them
    /// for every toast.
    pub fn ensure_aumid() {
        static REGISTERED: Mutex<Option<nebula_settings::AppIconName>> = Mutex::new(None);
        let variant = crate::app_icon::selected();
        let mut registered = REGISTERED.lock().unwrap_or_else(|error| error.into_inner());
        if *registered != Some(variant) {
            if let Err(err) = register_icon(variant) {
                log::warn!("notify: AUMID registration failed (toast may not appear): {err}");
            } else {
                *registered = Some(variant);
            }
        }
    }

    /// Write `DisplayName` + `IconUri` under the AUMID key. `RegSetKeyValueW`
    /// creates the missing subkey chain itself. The icon is best-effort: a
    /// failed write only costs the logo, never the toast.
    pub fn register_aumid() -> Result<(), String> {
        register_icon(crate::app_icon::selected())
    }

    fn register_icon(variant: nebula_settings::AppIconName) -> Result<(), String> {
        let subkey = format!(r"Software\Classes\AppUserModelId\{AUMID}");
        set_reg_sz(&subkey, "DisplayName", crate::brand::NAME)?;
        match ensure_icon_file(variant) {
            Some(icon) => set_reg_sz(&subkey, "IconUri", &icon.display().to_string())?,
            None => log::debug!("notify: toast icon not materialized; banner shows no logo"),
        }
        Ok(())
    }

    /// The materialized icon path, for diagnostics (`nebula notify-test`).
    pub fn icon_path() -> Option<PathBuf> {
        ensure_icon_file(crate::app_icon::selected())
    }

    fn ensure_icon_file(variant: nebula_settings::AppIconName) -> Option<PathBuf> {
        let bytes = crate::app_icon::png(variant, 256)?;
        let directory = crate::platform::dirs::data_dir();
        let path = directory.join(format!("toast_icon-{}.png", variant.settings_value()));
        let stale = std::fs::read(&path).ok().as_deref() != Some(bytes.as_ref());
        if stale {
            std::fs::create_dir_all(directory).ok()?;
            std::fs::write(&path, bytes).ok()?;
        }
        Some(path)
    }

    /// Set one REG_SZ value under HKCU\`subkey`, creating the key as needed.
    fn set_reg_sz(subkey: &str, name: &str, data: &str) -> Result<(), String> {
        let subkey_w = to_wide(subkey);
        let name_w = to_wide(name);
        let data_w = to_wide(data);
        let data_bytes = (data_w.len() * std::mem::size_of::<u16>()) as u32;

        // SAFETY: every pointer references a live, NUL-terminated UTF-16
        // buffer owned by this frame; RegSetKeyValueW copies the data before
        // returning and creates intermediate keys as needed.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                subkey_w.as_ptr(),
                name_w.as_ptr(),
                REG_SZ,
                data_w.as_ptr().cast(),
                data_bytes,
            )
        };

        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("RegSetKeyValueW({name}) failed with status {status}"))
        }
    }

    /// NUL-terminated UTF-16 for Win32 wide-string APIs.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
}
