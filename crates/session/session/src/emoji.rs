//! The emoji card's session half: the caret of the field the person types
//! into, and the emoji they pick written into that field.
//!
//! The card itself is the toolkit's ([`ui::widget::EmojiPicker`]), the one
//! every app opens; the chrome frames it and places it
//! ([`tilo_desktop::emoji`]). It never writes into an application. The chrome
//! hands what was picked to its [`EmojiLink`], and this module — beside
//! dictation the one place the session writes text the person did not type —
//! commits each pick through the compositor's text input, and says where the
//! focused field's caret stands so the card opens against it.
//!
//! The recents are the toolkit's own, kept on the device
//! ([`ui::emoji_catalogue`]): the card remembers a pick as it is made, and a
//! field that keeps its content secret takes that memory back.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use tilo_compositor::Compositor;
use tilo_desktop::emoji::{EmojiLink, EmojiPlace};
use ui::kurbo::Rect;

/// How long a pick waits for the field it belongs to to take text again: the
/// card takes the keyboard while it is open, so the field it was opened from
/// gets it back only once the card has closed.
const DELIVERY_WINDOW: Duration = Duration::from_secs(2);

/// A pick waiting to reach a field.
struct Pending {
    text: String,
    /// The moment the pick gives up.
    deadline: Instant,
}

/// The emoji card as the session hosts it: the chrome's link, and the pick on
/// its way into the field it was chosen from.
pub(crate) struct Emoji {
    link: EmojiLink,
    /// At most one: a newer pick replaces one that has not arrived yet.
    pending: RefCell<Option<Pending>>,
}

impl Emoji {
    /// Mount the card's session half, keeping its recents on this device.
    pub(crate) fn mount() -> Self {
        ui::emoji_catalogue::keep_recents_on_the_device();
        Self {
            link: EmojiLink::default(),
            pending: RefCell::new(None),
        }
    }

    /// The chrome's end of the link: what it opens against and what it picks.
    pub(crate) fn link(&self) -> EmojiLink {
        self.link.clone()
    }

    /// Carry the person's picks into the field they were typing in, and tell
    /// the chrome where that field's caret stands.
    ///
    /// `origin` is where the desk's own coordinates begin on the desktop,
    /// which the chrome's are measured from.
    pub(crate) fn step(&self, compositor: &mut Compositor, origin: (i32, i32), now: Instant) {
        for text in self.link.take_picks() {
            *self.pending.borrow_mut() = Some(Pending {
                text,
                deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
            });
        }
        self.deliver(compositor, now);
        self.link.place_at(Self::place(compositor, origin));
    }

    /// Write the pick in flight into the focused field, which reports back
    /// whether it keeps its content secret — a secret field's emoji is never
    /// remembered. A field that does not take text yet is asked again next
    /// pass, until the pick's window has passed.
    fn deliver(&self, compositor: &mut Compositor, now: Instant) {
        let Some(pending) = self.pending.borrow_mut().take() else {
            return;
        };
        if let Some(sensitive) = compositor.insert_active_text(&pending.text) {
            if sensitive {
                ui::emoji_catalogue::forget_recent(&pending.text);
            }
        } else if now < pending.deadline {
            *self.pending.borrow_mut() = Some(pending);
        } else {
            tracing::debug!("no field took the picked emoji in time");
        }
    }

    /// Where the card opens: the caret of the field the person types into and
    /// the display it stands on, in the chrome's coordinates. A field that
    /// reports no caret, or whose caret stands on no known display, leaves
    /// the display to the chrome, which centres the card on the one the
    /// person is working on.
    fn place(compositor: &Compositor, origin: (i32, i32)) -> EmojiPlace {
        let (ox, oy) = origin;
        let outputs: Vec<Rect> = compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .filter_map(|output| output.rect)
            .map(|(x, y, width, height)| {
                let (x, y) = (x.saturating_sub(ox), y.saturating_sub(oy));
                Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x.saturating_add(width)),
                    f64::from(y.saturating_add(height)),
                )
            })
            .collect();
        let caret = compositor.text_caret().map(|caret| {
            Rect::new(
                caret.x - f64::from(ox),
                caret.y - f64::from(oy),
                caret.x - f64::from(ox) + caret.width,
                caret.y - f64::from(oy) + caret.height,
            )
        });
        let viewport = caret.and_then(|caret| {
            outputs
                .iter()
                .copied()
                .find(|display| display.contains(caret.center()))
        });
        EmojiPlace { viewport, caret }
    }
}
