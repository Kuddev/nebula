//! Host organization is profile data. Recently used destinations are a separate bounded list.

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct HostOrganization {
    pub group: String,
    pub tags: Vec<String>,
    pub notes: String,
}

impl HostOrganization {
    pub(crate) fn from_inputs(group: &str, tags: &str, notes: &str) -> Result<Self, String> {
        let mut unique = Vec::new();
        for tag in tags.split([',', ';', '，', '；']).map(str::trim).filter(|t| !t.is_empty()) {
            if !unique.iter().any(|old: &String| old.to_lowercase() == tag.to_lowercase()) {
                unique.push(tag.to_owned());
            }
        }
        let value =
            Self { group: group.trim().to_owned(), tags: unique, notes: notes.trim().to_owned() };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.group.chars().count() > 128 || self.group.chars().any(char::is_control) {
            return Err("A group must contain at most 128 visible characters".into());
        }
        if self.tags.len() > 32
            || self.tags.iter().any(|tag| {
                tag.is_empty() || tag.chars().count() > 64 || tag.chars().any(char::is_control)
            })
        {
            return Err("Use at most 32 tags, each containing 1–64 visible characters".into());
        }
        if self.notes.chars().count() > 4096
            || self.notes.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("Host notes must contain at most 4096 characters".into());
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.group.is_empty() && self.tags.is_empty() && self.notes.is_empty()
    }
}

impl SshProfiles {
    pub(crate) fn destinations(&self) -> impl Iterator<Item = &str> {
        self.profiles.iter().map(|profile| profile.destination.as_str())
    }

    pub(crate) fn contains(&self, destination: &str) -> bool {
        self.profiles.iter().any(|profile| profile.destination == destination)
    }

    pub(crate) fn organization(&self, destination: &str) -> &HostOrganization {
        static EMPTY: HostOrganization =
            HostOrganization { group: String::new(), tags: Vec::new(), notes: String::new() };
        self.organization.get(destination).unwrap_or(&EMPTY)
    }

    pub(crate) fn set_organization(
        &mut self,
        destination: &str,
        value: HostOrganization,
    ) -> Result<(), String> {
        value.validate()?;
        if !self.contains(destination) {
            return Err("Save the host before its organization".into());
        }
        if value.is_empty() {
            self.organization.remove(destination);
        } else {
            self.organization.insert(destination.to_owned(), value);
        }
        Ok(())
    }

    /// Filter the merged sources in their existing order. Build one borrowed
    /// profile index per query, rather than scanning all profiles for every row.
    pub(crate) fn filter_hosts(
        &self,
        hosts: Vec<String>,
        query: &str,
        managed_only: bool,
        group: Option<&str>,
    ) -> Vec<String> {
        let profiles: std::collections::HashMap<_, _> =
            self.profiles.iter().map(|profile| (profile.destination.as_str(), profile)).collect();
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        hosts
            .into_iter()
            .filter(|destination| {
                let profile = profiles.get(destination.as_str());
                if managed_only && profile.is_none() {
                    return false;
                }
                let organization = self.organization(destination);
                if group.is_some_and(|group| organization.group != group) {
                    return false;
                }
                if terms.is_empty() {
                    return true;
                }
                let searchable = format!(
                    "{} {} {} {} {}",
                    destination,
                    profile.and_then(|p| p.label.as_deref()).unwrap_or_default(),
                    organization.group,
                    organization.tags.join(" "),
                    organization.notes
                )
                .to_lowercase();
                terms.iter().all(|term| searchable.contains(term))
            })
            .collect()
    }

