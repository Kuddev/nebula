const ASKPASS_FLAG: &str = "NEBULA_SSH_ASKPASS";
const DESTINATION_ENV: &str = "NEBULA_SSH_DESTINATION";
const ATTEMPT_ENV: &str = "NEBULA_SSH_ASKPASS_ATTEMPT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskpassAction {
    UseStored,
    ForgetAndPrompt,
    Prompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskpassPromptKind {
    Password,
    ConfirmHost,
    Other,
}

pub fn classify_prompt(prompt: &str) -> AskpassPromptKind {
    let lower = prompt.to_ascii_lowercase();
    if lower.contains("yes/no")
        || lower.contains("authenticity")
        || lower.contains("continue connecting")
    {
        AskpassPromptKind::ConfirmHost
    } else if lower.contains("password") {
        AskpassPromptKind::Password
    } else {
        AskpassPromptKind::Other
    }
}

pub fn credential_target(destination: &str) -> String {
    format!("Nebula/SSH/{destination}")
}

pub fn private_key_credential_target(private_key: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    use std::fmt::Write as _;

    let digest = Sha512::digest(private_key);
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(fingerprint, "{byte:02x}");
    }
    format!("Nebula/SSH/KeyPassphrase/{fingerprint}")
}

pub fn is_askpass_env(mut get: impl FnMut(&str) -> Option<String>) -> bool {
    get(ASKPASS_FLAG).as_deref() == Some("1")
        && get(DESTINATION_ENV).is_some_and(|destination| !destination.trim().is_empty())
}

pub fn askpass_action(has_stored_password: bool, attempt_marker_exists: bool) -> AskpassAction {
    match (has_stored_password, attempt_marker_exists) {
        (true, false) => AskpassAction::UseStored,
        (true, true) => AskpassAction::ForgetAndPrompt,
        (false, _) => AskpassAction::Prompt,
    }
}

