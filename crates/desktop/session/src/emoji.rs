//! The emoji card's session half: the caret of the field the person types into, and the emoji they
//! pick written into that field.
//! The card itself is the toolkit's ([`ui::widget::EmojiPicker`]), the one every app opens; the
//! chrome frames it and places it ([`tilo_session_core::desktop::emoji`]). It never writes into an application.
//! The chrome hands what was picked to its [`EmojiLink`], and this module — beside dictation the
//! one place the session writes text the person did not type — commits each pick through the
//! compositor's text input, and says where the focused field's caret stands so the card opens
//! against it.
//! How often each emoji was used is the toolkit's own, kept in the person's settings resource
//! ([`ui::emoji_catalogue`]): the card counts a pick as it is made, and a field that keeps its
//! content secret takes that count back. It is the person's, so every device of theirs opens the
//! picker with the emoji they use most, not with what one machine happened to see.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use tilo_compositor::Compositor;
use tilo_session_core::desktop::emoji::{EmojiLink, EmojiPlace};
use ui::kurbo::Rect;

/// How long a pick waits for the field it belongs to to take text again: the card takes the
/// keyboard while it is open, so the field it was opened from gets it back only once the card has
/// closed.
const DELIVERY_WINDOW: Duration = Duration::from_secs(2);

/// A pick waiting to reach a field.
struct Pending {
    text: String,
    /// The moment the pick gives up.
    deadline: Instant,
}

/// The emoji card as the session hosts it: the chrome's link, and the pick on its way into the
/// field it was chosen from.
pub(crate) struct Emoji {
    link: EmojiLink,
    /// At most one: a newer pick replaces one that has not arrived yet.
    pending: RefCell<Option<Pending>>,
}

/// How often a person used each emoji, kept in their settings: the record every device of theirs
/// reads and writes, in place of a file on one machine.
struct PersonUses;

impl ui::emoji_catalogue::UsesStore for PersonUses {
    fn load(&self) -> Vec<ui::emoji_catalogue::Used> {
        settings_store::source()
            .and_then(|source| settings_store::emoji::used(&source.load()))
            .and_then(|used| serde_json::from_str(&used).ok())
            .unwrap_or_default()
    }

    fn save(&self, used: &[ui::emoji_catalogue::Used]) {
        let Ok(used) = serde_json::to_string(used) else {
            return;
        };
        let origin = settings_store::ChangeOrigin::local("emoji.used");
        tilo_failure::ignored!(
            "the picker's emoji counts are a convenience: a write that does not reach the \
             person's settings leaves them as they were, and the next pick writes again",
            settings_store::emoji::keep(&used, &origin).ok()
        );
    }
}

impl Emoji {
    /// Mount the card's session half, keeping the emoji's counts in the person's settings.
    pub(crate) fn mount() -> Self {
        ui::emoji_catalogue::keep_uses_in(Box::new(PersonUses));
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
    /// `origin` is where the shell's own coordinates begin on the desktop,
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

    /// Write the pick in flight into the focused field, which reports back whether it keeps its
    /// content secret — a secret field's emoji is never remembered. A field that does not take text
    /// yet is asked again next pass, until the pick's window has passed.
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

    /// Where the card opens: the caret of the field the person types into and the display it stands
    /// on, in the chrome's coordinates. A field that reports no caret, or whose caret stands on no
    /// known display, leaves the display to the chrome, which centres the card on the one the
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The picker's recents are the person's settings: what one device's settings say is what
    /// every device's picker opens with, and a pick is written back there rather than to a file
    /// on this machine.
    #[test]
    fn the_pickers_recents_are_the_persons_settings() {
        let held = serde_json::Map::from_iter([(
            settings_store::emoji::SECTION.to_owned(),
            serde_json::json!({ "recent": ["🎉"] }),
        )]);
        let source = std::sync::Arc::new(settings_store::HostDocument::new(held, |_| Ok(())));
        let _scoped =
            settings_store::scoped_source(Some(std::sync::Arc::clone(&source)
                as std::sync::Arc<dyn settings_store::SettingsSource>));
        ui::emoji_catalogue::keep_recents_in(Box::new(PersonRecents));
        assert_eq!(
            ui::emoji_catalogue::recents(),
            ["🎉"],
            "the picker reads the settings, not this device's own list"
        );

        ui::emoji_catalogue::record_recent("🚀");
        assert_eq!(
            settings_store::emoji::recent(&source.load()),
            ["🚀", "🎉"],
            "a pick is written to the person's settings, newest first"
        );
    }
}
