use std::io;

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
pub fn load(target: &str) -> io::Result<Option<Vec<u8>>> {
    crate::ssh_credentials::windows_store::load_secret(target)
}

#[cfg(windows)]
pub fn store(target: &str, secret: &[u8]) -> io::Result<()> {
    store_with_username(target, "Nebula", secret)
}

#[cfg(windows)]
pub fn store_with_username(target: &str, username: &str, secret: &[u8]) -> io::Result<()> {
    crate::ssh_credentials::windows_store::save_secret(target, username, secret)
}

#[cfg(not(windows))]
pub fn store_with_username(target: &str, _username: &str, secret: &[u8]) -> io::Result<()> {
    store(target, secret)
}

#[cfg(windows)]
pub fn delete(target: &str) -> io::Result<()> {
    crate::ssh_credentials::windows_store::delete_secret(target)
}

#[cfg(target_os = "macos")]
pub fn load(target: &str) -> io::Result<Option<Vec<u8>>> {
    match security_framework::passwords::get_generic_password("Nebula", target) {
        Ok(secret) => Ok(Some(secret)),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(error) => Err(io::Error::other(format!("Keychain: {error}"))),
    }
}

#[cfg(target_os = "macos")]
pub fn store(target: &str, secret: &[u8]) -> io::Result<()> {
    security_framework::passwords::set_generic_password("Nebula", target, secret)
        .map_err(|error| io::Error::other(format!("Keychain: {error}")))
}

#[cfg(target_os = "macos")]
pub fn delete(target: &str) -> io::Result<()> {
    match security_framework::passwords::delete_generic_password("Nebula", target) {
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
    target: &str,
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
        command.arg("--label=Nebula SSH");
    }
    let mut child = command
        .args(["application", "Nebula", "target", target])
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
pub fn load(target: &str) -> io::Result<Option<Vec<u8>>> {
    if !can_store() {
        return Ok(None);
    }
    let output = invoke("lookup", target, None)?;
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
pub fn store(target: &str, secret: &[u8]) -> io::Result<()> {
    if !valid_secret(secret) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Credentials must be at most 4096 bytes and contain no NUL or line breaks",
        ));
    }
    if invoke("store", target, Some(secret))?.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("Secret Service could not save the credential"))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn delete(target: &str) -> io::Result<()> {
    let output = invoke("clear", target, None)?;
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

#[cfg(all(test, not(any(windows, target_os = "macos"))))]
mod tests {
    #[test]
    fn secret_tool_never_silently_truncates_a_credential() {
        assert!(super::valid_secret("口令 with spaces".as_bytes()));
        for secret in [&b"line\nbreak"[..], &b"carriage\rreturn"[..], &b"nul\0byte"[..]] {
            assert!(!super::valid_secret(secret));
        }
        assert!(!super::valid_secret(&vec![b'x'; 4097]));
    }
}
