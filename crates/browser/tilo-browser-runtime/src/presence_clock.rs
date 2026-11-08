//! The little a page times of the workspace's presence itself: how long a
//! cursor-chat bubble lingers and fades, and how a reaction floats up. What
//! the members say comes from the board ([`crate::peer`]); the board says
//! what and when it was said, and this turns that into what a frame draws,
//! on the page's own clock: a bubble and a reaction start animating here,
//! when they arrived.

use std::collections::HashMap;

/// A sent cursor-chat message stays this long, then fades.
pub(crate) const CHAT_LINGER_MS: f64 = 5000.0;
/// How long a sent message takes to fade at the end of its linger.
pub(crate) const CHAT_FADE_MS: f64 = 300.0;
/// How long a reaction floats up from the pointer before it is gone.
pub(crate) const REACTION_MS: f64 = 1200.0;
/// How far a reaction rises over its life, in surface pixels.
const REACTION_RISE_PX: f64 = 48.0;

/// How visible a member's cursor-chat bubble is (0 hidden, 1 fully shown):
/// fully while it is being typed, then for [`CHAT_LINGER_MS`] after it was
/// sent, fading over the last [`CHAT_FADE_MS`].
fn chat_visible(text: &str, sent: bool, sent_at: Option<f64>, at: f64) -> f32 {
    if !sent {
        return if text.is_empty() { 0.0 } else { 1.0 };
    }
    let Some(sent_at) = sent_at else {
        return 0.0;
    };
    let left = sent_at + CHAT_LINGER_MS - at;
    if left <= 0.0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a fraction between zero and one"
    )]
    let shown = (left / CHAT_FADE_MS).min(1.0) as f32;
    shown
}

/// A reaction `age` milliseconds after it arrived: how far it has risen and
/// how opaque it is, while it floats up (decelerating, fading over the second
/// half of its life). `None` before and after.
fn reaction_at(age: f64) -> Option<(f64, f32)> {
    if !(0.0..REACTION_MS).contains(&age) {
        return None;
    }
    let t = age / REACTION_MS;
    let rise = REACTION_RISE_PX * (1.0 - (1.0 - t) * (1.0 - t));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a fraction between zero and one"
    )]
    let opacity = (if t < 0.5 { 1.0 } else { (1.0 - t) * 2.0 }) as f32;
    Some((rise, opacity))
}

/// What a mount read of the board last frame, and the little it times
/// itself: a bubble and a reaction start their animation on this page's
/// clock, when they arrived here.
#[derive(Default)]
pub(crate) struct View {
    /// How many times the board's contents had changed when it was last read.
    #[cfg(target_arch = "wasm32")]
    pub(crate) revision: u64,
    /// Who was here, by publisher.
    #[cfg(target_arch = "wasm32")]
    pub(crate) seen: Vec<String>,
    /// Who a caret is drawn for, by publisher: a member who stops selecting
    /// has theirs taken away.
    #[cfg(target_arch = "wasm32")]
    pub(crate) carets: Vec<String>,
    /// When each member's chat said what, by publisher: the text and when it
    /// changed here.
    chat: HashMap<String, (String, f64)>,
    /// When each member's latest reaction arrived here: its count, and when.
    reaction: HashMap<String, (u32, f64)>,
    /// Keep drawing until then: the newest pointer heard plus the settle
    /// window, so it eases to rest.
    #[cfg(target_arch = "wasm32")]
    pub(crate) motion_until: f64,
    /// When a silent member's pointer has to leave the screen, as of the
    /// wake already asked for.
    #[cfg(target_arch = "wasm32")]
    pub(crate) wake_at: Option<f64>,
}

impl View {
    /// The bubble beside `publisher`'s pointer now, if any, with where its
    /// animation stands: its words, whether they are still being typed, and
    /// how opaque it is.
    pub(crate) fn chat(
        &mut self,
        publisher: &str,
        said: Option<(&str, bool)>,
        now: f64,
    ) -> Option<(String, bool, f32)> {
        let (text, sent) = said?;
        let sent_at = match self.chat.get(publisher) {
            Some((last, at)) if last == text => *at,
            _ => {
                self.chat
                    .insert(publisher.to_owned(), (text.to_owned(), now));
                now
            }
        };
        let opacity = chat_visible(text, sent, Some(sent_at), now);
        (opacity > 0.0).then(|| (text.to_owned(), !sent, opacity))
    }

