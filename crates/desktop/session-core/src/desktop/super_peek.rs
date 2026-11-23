//! The keys a held Super reaches, as the session says them and a shell shows them.
//!
//! Holding Super alone is reaching for a chord, or looking for one: the shell answers with a quiet
//! card naming what each key does while Super is down. What it shows is the session's to say
//! ([`SuperPeek::show`]), and letting Super go takes it away ([`SuperPeek::hide`]); the shell that
//! draws it decides what it looks like. The keys are the session's shortcut table as the person
//! bound it, read when the hold began. Clones share one peek.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::KeyHint;

/// A group of the peek: its title, and each row's keys and what they do.
pub type PeekGroup = (String, Vec<KeyHint>);

/// The keys a held Super reaches, as the session hands them over and a shell draws them.
#[derive(Clone, Default)]
pub struct SuperPeek {
    inner: Rc<RefCell<Inner>>,
}

#[derive(Default)]
struct Inner {
    groups: Vec<PeekGroup>,
    /// Whether the session wants it up.
    up: bool,
}

impl SuperPeek {
    /// Put the peek up with `groups` . A peek already up stays as it is.
    pub fn show(&self, groups: Vec<PeekGroup>) {
        let mut inner = self.inner.borrow_mut();
        if inner.up {
            return;
        }
        inner.groups = groups;
        inner.up = true;
    }

    /// Take it away: the hold ended.
    pub fn hide(&self) {
        self.inner.borrow_mut().up = false;
    }

    /// Whether the session wants it up.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.inner.borrow().up
    }

    /// What it shows, while it is up: each group's title and rows.
    #[must_use]
    pub fn groups(&self) -> Option<Vec<PeekGroup>> {
        let inner = self.inner.borrow();
        inner.up.then(|| inner.groups.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<PeekGroup> {
        vec![
            (
                "Windows".to_owned(),
                vec![
                    KeyHint::new(["Left", "Right"], "Move focus"),
                    KeyHint::new(["Up", "Down"], "Maximize or restore"),
                ],
            ),
            (
                "Agent and search".to_owned(),
                vec![KeyHint::new(["space"], "Search everywhere")],
            ),
        ]
    }

    tilo_test_env::scenario! {
        fn the_super_peek_holds_the_groups_of_a_hold;

        phase a_hold_shows_the_groups_and_letting_go_takes_them_away;
        {
            let peek = SuperPeek::default();
            assert!(!peek.is_up());
            assert!(peek.groups().is_none(), "nothing shows before a hold");
            peek.show(groups());
            assert!(peek.is_up());
            assert_eq!(peek.groups().map(|groups| groups.len()), Some(2));
            peek.hide();
            assert!(!peek.is_up());
            assert!(peek.groups().is_none(), "letting go takes it away");
        }

        phase a_second_show_while_up_keeps_what_the_hold_began_with;
        {
            let peek = SuperPeek::default();
            peek.show(groups());
            peek.show(Vec::new());
            assert_eq!(peek.groups().map(|groups| groups.len()), Some(2));
        }
    }
}
