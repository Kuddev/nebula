use std::borrow::Cow;
use std::io;

struct CredentialIdentity<'a> {
    service: &'static str,
    target: Cow<'a, str>,
}

impl<'a> CredentialIdentity<'a> {
    fn current(target: &'a str) -> Self {
        let target = match target.strip_prefix("Nebula/") {
            Some(suffix) => Cow::Owned(format!("Pebrel/{suffix}")),
            None => Cow::Borrowed(target),
        };
        Self { service: "Pebrel", target }
    }

    fn legacy(target: &'a str) -> Option<Self> {
        let suffix = target.strip_prefix("Pebrel/").or_else(|| target.strip_prefix("Nebula/"));
        #[cfg(windows)]
        if suffix.is_none() {
            return None;
        }
        let target = match suffix {
            Some(suffix) => Cow::Owned(format!("Nebula/{suffix}")),
            None => Cow::Borrowed(target),
        };
        Some(Self { service: "Nebula", target })
    }
}

pub fn load(target: &str) -> io::Result<Option<Vec<u8>>> {
    load_with(target, load_at)
}

fn load_with(
    target: &str,
    mut load: impl FnMut(&CredentialIdentity<'_>) -> io::Result<Option<Vec<u8>>>,
) -> io::Result<Option<Vec<u8>>> {
    if let Some(secret) = load(&CredentialIdentity::current(target))? {
        return Ok(Some(secret));
    }
    match CredentialIdentity::legacy(target) {
        Some(identity) => load(&identity),
        None => Ok(None),
    }
}

pub fn store(target: &str, secret: &[u8]) -> io::Result<()> {
    store_with_username(target, "Pebrel", secret)
}

pub fn store_with_username(target: &str, username: &str, secret: &[u8]) -> io::Result<()> {
    store_at(&CredentialIdentity::current(target), username, secret)
}

pub fn delete(target: &str) -> io::Result<()> {
    delete_with(target, delete_at)
}

fn delete_with(
    target: &str,
    mut delete: impl FnMut(&CredentialIdentity<'_>) -> io::Result<()>,
) -> io::Result<()> {
    // Remove the fallback first: a failed legacy deletion must not expose it
    // by removing the newer credential that currently takes precedence.
    if let Some(identity) = CredentialIdentity::legacy(target) {
        delete(&identity)?;
    }
    delete(&CredentialIdentity::current(target))
}

pub fn can_store() -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    {
        true
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        secret_tool().is_some()
    }
}

#[cfg(windows)]
fn load_at(identity: &CredentialIdentity<'_>) -> io::Result<Option<Vec<u8>>> {
    crate::ssh_credentials::windows_store::load_secret(&identity.target)
}

#[cfg(windows)]
fn store_at(identity: &CredentialIdentity<'_>, username: &str, secret: &[u8]) -> io::Result<()> {
    crate::ssh_credentials::windows_store::save_secret(&identity.target, username, secret)
}

#[cfg(windows)]
fn delete_at(identity: &CredentialIdentity<'_>) -> io::Result<()> {
    crate::ssh_credentials::windows_store::delete_secret(&identity.target)
}

#[cfg(target_os = "macos")]
fn load_at(identity: &CredentialIdentity<'_>) -> io::Result<Option<Vec<u8>>> {
    match security_framework::passwords::get_generic_password(identity.service, &identity.target) {
        Ok(secret) => Ok(Some(secret)),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(error) => Err(io::Error::other(format!("Keychain: {error}"))),
    }
}

#[cfg(target_os = "macos")]
fn store_at(identity: &CredentialIdentity<'_>, _username: &str, secret: &[u8]) -> io::Result<()> {
    security_framework::passwords::set_generic_password(identity.service, &identity.target, secret)
        .map_err(|error| io::Error::other(format!("Keychain: {error}")))
}

#[cfg(target_os = "macos")]
fn delete_at(identity: &CredentialIdentity<'_>) -> io::Result<()> {
    match security_framework::passwords::delete_generic_password(identity.service, &identity.target)
    {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(io::Error::other(format!("Keychain: {error}"))),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn secret_tool() -> Option<std::path::PathBuf> {
    ["/usr/bin/secret-tool", "/bin/secret-tool"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn invoke(
    operation: &str,
    identity: &CredentialIdentity<'_>,
    secret: Option<&[u8]>,
) -> io::Result<std::process::Output> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let program = secret_tool().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Install libsecret-tools and enable a Secret Service keyring to save credentials",
        )
    })?;
    let mut command = Command::new(program);
    command.arg(operation);
    if operation == "store" {
        command.arg("--label=Pebrel SSH");
    }
    let mut child = command
        .args(["application", identity.service, "target", &identity.target])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(secret) = secret {
        if let Err(error) = child.stdin.take().unwrap().write_all(secret) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    } else {
        drop(child.stdin.take());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Secret Service did not respond"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn load_at(identity: &CredentialIdentity<'_>) -> io::Result<Option<Vec<u8>>> {
    if !can_store() {
        return Ok(None);
    }
    let output = invoke("lookup", identity, None)?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut value = output.stdout;
    if value.last() == Some(&b'\n') {
        value.pop();
    }
    Ok(Some(value))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn store_at(identity: &CredentialIdentity<'_>, _username: &str, secret: &[u8]) -> io::Result<()> {
    if !valid_secret(secret) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Credentials must be at most 4096 bytes and contain no NUL or line breaks",
        ));
    }
    if invoke("store", identity, Some(secret))?.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("Secret Service could not save the credential"))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn delete_at(identity: &CredentialIdentity<'_>) -> io::Result<()> {
    let output = invoke("clear", identity, None)?;
    if output.status.success() || output.status.code() == Some(1) {
        Ok(())
    } else {
        Err(io::Error::other("Secret Service could not remove the credential"))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn valid_secret(secret: &[u8]) -> bool {
    secret.len() <= 4096 && !secret.iter().any(|byte| matches!(*byte, 0 | b'\n' | b'\r'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn key(identity: &CredentialIdentity<'_>) -> (String, String) {
        (identity.service.to_owned(), identity.target.to_string())
    }

    #[test]
    fn legacy_targets_write_to_pebrel_and_read_the_existing_secret() {
        for target in ["Pebrel/SSH/user@host", "Nebula/SSH/user@host"] {
            let current = CredentialIdentity::current(target);
            assert_eq!(key(&current), ("Pebrel".into(), "Pebrel/SSH/user@host".into()));
            let stored = HashMap::from([(
                ("Nebula".into(), "Nebula/SSH/user@host".into()),
                b"old password".to_vec(),
            )]);
            assert_eq!(
                load_with(target, |identity| Ok(stored.get(&key(identity)).cloned())).unwrap(),
                Some(b"old password".to_vec())
            );
        }
    }

    #[test]
    fn replacing_and_forgetting_a_secret_cannot_restore_the_legacy_value() {
        let target = "Pebrel/AI/provider";
        let mut stored = HashMap::from([
            (("Nebula".into(), "Nebula/AI/provider".into()), b"old key".to_vec()),
            (("Pebrel".into(), "Pebrel/AI/provider".into()), b"new key".to_vec()),
        ]);
        assert_eq!(
            load_with(target, |identity| Ok(stored.get(&key(identity)).cloned())).unwrap(),
            Some(b"new key".to_vec())
        );
        delete_with(target, |identity| {
            stored.remove(&key(identity));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            load_with(target, |identity| Ok(stored.get(&key(identity)).cloned())).unwrap(),
            None
        );
        assert!(stored.is_empty());
    }

    #[test]
    fn a_failed_legacy_deletion_preserves_the_current_secret() {
        let mut removed = Vec::new();
        let error = delete_with("Pebrel/SSH/user@host", |identity| {
            removed.push(key(identity));
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(removed, [("Nebula".into(), "Nebula/SSH/user@host".into())]);
    }

    #[test]
    fn a_read_failure_does_not_fall_back_to_an_older_secret() {
        let mut reads = Vec::new();
        let error = load_with("Pebrel/SSH/user@host", |identity| {
            reads.push(key(identity));
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(reads, [("Pebrel".into(), "Pebrel/SSH/user@host".into())]);
    }

    #[cfg(not(windows))]
    #[test]
    fn unbranded_targets_keep_their_name_and_read_the_old_service() {
        let target = "user-owned-service-key";
        let current = CredentialIdentity::current(target);
        let legacy = CredentialIdentity::legacy(target).unwrap();
        assert_eq!(current.target, target);
        assert_eq!(legacy.target, target);
        assert_eq!(current.service, "Pebrel");
        assert_eq!(legacy.service, "Nebula");
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn secret_tool_never_silently_truncates_a_credential() {
        assert!(super::valid_secret("口令 with spaces".as_bytes()));
        for secret in [&b"line\nbreak"[..], &b"carriage\rreturn"[..], &b"nul\0byte"[..]] {
            assert!(!super::valid_secret(secret));
        }
        assert!(!super::valid_secret(&vec![b'x'; 4097]));
    }
}
