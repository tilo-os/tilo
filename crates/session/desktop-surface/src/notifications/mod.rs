//! Desktop notifications as the shell draws them: the live banners and the centre, over the log
//! every host keeps ([`tilo_session_core::desktop::notifications`]). A host owns a [`NotificationLog`] and answers the effects only it can perform
//! (telling a sender it closed, routing an action to another device) through [`NotificationHost`]:
//! the native desktop feeds the log from the notification service, the browser its own notices.

use std::cell::Ref;

use tilo_session_core::desktop::notifications::{HistoryActivation, Notification, NotificationLog};

mod center;
mod reader;
mod stack;

pub use center::{
    CenterLayout, CenterMetrics, CenterOutcome, NotificationCenter, RowControl, RowShape,
};
pub use stack::{Banner, BannerAsk, BannerFeed, CORNER_INSET, NotificationStack, transient_slot};

/// What the banner stack and the notification centre need from the desktop that shows them. Reads
/// go through [`Self::log`]; every mutation is the host's, so it can answer the sender and persist
/// what changed.
pub trait NotificationHost {
    /// The live banners and retained history.
    fn log(&self) -> Ref<'_, NotificationLog>;

    /// While locked, banners collapse to a redacted count.
    fn session_locked(&self) -> bool {
        false
    }

    /// Why fresh banners are being held (Do Not Disturb, presenting, a focus schedule), for the
    /// notification centre's header. `None` when they show.
    fn quiet_status(&self) -> Option<String> {
        None
    }

    /// The banner stack laid out `on_screen` (newest first) at `now` , and the person `held` them
    /// (the pointer on the stack, keyboard focus in a banner). Forward
    /// to [`NotificationLog::present`]: a banner ages only while it is on screen and unheld.
    /// Viewer-local, no repaint.
    fn present(&self, now: ui::time::Instant, on_screen: &[u64], held: bool);

    /// The person asked for the notification centre (the card in place of banners that did not
    /// fit).
    fn open_center(&self);

    /// Host-owned choices for this notification. The shared banner draws at most two using the
    /// shared toast control; default activation remains the body of the card. Empty when no live
    /// host action is available.
    fn actions_for(&self, _notification: &Notification) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Invoke one choice returned by `actions_for` , through the same host operation exposed to
    /// automation. The host preserves the card until durable completion and reports a refusal
    /// instead of silently removing it.
    fn invoke_action(&self, _id: u64, _key: &str) {}

    /// The person closed live banner `id` .
    fn dismiss(&self, id: u64);

    /// The person selected live banner `id` (not its close button).
    fn activate(&self, id: u64);

    /// The person answered live banner `id` 's inline reply with `text` . The banners app carries
    /// this to its link; a host that paints banners itself takes no reply, and its stack offers
    /// none.
    fn reply(&self, _id: u64, _text: &str) {}

    /// The person opened retained notification `id` from the centre.
    fn activate_history(&self, id: u64) -> HistoryActivation;

    /// The person dismissed retained notification `id` from the centre.
    fn dismiss_history(&self, id: u64);

    /// The person dismissed every retained notification of app group `key` .
    fn dismiss_history_group(&self, key: &str) {
        let ids = self.log().group_ids(key);
        for id in ids {
            self.dismiss_history(id);
        }
    }

    /// Clear the retained history, reversibly.
    fn clear_history(&self);

    /// Restore the most recent Clear all while it is still reversible.
    fn undo_clear(&self);

    /// Viewer-local centre state (selection, folding, scrolling) changed and the host should
    /// repaint.
    fn changed(&self) {}
}

#[cfg(test)]
mod copy_lint_tests {
    /// The banners and the centre build every string from the catalogue.
    #[test]
    fn copy_builders_take_catalogue_strings_not_english_literals() {
        tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/notifications"))
            .min_files(2)
            .check();
    }
}
