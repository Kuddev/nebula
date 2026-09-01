//! Password-protected backups of the files Nebula owns in its data directory.
//!
//! This module deliberately does not discover files by walking the user's home
//! directory. The allowlist below is the security boundary for both export and
//! import. In particular, AI session stores, environment variables, SSH key
//! material, and credential stores are never read or written here.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"NEBUBAK1";
const ARCHIVE_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupCategory {
    Appearance,
    Config,
    Ssh,
    Sync,
    Assistant,
    Session,
    DirectoryHistory,
    CommandHistory,
    Fonts,
}

// `pub`（而非 pub(crate)）：作为 `EventType::NebulaBackupRemote` 的字段随
// 公有枚举可达；bin crate 里只为消除 private-interfaces 告警。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupSelection {
    pub appearance: bool,
    pub config: bool,
    pub ssh: bool,
    pub sync: bool,
    pub assistant: bool,
    pub session: bool,
    pub directory_history: bool,
    pub command_history: bool,
    pub fonts: bool,
}

impl Default for BackupSelection {
    fn default() -> Self {
        Self {
            appearance: true,
            config: false,
            ssh: false,
            sync: false,
            assistant: false,
            session: false,
            directory_history: false,
            command_history: false,
            fonts: false,
        }
    }
}

impl BackupSelection {
    pub(crate) fn is_empty(self) -> bool {
        self.categories().next().is_none()
    }

