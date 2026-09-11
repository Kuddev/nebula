//! A captured file action may finish only in the browser scope that initiated it.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RemoteTransferTarget {
    pub pane: u64,
    pub destination: String,
    pub path: String,
    pub navigation_generation: u64,
}

impl RemoteTransferTarget {
    pub(super) fn is_current(&self, pane: Option<u64>, destination: &str, generation: u64) -> bool {
        pane == Some(self.pane)
            && destination == self.destination
            && generation == self.navigation_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> RemoteTransferTarget {
        RemoteTransferTarget {
            pane: 12,
            destination: "user@alpha".into(),
            path: "/srv/subfolder".into(),
            navigation_generation: 8,
        }
    }

    #[test]
    fn folder_drop_keeps_its_explicit_destination_within_the_same_browser_scope() {
        let target = target();
        assert!(target.is_current(Some(12), "user@alpha", 8));
        assert_eq!(target.path, "/srv/subfolder");
    }

    #[test]
    fn another_pane_on_the_same_host_cannot_receive_a_late_picker_result() {
        assert!(!target().is_current(Some(13), "user@alpha", 8));
        assert!(!target().is_current(None, "user@alpha", 8));
    }

    #[test]
    fn switching_host_or_navigating_invalidates_conflict_confirmation() {
        assert!(!target().is_current(Some(12), "user@beta", 8));
        assert!(!target().is_current(Some(12), "user@alpha", 9));
    }
}