    pub(crate) fn groups(&self) -> Vec<String> {
        self.organization
            .values()
            .map(|value| value.group.trim())
            .filter(|group| !group.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

/// Both shells adapt this rule. No filesystem reads or renderer state live here.
pub(crate) fn merge_host_sources<'a>(
    recent: &'a [String],
    pinned: &[String],
    hidden: &[String],
    configured: &'a [String],
    managed: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let hidden: std::collections::HashSet<_> = hidden.iter().map(String::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    let mut hosts = Vec::new();
    for host in recent
        .iter()
        .map(String::as_str)
        .chain(managed)
        .chain(configured.iter().map(String::as_str))
    {
        if !hidden.contains(host) && seen.insert(host.to_owned()) {
            hosts.push(host.to_owned());
        }
    }
    hosts.sort_by_key(|host| pinned.iter().position(|pin| pin == host).unwrap_or(usize::MAX));
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(destination: &str) -> SshProfileAuth {
        SshProfileAuth {
            destination: destination.into(),
            auth: SshAuthMode::Auto,
            private_keys: vec![],
            label: Some("Production database".into()),
            icon: None,
            connection: Default::default(),
        }
    }

    #[test]
    fn two_hundred_managed_hosts_survive_a_twenty_entry_recent_list() {
        let managed: Vec<_> = (0..200).map(|n| format!("root@server-{n}")).collect();
        let recent = managed[180..].to_vec();
        let hosts = merge_host_sources(&recent, &[], &[], &[], managed.iter().map(String::as_str));
        assert_eq!(hosts.len(), 200);
        assert!(hosts.contains(&managed[0]));
        assert_eq!(hosts[0], managed[180]);
    }

    #[test]
    fn search_preserves_source_order_and_combines_group_and_management_filters() {
        let mut profiles = SshProfiles::default();
        profiles.upsert(host("root@db"));
        profiles
            .set_organization(
                "root@db",
                HostOrganization::from_inputs("生产", "linux", "Owner: Alice").unwrap(),
            )
            .unwrap();
        let hosts = vec!["config-only".into(), "root@db".into(), "recent-only".into()];
        assert_eq!(profiles.filter_hosts(hosts.clone(), "  ", false, None), hosts);
        assert_eq!(profiles.filter_hosts(hosts.clone(), "", true, None), vec!["root@db"]);
        assert_eq!(
            profiles.filter_hosts(hosts.clone(), "", false, Some("")),
            vec!["config-only", "recent-only"]
        );
        assert_eq!(
            profiles.filter_hosts(hosts.clone(), "DATABASE alice LINUX", false, Some("生产")),
            vec!["root@db"]
        );
        assert!(profiles.filter_hosts(hosts.clone(), "alice", false, Some("")).is_empty());
        assert_eq!(profiles.filter_hosts(hosts, "CONFIG", false, None), vec!["config-only"]);
    }

    #[test]
    fn hidden_and_pinned_rules_apply_to_every_source_without_duplicates() {
        let hosts = merge_host_sources(
            &["a".into()],
            &["b".into()],
            &["c".into()],
            &["a".into(), "c".into()],
            ["a", "b", "c"],
        );
        assert_eq!(hosts, vec!["b", "a"]);
    }

    #[test]
    fn organization_survives_auth_changes_rename_and_round_trip() {
        let mut profiles = SshProfiles::default();
        profiles.upsert(host("root@a"));
        profiles
            .set_organization(
                "root@a",
                HostOrganization::from_inputs("生产", "DB,db,linux", "负责人 Alice").unwrap(),
            )
            .unwrap();
        profiles.upsert(host("root@a"));
        profiles.rename("root@a", "root@b");
        let restored: SshProfiles =
            serde_json::from_str(&serde_json::to_string(&profiles).unwrap()).unwrap();
        assert_eq!(restored.organization("root@b").tags, vec!["DB", "linux"]);
        assert_eq!(
            restored.filter_hosts(vec!["root@b".into()], "生产 alice", false, None),
            vec!["root@b"]
        );
        assert!(restored.filter_hosts(vec!["root@b".into()], "missing", false, None).is_empty());
        profiles.remove("root@b");
        assert!(profiles.organization.is_empty());
    }

    #[test]
    fn old_profiles_load_without_organization() {
        let profiles: SshProfiles = serde_json::from_str(r#"{"version":1,"profiles":[]}"#).unwrap();
        assert!(profiles.groups().is_empty());
    }

    #[test]
    fn stale_profile_save_cannot_erase_another_windows_host_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.json");
        let mut initial = SshProfiles::default();
        initial.upsert(host("a"));
        initial.save(&path).unwrap();
        let mut first = SshProfiles::load(&path).unwrap();
        let mut stale = SshProfiles::load(&path).unwrap();
        first.upsert(host("b"));
        first.save(&path).unwrap();
        stale.upsert(host("c"));
        assert!(stale.save(&path).is_err());
        let loaded = SshProfiles::load(&path).unwrap();
        assert!(loaded.contains("b"));
        assert!(!loaded.contains("c"));
        first.upsert(host("d"));
        first.save(&path).unwrap();
        assert!(SshProfiles::load(&path).unwrap().contains("d"));
    }
}
