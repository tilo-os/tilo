//! The one card Files shows over its content when something needs the
//! person: a change they may want to take back, or a failure with a way on.
//!
//! Everything else stays quiet. A change the listing already shows (a new
//! folder, a rename, a copy put on the clipboard) says nothing; neutral
//! news is the transient status bubble's. A notice is the shared toast card
//! ([`ui::widget::toast::paint_transient`]) with at most one action and a
//! close button, at the bottom of the file view; it goes away by itself,
//! but not while the pointer rests on it.

use std::path::PathBuf;

use ui::time::{Duration, Instant};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastCardHit, ToastKind};
use vello::kurbo::{Point, Rect};

/// How long a notice stays up once the pointer is not on it.
const LIFETIME: Duration = Duration::from_secs(8);

/// What a notice's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    /// Take the change back: the window's Undo, the same one Ctrl+Z does.
    Undo,
    /// Choose another app to open `path` with.
    OpenWith(PathBuf),
}

impl NoticeAction {
    fn label(&self) -> String {
        match self {
            Self::Undo => crate::i18n::t("files-notice-undo"),
            Self::OpenWith(_) => crate::i18n::t("files-notice-open-with"),
        }
    }
}

/// Which part of the card the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Card,
    Action,
    Close,
}

/// What a click on the card asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeClick {
    /// The click landed on the card but asked for nothing.
    Absorbed,
    Dismissed,
    Action(NoticeAction),
}

#[derive(Clone, Debug)]
struct Shown {
    title: String,
    body: String,
    kind: ToastKind,
    action: Option<NoticeAction>,
    /// When the lifetime started: the notice was shown or the pointer left.
    since: Instant,
}

