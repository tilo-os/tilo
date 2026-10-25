//! What Files says when something needs the person: a change they may want
//! to take back, or a failure with a way on.
//!
//! It waits, and it does not expire. An outcome with a way on is an item in
//! [`ui::attention`] — the app that asks, what it is about, and exactly one
//! action — and one without is recorded in the ledger. Nothing here is said
//! in a notice with a lifetime: a message that leaves after eight seconds
//! teaches people not to read the ones that matter.

use std::path::PathBuf;

/// The verb prefix a waiting row's action carries: the app reads which
/// action by what follows it.
pub(crate) const NOTICE_VERB: &str = "files.notice";

/// What a notice's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    /// Take the change back: the window's Undo, the same one Ctrl+Z does.
    Undo,
    /// Choose another app to open `path` with.
    OpenWith(PathBuf),
    /// Stop the copy or move under way.
    CancelTransfer,
    /// Hand the received-file service the shared-file link `0` again.
    ReceiveAgain(String),
    /// Open the version history of `path`, where a change to it that
    /// failed can be made again.
    OpenHistory(PathBuf),
}

impl NoticeAction {
    fn label(&self) -> String {
        match self {
            Self::Undo => crate::i18n::t("files-notice-undo"),
            Self::OpenWith(_) => crate::i18n::t("files-notice-open-with"),
            Self::CancelTransfer => crate::i18n::t("files-notice-cancel"),
            Self::ReceiveAgain(_) => crate::i18n::t("files-notice-try-again"),
            Self::OpenHistory(_) => crate::i18n::t("files-notice-open-history"),
        }
    }

    /// The verb this action waits on: Files' own, so another app's row is
    /// never mistaken for this one.
    fn verb(&self) -> String {
        let kind = match self {
            Self::Undo => "undo",
            Self::OpenWith(_) => "open-with",
            Self::CancelTransfer => "cancel-transfer",
            Self::ReceiveAgain(_) => "receive-again",
            Self::OpenHistory(_) => "open-history",
        };
        format!("{NOTICE_VERB}.{kind}")
    }
}

/// Files, as the waiting stack and the ledger name it.
fn owner() -> ui::attention::Owner {
    ui::attention::Owner::new("so.tilo.files", "Files").with_icon(ui::icons::Icon::Folder)
}

#[derive(Clone, Debug)]
struct Shown {
    title: String,
    body: String,
    action: Option<NoticeAction>,
}

/// What Files last said, and where it waits.
#[derive(Debug, Default)]
pub(crate) struct Notice {
    shown: Option<Shown>,
    /// The row raised for it, and the action that row waits on: what the
    /// person pressed is read back by the row's identity.
    waiting: Option<(ui::attention::ItemId, NoticeAction)>,
    /// What was already said, so one outcome is recorded once.
    said: Option<String>,
}

impl Clone for Notice {
    fn clone(&self) -> Self {
        Self {
            shown: self.shown.clone(),
            waiting: None,
            said: self.said.clone(),
        }
    }
}

impl Notice {
    /// Show `title` (and `body`, when not empty) with `action`, replacing
    /// any notice already up.
    pub(crate) fn show(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        action: Option<NoticeAction>,
    ) {
        self.shown = Some(Shown {
            title: title.into(),
            body: body.into(),
            action,
        });
    }

    /// Show how far a copy or move is: the same notice updated in place, up
    /// until the transfer ends.
    pub(crate) fn show_progress(&mut self, title: String, body: String) {
        if let Some(shown) = &mut self.shown
            && shown.action == Some(NoticeAction::CancelTransfer)
        {
            shown.title = title;
            shown.body = body;
            return;
        }
        self.show(title, body, Some(NoticeAction::CancelTransfer));
    }

    /// Whether the notice is a transfer's progress.
    pub(crate) fn shows_progress(&self) -> bool {
        self.action() == Some(&NoticeAction::CancelTransfer)
    }

    pub(crate) fn dismiss(&mut self) -> bool {
        // What was said is said: if the same thing happens again after the
        // person answered it, it is news again.
        self.said = None;
        self.shown.take().is_some()
    }

