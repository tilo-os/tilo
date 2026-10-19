//! Cursor chat and reactions: what people say beside their pointers.
//!
//! Talking, not chat: a message shows next to the pointer of the person
//! typing it, live while they type and for [`CURSOR_CHAT_LINGER`] after they
//! send it, then fades. Nothing keeps it. A message that is one emoji is a
//! reaction instead, floating up from the pointer ([`crate::motion::Float`]).
//!
//! This is the store a painter reads; transports fill it (the shell from
//! the live presence plane) and decide who may say what. Timing is the
//! receiver's own clock, so a sender's skewed clock cannot keep a message up.

use std::collections::HashMap;

use crate::motion::{Float, Transient};
use crate::permissions::UserId;
use crate::time::{Duration, Instant};
use crate::tokens::EffectiveMotion;

/// How long a sent message stays beside the pointer before it is gone.
pub const CURSOR_CHAT_LINGER: Duration = Duration::from_secs(5);

/// Longest message, in characters (the live plane's own bound).
pub const CURSOR_CHAT_MAX_CHARS: usize = 120;

/// How long a still pointer keeps its name beside it. The pointer itself
/// stays until it goes quiet ([`super::POINTER_IDLE`]); the name is there to
/// say who is moving, and steps aside once they stop.
pub const POINTER_NAME_LINGER: Duration = Duration::from_secs(2);

/// What one person is saying beside their pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Said {
    text: String,
    /// When it arrived here, to fade it in.
    shown: Instant,
    /// When they sent it; `None` while they are typing.
    sent: Option<Instant>,
}

/// A reaction playing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reaction {
    seq: u32,
    emoji: String,
    started: Instant,
}

/// A cursor-chat bubble as a painter draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatBubble {
    pub text: String,
    /// Still being typed: the bubble shows a caret after the text.
    pub typing: bool,
    pub opacity: f32,
}

/// A reaction as a painter draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReactionFloat {
    pub emoji: String,
    /// How far above the pointer it has risen, in logical pixels.
    pub rise: f64,
    pub opacity: f32,
}

/// Everyone's cursor chat and reactions in one presence store.
#[derive(Clone, Debug, Default)]
pub struct CursorChats {
    said: HashMap<UserId, Said>,
    reactions: HashMap<UserId, Reaction>,
    /// The last reaction counter heard from each person, kept after the
    /// reaction is over so a restated frame does not play it again.
    reaction_seen: HashMap<UserId, u32>,
}

impl CursorChats {
    /// `user` says `text` beside their pointer (`sent` once they pressed
    /// Enter), or stops saying anything with `None`. Returns whether what is
    /// shown changed.
    pub fn say(&mut self, user: &UserId, said: Option<(&str, bool)>, now: Instant) -> bool {
        let Some((text, sent)) = said.filter(|(text, _)| !text.trim().is_empty()) else {
            return self.said.remove(user).is_some();
        };
        let text: String = text.chars().take(CURSOR_CHAT_MAX_CHARS).collect();
        match self.said.get_mut(user) {
            Some(current) if current.text == text && current.sent.is_some() == sent => false,
            Some(current) => {
                // A sent message restarts its linger when it changes; a
                // restated one (a keepalive) does not.
                if sent && (current.sent.is_none() || current.text != text) {
                    current.sent = Some(now);
                } else if !sent {
                    current.sent = None;
                }
                current.text = text;
                true
            }
            None => {
                self.said.insert(
                    user.clone(),
                    Said {
                        text,
                        shown: now,
                        sent: sent.then_some(now),
                    },
                );
                true
            }
        }
    }

    /// `user` reacted with `emoji`, their `seq`th reaction. Plays once per
    /// new counter; returns whether it started.
    pub fn react(&mut self, user: &UserId, seq: u32, emoji: &str, now: Instant) -> bool {
        if emoji.is_empty() || self.reaction_seen.get(user) == Some(&seq) {
            return false;
        }
        self.reaction_seen.insert(user.clone(), seq);
        self.reactions.insert(
            user.clone(),
            Reaction {
                seq,
                emoji: emoji.to_owned(),
                started: now,
            },
        );
        true
    }

    /// Forget everything `user` said and reacted: they left, or are no
    /// longer shown to this person.
    pub fn forget(&mut self, user: &UserId) -> bool {
        let said = self.said.remove(user).is_some();
        let reacted = self.reactions.remove(user).is_some();
        self.reaction_seen.remove(user);
        said || reacted
    }