/// The notice currently up, if any, with the geometry it was painted at.
#[derive(Clone, Debug, Default)]
pub(crate) struct Notice {
    shown: Option<Shown>,
    hit: ToastCardHit,
    hovered: Option<Part>,
    /// The part a press on the card landed on; its release acts only there.
    pressed: Option<Part>,
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
        self.hit = ToastCardHit::default();
    }

    pub(crate) fn dismiss(&mut self) -> bool {
        self.hovered = None;
        self.pressed = None;
        self.hit = ToastCardHit::default();
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
            self.hovered.is_none()
                && !ui::time::wall_frozen()
                && now.saturating_duration_since(shown.since) > LIFETIME
        });
        over && self.dismiss()
    }

    /// When the notice expires, for the widget's wake deadline.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if ui::time::wall_frozen() || self.hovered.is_some() {
            return None;
        }
        self.shown.as_ref().map(|shown| shown.since + LIFETIME)
    }

    /// Where the action button was painted (`Rect::ZERO` when it wasn't).
    #[cfg(test)]
    pub(crate) fn action_rect(&self) -> Rect {
        self.hit.primary_action_rect
    }

    /// Whether `point` is on the painted card.
    pub(crate) fn contains(&self, point: Point) -> bool {
        self.shown.is_some() && self.hit.card_rect.contains(point)
    }

    /// Follow the pointer; whether what is hovered changed.
    pub(crate) fn pointer_moved(&mut self, point: Point) -> bool {
        let part = self.part_at(point);
        if part == self.hovered {
            return false;
        }
        if part.is_none()
            && let Some(shown) = &mut self.shown
        {
            // The pointer kept it up; give the person the full time again.
            shown.since = ui::time::now();
        }
        self.hovered = part;
        true
    }

    /// A press at `point`; whether it landed on the card (and so is the
    /// card's, not the file view's under it).
    pub(crate) fn press(&mut self, point: Point) -> bool {
        self.pressed = self.part_at(point);
        self.pressed.is_some()
    }

    /// The release of a press: what it asked for, or `None` when the press
    /// wasn't on the card.
    pub(crate) fn release(&mut self, point: Point) -> Option<NoticeClick> {
        let pressed = self.pressed.take()?;
        let click = match self.part_at(point) {
            Some(Part::Close) if pressed == Part::Close => NoticeClick::Dismissed,
            Some(Part::Action) if pressed == Part::Action => self
                .action()
                .cloned()
                .map_or(NoticeClick::Absorbed, NoticeClick::Action),
            _ => NoticeClick::Absorbed,
        };
        Some(self.activate(click))
    }

    /// Activate the action (or the close button) without a pointer, as
    /// assistive technology does.
    pub(crate) fn activate(&mut self, click: NoticeClick) -> NoticeClick {
        if click != NoticeClick::Absorbed {
            self.dismiss();
        }
        click
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        self.shown.as_ref()?;
        let hit = &self.hit;
        if hit.primary_action_rect.contains(point) {
            Some(Part::Action)
        } else if hit.close_rect.contains(point) {
            Some(Part::Close)
        } else if hit.card_rect.contains(point) {
            Some(Part::Card)
        } else {
            None
        }
    }

    /// Paint the notice at the bottom of `area`.
    pub(crate) fn paint(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        area: Rect,
    ) {
        let Some(shown) = &self.shown else {
            self.hit = ToastCardHit::default();
            return;
        };
        let label = shown.action.as_ref().map(NoticeAction::label);
        let mut card = ToastCard::new(&shown.title, shown.kind)
            .body(&shown.body)
            .closable(true)
            .close_hovered(self.hovered == Some(Part::Close));
        if let Some(label) = &label {
            card = card.action(label, self.hovered == Some(Part::Action));
        }
        self.hit = ui::widget::toast::paint_transient(
            scene,
            transform,
            text_renderer,
            theme,
            area,
            ToastAnchor::BottomCenter,
            &card,
        );
    }

    /// The notice as a live region: a failure interrupts, anything else
    /// waits its turn.
    pub(crate) fn access_node(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Politeness, Role};
        let shown = self.shown.as_ref()?;
        Some(
            AccessNode::new(Role::StaticText)
                .with_name(self.line()?)
                .with_bounds(self.hit.card_rect)
                .with_live(if shown.kind == ToastKind::Error {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_answers_clicks_on_its_parts_and_stays_while_hovered() {
        let mut notice = Notice::default();
        notice.show(
            "Moved 2 items to Trash",
            "",
            ToastKind::Success,
            Some(NoticeAction::Undo),
        );
        notice.hit = ToastCardHit {
            card_rect: Rect::new(0.0, 0.0, 300.0, 48.0),
            close_rect: Rect::new(270.0, 10.0, 290.0, 30.0),
            secondary_action_rect: Rect::ZERO,
            primary_action_rect: Rect::new(200.0, 10.0, 260.0, 38.0),
        };
        assert!(notice.offers_undo());
        assert!(notice.press(Point::new(10.0, 10.0)));
        assert_eq!(
            notice.release(Point::new(10.0, 10.0)),
            Some(NoticeClick::Absorbed)
        );
        assert!(notice.is_shown());
        assert!(!notice.press(Point::new(400.0, 10.0)));
        assert_eq!(notice.release(Point::new(400.0, 10.0)), None);
        // A press that slides off the button onto the card does nothing.
        assert!(notice.press(Point::new(220.0, 20.0)));
        assert_eq!(
            notice.release(Point::new(10.0, 10.0)),
            Some(NoticeClick::Absorbed)
        );
        assert!(notice.is_shown());

        // While the pointer rests on it, it never expires.
        assert!(notice.pointer_moved(Point::new(20.0, 20.0)));
        let later = ui::time::now() + LIFETIME + LIFETIME;
        assert!(!notice.expire(later));
        assert_eq!(notice.deadline(), None);

        assert!(notice.press(Point::new(220.0, 20.0)));
        assert_eq!(
            notice.release(Point::new(220.0, 20.0)),
            Some(NoticeClick::Action(NoticeAction::Undo))
        );
        assert!(!notice.is_shown(), "acting on a notice takes it down");
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
            assert!(notice.expire(shown_at + LIFETIME + Duration::from_millis(1)));
            assert!(!notice.is_shown());
        }
    }
}