    fn categories(self) -> impl Iterator<Item = BackupCategory> {
        [
            (self.appearance, BackupCategory::Appearance),
            (self.config, BackupCategory::Config),
            (self.ssh, BackupCategory::Ssh),
            (self.sync, BackupCategory::Sync),
            (self.assistant, BackupCategory::Assistant),
            (self.session, BackupCategory::Session),
            (self.directory_history, BackupCategory::DirectoryHistory),
            (self.command_history, BackupCategory::CommandHistory),
            (self.fonts, BackupCategory::Fonts),
        ]
        .into_iter()
        .filter_map(|(selected, category)| selected.then_some(category))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackupEntry {
    pub category: BackupCategory,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackupManifest {
    pub version: u32,
    pub categories: Vec<BackupCategory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackupArchive {
    pub manifest: BackupManifest,
    pub entries: Vec<BackupEntry>,
}

fn validate_passphrase(passphrase: &str) -> Result<(), String> {
    (passphrase.chars().count() >= 8)
        .then_some(())
        .ok_or_else(|| "backup passphrase must be at least 8 characters".to_owned())
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    let params = argon2::Params::new(19_456, 2, 1, Some(KEY_LEN))
        .map_err(|error| format!("argon2 parameters: {error}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| format!("argon2: {error}"))?;
    Ok(key)
}

/// Serialize and encrypt an archive as `NEBUBAK1 | salt | nonce | ciphertext`.
pub(crate) fn seal(archive: &BackupArchive, passphrase: &str) -> Result<Vec<u8>, String> {
    validate_passphrase(passphrase)?;
    if archive.manifest.version != ARCHIVE_VERSION {
        return Err("unsupported backup archive version".to_owned());
    }
    validate_archive(archive)?;
    let plaintext =
        serde_json::to_vec(archive).map_err(|error| format!("serialize backup: {error}"))?;
    let mut salt = [0; SALT_LEN];
    let mut nonce = [0; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|error| format!("random salt: {error}"))?;
    getrandom::fill(&mut nonce).map_err(|error| format!("random nonce: {error}"))?;
    let key = derive_key(passphrase, &salt)?;
    let ciphertext = Aes256Gcm::new((&key).into())
        .encrypt(
            &Nonce::try_from(&nonce[..]).map_err(|_| "invalid backup nonce".to_owned())?,
            Payload { msg: &plaintext, aad: MAGIC },
        )
        .map_err(|_| "backup encryption failed".to_owned())?;
    let mut output = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&salt);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

/// Authenticate, decrypt, deserialize, and validate a complete archive.
pub(crate) fn open(packet: &[u8], passphrase: &str) -> Result<BackupArchive, String> {
    validate_passphrase(passphrase)?;
    let header_len = MAGIC.len() + SALT_LEN + NONCE_LEN;
    if packet.len() <= header_len || packet.get(..MAGIC.len()) != Some(MAGIC) {
        return Err("not a Nebula encrypted backup or unsupported version".to_owned());
    }
    let salt = &packet[MAGIC.len()..MAGIC.len() + SALT_LEN];
    let nonce = &packet[MAGIC.len() + SALT_LEN..header_len];
    let key = derive_key(passphrase, salt)?;
    let plaintext = Aes256Gcm::new((&key).into())
        .decrypt(
            &Nonce::try_from(nonce).map_err(|_| "invalid backup nonce".to_owned())?,
            Payload { msg: &packet[header_len..], aad: MAGIC },
        )
        .map_err(|_| "backup authentication failed".to_owned())?;
    let archive: BackupArchive = serde_json::from_slice(&plaintext)
        .map_err(|error| format!("invalid backup archive: {error}"))?;
    validate_archive(&archive)?;
    Ok(archive)
}

pub(crate) fn collect(selection: BackupSelection) -> Result<BackupArchive, String> {
    collect_from(crate::platform::dirs::data_dir(), selection)
}

fn collect_from(root: &Path, selection: BackupSelection) -> Result<BackupArchive, String> {
    let mut entries = Vec::new();
    for category in selection.categories() {
        match category {
            BackupCategory::Appearance => {
                add_file(root, category, "nebula_settings.txt", &mut entries, filter_settings)
            },
            BackupCategory::Config => {
                add_file(root, category, "nebula.lua", &mut entries, identity);
                add_file(root, category, "terminal_profiles.json", &mut entries, identity);
            },
            BackupCategory::Ssh => add_sanitized_ssh(root, &mut entries)?,
            BackupCategory::Sync => {
                add_file(root, category, "nebula_sync.txt", &mut entries, identity)
            },
            BackupCategory::Assistant => {
                add_file(root, category, "nebula_assistant.txt", &mut entries, identity)
            },
            BackupCategory::Session => {
                add_file(root, category, "session.json", &mut entries, identity)
            },
            BackupCategory::DirectoryHistory => {
                add_file(root, category, "directory_history.json", &mut entries, identity)
            },
            BackupCategory::CommandHistory => {
                for file_name in crate::nebula_history::history_file_names() {
                    add_file(root, category, file_name, &mut entries, identity);
                }
            },
            BackupCategory::Fonts => add_fonts(root, &mut entries)?,
        }
    }
    Ok(BackupArchive {
        manifest: BackupManifest {
            version: ARCHIVE_VERSION,
            categories: selection.categories().collect(),
        },
        entries,
    })
}

fn identity(bytes: Vec<u8>) -> Vec<u8> {
    bytes
}

fn filter_settings(bytes: Vec<u8>) -> Vec<u8> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            // 尾部换行会被 `split` 解析成空记录；丢弃它可保证跨平台归档内容稳定。
            if line.is_empty() {
                return false;
            }
            let key = line.split(|byte| *byte == b'=').next().unwrap_or_default();
            let key = String::from_utf8_lossy(key).trim().to_ascii_lowercase();
            !key.starts_with("ssh_proxy_")
                && !matches!(key.as_str(), "pinned_hosts" | "saved_hosts" | "hidden_hosts")
        })
        .fold(Vec::new(), |mut output, line| {
            if !output.is_empty() {
                output.push(b'\n');
            }
            output.extend_from_slice(line);
            output
        })
}

fn add_file(
    root: &Path,
    category: BackupCategory,
    name: &str,
    entries: &mut Vec<BackupEntry>,
    transform: fn(Vec<u8>) -> Vec<u8>,
) {
    let path = root.join(name);
    if let Ok(bytes) = fs::read(path) {
        entries.push(BackupEntry { category, name: name.to_owned(), bytes: transform(bytes) });
    }
}

fn add_sanitized_ssh(root: &Path, entries: &mut Vec<BackupEntry>) -> Result<(), String> {
    let path = root.join("ssh_profiles.json");
    let Ok(bytes) = fs::read(path) else { return Ok(()) };
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid SSH profile file: {error}"))?;
    if let Some(profiles) = value.get_mut("profiles").and_then(serde_json::Value::as_array_mut) {
        for profile in profiles {
            if let Some(object) = profile.as_object_mut() {
                object.remove("private_keys");
                object.remove("proxy");
            }
        }
    }
    entries.push(BackupEntry {
        category: BackupCategory::Ssh,
        name: "ssh_profiles.json".to_owned(),
        bytes: serde_json::to_vec(&value)
            .map_err(|error| format!("serialize SSH profiles: {error}"))?,
    });
    Ok(())
}

fn add_fonts(root: &Path, entries: &mut Vec<BackupEntry>) -> Result<(), String> {
    let fonts = root.join("fonts");
    if !fonts.exists() {
        return Ok(());
    }
    let mut stack = vec![fonts.clone()];
    while let Some(directory) = stack.pop() {
        for item in fs::read_dir(directory).map_err(|error| format!("read fonts: {error}"))? {
            let path = item.map_err(|error| format!("read fonts entry: {error}"))?.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| format!("font metadata: {error}"))?;
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let name = path
                    .strip_prefix(root)
                    .map_err(|_| "font path escaped data directory".to_owned())?;
                entries.push(BackupEntry {
                    category: BackupCategory::Fonts,
                    name: name.to_string_lossy().replace('\\', "/"),
                    bytes: fs::read(path).map_err(|error| format!("read font: {error}"))?,
                });
            }
        }
    }
    Ok(())
}

/// Authenticate the complete encrypted packet before writing any entry.
pub(crate) fn restore(packet: &[u8], passphrase: &str) -> Result<(), String> {
    let archive = open(packet, passphrase)?;
    restore_to(crate::platform::dirs::data_dir(), &archive)
}

fn restore_to(root: &Path, archive: &BackupArchive) -> Result<(), String> {
    validate_archive(archive)?;
    let paths = archive
        .entries
        .iter()
        .map(|entry| restore_path(root, &entry.name))
        .collect::<Result<Vec<_>, _>>()?;
    for (entry, path) in archive.entries.iter().zip(paths) {
        crate::atomic_file::write(&path, &entry.bytes)
            .map_err(|error| format!("restore {}: {error}", entry.name))?;
    }
    Ok(())
}

fn safe_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe backup path: {name:?}"));
    }
    Ok(root.join(path))
}