    /// The bubble to draw beside `user`'s pointer at `now`.
    #[must_use]
    pub fn bubble(
        &self,
        user: &UserId,
        now: Instant,
        motion: EffectiveMotion,
    ) -> Option<ChatBubble> {
        let said = self.said.get(user)?;
        let opacity = match said.sent {
            None => Transient::opacity(
                Transient::READOUT,
                (said.shown, now + CURSOR_CHAT_LINGER),
                now,
                motion,
            ),
            Some(sent) => Transient::opacity(
                Transient::READOUT,
                (said.shown, sent + CURSOR_CHAT_LINGER),
                now,
                motion,
            ),
        };
        (opacity > 0.0).then(|| ChatBubble {
            text: said.text.clone(),
            typing: said.sent.is_none(),
            opacity,
        })
    }

    /// The reaction to draw rising from `user`'s pointer at `now`.
    #[must_use]
    pub fn reaction(
        &self,
        user: &UserId,
        now: Instant,
        motion: EffectiveMotion,
    ) -> Option<ReactionFloat> {
        let reaction = self.reactions.get(user)?;
        let (rise, opacity) = Float::at(now.saturating_duration_since(reaction.started), motion)?;
        Some(ReactionFloat {
            emoji: reaction.emoji.clone(),
            rise,
            opacity,
        })
    }

    /// Whether `user` is saying something at `now`: their pointer stays and
    /// keeps its name while they do.
    #[must_use]
    pub fn talking(&self, user: &UserId, now: Instant) -> bool {
        self.said
            .get(user)
            .is_some_and(|said| said.sent.is_none_or(|sent| now < sent + CURSOR_CHAT_LINGER))
    }

    /// Drop messages that have lingered and reactions that are over.
    /// Returns whether anything went.
    pub fn prune(&mut self, now: Instant) -> bool {
        let before = self.said.len() + self.reactions.len();
        self.said
            .retain(|_, said| said.sent.is_none_or(|sent| now < sent + CURSOR_CHAT_LINGER));
        self.reactions
            .retain(|_, reaction| now.saturating_duration_since(reaction.started) < Float::LIFE);
        before != self.said.len() + self.reactions.len()
    }

    /// Whether anything is arriving, fading or floating at `now`, so the
    /// painter needs another frame. Asks no more of the motion than the
    /// slowest it can be: a host without the person's motion settings may
    /// paint a frame too many, never one too few.
    #[must_use]
    pub fn animating(&self, now: Instant) -> bool {
        let slow = crate::tokens::Motion::default().durations.slow();
        !self.reactions.is_empty()
            || self.said.values().any(|said| {
                now.saturating_duration_since(said.shown) < slow
                    || said.sent.is_some_and(|sent| {
                        let gone = sent + CURSOR_CHAT_LINGER;
                        now < gone && gone.saturating_duration_since(now) <= slow
                    })
            })
    }

    /// When the next change after `now` happens on its own (a sent message
    /// starts to fade), for a host that sleeps between frames.
    #[must_use]
    pub fn next_change(&self, now: Instant, motion: EffectiveMotion) -> Option<Instant> {
        self.said
            .values()
            .filter_map(|said| said.sent)
            .flat_map(|sent| {
                let gone = sent + CURSOR_CHAT_LINGER;
                [
                    Transient::exit_starts(Transient::READOUT, gone, motion),
                    gone,
                ]
            })
            .filter(|at| *at > now)
            .min()
    }
}

