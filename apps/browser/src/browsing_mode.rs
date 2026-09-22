//! Immutable per-window browsing policy.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowsingMode {
    #[default]
    Normal,
    Private,
}

impl BrowsingMode {
    pub const fn is_private(self) -> bool {
        matches!(self, Self::Private)
    }

    pub const fn persists_personal_state(self) -> bool {
        matches!(self, Self::Normal)
    }

    pub const fn allows_session_recovery(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// Task 8 consumes this gate before offering or executing credential saves.
    pub const fn allows_credential_persistence(self) -> bool {
        matches!(self, Self::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mode_denies_every_personal_persistence_boundary() {
        let private = BrowsingMode::Private;
        assert!(!private.persists_personal_state());
        assert!(!private.allows_session_recovery());
        assert!(!private.allows_credential_persistence());
        assert!(BrowsingMode::Normal.persists_personal_state());
        assert!(BrowsingMode::Normal.allows_session_recovery());
        assert!(BrowsingMode::Normal.allows_credential_persistence());
    }
}
