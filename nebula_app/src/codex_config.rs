//! Explicit export of one Nebula provider to Codex CLI configuration.
//!
//! Nebula normally keeps API keys in the Windows Credential Manager. Codex's
//! file auth mode consumes `OPENAI_API_KEY` from `auth.json`, so this module is
//! called only after a second user confirmation and always backs up both live
//! files before replacing them.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as JsonValue};
use toml_edit::{DocumentMut, value};
use zeroize::Zeroizing;

use crate::ai_providers::AiProvider;

pub fn apply_provider(provider: &AiProvider) -> Result<PathBuf, String> {
    if !provider.kind.uses_openai_protocol() {
        return Err("Codex 需要 Responses/OpenAI 兼容供应商".to_owned());
    }
    let secret = crate::ai_providers::load_api_key(&provider.id)
        .map_err(|_| "无法从凭据管理器读取 API Key".to_owned())?
        .ok_or_else(|| "请先保存 API Key".to_owned())?;
    let key = Zeroizing::new(
        String::from_utf8(secret).map_err(|_| "凭据管理器中的 API Key 编码无效".to_owned())?,
    );
    let home = codex_home().ok_or_else(|| "无法确定 Codex 配置目录".to_owned())?;
    apply_to_dir(&home, provider, key.as_str()).map_err(|error| error.to_string())?;
    Ok(home)
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .map(|home| home.join(".codex"))
    })
}

fn provider_key(provider: &AiProvider) -> String {
    let mut key = String::from("nebula_");
    for character in provider.id.chars() {
        if character.is_ascii_alphanumeric() {
            key.push(character.to_ascii_lowercase());
        } else if !key.ends_with('_') {
            key.push('_');
        }
    }
    key.trim_end_matches('_').to_owned()
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(".nebula.bak");
    PathBuf::from(name)
}

fn backup(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::copy(path, backup_path(path))?;
    }
    Ok(())
}

fn restore(path: &Path, old: Option<&[u8]>) {
    match old {
        Some(bytes) => {
            let _ = std::fs::write(path, bytes);
        },
        None => {
            let _ = std::fs::remove_file(path);
        },
    }
}

fn apply_to_dir(home: &Path, provider: &AiProvider, api_key: &str) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    let auth_path = home.join("auth.json");
    let config_path = home.join("config.toml");
    let old_auth = std::fs::read(&auth_path).ok();
    let old_config = std::fs::read(&config_path).ok();
    backup(&auth_path)?;
    backup(&config_path)?;

    let mut auth = match old_auth.as_deref() {
        Some(bytes) => serde_json::from_slice::<JsonValue>(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        None => JsonValue::Object(Map::new()),
    };
    let object = auth.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "Codex auth.json 顶层必须是 JSON 对象")
    })?;
    object.insert("OPENAI_API_KEY".to_owned(), JsonValue::String(api_key.to_owned()));
    let auth_text = Zeroizing::new(
        serde_json::to_string_pretty(&auth)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    );

    let config_text = match old_config.as_deref() {
        Some(bytes) => std::str::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        None => "",
    };
    let mut document = if config_text.trim().is_empty() {
        DocumentMut::new()
    } else {
        config_text
            .parse::<DocumentMut>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    let provider_key = provider_key(provider);
    document["model_provider"] = value(provider_key.clone());
    document["model"] = value(provider.model.trim());
    document["model_providers"][provider_key.as_str()]["name"] = value(provider.name.trim());
    document["model_providers"][provider_key.as_str()]["base_url"] =
        value(provider.base_url.trim_end_matches('/'));
    document["model_providers"][provider_key.as_str()]["wire_api"] = value("responses");
    document["model_providers"][provider_key.as_str()]["env_key"] = value("OPENAI_API_KEY");
    document["model_providers"][provider_key.as_str()]["requires_openai_auth"] = value(true);
    document["features"]["goals"] = value(provider.codex_goals);
    document["features"]["remote_compaction_v2"] = value(provider.codex_remote_compaction);

    std::fs::write(&auth_path, auth_text.as_bytes())?;
    if let Err(error) = std::fs::write(&config_path, document.to_string().as_bytes()) {
        restore(&auth_path, old_auth.as_deref());
        restore(&config_path, old_config.as_deref());
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_providers::ProviderKind;

    #[test]
    fn apply_preserves_unknown_config_and_auth_fields() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            "# keep this comment\nunknown_setting = 7\n\n[features]\napps = true\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("auth.json"), r#"{"tokens":{"access_token":"keep"}}"#)
            .unwrap();
        let mut provider = AiProvider::preset(ProviderKind::Custom, "custom-7");
        provider.name = "Example".to_owned();
        provider.base_url = "https://example.test/v1".to_owned();
        provider.model = "gpt-test".to_owned();
        provider.codex_goals = true;
        apply_to_dir(temp.path(), &provider, "sk-secret").unwrap();

        let config = std::fs::read_to_string(temp.path().join("config.toml")).unwrap();
        assert!(config.contains("# keep this comment"));
        assert!(config.contains("unknown_setting = 7"));
        assert!(config.contains("apps = true"));
        assert!(config.contains("goals = true"));
        let auth: JsonValue =
            serde_json::from_slice(&std::fs::read(temp.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-secret");
        assert_eq!(auth["tokens"]["access_token"], "keep");
        assert!(backup_path(&temp.path().join("config.toml")).exists());
        assert!(backup_path(&temp.path().join("auth.json")).exists());
    }
}