/// The emoji `text` is, when it is exactly one (surrounding spaces aside):
/// what makes a cursor-chat message a reaction rather than words.
#[must_use]
pub fn single_emoji(text: &str) -> Option<&str> {
    let text = text.trim();
    let mut graphemes = crate::text_break::graphemes(text);
    let first = graphemes.next()?;
    (graphemes.next().is_none() && crate::text_render::grapheme_is_emoji(first)).then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Motion;

    fn on() -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), false)
    }

    fn ada() -> UserId {
        UserId("peer@1".into())
    }

    /// Typed text shows live; a sent message lingers for five seconds from
    /// when it was sent (a restatement does not restart it), then is gone.
    #[test]
    fn a_message_shows_while_typed_and_lingers_after_it_is_sent() {
        let t0 = crate::time::now();
        let mut chats = CursorChats::default();
        assert!(chats.say(&ada(), Some(("hel", false)), t0));
        let later = t0 + Duration::from_secs(1);
        let typing = chats.bubble(&ada(), later, on()).unwrap();
        assert_eq!((typing.text.as_str(), typing.typing), ("hel", true));
        assert!((typing.opacity - 1.0).abs() < f32::EPSILON);
        assert!(
            chats.talking(&ada(), t0 + Duration::from_secs(60)),
            "typing has no deadline"
        );

        let sent = t0 + Duration::from_secs(2);
        assert!(chats.say(&ada(), Some(("hello", true)), sent));
        assert!(!chats.say(&ada(), Some(("hello", true)), sent + Duration::from_secs(3)));
        let shown = chats
            .bubble(&ada(), sent + Duration::from_secs(4), on())
            .unwrap();
        assert!(!shown.typing);
        let fading = chats
            .bubble(
                &ada(),
                sent + CURSOR_CHAT_LINGER - Duration::from_millis(50),
                on(),
            )
            .unwrap();
        assert!(fading.opacity < 1.0, "it fades at the end of its linger");
        assert!(
            chats
                .bubble(&ada(), sent + CURSOR_CHAT_LINGER, on())
                .is_none()
        );
        assert!(chats.prune(sent + CURSOR_CHAT_LINGER));
        assert!(!chats.talking(&ada(), sent + CURSOR_CHAT_LINGER));
    }

    /// Clearing or emptying the message takes the bubble away at once
    /// (Esc on the sender's side); whitespace is nothing to show.
    #[test]
    fn a_cancelled_or_blank_message_is_not_shown() {
        let t0 = crate::time::now();
        let mut chats = CursorChats::default();
        chats.say(&ada(), Some(("hi", false)), t0);
        assert!(chats.say(&ada(), None, t0));
        assert!(chats.bubble(&ada(), t0, on()).is_none());
        assert!(!chats.say(&ada(), Some(("   ", false)), t0));
        let long = "x".repeat(CURSOR_CHAT_MAX_CHARS + 10);
        chats.say(&ada(), Some((&long, false)), t0);
        assert_eq!(
            chats
                .bubble(&ada(), t0 + Duration::from_secs(1), on())
                .unwrap()
                .text
                .chars()
                .count(),
            CURSOR_CHAT_MAX_CHARS
        );
    }

    /// A reaction rises and fades over its life, plays once per counter,
    /// and does not rise under reduced motion.
    #[test]
    fn a_reaction_floats_up_once_and_is_gone_after_its_life() {
        let t0 = crate::time::now();
        let mut chats = CursorChats::default();
        assert!(chats.react(&ada(), 3, "\u{1f389}", t0));
        assert!(!chats.react(&ada(), 3, "\u{1f389}", t0 + Duration::from_millis(500)));
        let start = chats.reaction(&ada(), t0, on()).unwrap();
        let mid = chats.reaction(&ada(), t0 + Float::LIFE / 2, on()).unwrap();
        assert!(mid.rise > start.rise, "it rises");
        let late = chats
            .reaction(&ada(), t0 + Float::LIFE * 9 / 10, on())
            .unwrap();
        assert!(late.opacity < mid.opacity, "and fades");
        assert!(chats.reaction(&ada(), t0 + Float::LIFE, on()).is_none());
        let still = EffectiveMotion::new(Motion::default(), true);
        assert!(
            chats
                .reaction(&ada(), t0 + Float::LIFE / 2, still)
                .is_some_and(|float| float.rise == 0.0),
            "reduced motion: it fades in place"
        );
        assert!(chats.prune(t0 + Float::LIFE));
        assert!(
            !chats.react(&ada(), 3, "\u{1f389}", t0 + Float::LIFE),
            "seen stays"
        );
        assert!(chats.react(&ada(), 4, "\u{1f44d}", t0 + Float::LIFE));
    }

    /// Exactly one emoji is a reaction: a skin tone or a joined family is
    /// one; a word, two emoji or an emoji with words is a message.
    #[test]
    fn one_emoji_alone_is_a_reaction() {
        assert_eq!(single_emoji(" \u{1f44d} "), Some("\u{1f44d}"));
        assert_eq!(
            single_emoji("\u{1f44b}\u{1f3fd}"),
            Some("\u{1f44b}\u{1f3fd}")
        );
        assert_eq!(
            single_emoji("\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}"),
            Some("\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}")
        );
        assert_eq!(single_emoji("\u{2764}\u{fe0f}"), Some("\u{2764}\u{fe0f}"));
        for words in ["ok", "\u{1f44d}\u{1f44d}", "\u{1f44d} nice", "", "1", "#"] {
            assert_eq!(single_emoji(words), None, "{words:?}");
        }
    }
}