#[cfg(windows)]
mod windows_store {
    use super::{AskpassAction, askpass_action, classify_prompt, credential_target};
    use std::ffi::c_void;
    use std::io::{self, Write};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CREDUI_FLAGS_ALWAYS_SHOW_UI,
        CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_GENERIC_CREDENTIALS,
        CREDUI_FLAGS_SHOW_SAVE_CHECK_BOX, CredDeleteW, CredFree, CredReadW,
        CredUIPromptForCredentialsW, CredWriteW,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn load_secret(target_name: &str) -> io::Result<Option<Vec<u8>>> {
        let target = wide(target_name);
        let mut raw = null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if ok == 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or_default();
            if code == ERROR_NOT_FOUND as i32 {
                return Ok(None);
            }
            return Err(io::Error::last_os_error());
        }
        let result = unsafe {
            let cred = &*raw;
            if cred.CredentialBlob.is_null() || cred.CredentialBlobSize == 0 {
                None
            } else {
                Some(
                    std::slice::from_raw_parts(
                        cred.CredentialBlob,
                        cred.CredentialBlobSize as usize,
                    )
                    .to_vec(),
                )
            }
        };
        unsafe { CredFree(raw as *const c_void) };
        Ok(result)
    }

    pub fn save_secret(target_name: &str, username: &str, secret: &[u8]) -> io::Result<()> {
        if secret.len() > u32::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "password too long"));
        }
        let mut target = wide(target_name);
        let mut user = wide(username);
        let mut blob = secret.to_vec();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: null_mut(),
            LastWritten: windows_sys::Win32::Foundation::FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: null_mut(),
            UserName: user.as_mut_ptr(),
        };
        let ok = unsafe { CredWriteW(&cred, 0) };
        blob.fill(0);
        target.fill(0);
        user.fill(0);
        if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    pub fn delete_secret(target_name: &str) -> io::Result<()> {
        let target = wide(target_name);
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or_default();
            if code == ERROR_NOT_FOUND as i32 {
                return Ok(());
            }
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn load_password(destination: &str) -> io::Result<Option<Vec<u8>>> {
        load_secret(&credential_target(destination))
    }

    pub fn save_password(destination: &str, password: &[u8]) -> io::Result<()> {
        save_secret(
            &credential_target(destination),
            username_hint(destination).as_deref().unwrap_or(""),
            password,
        )
    }

    pub fn delete_password(destination: &str) -> io::Result<()> {
        delete_secret(&credential_target(destination))
    }

    pub fn prompt_password(
        destination: &str,
        initial: Option<&[u8]>,
        allow_save: bool,
    ) -> io::Result<Option<(Vec<u8>, bool)>> {
        let mut target = wide(&credential_target(destination));
        let mut username = vec![0u16; 512];
        if let Some(hint) = username_hint(destination) {
            for (slot, value) in username.iter_mut().zip(hint.encode_utf16()) {
                *slot = value;
            }
        }
        let mut password = vec![0u16; 512];
        if let Some(initial) = initial {
            let text = String::from_utf8_lossy(initial);
            for (slot, value) in password.iter_mut().zip(text.encode_utf16()) {
                *slot = value;
            }
        }
        let mut save = 0;
        let mut flags = CREDUI_FLAGS_GENERIC_CREDENTIALS
            | CREDUI_FLAGS_ALWAYS_SHOW_UI
            | CREDUI_FLAGS_DO_NOT_PERSIST;
        if allow_save {
            flags |= CREDUI_FLAGS_SHOW_SAVE_CHECK_BOX;
        }
        let result = unsafe {
            CredUIPromptForCredentialsW(
                null(),
                target.as_ptr(),
                null(),
                0,
                username.as_mut_ptr(),
                username.len() as u32,
                password.as_mut_ptr(),
                password.len() as u32,
                &mut save,
                flags,
            )
        };
        target.fill(0);
        username.fill(0);
        if result == ERROR_CANCELLED {
            password.fill(0);
            return Ok(None);
        }
        if result != ERROR_SUCCESS {
            password.fill(0);
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let end = password.iter().position(|x| *x == 0).unwrap_or(password.len());
        let bytes = String::from_utf16(&password[..end])
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "password is not valid UTF-16")
            })?
            .into_bytes();
        password.fill(0);
        Ok(Some((bytes, save != 0)))
    }

    pub fn run_askpass() -> i32 {
        let prompt = std::env::args().nth(1).unwrap_or_default();
        if classify_prompt(&prompt) == super::AskpassPromptKind::ConfirmHost {
            return confirm_host(&prompt);
        }
        let destination = match std::env::var(super::DESTINATION_ENV) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return 1,
        };
        let marker = std::env::var(super::ATTEMPT_ENV).ok();
        let marker_exists = marker.as_deref().is_some_and(|path| Path::new(path).exists());
        let stored = load_password(&destination).ok().flatten();
        match askpass_action(stored.is_some(), marker_exists) {
            AskpassAction::UseStored => {
                if let Some(path) = marker.as_deref() {
                    let _ = std::fs::write(path, b"1");
                }
                if let Some(mut password) = stored {
                    let result = write_askpass_response(&password);
                    password.fill(0);
                    return result.map(|_| 0).unwrap_or(1);
                }
                1
            },
            AskpassAction::ForgetAndPrompt => {
                let _ = delete_password(&destination);
                prompt_and_return(&destination, None, marker.as_deref(), true)
            },
            AskpassAction::Prompt => prompt_and_return(
                &destination,
                stored.as_deref(),
                marker.as_deref(),
                classify_prompt(&prompt) == super::AskpassPromptKind::Password,
            ),
        }
    }

    fn prompt_and_return(
        destination: &str,
        initial: Option<&[u8]>,
        marker: Option<&str>,
        allow_save: bool,
    ) -> i32 {
        if let Some(path) = marker {
            let _ = std::fs::write(path, b"1");
        }
        match prompt_password(destination, initial, allow_save) {
            Ok(Some((mut password, save))) => {
                if save {
                    let _ = save_password(destination, &password);
                }
                let result = write_askpass_response(&password);
                password.fill(0);
                result.map(|_| 0).unwrap_or(1)
            },
            _ => 1,
        }
    }

    fn username_hint(destination: &str) -> Option<String> {
        let value = destination.strip_prefix("ssh://").unwrap_or(destination);
        let user = value.split('@').next()?;
        (!user.is_empty() && value.contains('@')).then(|| user.to_owned())
    }

    fn write_askpass_response(password: &[u8]) -> io::Result<()> {
        let mut out = io::stdout();
        out.write_all(password)?;
        out.write_all(b"\n")?;
        out.flush()
    }

    fn confirm_host(prompt: &str) -> i32 {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IDYES, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
        };
        let text = wide(prompt);
        let title = wide("Nebula SSH");
        let answer = unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND,
            )
        };
        if answer == IDYES {
            let mut out = io::stdout();
            out.write_all(b"yes\n").and_then(|_| out.flush()).map(|_| 0).unwrap_or(1)
        } else {
            1
        }
    }
}

#[cfg(windows)]
pub fn run_askpass_from_env() -> Option<i32> {
    is_askpass_env(|key| std::env::var(key).ok()).then(windows_store::run_askpass)
}

#[cfg(windows)]
pub fn store_password(destination: &str, password: &[u8]) -> std::io::Result<()> {
    windows_store::save_password(destination, password)
}

#[cfg(windows)]
pub fn load_stored_password(destination: &str) -> std::io::Result<Option<Vec<u8>>> {
    windows_store::load_password(destination)
}