fn restore_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = safe_path(root, name)?;
    let mut current = root.to_owned();
    for component in Path::new(name).components() {
        let Component::Normal(component) = component else { unreachable!() };
        current.push(component);
        if current == path {
            break;
        }
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("backup destination has unsafe parent: {}", current.display()));
            }
        }
    }
    Ok(path)
}

fn validate_archive(archive: &BackupArchive) -> Result<(), String> {
    if archive.manifest.version != ARCHIVE_VERSION {
        return Err("unsupported backup archive version".to_owned());
    }
    let categories: HashSet<_> = archive.manifest.categories.iter().copied().collect();
    if categories.len() != archive.manifest.categories.len() {
        return Err("duplicate backup category".to_owned());
    }
    let mut names = HashSet::new();
    for entry in &archive.entries {
        if !categories.contains(&entry.category) || !names.insert(entry.name.clone()) {
            return Err("invalid or duplicate backup entry".to_owned());
        }
        safe_path(Path::new("."), &entry.name)?;
        let allowed = match entry.category {
            BackupCategory::Appearance => {
                entry.name == "nebula_settings.txt"
                    && filter_settings(entry.bytes.clone()) == entry.bytes
            },
            BackupCategory::Config => {
                matches!(entry.name.as_str(), "nebula.lua" | "terminal_profiles.json")
                    && (entry.name != "terminal_profiles.json"
                        || serde_json::from_slice::<serde_json::Value>(&entry.bytes).is_ok())
            },
            BackupCategory::Ssh => {
                entry.name == "ssh_profiles.json" && ssh_is_sanitized(&entry.bytes)
            },
            BackupCategory::Sync => entry.name == "nebula_sync.txt",
            BackupCategory::Assistant => entry.name == "nebula_assistant.txt",
            BackupCategory::Session => entry.name == "session.json",
            BackupCategory::DirectoryHistory => entry.name == "directory_history.json",
            BackupCategory::CommandHistory => {
                crate::nebula_history::history_file_names().contains(&entry.name.as_str())
            },
            BackupCategory::Fonts => {
                entry.name.starts_with("fonts/") && entry.name.len() > "fonts/".len()
            },
        };
        if !allowed {
            return Err(format!("entry is not allowed in {:?}: {}", entry.category, entry.name));
        }
    }
    Ok(())
}

