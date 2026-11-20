//! Showing the item an open asked for.
//!
//! A search hit, a notice or a launch names an anchor: an item another part
//! of the workspace holds. The window that shows it may not have taken that
//! part in yet — a message a notice was read from arrives after the notice
//! — so an open the window cannot show is not dropped. It waits in
//! [`Opens`], in the order asked, and is asked for again at each sync until
//! a view shows it ([`OpenOutcome::Shown`]) or says it is gone
//! ([`OpenOutcome::Gone`]). A gone anchor is dropped: a dead one is never asked
//! for forever.
//!
//! An app whose views can show an item at once says so from its
//! [`ViewCx::on_open`](super::ViewCx::on_open) callback. An app that shows
//! items on its own sync takes the queue with
//! [`ViewCx::opens`](super::ViewCx::opens) and settles each anchor it shows
//! in [`Opens::show`].

use std::cell::RefCell;
use std::rc::Rc;

/// What showing the item an open names came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// The window shows it.
    Shown,
    /// The window does not hold what the anchor names yet: keep it and ask
    /// again at the next sync.
    NotYet,
    /// The item is gone for good: drop the anchor, and tell why.
    Gone(String),
}

impl OpenOutcome {
    /// The outcome of an open whose item is gone for `reason`.
    #[must_use]
    pub fn gone(reason: impl Into<String>) -> Self {
        Self::Gone(reason.into())
    }
}

impl From<()> for OpenOutcome {
    fn from((): ()) -> Self {
        Self::Shown
    }
}

impl<E: Into<tilo_resource::Refusal>> From<Result<(), E>> for OpenOutcome {
    fn from(outcome: Result<(), E>) -> Self {
        match outcome {
            Ok(()) => Self::Shown,
            Err(refusal) => Self::Gone(refusal.into().into_reason()),
        }
    }
}

/// The anchors a window was asked to show and has not shown yet, in the
/// order asked. One per window: the launch that opened it, an open action,
/// a picked search hit and its views all ask into the same queue.
#[derive(Clone, Default)]
pub struct Opens(Rc<RefCell<Vec<String>>>);

impl Opens {
    /// Ask to show `anchor`. An anchor already waiting is kept where it is:
    /// items show in the order they were asked for, however many syncs the
    /// first needs.
    pub fn ask(&self, anchor: &str) {
        let mut waiting = self.0.borrow_mut();
        if !waiting.iter().any(|kept| kept == anchor) {
            waiting.push(anchor.to_owned());
        }
    }

    /// The anchors to show, oldest first.
    #[must_use]
    pub fn waiting(&self) -> Vec<String> {
        self.0.borrow().clone()
    }

    /// The window has shown `anchor`, or never will: stop asking for it.
    pub fn settled(&self, anchor: &str) {
        self.0.borrow_mut().retain(|kept| kept != anchor);
    }

    /// Show every waiting anchor with `show`, in the order asked: the ones
    /// it shows or reports gone are dropped, the ones it cannot show yet
    /// stay, and what was asked meanwhile stands after them.
    pub fn show(&self, mut show: impl FnMut(&str) -> OpenOutcome) {
        let mut waiting = std::mem::take(&mut *self.0.borrow_mut());
        waiting.retain(|anchor| show(anchor) == OpenOutcome::NotYet);
        let mut current = self.0.borrow_mut();
        waiting.append(&mut current);
        *current = waiting;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An anchor the window cannot show yet is kept — still in the order it
    /// was asked for — and asked for again until it shows; one whose item is
    /// gone is dropped rather than asked for forever.
    #[test]
    fn an_anchor_the_window_cannot_show_yet_is_kept_and_a_gone_one_is_dropped() {
        let opens = Opens::default();
        opens.ask("late");
        opens.ask("later");
        // Neither has arrived; the second is asked for meanwhile.
        opens.show(|_| OpenOutcome::NotYet);
        opens.ask("last");
        assert_eq!(
            opens.waiting(),
            ["late", "later", "last"],
            "kept in the order asked"
        );
        // The first two arrive in the same sync; the third is still on its way.
        let mut answered = [OpenOutcome::Shown, OpenOutcome::Gone("deleted".into())].into_iter();
        opens.show(|_| answered.next().unwrap_or(OpenOutcome::NotYet));
        assert_eq!(
            opens.waiting(),
            ["last"],
            "shown and gone anchors are dropped"
        );
        opens.show(|_| OpenOutcome::Shown);
        assert!(
            opens.waiting().is_empty(),
            "nothing waits once it has shown"
        );
    }

    /// The same anchor asked for twice while it waits is shown once.
    #[test]
    fn an_anchor_waiting_is_not_asked_for_twice() {
        let opens = Opens::default();
        opens.ask("message:1");
        opens.ask("message:1");
        assert_eq!(opens.waiting(), ["message:1"]);
    }
}
