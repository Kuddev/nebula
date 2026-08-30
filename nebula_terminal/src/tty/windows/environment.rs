use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

use crate::tty::Options;

const SYSTEM_ENVIRONMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENVIRONMENT_KEY: &str = r"Environment";
const VOLATILE_ENVIRONMENT_KEY: &str = r"Volatile Environment";
const PATH_VARIABLES: [&str; 3] = ["Path", "LibPath", "Os2LibPath"];

static ENVIRONMENT_STATE: OnceLock<Mutex<EnvironmentState>> = OnceLock::new();

/// 为新建的 Windows 终端重建子进程环境。
///
/// `options.env` 里的现有条目是 pane 专属覆盖，最后叠加到最新注册表快照上。
pub fn refresh_environment(options: &mut Options) -> io::Result<()> {
    let state = ENVIRONMENT_STATE
        .get_or_init(|| Mutex::new(EnvironmentState::new(CaseInsensitiveEnv::from_process())));
    let mut state = state
        .lock()
        .map_err(|_| io::Error::other("Windows environment refresh state is poisoned"))?;
    let snapshot = RegistrySnapshot::read()?;

    // 只有在注册表读取完整成功后才取走原覆盖项；失败时 pane 仍可沿用旧的继承模式。
    let overrides = CaseInsensitiveEnv::from_hash_map(std::mem::take(&mut options.env));
    options.env = state.merge(snapshot, overrides).into_hash_map();
    options.env_is_complete = true;
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct CaseInsensitiveEnv {
    entries: BTreeMap<String, EnvironmentEntry>,
}

#[derive(Clone, Debug)]
struct EnvironmentEntry {
    name: String,
    value: String,
}

impl CaseInsensitiveEnv {
    fn from_process() -> Self {
        let mut environment = Self::default();
        for (name, value) in std::env::vars_os() {
            environment
                .insert(name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned());
        }
        environment
    }

    fn from_hash_map(entries: HashMap<String, String>) -> Self {
        let mut environment = Self::default();
        for (name, value) in entries {
            environment.insert(name, value);
        }
        environment
    }

    fn insert(&mut self, name: String, value: String) {
        self.entries.insert(normalize_name(&name), EnvironmentEntry { name, value });
    }

    fn append_path(&mut self, name: String, value: String) {
        if value.is_empty() {
            return;
        }
        let normalized = normalize_name(&name);
        if let Some(existing) = self.entries.get_mut(&normalized) {
            if !existing.value.is_empty() && !existing.value.ends_with(';') {
                existing.value.push(';');
            }
            existing.value.push_str(&value);
        } else {
            self.entries.insert(normalized, EnvironmentEntry { name, value });
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(&normalize_name(name)).map(|entry| entry.value.as_str())
    }

    fn remove_normalized(&mut self, name: &str) {
        self.entries.remove(name);
    }

    fn into_hash_map(self) -> HashMap<String, String> {
        self.entries.into_values().map(|entry| (entry.name, entry.value)).collect()
    }
}

fn normalize_name(name: &str) -> String {
    name.to_uppercase()
}

struct EnvironmentState {
    inherited: CaseInsensitiveEnv,
    registry_keys: HashSet<String>,
}

impl EnvironmentState {
    fn new(inherited: CaseInsensitiveEnv) -> Self {
        Self { inherited, registry_keys: HashSet::new() }
    }

    fn merge(
        &mut self,
        snapshot: RegistrySnapshot,
        overrides: CaseInsensitiveEnv,
    ) -> CaseInsensitiveEnv {
        let current_keys = snapshot.keys();
        let mut environment = self.inherited.clone();

        // 进程环境里混有启动时的注册表值。记住历次出现过的键，才能让运行期间
        // 被删除的注册表变量从新 pane 消失，同时保住从父进程继承的私有变量。
        for name in self.registry_keys.iter().chain(&current_keys) {
            environment.remove_normalized(name);
        }
        for hive in snapshot.hives {
            apply_registry_hive(&mut environment, &self.inherited, &hive);
        }
        for entry in overrides.entries.into_values() {
            environment.insert(entry.name, entry.value);
        }

        self.registry_keys.extend(current_keys);
        environment
    }
}

struct RegistrySnapshot {
    hives: Vec<Vec<RegistryValue>>,
}

impl RegistrySnapshot {
    fn read() -> io::Result<Self> {
        let mut hives = vec![
            read_registry_key(HKEY_LOCAL_MACHINE, SYSTEM_ENVIRONMENT_KEY)?,
            read_registry_key(HKEY_CURRENT_USER, USER_ENVIRONMENT_KEY)?,
            read_registry_key(HKEY_CURRENT_USER, VOLATILE_ENVIRONMENT_KEY)?,
        ];

        let mut session_id = 0;
        let has_session = unsafe { ProcessIdToSessionId(std::process::id(), &mut session_id) } != 0;
        if !has_session {
            return Err(io::Error::last_os_error());
        }
        hives.push(read_registry_key(
            HKEY_CURRENT_USER,
            &format!(r"{VOLATILE_ENVIRONMENT_KEY}\{session_id}"),
        )?);

        Ok(Self { hives })
    }

    fn keys(&self) -> HashSet<String> {
        self.hives.iter().flatten().map(|value| normalize_name(&value.name)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryValueKind {
    String,
    ExpandString,
    Unsupported,
}

#[derive(Clone, Debug)]
struct RegistryValue {
    name: String,
    value: String,
    kind: RegistryValueKind,
}

/// `inherited` 是删除注册表键之前的进程原始环境，用作展开兜底：`USERPROFILE`
/// 等登录期变量只存于 Volatile Environment，在该 hive 落表之前展开
/// `%USERPROFILE%` 引用必须回退到它，否则子进程拿到字面量相对路径。
fn apply_registry_hive(
    environment: &mut CaseInsensitiveEnv,
    inherited: &CaseInsensitiveEnv,
    values: &[RegistryValue],
) {
    // REG_SZ 先落表，随后 REG_EXPAND_SZ 才能引用同一 hive 中已经载入的变量，
    // 引用尚未落表的 hive 间变量时回退 `inherited`。
    for expand_pass in [false, true] {
        for registry_value in values {
            let is_path = is_path_variable(&registry_value.name);
            let is_expand = registry_value.kind == RegistryValueKind::ExpandString
                || (is_path && registry_value.kind == RegistryValueKind::String);
            let supported = registry_value.kind != RegistryValueKind::Unsupported;
            if !supported || is_expand != expand_pass {
                continue;
            }

            let value = if is_expand {
                expand_environment_variables(&registry_value.value, environment, inherited)
            } else {
                registry_value.value.clone()
            };
            if value.is_empty() {
                continue;
            }
            if is_path {
                environment.append_path(registry_value.name.clone(), value);
            } else {
                environment.insert(registry_value.name.clone(), value);
            }
        }
    }
}

fn expand_environment_variables(
    input: &str,
    environment: &CaseInsensitiveEnv,
    inherited: &CaseInsensitiveEnv,
) -> String {
    let mut output = String::with_capacity(input.len());
    let mut variable = None::<String>;

    for character in input.chars() {
        if character == '%' {
            if let Some(name) = variable.take() {
                if let Some(value) = environment.get(&name).or_else(|| inherited.get(&name)) {
                    output.push_str(value);
                } else {
                    output.push('%');
                    output.push_str(&name);
                    output.push('%');
                }
            } else {
                variable = Some(String::new());
            }
        } else if let Some(name) = variable.as_mut() {
            name.push(character);
        } else {
            output.push(character);
        }
    }

    if let Some(name) = variable {
        output.push('%');
        output.push_str(&name);
    }
    output
}

fn is_path_variable(name: &str) -> bool {
    PATH_VARIABLES.iter().any(|path_name| name.eq_ignore_ascii_case(path_name))
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn read_registry_key(root: HKEY, path: &str) -> io::Result<Vec<RegistryValue>> {
    let path: Vec<u16> = OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    let mut raw_key = ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut raw_key) };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(Vec::new());
    }
    check_registry_status(status)?;
    let key = RegistryKey(raw_key);

    let mut value_count = 0;
    let mut max_name_length = 0;
    let mut max_data_length = 0;
    let status = unsafe {
        RegQueryInfoKeyW(
            key.0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut value_count,
            &mut max_name_length,
            &mut max_data_length,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    check_registry_status(status)?;

    let mut values = Vec::with_capacity(value_count as usize);
    let mut name_buffer = vec![0_u16; max_name_length.saturating_add(1).max(1) as usize];
    let mut data_buffer = vec![0_u8; max_data_length.max(2) as usize];
    let mut index = 0;

    loop {
        let mut retries = 0;
        let (name_length, data_length, value_type) = loop {
            let mut name_length = name_buffer.len() as u32;
            let mut data_length = data_buffer.len() as u32;
            let mut value_type = 0;
            let status = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                    ptr::null(),
                    &mut value_type,
                    data_buffer.as_mut_ptr(),
                    &mut data_length,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                return Ok(values);
            }
            if status == ERROR_MORE_DATA && retries < 8 {
                name_buffer
                    .resize(name_buffer.len().saturating_mul(2).max(name_length as usize + 1), 0);
                data_buffer.resize(
                    data_buffer.len().saturating_mul(2).max(data_length as usize).max(2),
                    0,
                );
                retries += 1;
                continue;
            }
            check_registry_status(status)?;
            break (name_length, data_length, value_type);
        };

        let name = String::from_utf16_lossy(&name_buffer[..name_length as usize]);
        if !name.is_empty() {
            let kind = match value_type {
                REG_SZ => RegistryValueKind::String,
                REG_EXPAND_SZ => RegistryValueKind::ExpandString,
                _ => RegistryValueKind::Unsupported,
            };
            let value = if kind == RegistryValueKind::Unsupported {
                String::new()
            } else {
                decode_registry_string(&data_buffer[..data_length as usize])
            };
            values.push(RegistryValue { name, value, kind });
        }
        index += 1;
    }
}

fn decode_registry_string(bytes: &[u8]) -> String {
    let mut wide: Vec<u16> =
        bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    if let Some(end) = wide.iter().position(|character| *character == 0) {
        wide.truncate(end);
    }
    String::from_utf16_lossy(&wide)
}

fn check_registry_status(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(status as i32)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(entries: &[(&str, &str)]) -> CaseInsensitiveEnv {
        let mut environment = CaseInsensitiveEnv::default();
        for (name, value) in entries {
            environment.insert((*name).to_owned(), (*value).to_owned());
        }
        environment
    }

    fn plain(name: &str, value: &str) -> RegistryValue {
        RegistryValue {
            name: name.to_owned(),
            value: value.to_owned(),
            kind: RegistryValueKind::String,
        }
    }

    fn expanded(name: &str, value: &str) -> RegistryValue {
        RegistryValue {
            name: name.to_owned(),
            value: value.to_owned(),
            kind: RegistryValueKind::ExpandString,
        }
    }

    fn snapshot(hives: Vec<Vec<RegistryValue>>) -> RegistrySnapshot {
        RegistrySnapshot { hives }
    }

    #[test]
    fn user_variables_override_machine_variables() {
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state.merge(
            snapshot(vec![vec![plain("SDK", "machine")], vec![plain("SDK", "user")]]),
            CaseInsensitiveEnv::default(),
        );

        assert_eq!(merged.get("SDK"), Some("user"));
    }

    #[test]
    fn path_variables_append_machine_before_user() {
        let machine = PATH_VARIABLES.map(|name| plain(name, "machine"));
        let user = PATH_VARIABLES.map(|name| plain(name, "user"));
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state
            .merge(snapshot(vec![machine.to_vec(), user.to_vec()]), CaseInsensitiveEnv::default());

        for name in PATH_VARIABLES {
            assert_eq!(merged.get(name), Some("machine;user"), "{name}");
        }
    }

    #[test]
    fn variable_names_are_case_insensitive() {
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state.merge(
            snapshot(vec![vec![plain("MixedCase", "machine")], vec![plain("mIXEDcASE", "user")]]),
            CaseInsensitiveEnv::default(),
        );

        assert_eq!(merged.entries.len(), 1);
        assert_eq!(merged.get("MIXEDCASE"), Some("user"));
    }

    #[test]
    fn expandable_values_see_plain_values_from_the_same_hive() {
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state.merge(
            snapshot(vec![vec![expanded("SDK", r"%ROOT%\sdk"), plain("ROOT", r"C:\tools")]]),
            CaseInsensitiveEnv::default(),
        );

        assert_eq!(merged.get("SDK"), Some(r"C:\tools\sdk"));
    }

    #[test]
    fn expandable_values_fall_back_to_the_process_environment() {
        // Volatile Environment 排在 HKCU\Environment 之后：TEMP 引用的
        // USERPROFILE 展开时还没落表，且已在 merge 开头从继承环境中删除，
        // 必须回退进程启动环境，否则 pane 的 TEMP/TMP 变成字面量相对路径，
        // shell 一写临时文件就在启动目录下建出 %USERPROFILE% 文件夹。
        // 用户目录随电脑而不同，直接从当前进程取真实 USERPROFILE，断言值
        // 也由它推导，避免把某台机器的用户名写进测试导致其他电脑失败。
        let profile = std::env::var_os("USERPROFILE")
            .expect("USERPROFILE must be set in the test process")
            .to_string_lossy()
            .into_owned();
        let temp = format!(r"%USERPROFILE%\AppData\Local\Temp");
        let mut state = EnvironmentState::new(environment(&[("USERPROFILE", profile.as_str())]));
        let merged = state.merge(
            snapshot(vec![
                Vec::new(),
                vec![expanded("TEMP", temp.as_str())],
                vec![plain("USERPROFILE", profile.as_str())],
            ]),
            CaseInsensitiveEnv::default(),
        );

        let expected_temp = format!(r"{profile}\AppData\Local\Temp");
        assert_eq!(merged.get("TEMP"), Some(expected_temp.as_str()));
        assert_eq!(merged.get("USERPROFILE"), Some(profile.as_str()));
    }

    #[test]
    fn path_stored_as_reg_sz_is_still_expanded() {
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state.merge(
            snapshot(vec![vec![plain("Path", r"%ROOT%\bin"), plain("ROOT", r"C:\tools")]]),
            CaseInsensitiveEnv::default(),
        );

        assert_eq!(merged.get("PATH"), Some(r"C:\tools\bin"));
    }

    #[test]
    fn deleted_registry_values_do_not_remove_process_private_values() {
        let mut state = EnvironmentState::new(environment(&[
            ("SESSION_ONLY", "keep"),
            ("RemovedLater", "stale inherited value"),
        ]));
        let first = state.merge(
            snapshot(vec![vec![plain("RemovedLater", "fresh")]]),
            CaseInsensitiveEnv::default(),
        );
        assert_eq!(first.get("RemovedLater"), Some("fresh"));

        let second = state.merge(snapshot(vec![Vec::new()]), CaseInsensitiveEnv::default());
        assert_eq!(second.get("RemovedLater"), None);
        assert_eq!(second.get("SESSION_ONLY"), Some("keep"));
    }

    #[test]
    fn pane_overrides_are_applied_after_the_registry_snapshot() {
        let mut state = EnvironmentState::new(CaseInsensitiveEnv::default());
        let merged = state.merge(
            snapshot(vec![vec![plain("PaneValue", "registry")]]),
            environment(&[("pANEvALUE", "pane")]),
        );

        assert_eq!(merged.get("PANEvalue"), Some("pane"));
        assert_eq!(merged.entries.len(), 1);
    }
}