fn ssh_is_sanitized(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else { return false };
    value.get("profiles").and_then(serde_json::Value::as_array).is_none_or(|profiles| {
        profiles.iter().all(|profile| {
            profile.as_object().is_none_or(|object| {
                !object.contains_key("private_keys") && !object.contains_key("proxy")
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn archive() -> BackupArchive {
        BackupArchive {
            manifest: BackupManifest { version: 1, categories: vec![BackupCategory::Appearance] },
            entries: vec![BackupEntry {
                category: BackupCategory::Appearance,
                name: "nebula_settings.txt".into(),
                bytes: b"theme=dark".to_vec(),
            }],
        }
    }

    #[test]
    fn defaults_only_select_appearance() {
        let selection = BackupSelection::default();
        assert_eq!(selection.categories().collect::<Vec<_>>(), vec![BackupCategory::Appearance]);
        assert!(!selection.is_empty());

        let empty = BackupSelection { appearance: false, ..selection };
        assert!(empty.is_empty());
    }

    #[test]
    fn roundtrip_and_wrong_password_fail() {
        let packet = seal(&archive(), "correct horse").unwrap();
        assert_eq!(open(&packet, "correct horse").unwrap(), archive());
        assert!(open(&packet, "wrong horse").is_err());
    }

    #[test]
    fn collection_filters_sensitive_settings_and_restore_is_atomic() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("nebula_settings.txt"),
            "theme=dark\nssh_proxy_password=secret\npinned_hosts=x\n",
        )
        .unwrap();
        let collected = collect_from(directory.path(), BackupSelection::default()).unwrap();
        assert_eq!(collected.entries[0].bytes, b"theme=dark".to_vec());
        restore_to(directory.path(), &collected).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("nebula_settings.txt")).unwrap(),
            "theme=dark"
        );
    }

    #[test]
    fn config_collection_includes_lua_and_terminal_profiles() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("nebula.lua"), "return {}").unwrap();
        fs::write(directory.path().join("terminal_profiles.json"), "{\"profiles\":[]}").unwrap();
        let selection =
            BackupSelection { appearance: false, config: true, ..BackupSelection::default() };

        let collected = collect_from(directory.path(), selection).unwrap();
        assert_eq!(
            collected.entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            vec!["nebula.lua", "terminal_profiles.json"]
        );
    }

    #[test]
    fn command_history_collection_and_restore_include_all_scopes() {
        let source = tempdir().unwrap();
        for (index, file_name) in
            crate::nebula_history::history_file_names().into_iter().enumerate()
        {
            fs::write(source.path().join(file_name), format!("history-{index}")).unwrap();
        }
        let selection = BackupSelection {
            appearance: false,
            command_history: true,
            ..BackupSelection::default()
        };

        let archive = collect_from(source.path(), selection).unwrap();
        assert_eq!(
            archive.entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            crate::nebula_history::history_file_names()
        );

        let restored = tempdir().unwrap();
        restore_to(restored.path(), &archive).unwrap();
        for (index, file_name) in
            crate::nebula_history::history_file_names().into_iter().enumerate()
        {
            assert_eq!(
                fs::read_to_string(restored.path().join(file_name)).unwrap(),
                format!("history-{index}")
            );
        }
    }

    #[test]
    fn invalid_terminal_profiles_json_is_rejected_before_restore() {
        let directory = tempdir().unwrap();
        let archive = BackupArchive {
            manifest: BackupManifest { version: 1, categories: vec![BackupCategory::Config] },
            entries: vec![
                BackupEntry {
                    category: BackupCategory::Config,
                    name: "nebula.lua".into(),
                    bytes: b"return {}".to_vec(),
                },
                BackupEntry {
                    category: BackupCategory::Config,
                    name: "terminal_profiles.json".into(),
                    bytes: b"not json".to_vec(),
                },
            ],
        };

        assert!(restore_to(directory.path(), &archive).is_err());
        assert!(!directory.path().join("nebula.lua").exists());
    }

    #[test]
    fn path_traversal_is_rejected_before_writes() {
        let directory = tempdir().unwrap();
        let archive = BackupArchive {
            manifest: BackupManifest { version: 1, categories: vec![BackupCategory::Appearance] },
            entries: vec![BackupEntry {
                category: BackupCategory::Appearance,
                name: "../outside".into(),
                bytes: b"bad".to_vec(),
            }],
        };
        assert!(restore_to(directory.path(), &archive).is_err());
        assert!(!directory.path().parent().unwrap().join("outside").exists());
    }
}
