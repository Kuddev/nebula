//! GPUI 壳的 SSH 主机列表数据层。
//!
//! 与旧壳读写同一份 `nebula_settings.txt` 的三个键（`saved_hosts` /
//! `pinned_hosts` / `hidden_hosts`，逗号分隔），排序与隐藏策略复用
//! `display::merge_ssh_hosts` 单一权威——两壳交替增删主机不会产生第二套
//! 次序。凭据（Credential Manager 条目）在这里**永不**触碰：删除主机只
//! 动列表，连接路径的密码/私钥语义由 `ssh_session` 业务层负责。

/// `saved_hosts` 的容量上限（旧壳同值）：自动保存的目的地最多保留 20 条。
pub const SAVED_HOSTS_CAP: usize = 20;

#[derive(Default, Clone)]
pub struct SshHostLists {
    /// 自动/手动保存的目的地（最新在前）。
    pub saved: Vec<String>,
    /// 置顶（浮到列表顶部，顺序即置顶次序）。
    pub pinned: Vec<String>,
    /// 隐藏的 `~/.ssh/config` 别名（Nebula 不改用户的 config 文件，
    /// 隐藏是对 config 源最强的"删除"）。
    pub hidden: Vec<String>,
}

fn parse_list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

impl SshHostLists {
    pub fn load() -> Self {
        let raw = nebula_settings::RawSettings::load();
        Self {
            saved: parse_list(raw.value("saved_hosts")),
            pinned: parse_list(raw.value("pinned_hosts")),
            hidden: parse_list(raw.value("hidden_hosts")),
        }
    }

    /// 展示顺序的单一权威（saved ∪ config − hidden，置顶浮头）。
    pub fn merged(&self) -> Vec<String> {
        crate::display::merge_ssh_hosts(&self.saved, &self.pinned, &self.hidden)
    }

    /// 隐藏区（设置页"已隐藏"折叠列表的数据源）。
    pub fn hidden_hosts(&self) -> &[String] {
        &self.hidden
    }

    pub fn is_pinned(&self, host: &str) -> bool {
        self.pinned.iter().any(|entry| entry == host)
    }

    /// 该主机是否来自 `~/.ssh/config`（决定删除语义：隐藏 vs 移除）。
    pub fn is_from_config(&self, host: &str) -> bool {
        crate::ssh::ssh_config_hosts().iter().any(|entry| entry == host)
    }

    /// 新增/提升一个保存的目的地（头插、去重、截断到容量上限）。
    pub fn remember(&mut self, host: &str) {
        self.saved.retain(|entry| entry != host);
        self.saved.insert(0, host.to_owned());
        self.saved.truncate(SAVED_HOSTS_CAP);
        self.hidden.retain(|entry| entry != host);
    }

    pub fn toggle_pin(&mut self, host: &str) {
        if let Some(pos) = self.pinned.iter().position(|entry| entry == host) {
            self.pinned.remove(pos);
        } else {
            self.pinned.push(host.to_owned());
        }
    }

    /// 删除一个目的地（旧壳 `remove_ssh_host_from_lists` 同义）：config
    /// 源改隐藏，Nebula 管理的直接移除；两种都顺带取消置顶。
    pub fn remove(&mut self, host: &str) {
        let from_config = self.is_from_config(host);
        self.saved.retain(|entry| entry != host);
        self.pinned.retain(|entry| entry != host);
        if from_config && !self.hidden.iter().any(|entry| entry == host) {
            self.hidden.push(host.to_owned());
        }
    }

    /// 把隐藏的 config 别名放回列表。
    pub fn restore_hidden(&mut self, host: &str) {
        self.hidden.retain(|entry| entry != host);
    }

    /// 三键原地写回 `nebula_settings.txt`（与旧壳全量写并存时后写者胜，
    /// 既有多窗口语义）。
    pub fn persist(&self) -> std::io::Result<()> {
        nebula_settings::persist_keys(&[
            ("saved_hosts", self.saved.join(",")),
            ("pinned_hosts", self.pinned.join(",")),
            ("hidden_hosts", self.hidden.join(",")),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list_trims_and_drops_empties() {
        assert_eq!(
            parse_list(Some(" a@b , ,c@d:2222 ")),
            vec!["a@b".to_owned(), "c@d:2222".to_owned()]
        );
        assert!(parse_list(None).is_empty());
    }

    #[test]
    fn remember_dedupes_head_inserts_and_caps() {
        let mut lists = SshHostLists::default();
        for i in 0..SAVED_HOSTS_CAP + 5 {
            lists.remember(&format!("user@host{i}"));
        }
        assert_eq!(lists.saved.len(), SAVED_HOSTS_CAP);
        assert_eq!(lists.saved[0], format!("user@host{}", SAVED_HOSTS_CAP + 4));

        lists.remember("user@host20");
        assert_eq!(lists.saved[0], "user@host20");
        assert_eq!(
            lists.saved.iter().filter(|entry| *entry == "user@host20").count(),
            1,
            "re-remember moves instead of duplicating"
        );
    }

    #[test]
    fn remove_keeps_credentials_out_and_unpins() {
        let mut lists = SshHostLists {
            saved: vec!["a@b".into(), "c@d".into()],
            pinned: vec!["a@b".into()],
            hidden: Vec::new(),
        };
        lists.remove("a@b");
        assert!(!lists.saved.iter().any(|entry| entry == "a@b"));
        assert!(lists.pinned.is_empty());
        // 非 config 源不进隐藏区（config 源依赖 ~/.ssh/config 的环境，
        // 在单测里不制造）。
        assert!(lists.hidden.is_empty());
    }

    #[test]
    fn toggle_pin_roundtrips() {
        let mut lists = SshHostLists::default();
        lists.toggle_pin("a@b");
        assert!(lists.is_pinned("a@b"));
        lists.toggle_pin("a@b");
        assert!(!lists.is_pinned("a@b"));
    }
}
