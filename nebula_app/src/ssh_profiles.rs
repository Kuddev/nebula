use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

const PROFILE_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthMode {
    #[default]
    Auto,
    Password,
    PublicKey,
    KeyboardInteractive,
}

impl<'de> Deserialize<'de> for SshAuthMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "password" => Self::Password,
            "public_key" => Self::PublicKey,
            // v0.5 移除了 Agent；旧配置回退到 Auto，避免升级后配置失效。
            "agent" => Self::Auto,
            "keyboard_interactive" => Self::KeyboardInteractive,
            _ => Self::Auto,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfileAuth {
    pub destination: String,
    #[serde(default)]
    pub auth: SshAuthMode,
    #[serde(default)]
    pub private_keys: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshProfiles {
    #[serde(default = "profile_version")]
    version: u32,
    #[serde(default)]
    profiles: Vec<SshProfileAuth>,
}

impl Default for SshProfiles {
    fn default() -> Self {
        Self { version: PROFILE_VERSION, profiles: Vec::new() }
    }
}

impl SshProfiles {
    pub fn load(path: &Path) -> io::Result<Self> {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        let mut profiles: Self = serde_json::from_slice(&data)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        for profile in &mut profiles.profiles {
            deduplicate_key_paths(&mut profile.private_keys);
        }
        profiles.version = PROFILE_VERSION;
        Ok(profiles)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temporary = path.with_extension(format!("nebula-tmp-{}", std::process::id()));
        std::fs::write(&temporary, data)?;
        let result = replace_file(&temporary, path);
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn for_destination(&self, destination: &str) -> SshProfileAuth {
        self.profiles
            .iter()
            .find(|profile| profile.destination == destination)
            .cloned()
            .unwrap_or_else(|| SshProfileAuth {
                destination: destination.to_owned(),
                auth: SshAuthMode::Auto,
                private_keys: Vec::new(),
            })
    }

    pub fn upsert(&mut self, mut profile: SshProfileAuth) {
        deduplicate_key_paths(&mut profile.private_keys);
        if let Some(existing) =
            self.profiles.iter_mut().find(|existing| existing.destination == profile.destination)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn remove(&mut self, destination: &str) {
        self.profiles.retain(|profile| profile.destination != destination);
    }

    pub fn rename(&mut self, old: &str, new: &str) {
        let Some(mut profile) =
            self.profiles.iter().find(|profile| profile.destination == old).cloned()
        else {
            return;
        };
        self.remove(old);
        profile.destination = new.to_owned();
        self.upsert(profile);
    }
}

fn profile_version() -> u32 {
    PROFILE_VERSION
}

fn deduplicate_key_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::<String>::new();
    paths.retain(|path| {
        let normalized = path.to_string_lossy().to_lowercase();
        if seen.contains(&normalized) {
            false
        } else {
            seen.push(normalized);
            true
        }
    });
}

fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{SshAuthMode, SshProfileAuth, SshProfiles};
    use std::path::PathBuf;

    #[test]
    fn missing_profile_defaults_to_auto_without_keys() {
        let profiles = SshProfiles::default();

        assert_eq!(profiles.for_destination("dev@example.com").auth, SshAuthMode::Auto);
        assert!(profiles.for_destination("dev@example.com").private_keys.is_empty());
    }

    #[test]
    fn profile_round_trip_preserves_mode_and_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh_profiles.json");
        let mut profiles = SshProfiles::default();
        profiles.upsert(SshProfileAuth {
            destination: "dev@example.com".to_owned(),
            auth: SshAuthMode::PublicKey,
            private_keys: vec![PathBuf::from(r"C:\Keys\first"), PathBuf::from(r"C:\Keys\second")],
        });
        profiles.save(&path).unwrap();

        let loaded = SshProfiles::load(&path).unwrap();
        let profile = loaded.for_destination("dev@example.com");
        assert_eq!(profile.auth, SshAuthMode::PublicKey);
        assert_eq!(
            profile.private_keys,
            vec![PathBuf::from(r"C:\Keys\first"), PathBuf::from(r"C:\Keys\second")]
        );
    }

    #[test]
    fn duplicate_windows_key_paths_are_removed_without_reordering() {
        let mut profiles = SshProfiles::default();
        profiles.upsert(SshProfileAuth {
            destination: "dev@example.com".to_owned(),
            auth: SshAuthMode::Auto,
            private_keys: vec![
                PathBuf::from(r"C:\Keys\id_ed25519"),
                PathBuf::from(r"c:\keys\ID_ED25519"),
                PathBuf::from(r"C:\Keys\id_rsa"),
            ],
        });

        assert_eq!(
            profiles.for_destination("dev@example.com").private_keys,
            vec![PathBuf::from(r"C:\Keys\id_ed25519"), PathBuf::from(r"C:\Keys\id_rsa")]
        );
    }

    #[test]
    fn renaming_profile_moves_auth_metadata() {
        let mut profiles = SshProfiles::default();
        profiles.upsert(SshProfileAuth {
            destination: "old@example.com".to_owned(),
            auth: SshAuthMode::PublicKey,
            private_keys: vec![PathBuf::from(r"C:\Keys\id_ed25519")],
        });

        profiles.rename("old@example.com", "new@example.com");

        assert_eq!(profiles.for_destination("old@example.com").auth, SshAuthMode::Auto);
        assert_eq!(profiles.for_destination("new@example.com").auth, SshAuthMode::PublicKey);
    }

    #[test]
    fn legacy_agent_mode_migrates_to_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh_profiles.json");
        std::fs::write(
            &path,
            r#"{"version":1,"profiles":[{"destination":"dev@example.com","auth":"agent","private_keys":[]}]}"#,
        )
        .unwrap();

        let loaded = SshProfiles::load(&path).unwrap();
        assert_eq!(loaded.for_destination("dev@example.com").auth, SshAuthMode::Auto);
    }

    #[test]
    fn unknown_auth_mode_falls_back_to_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh_profiles.json");
        std::fs::write(
            &path,
            r#"{"version":1,"profiles":[{"destination":"dev@example.com","auth":"future-mode","private_keys":[]}]}"#,
        )
        .unwrap();

        let loaded = SshProfiles::load(&path).unwrap();
        assert_eq!(loaded.for_destination("dev@example.com").auth, SshAuthMode::Auto);
    }
}