    /// The reaction above `publisher`'s pointer now, if one is in flight: its
    /// emoji, how far it has risen and how opaque it is.
    pub(crate) fn reaction(
        &mut self,
        publisher: &str,
        said: Option<(u32, &str)>,
        now: f64,
    ) -> Option<(String, f64, f32)> {
        let (seq, emoji) = said?;
        let at = match self.reaction.get(publisher) {
            Some((last, at)) if *last == seq => *at,
            _ => {
                self.reaction.insert(publisher.to_owned(), (seq, now));
                now
            }
        };
        reaction_at(now - at).map(|(rise, opacity)| (emoji.to_owned(), rise, opacity))
    }

    /// Whether a bubble or reaction is still animating beside `publisher`.
    pub(crate) fn animating(&self, publisher: &str, now: f64) -> bool {
        let chat = self
            .chat
            .get(publisher)
            .is_some_and(|(_, at)| now < at + CHAT_LINGER_MS + CHAT_FADE_MS);
        let reaction = self
            .reaction
            .get(publisher)
            .is_some_and(|(_, at)| now < at + REACTION_MS);
        chat || reaction
    }

    /// Whether a shown pointer is still easing towards where it landed: it
    /// was heard within the settle window, so the next frame draws it further
    /// on.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn settling(&self, now: f64) -> bool {
        now < self.motion_until
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fraction as a test compares it.
    fn near(shown: f32, expected: f32) -> bool {
        (shown - expected).abs() < 1e-6
    }

    #[test]
    fn a_typed_bubble_is_whole_and_a_sent_one_lingers_then_fades() {
        assert!(near(chat_visible("hi", false, None, 100.0), 1.0));
        assert!(near(chat_visible("", false, None, 100.0), 0.0));
        assert!(near(chat_visible("hi", true, Some(1_000.0), 1_500.0), 1.0));
        let fading = chat_visible("hi", true, Some(1_000.0), 1_000.0 + CHAT_LINGER_MS - 150.0);
        assert!(near(fading, 0.5), "{fading}");
        assert!(near(
            chat_visible("hi", true, Some(1_000.0), 1_000.0 + CHAT_LINGER_MS),
            0.0
        ));
    }

    #[test]
    fn a_reaction_rises_decelerating_and_fades_over_its_second_half() {
        assert_eq!(reaction_at(-1.0), None);
        let (rise, opacity) = reaction_at(0.0).unwrap();
        assert!(rise.abs() < 1e-9 && near(opacity, 1.0));
        let (rise, opacity) = reaction_at(REACTION_MS / 2.0).unwrap();
        assert!((rise - REACTION_RISE_PX * 0.75).abs() < 1e-9, "{rise}");
        assert!(near(opacity, 1.0), "{opacity}");
        let (rise, opacity) = reaction_at(REACTION_MS * 0.75).unwrap();
        assert!((rise - REACTION_RISE_PX * 0.9375).abs() < 1e-9, "{rise}");
        assert!(near(opacity, 0.5), "{opacity}");
        assert_eq!(reaction_at(REACTION_MS), None);
    }

    #[test]
    fn a_bubble_and_a_reaction_are_timed_from_when_they_arrived_here() {
        let mut view = View::default();
        assert!(view.chat("ada", Some(("hi", true)), 5_000.0).is_some());
        assert!(view.animating("ada", 5_100.0));
        // The same words said again are the same bubble, not a new one.
        assert!(view.chat("ada", Some(("hi", true)), 6_000.0).is_some());
        assert!(!view.animating("ada", 5_000.0 + CHAT_LINGER_MS + CHAT_FADE_MS));
        assert!(view.reaction("ada", Some((2, "🎉")), 7_000.0).is_some());
        assert!(view.animating("ada", 7_000.0 + REACTION_MS - 1.0));
        assert!(
            !view.animating("ada", 5_000.0 + CHAT_LINGER_MS + CHAT_FADE_MS + REACTION_MS),
            "both are over: nothing is drawn for this member again"
        );
    }
}
