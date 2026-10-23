//! The one notice Files shows when something needs the person: a change
//! they may want to take back, or a failure with a way on.
//!
//! Everything else stays quiet. A change the listing already shows (a new
//! folder, a rename, a copy put on the clipboard) says nothing; neutral
//! news is the transient status bubble's. A notice is the window bar's
//! ([`ui::window_toolbar::notify`]), with at most one action beside it
//! there; it goes away by itself. A notice with an action stays for
//! [`LIFETIME`]; one without leaves after the shared notice lifetime
//! ([`ui::widget::toast::NOTICE_LIFETIME`]), as every such notice does. A
//! transfer's progress stays until the transfer ends.

use std::path::PathBuf;

use ui::time::{Duration, Instant};
use ui::widget::toast::ToastKind;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

/// How long a notice with an action stays up.
const LIFETIME: Duration = Duration::from_secs(8);

/// The bar action id of a notice's button.
pub(crate) const NOTICE_ACTION: &str = "files-notice-action";

/// What a notice's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    /// Take the change back: the window's Undo, the same one Ctrl+Z does.
    Undo,
    /// Choose another app to open `path` with.
    OpenWith(PathBuf),
    /// Stop the copy or move under way.
    CancelTransfer,
}

impl NoticeAction {
    fn label(&self) -> String {
        match self {
            Self::Undo => crate::i18n::t("files-notice-undo"),
            Self::OpenWith(_) => crate::i18n::t("files-notice-open-with"),
            Self::CancelTransfer => crate::i18n::t("files-notice-cancel"),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Undo => "undo-2",
            Self::OpenWith(_) => "app-window",
            Self::CancelTransfer => "x",
        }
    }
}

#[derive(Clone, Debug)]
struct Shown {
    title: String,
    body: String,
    kind: ToastKind,
    action: Option<NoticeAction>,
    /// When the notice went up.
    since: Instant,
}

/// The notice currently up, if any, and what the window's bar says of it.
#[derive(Debug, Default)]
pub(crate) struct Notice {
    shown: Option<Shown>,
    published: Option<ToolbarNotice>,
}

/// A copy keeps the message but not the bar's notice: the original took it
/// up and takes it down.
impl Clone for Notice {
    fn clone(&self) -> Self {
        Self {
            shown: self.shown.clone(),
            published: None,
        }
    }
}

/// A notice gone with its window leaves the window's bar with it.
impl Drop for Notice {
    fn drop(&mut self) {
        if let Some(notice) = self.published.take() {
            ui::window_toolbar::withdraw(&notice);
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
        kind: ToastKind,
        action: Option<NoticeAction>,
    ) {
        self.shown = Some(Shown {
            title: title.into(),
            body: body.into(),
            kind,
            action,
            since: ui::time::now(),
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
        self.show(
            title,
            body,
            ToastKind::Info,
            Some(NoticeAction::CancelTransfer),
        );
    }

    /// How long the notice stays up: one that offers nothing to do leaves
    /// as every such notice does.
    fn lifetime(&self) -> Duration {
        if self.action().is_some() {
            LIFETIME
        } else {
            ui::widget::toast::NOTICE_LIFETIME
        }
    }

    /// Whether the notice is a transfer's progress.
    pub(crate) fn shows_progress(&self) -> bool {
        self.action() == Some(&NoticeAction::CancelTransfer)
    }

    pub(crate) fn dismiss(&mut self) -> bool {
        self.shown.take().is_some()
    }

    pub(crate) fn is_shown(&self) -> bool {
        self.shown.is_some()
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

    /// Take the notice down once its lifetime is over; whether it did.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let over = self.shown.as_ref().is_some_and(|shown| {
            shown.action != Some(NoticeAction::CancelTransfer)
                && !ui::time::wall_frozen()
                && now.saturating_duration_since(shown.since) > self.lifetime()
        });
        over && self.dismiss()
    }

    /// When the notice expires, for the widget's wake deadline.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if ui::time::wall_frozen() || self.shows_progress() {
            return None;
        }
        self.shown
            .as_ref()
            .map(|shown| shown.since + self.lifetime())
    }

    /// The notice as the window's bar says it: its line, and its button.
    fn as_toolbar_notice(&self) -> Option<ToolbarNotice> {
        let shown = self.shown.as_ref()?;
        let notice = ToolbarNotice::new(self.line()?, shown.kind);
        Some(match &shown.action {
            Some(action) => notice.action(ToolbarAction::new(
                NOTICE_ACTION,
                action.icon(),
                action.label(),
            )),
            None => notice,
        })
    }

    /// Keep the window's bar saying the notice while it is up, and take it
    /// down once it has gone; call on every sync. A notice shown since from
    /// elsewhere in the window stays.
    pub(crate) fn publish(&mut self) {
        let want = self.as_toolbar_notice();
        if self.published == want {
            return;
        }
        if let Some(gone) = self.published.take() {
            ui::window_toolbar::withdraw(&gone);
        }
        if let Some(notice) = want {
            ui::window_toolbar::notify(Some(notice.clone()));
            self.published = Some(notice);
        }
    }

    /// The notice's button was pressed in the bar: what it asked for, and
    /// the notice is down.
    pub(crate) fn press(&mut self) -> Option<NoticeAction> {
        let action = self.action().cloned()?;
        self.dismiss();
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_is_the_window_bars_with_its_button_and_a_press_takes_it_down() {
        let mut notice = Notice::default();
        notice.show(
            "Moved 2 items to Trash",
            "",
            ToastKind::Success,
            Some(NoticeAction::Undo),
        );
        notice.publish();
        let shown = ui::window_toolbar::notice().expect("the bar says it");
        assert_eq!(shown.text, "Moved 2 items to Trash");
        assert_eq!(shown.kind, ToastKind::Success);
        assert_eq!(
            shown.action.map(|action| action.id),
            Some(NOTICE_ACTION.to_owned()),
            "Undo is the bar's action while the notice shows"
        );
        assert!(notice.offers_undo());
        assert_eq!(notice.press(), Some(NoticeAction::Undo));
        assert!(!notice.is_shown(), "acting on a notice takes it down");
        assert_eq!(notice.press(), None, "a second press asks for nothing");
        notice.publish();
        assert_eq!(ui::window_toolbar::notice(), None);
    }

    #[test]
    fn a_notice_expires_after_its_lifetime() {
        let mut notice = Notice::default();
        notice.show(
            "Couldn’t open “a.pdf”",
            "No app opens PDF files",
            ToastKind::Error,
            None,
        );
        let shown_at = ui::time::now();
        assert!(!notice.expire(shown_at));
        if !ui::time::wall_frozen() {
            assert!(
                notice.expire(
                    shown_at + ui::widget::toast::NOTICE_LIFETIME + Duration::from_millis(1)
                )
            );
            assert!(!notice.is_shown());
        }
    }
}
