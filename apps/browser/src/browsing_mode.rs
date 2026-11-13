//! Immutable per-window browsing policy.

/// Catalogue message: why a window without account custody refuses the
/// person's website accounts.
pub(crate) const NO_ACCOUNT_CUSTODY: &str = "browser-no-account-custody";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowsingMode {
    #[default]
    Normal,
    Private,
    /// A window opened for an agent ([`ui::webview::VISITOR_WINDOW_FLAG`]): a
    /// fresh profile thrown away with it, and none of the person's account
    /// custody.
    Visitor,
}

impl BrowsingMode {
    pub const fn of_launch(private: bool, visitor: bool) -> Self {
        if private {
            Self::Private
        } else if visitor {
            Self::Visitor
        } else {
            Self::Normal
        }
    }

    pub const fn is_private(self) -> bool {
        matches!(self, Self::Private)
    }

    pub const fn persists_personal_state(self) -> bool {
        matches!(self, Self::Normal | Self::Visitor)
    }

    /// Whether the window holds the person's cookies and sign-ins, saved
    /// passwords and website accounts.
    pub const fn holds_account_custody(self) -> bool {
        matches!(self, Self::Normal)
    }

    pub const fn has_own_profile(self) -> bool {
        matches!(self, Self::Private | Self::Visitor)
    }

    /// Whether every departure waits for the person's answer.
    pub const fn holds_departures(self) -> bool {
        matches!(self, Self::Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mode_denies_every_personal_persistence_boundary() {
        let private = BrowsingMode::Private;
        assert!(!private.persists_personal_state());
        assert!(BrowsingMode::Normal.persists_personal_state());
    }

    #[test]
    fn only_the_persons_own_window_holds_their_account_custody() {
        assert!(BrowsingMode::Normal.holds_account_custody());
        for mode in [BrowsingMode::Private, BrowsingMode::Visitor] {
            assert!(!mode.holds_account_custody(), "{mode:?}");
            assert!(mode.has_own_profile(), "{mode:?}");
        }
        assert!(!BrowsingMode::Normal.has_own_profile());
        assert_eq!(BrowsingMode::of_launch(true, true), BrowsingMode::Private);
        assert_eq!(BrowsingMode::of_launch(false, true), BrowsingMode::Visitor);
        assert_eq!(BrowsingMode::of_launch(false, false), BrowsingMode::Normal);
    }

    #[test]
    fn only_the_window_an_agent_operates_holds_what_the_page_sends() {
        assert!(BrowsingMode::Visitor.holds_departures());
        for mode in [BrowsingMode::Normal, BrowsingMode::Private] {
            assert!(!mode.holds_departures(), "{mode:?}");
        }
    }
}
