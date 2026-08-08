//! User terminal profiles discovered from a directory or saved explicitly.

use std::io;
#[cfg(windows)]
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const STORE_VERSION: u32 = 1;
const STORE_FILE: &str = "terminal_profiles.json";

/// A launchable terminal profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalProfile {
    /// Stable profile identity derived from the shell type and command path.
    pub id: String,
    pub name: String,
    /// Absolute executable path.
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Stable shell integration id (`pwsh`, `powershell`, `bash`, `nu`, `cmd`).
    pub shell_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProfileStore {
    version: u32,
    #[serde(default)]
    profiles: Vec<TerminalProfile>,
}

/// Versioned collection stored in `terminal_profiles.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TerminalProfiles {
    profiles: Vec<TerminalProfile>,
}

impl TerminalProfiles {
    pub(crate) fn load() -> io::Result<Self> {
        Self::load_from(&store_path())
    }

    pub(crate) fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        let store: ProfileStore = serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if store.version != STORE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported terminal profile version {}", store.version),
            ));
        }
        for profile in &store.profiles {
            validate(profile)?;
        }
        // 配置文件可能来自另一种 CPU 架构，或其安装目录后来被删除。加载时
        // 直接隐藏这些条目，避免菜单仍允许点击并最终得到 ERROR_BAD_EXE_FORMAT。
        let profiles = store
            .profiles
            .into_iter()
            .filter(|profile| command_is_launchable(&profile.command))
            .collect();
        Ok(Self { profiles })
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        self.save_to(&store_path())
    }

    pub(crate) fn save_to(&self, path: &Path) -> io::Result<()> {
        for profile in &self.profiles {
            validate(profile)?;
        }
        let Some(_lock) = crate::atomic_file::try_lock(path)? else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "terminal profile store is locked by another process",
            ));
        };
        let store = ProfileStore { version: STORE_VERSION, profiles: self.profiles.clone() };
        let bytes = serde_json::to_vec_pretty(&store).map_err(io::Error::other)?;
        crate::atomic_file::write(path, &bytes)
    }

    pub(crate) fn profiles(&self) -> &[TerminalProfile] {
        &self.profiles
    }

    pub(crate) fn as_config_profiles(&self) -> Vec<crate::config::ui_config::Profile> {
        self.profiles
            .iter()
            .map(|profile| crate::config::ui_config::Profile {
                name: profile.name.clone(),
                command: profile.command.to_string_lossy().into_owned(),
                args: profile.args.clone(),
                cwd: profile.cwd.clone(),
                shell_id: Some(profile.shell_id.clone()),
                terminal_profile_id: Some(profile.id.clone()),
            })
            .collect()
    }

    /// Replace the profile for the same command, or append a new command.
    pub(crate) fn upsert(&mut self, profile: TerminalProfile) -> io::Result<()> {
        validate(&profile)?;
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|existing| commands_equal(&existing.command, &profile.command))
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
        Ok(())
    }
}

/// Scan only `directory` and its immediate `bin` child for known Windows shells.
pub(crate) fn scan_directory(directory: &Path) -> io::Result<Vec<TerminalProfile>> {
    let directory = directory.canonicalize()?;
    if !directory.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "scan path is not a directory"));
    }

    let roots = [directory.clone(), directory.join("bin")];
    let shells = [
        ("pwsh.exe", "PowerShell 7", "pwsh", &["-NoLogo"][..]),
        ("powershell.exe", "Windows PowerShell", "powershell", &["-NoLogo"][..]),
        ("bash.exe", "Git Bash", "bash", &["--login", "-i"][..]),
        ("nu.exe", "Nushell", "nu", &[][..]),
        ("cmd.exe", "Command Prompt", "cmd", &[][..]),
    ];
    let mut profiles = Vec::new();

    for (file_name, name, shell_id, args) in shells {
        for root in &roots {
            let candidate = root.join(file_name);
            if !candidate.is_file() {
                continue;
            }
            let command = candidate.canonicalize()?;
            if !command_is_launchable(&command) {
                continue;
            }
            if profiles
                .iter()
                .any(|profile: &TerminalProfile| commands_equal(&profile.command, &command))
            {
                continue;
            }
            profiles.push(TerminalProfile {
                id: stable_id(shell_id, &command),
                name: name.to_owned(),
                command,
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                cwd: None,
                shell_id: shell_id.to_owned(),
            });
        }
    }
    Ok(profiles)
}

pub(crate) fn store_path() -> PathBuf {
    crate::platform::dirs::data_dir().join(STORE_FILE)
}

fn stable_id(shell_id: &str, command: &Path) -> String {
    let command = command.to_string_lossy();
    #[cfg(windows)]
    let command = command.to_ascii_lowercase();
    let digest = Sha256::digest(command.as_bytes());
    let suffix = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("{shell_id}-{suffix}")
}

fn commands_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn command_is_launchable(command: &Path) -> bool {
    if !command.is_file() {
        return false;
    }

    #[cfg(windows)]
    {
        pe_machine(command).is_ok_and(machine_is_supported)
    }
    #[cfg(not(windows))]
    {
        true
    }
}