#[cfg(windows)]
pub fn store_private_key_passphrase(private_key: &[u8], passphrase: &[u8]) -> std::io::Result<()> {
    windows_store::save_secret(
        &private_key_credential_target(private_key),
        "private-key",
        passphrase,
    )
}

#[cfg(windows)]
pub fn load_private_key_passphrase(private_key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
    windows_store::load_secret(&private_key_credential_target(private_key))
}

#[cfg(windows)]
pub fn forget_private_key_passphrase(private_key: &[u8]) -> std::io::Result<()> {
    windows_store::delete_secret(&private_key_credential_target(private_key))
}

#[cfg(windows)]
pub fn prompt_password(
    destination: &str,
    initial: Option<&[u8]>,
    allow_save: bool,
) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    windows_store::prompt_password(destination, initial, allow_save)
}

#[cfg(not(windows))]
pub fn load_stored_password(_destination: &str) -> std::io::Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(windows)]
pub fn forget_password(destination: &str) -> std::io::Result<()> {
    windows_store::delete_password(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_target_is_stable_and_namespaced() {
        assert_eq!(credential_target("root@example.com"), "Nebula/SSH/root@example.com");
        assert_eq!(
            credential_target("ssh://admin@example.com:2222"),
            "Nebula/SSH/ssh://admin@example.com:2222"
        );
    }

    #[test]
    fn askpass_mode_requires_destination_and_flag() {
        let mut env = std::collections::HashMap::new();
        env.insert("NEBULA_SSH_ASKPASS".to_owned(), "1".to_owned());
        assert!(!is_askpass_env(|key| env.get(key).cloned()));

        env.insert("NEBULA_SSH_DESTINATION".to_owned(), "root@example.com".to_owned());
        assert!(is_askpass_env(|key| env.get(key).cloned()));
    }

    #[test]
    fn password_bytes_are_not_part_of_credential_target() {
        let target = credential_target("alice@host");
        assert!(!target.contains("correct horse battery staple"));
    }

    #[test]
    fn private_key_passphrase_target_depends_on_contents_not_path() {
        let first = private_key_credential_target(b"encrypted-private-key");
        let moved = private_key_credential_target(b"encrypted-private-key");
        let changed = private_key_credential_target(b"different-private-key");

        assert_eq!(first, moved);
        assert_ne!(first, changed);
        assert!(first.starts_with("Nebula/SSH/KeyPassphrase/"));
        assert!(!first.contains("Users"));
        assert!(!first.contains(".ssh"));
    }

    #[test]
    fn first_saved_password_is_used_without_prompting() {
        assert_eq!(askpass_action(true, false), AskpassAction::UseStored);
    }

    #[test]
    fn repeated_request_for_saved_password_forgets_and_prompts() {
        assert_eq!(askpass_action(true, true), AskpassAction::ForgetAndPrompt);
    }

    #[test]
    fn missing_password_prompts() {
        assert_eq!(askpass_action(false, false), AskpassAction::Prompt);
        assert_eq!(askpass_action(false, true), AskpassAction::Prompt);
    }

    #[test]
    fn askpass_prompt_kind_distinguishes_password_and_host_confirmation() {
        assert_eq!(classify_prompt("root@example.com's password:"), AskpassPromptKind::Password);
        assert_eq!(
            classify_prompt("Are you sure you want to continue connecting (yes/no/[fingerprint])?"),
            AskpassPromptKind::ConfirmHost
        );
        assert_eq!(classify_prompt("Verification code:"), AskpassPromptKind::Other);
    }

    #[cfg(windows)]
    #[test]
    fn credential_manager_round_trip() {
        let destination = format!("nebula-test-{}@example.invalid", std::process::id());
        let password = b"not-a-real-password";
        let _ = windows_store::delete_password(&destination);
        windows_store::save_password(&destination, password).unwrap();
        assert_eq!(
            windows_store::load_password(&destination).unwrap().as_deref(),
            Some(password.as_slice())
        );
        windows_store::delete_password(&destination).unwrap();
        assert_eq!(windows_store::load_password(&destination).unwrap(), None);
    }

    #[cfg(windows)]
    #[test]
    fn private_key_passphrase_uses_its_own_credential_namespace() {
        let key = format!("test-private-key-{}", std::process::id()).into_bytes();
        let passphrase = b"not-a-real-passphrase";
        let _ = forget_private_key_passphrase(&key);

        store_private_key_passphrase(&key, passphrase).unwrap();
        assert_eq!(
            load_private_key_passphrase(&key).unwrap().as_deref(),
            Some(passphrase.as_slice())
        );
        forget_private_key_passphrase(&key).unwrap();
        assert_eq!(load_private_key_passphrase(&key).unwrap(), None);
    }
}
