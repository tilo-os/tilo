//! An ask about the person themselves (keeping who they are here), which
//! the desktop puts to them from their own avatar in the top bar rather
//! than as a banner over the windows.
//!
//! The host offers it like any other host action ([`HostAction`]); one
//! whose source is [`HostActionSource::Identity`] is held here instead of
//! posted as a notice. The bar shows it open, as a small popover beside the
//! avatar, until the person answers it, puts it off, or clicks elsewhere;
//! clicked away it stays as a dot on the avatar, which opens it again. It
//! never covers a window, so it can never take a click meant for one.

use super::DesktopHandle;
use super::search::{HostAction, HostActionSource};

/// The ask the bar shows from the person's own avatar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalAsk {
    /// What answering it does: the host action, under its own title.
    pub action: HostAction,
    /// The question ("Keep your name and work?").
    pub title: String,
    /// What answering brings, for assistive technology and where there is
    /// room for it.
    pub body: String,
    /// Shown as the popover; `false` while it waits as the avatar's dot.
    pub open: bool,
}

impl HostAction {
    /// Whether an offer of this action is about the person themselves, and
    /// so asked from their avatar ([`DesktopHandle::ask_personally`]).
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.source == Some(HostActionSource::Identity)
    }
}

impl DesktopHandle {
    /// Ask the person `title` from their avatar, answered by `action`. It
    /// replaces an earlier ask and opens at once.
    pub fn ask_personally(&self, action: HostAction, title: String, body: String) {
        let mut shared = self.shared.borrow_mut();
        shared.personal_ask = Some(PersonalAsk {
            action,
            title,
            body,
            open: true,
        });
        shared.personal_ask_revision = shared.personal_ask_revision.wrapping_add(1);
    }

    /// The ask waiting for the person, open or as the avatar's dot.
    pub fn personal_ask(&self) -> Option<PersonalAsk> {
        self.shared.borrow().personal_ask.clone()
    }

    /// Changes whenever the ask, or whether it is open, does.
    pub fn personal_ask_revision(&self) -> u64 {
        self.shared.borrow().personal_ask_revision
    }

    /// Show the waiting ask open, or fold it into the avatar's dot. Returns
    /// whether anything changed.
    pub fn set_personal_ask_open(&self, open: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ask) = shared.personal_ask.as_mut() else {
            return false;
        };
        if ask.open == open {
            return false;
        }
        ask.open = open;
        shared.personal_ask_revision = shared.personal_ask_revision.wrapping_add(1);
        true
    }

    /// The person answered the ask: the host performs its action
    /// ([`Self::take_host_choices`]).
    pub fn answer_personal_ask(&self) {
        let mut shared = self.shared.borrow_mut();
        if let Some(ask) = shared.personal_ask.take() {
            shared.host_choices.push(ask.action.id);
            shared.personal_ask_revision = shared.personal_ask_revision.wrapping_add(1);
        }
    }

    /// The person put the ask off: it goes. What it offered stays in search
    /// and in their profile card.
    pub fn put_off_personal_ask(&self) {
        let mut shared = self.shared.borrow_mut();
        if shared.personal_ask.take().is_some() {
            shared.personal_ask_revision = shared.personal_ask_revision.wrapping_add(1);
        }
    }

    /// The host no longer offers `ids` (done, perhaps from search): an ask
    /// for one of them asks for something already done.
    pub(super) fn withdraw_personal_ask(&self, ids: &[String]) {
        let mut shared = self.shared.borrow_mut();
        if shared
            .personal_ask
            .as_ref()
            .is_some_and(|ask| ids.contains(&ask.action.id))
        {
            shared.personal_ask = None;
            shared.personal_ask_revision = shared.personal_ask_revision.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep() -> HostAction {
        serde_json::from_value(serde_json::json!({
            "id": "keep-identity",
            "title": "Keep with passkey",
            "source": "identity",
        }))
        .unwrap_or_else(|error| panic!("a host action: {error}"))
    }

    struct Notices;

    impl crate::TileSource for Notices {
        fn tile(
            &mut self,
            _: &tilo_desktop_model::Workspace,
            tab: &tilo_desktop_model::Tab,
        ) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    fn handle() -> DesktopHandle {
        let mut minter = tilo_desktop_model::IdMinter::new(7);
        let tree = crate::seed_tree(&mut minter, "Desk", Vec::new());
        crate::DesktopSurface::new(tree, 7, Box::new(Notices)).1
    }

    #[test]
    fn an_identity_ask_waits_on_the_avatar_until_answered_or_put_off() {
        let handle = handle();
        assert!(keep().is_personal());
        handle.ask_personally(keep(), "Keep your name and work?".into(), String::new());
        assert!(handle.personal_ask().is_some_and(|ask| ask.open));

        assert!(handle.set_personal_ask_open(false), "clicked away: a dot");
        assert!(handle.personal_ask().is_some_and(|ask| !ask.open));
        assert!(
            handle.set_personal_ask_open(true),
            "the avatar opens it again"
        );

        handle.answer_personal_ask();
        assert!(handle.personal_ask().is_none());
        assert_eq!(handle.take_host_choices(), vec!["keep-identity".to_owned()]);

        handle.ask_personally(keep(), "Keep your name and work?".into(), String::new());
        handle.put_off_personal_ask();
        assert!(handle.personal_ask().is_none());
        assert!(
            handle.take_host_choices().is_empty(),
            "put off performs nothing"
        );
    }

    #[test]
    fn an_ask_for_an_action_no_longer_offered_goes() {
        let handle = handle();
        let _ = handle.set_host_actions(vec![keep()]);
        handle.ask_personally(keep(), "Keep your name and work?".into(), String::new());
        let _ = handle.set_host_actions(Vec::new());
        assert!(
            handle.personal_ask().is_none(),
            "kept from search meanwhile"
        );
    }
}