#[cfg(windows)]
fn pe_machine(command: &Path) -> io::Result<u16> {
    let mut file = std::fs::File::open(command)?;
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing DOS executable header"));
    }

    // PE 的 Machine 字段不在固定文件偏移：先从 DOS 头 0x3c 读取 e_lfanew，
    // 再核对 PE 签名，避免把安装目录名里的 x64/arm64 当成可信架构信息。
    let pe_offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().unwrap()) as u64;
    file.seek(SeekFrom::Start(pe_offset))?;
    let mut header = [0_u8; 6];
    file.read_exact(&mut header)?;
    if &header[..4] != b"PE\0\0" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing PE signature"));
    }
    Ok(u16::from_le_bytes([header[4], header[5]]))
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn machine_is_supported(machine: u16) -> bool {
    matches!(machine, 0x014c | 0x8664)
}

#[cfg(all(windows, target_arch = "x86"))]
fn machine_is_supported(machine: u16) -> bool {
    machine == 0x014c
}

#[cfg(all(windows, target_arch = "aarch64"))]
fn machine_is_supported(machine: u16) -> bool {
    matches!(machine, 0x014c | 0x8664 | 0xaa64)
}

#[cfg(all(
    windows,
    not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64"))
))]
fn machine_is_supported(_machine: u16) -> bool {
    true
}

fn validate(profile: &TerminalProfile) -> io::Result<()> {
    if profile.id.trim().is_empty()
        || profile.name.trim().is_empty()
        || profile.shell_id.trim().is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal profile id, name, and shell_id must not be empty",
        ));
    }
    if !profile.command.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal profile command must be absolute",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{TerminalProfile, TerminalProfiles, scan_directory};

    #[cfg(windows)]
    fn write_pe(path: &std::path::Path, machine: u16) {
        let mut bytes = vec![0_u8; 0x86];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(0x80_u32).to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    const TEST_MACHINE: u16 = 0x8664;
    #[cfg(all(windows, target_arch = "x86"))]
    const TEST_MACHINE: u16 = 0x014c;
    #[cfg(all(windows, target_arch = "aarch64"))]
    const TEST_MACHINE: u16 = 0xaa64;

    #[test]
    fn scans_selected_directory_and_bin_in_stable_order() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("bin")).unwrap();
        #[cfg(windows)]
        {
            write_pe(&temp.path().join("cmd.exe"), TEST_MACHINE);
            write_pe(&temp.path().join("bin").join("pwsh.exe"), TEST_MACHINE);
        }
        #[cfg(not(windows))]
        {
            std::fs::write(temp.path().join("cmd.exe"), b"").unwrap();
            std::fs::write(temp.path().join("bin").join("pwsh.exe"), b"").unwrap();
        }
        std::fs::write(temp.path().join("ignored.exe"), b"").unwrap();

        let profiles = scan_directory(temp.path()).unwrap();
        assert_eq!(
            profiles.iter().map(|profile| profile.shell_id.as_str()).collect::<Vec<_>>(),
            ["pwsh", "cmd"]
        );
        assert!(profiles.iter().all(|profile| profile.command.is_absolute()));
        assert_eq!(profiles[0].name, "PowerShell 7");
        assert_eq!(profiles[0].args, ["-NoLogo"]);
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn scan_skips_arm64_shells_in_an_x64_build() {
        let temp = tempfile::tempdir().unwrap();
        write_pe(&temp.path().join("pwsh.exe"), 0xaa64);

        assert!(scan_directory(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn json_store_roundtrips_and_reports_malformed_data() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profiles.json");
        let profile = profile(temp.path().join("pwsh.exe"), "first");
        #[cfg(windows)]
        write_pe(&profile.command, TEST_MACHINE);
        #[cfg(not(windows))]
        std::fs::write(&profile.command, b"").unwrap();
        let mut profiles = TerminalProfiles::default();
        profiles.upsert(profile.clone()).unwrap();
        profiles.save_to(&path).unwrap();

        assert_eq!(TerminalProfiles::load_from(&path).unwrap().profiles(), [profile]);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(
            TerminalProfiles::load_from(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn upsert_deduplicates_commands() {
        let temp = tempfile::tempdir().unwrap();
        let command = temp.path().join("pwsh.exe");
        let mut profiles = TerminalProfiles::default();
        profiles.upsert(profile(command.clone(), "first")).unwrap();

        #[cfg(windows)]
        let duplicate = std::path::PathBuf::from(command.to_string_lossy().to_ascii_uppercase());
        #[cfg(not(windows))]
        let duplicate = command;
        profiles.upsert(profile(duplicate, "replacement")).unwrap();

        assert_eq!(profiles.profiles().len(), 1);
        assert_eq!(profiles.profiles()[0].name, "replacement");
    }

    fn profile(command: std::path::PathBuf, name: &str) -> TerminalProfile {
        TerminalProfile {
            id: "pwsh-test".into(),
            name: name.into(),
            command,
            args: vec!["-NoLogo".into()],
            cwd: None,
            shell_id: "pwsh".into(),
        }
    }
}
