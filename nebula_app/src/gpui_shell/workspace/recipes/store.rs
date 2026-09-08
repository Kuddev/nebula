//! Named layouts reuse the session schema and restore path. No command history or AI resume.

use crate::session::{LayoutSession, Session};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Recipe {
    version: u32,
    pub(super) name: String,
    pub(super) session: Session,
    #[serde(skip)]
    pub(super) path: PathBuf,
}

pub(super) fn directory() -> PathBuf {
    nebula_settings::settings_dir().join("recipes")
}

fn clear_agents(layout: &mut LayoutSession) {
    match layout {
        LayoutSession::Pane { agent, .. } => *agent = None,
        LayoutSession::Split { first, second, .. } => {
            clear_agents(first);
            clear_agents(second);
        },
    }
}

fn valid_layout(layout: &LayoutSession, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    match layout {
        LayoutSession::Pane { cwd, agent } => {
            !cwd.contains('\0') && cwd.len() <= 32768 && agent.is_none()
        },
        LayoutSession::Split { ratio_permille, first, second, .. } => {
            (1..1000).contains(ratio_permille)
                && valid_layout(first, depth + 1)
                && valid_layout(second, depth + 1)
        },
    }
}

impl Recipe {
    pub(super) fn new(name: String, mut session: Session) -> Result<Self, String> {
        for tab in &mut session.tabs {
            if let Some(layout) = &mut tab.layout {
                clear_agents(layout);
            }
        }
        session.window = None;
        let recipe =
            Self { version: 1, name: name.trim().to_owned(), session, path: PathBuf::new() };
        recipe.validate()?;
        Ok(recipe)
    }

    pub(super) fn panes(&self) -> usize {
        self.session
            .tabs
            .iter()
            .map(|tab| tab.layout.as_ref().map_or(1, LayoutSession::pane_count))
            .sum()
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.session.version > Session::new(0, vec![]).version {
            return Err("Unsupported recipe version".into());
        }
        if self.name.is_empty()
            || self.name.chars().count() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err("Layout names must contain 1–80 visible characters".into());
        }
        if self.session.tabs.is_empty() || self.session.tabs.len() > 32 || self.panes() > 64 {
            return Err("A recipe needs 1–32 tabs and at most 64 panes".into());
        }
        if self.session.tabs.iter().any(|tab| {
            tab.cwd.contains('\0')
                || tab.layout.as_ref().is_some_and(|layout| !valid_layout(layout, 0))
        }) {
            return Err("Invalid layout tree".into());
        }
        Ok(())
    }
}

pub(super) fn save_at(directory: &Path, recipe: &Recipe) -> Result<(), String> {
    recipe.validate()?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(recipe).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Recipe is too large".into());
    }
    let mut file = tempfile::Builder::new()
        .prefix("layout-")
        .suffix(".tmp")
        .tempfile_in(directory)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    let destination = file.path().with_extension("json");
    file.persist_noclobber(destination).map_err(|error| error.error.to_string())?;
    Ok(())
}

pub(super) fn list_at(directory: &Path) -> Result<Vec<Recipe>, String> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Recipe is too large".into());
        }
        let mut recipe: Recipe = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        recipe.validate()?;
        recipe.path = path;
        entries.push(recipe);
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AgentSession, SplitAxis, TabSession};

    #[test]
    fn named_layout_round_trip_preserves_splits_and_discards_agent_replay() {
        let mut tab = TabSession::single("D:/api".into(), Some("API".into()), None);
        tab.layout = Some(LayoutSession::Split {
            axis: SplitAxis::TopBottom,
            ratio_permille: 370,
            first: Box::new(LayoutSession::Pane {
                cwd: "D:/api".into(),
                agent: Some(AgentSession {
                    source: "claude".into(),
                    session_id: Some("private-id".into()),
                }),
            }),
            second: Box::new(LayoutSession::Pane { cwd: "D:/web".into(), agent: None }),
        });
        let recipe = Recipe::new("开发环境".into(), Session::new(0, vec![tab])).unwrap();
        let directory = tempfile::tempdir().unwrap();
        save_at(directory.path(), &recipe).unwrap();
        let entries = list_at(directory.path()).unwrap();
        assert_eq!(entries[0].name, "开发环境");
        assert_eq!(entries[0].session.tabs, recipe.session.tabs);
        assert_eq!(entries[0].panes(), 2);
        assert!(!std::fs::read_to_string(&entries[0].path).unwrap().contains("private-id"));
    }

    #[test]
    fn names_do_not_control_paths_and_unknown_versions_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let tab = TabSession::single("/tmp".into(), None, None);
        let mut recipe = Recipe::new("../project".into(), Session::new(0, vec![tab])).unwrap();
        save_at(directory.path(), &recipe).unwrap();
        assert_eq!(list_at(directory.path()).unwrap()[0].path.parent(), Some(directory.path()));
        recipe.version = 999;
        assert!(save_at(directory.path(), &recipe).is_err());
        assert!(Recipe::new("".into(), Session::new(0, vec![])).is_err());
    }
}