    /// What the notice says, as one line: its title, then its reason.
    pub(crate) fn line(&self) -> Option<String> {
        let shown = self.shown.as_ref()?;
        Some(if shown.body.is_empty() {
            shown.title.clone()
        } else {
            format!("{}. {}", shown.title, shown.body)
        })
    }

    pub(crate) fn action(&self) -> Option<&NoticeAction> {
        self.shown.as_ref().and_then(|shown| shown.action.as_ref())
    }

    /// Whether an Undo notice is up: a new change makes it stale, since
    /// Undo would then take back the newer change instead.
    pub(crate) fn offers_undo(&self) -> bool {
        self.action() == Some(&NoticeAction::Undo)
    }

    /// Where the notice goes: a change to take back or a failure with a way
    /// on waits for the person, and anything else is recorded. Call on every
    /// sync; one outcome is said once.
    pub(crate) fn publish(&mut self) {
        let Some(line) = self.line() else {
            return;
        };
        if self.said.as_deref() == Some(line.as_str()) {
            return;
        }
        self.said = Some(line.clone());
        match self.action().cloned() {
            Some(action) => {
                let published = ui::attention::publish(
                    ui::attention::Notice::needs(
                        owner(),
                        line,
                        ui::attention::Action::new(action.verb(), action.label()),
                    )
                    .urgent(match action {
                        NoticeAction::CancelTransfer => ui::attention::Urgency::Noted,
                        _ => ui::attention::Urgency::Waiting,
                    }),
                );
                if let ui::attention::Published::Waiting(item) = published {
                    self.waiting = Some((item, action));
                }
            }
            None => {
                ui::attention::publish(ui::attention::Notice::did(owner(), line));
            }
        }
    }

    /// The row a person pressed, if it was this notice's: what it asked for,
    /// and the notice is down.
    pub(crate) fn pressed(&mut self, acted: &ui::attention::Acted) -> Option<NoticeAction> {
        let (item, action) = self.waiting.as_ref()?;
        if *item != acted.item || acted.action.verb != action.verb() {
            return None;
        }
        let action = action.clone();
        self.waiting = None;
        self.dismiss();
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change to take back waits for the person, with one action, and
    /// acting on that row takes it down. Nothing here has a lifetime.
    #[test]
    fn a_notice_waits_with_its_button_and_acting_takes_it_down() {
        ui::attention::clear();
        let mut notice = Notice::default();
        notice.show("Moved 2 items to Trash", "", Some(NoticeAction::Undo));
        notice.publish();
        let waiting = ui::attention::items();
        assert_eq!(waiting.len(), 1, "the change waits");
        assert_eq!(waiting[0].title(), "Moved 2 items to Trash");
        assert_eq!(waiting[0].owner().name, "Files");
        assert_eq!(waiting[0].action().label, t_undo());
        assert!(notice.offers_undo());
        let acted = ui::attention::take(waiting[0].id()).expect("it waits on a verb");
        assert_eq!(notice.pressed(&acted), Some(NoticeAction::Undo));
        assert_eq!(notice.line(), None, "acting on it takes it down");
        assert_eq!(
            notice.pressed(&acted),
            None,
            "a second press asks for nothing"
        );
        assert!(ui::attention::items().is_empty());
    }

    fn t_undo() -> String {
        crate::i18n::t("files-notice-undo")
    }

    /// An outcome with nothing to do about it is recorded, not thrown at the
    /// window: it does not expire, and it asks nobody.
    #[test]
    fn an_outcome_with_nothing_to_do_is_recorded() {
        ui::attention::clear();
        ui::attention::clear_ledger();
        let mut notice = Notice::default();
        notice.show("Couldn’t open “a.pdf”", "No app opens PDF files", None);
        notice.publish();
        assert!(ui::attention::items().is_empty(), "nothing to act on");
        assert!(
            ui::attention::entries()
                .iter()
                .any(|entry| entry.what().contains("Couldn’t open")),
            "the ledger keeps it"
        );
    }

    /// The same outcome is said once, however many frames pass.
    #[test]
    fn one_outcome_is_recorded_once() {
        ui::attention::clear();
        ui::attention::clear_ledger();
        let mut notice = Notice::default();
        notice.show("Copied 3 files", "", None);
        notice.publish();
        notice.publish();
        notice.publish();
        assert_eq!(ui::attention::entries().len(), 1);
    }
}
