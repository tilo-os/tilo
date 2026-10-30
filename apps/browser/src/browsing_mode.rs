//! Immutable per-window browsing policy.

/// Why a window opened without the person's account custody refuses their
/// website accounts.
pub(crate) const NO_ACCOUNT_CUSTODY: &str = "This window browses without your sign-ins. Open \
     the page in a Browser window of your own to use your accounts there";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowsingMode {
    /// The person's own window: their profile, with its cookies, sign-ins,
    /// saved passwords and website accounts.
    #[default]
    Normal,
    /// Nothing from it is kept after it closes, and it is nobody's runtime
    /// participant.
    Private,
    /// A window opened for an agent ([`ui::webview::VISITOR_WINDOW_FLAG`]):
    /// it browses as any visitor would, on a fresh profile of its own that
    /// is thrown away with it. It holds none of the person's account
    /// custody (no cookies, sign-ins, saved passwords or website accounts),
    /// and is otherwise the workspace's window like any other.
    Visitor,
}

impl BrowsingMode {
    /// The mode a launch asks for: private, a visitor, or the person's own.
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

    /// Whether the window holds the person's account custody: their
    /// profile's cookies and sign-ins, saved passwords and addresses, and
    /// their website accounts. Reading a page that holds it takes the
    /// authority to operate it; reading one without takes only a
    /// visitor's.
    pub const fn holds_account_custody(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// Whether the engine browses on a fresh profile of the window's own,
    /// removed when the window is done.
    pub const fn has_own_profile(self) -> bool {
        matches!(self, Self::Private | Self::Visitor)
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
}
