//! Other people's pointers that a host has already placed in its own
//! surface's coordinates (the browser page, which interpolates each member's
//! live pointer itself): drawn by the frame with the same arrow, name tag
//! and colour a native desktop draws them with, never by a second renderer.

use super::{ChatBubble, PeerCursor, PresenceCtx, ReactionFloat};
use crate::permissions::UserId;
use vello::peniko::Color;

/// One placed pointer as the host hands it over.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedPointer {
    /// Who points: the id presence names them by (their live publisher).
    pub user: UserId,
    /// Where, in the surface's logical pixels.
    pub at: (f64, f64),
    /// The name their own presence gives, for someone presence does not
    /// name yet.
    pub name: Option<String>,
    /// Whether their presence says they are an agent.
    pub agent: bool,
    /// What an agent is doing, shown after its name.
    pub activity: Option<String>,
    /// What they are saying beside their pointer.
    pub chat: Option<ChatBubble>,
    /// A reaction floating up from their pointer.
    pub reaction: Option<ReactionFloat>,
}

impl PresenceCtx {
    /// `placed` as the frame draws it: in the colour and under the name
    /// this presence knows the person by, so a pointer always matches their
    /// avatar. Someone presence has no name for yet goes by the one their
    /// own presence gives, else the start of their id, never the whole key.
    #[must_use]
    pub fn placed_cursor(&self, placed: PlacedPointer) -> PeerCursor {
        let is_agent = placed.agent || self.is_agent(&placed.user);
        // An agent's tag carries the agent mark, so its name stands alone.
        let known = if is_agent {
            self.names.get(&placed.user).cloned()
        } else {
            self.names
                .contains_key(&placed.user)
                .then(|| self.display_name(&placed.user))
        };
        let name = known
            .or_else(|| {
                placed
                    .name
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| placed.user.0.chars().take(8).collect());
        let label = match placed.activity.as_deref().map(str::trim) {
            Some(activity) if is_agent && !activity.is_empty() => format!("{name} · {activity}"),
            _ => name,
        };
        let (r, g, b) = self.color_for(&placed.user);
        PeerCursor {
            pos: placed.at,
            label,
            color: Color::from_rgb8(r, g, b),
            is_agent,
            opacity: 1.0,
            name_opacity: 1.0,
            chat: placed.chat,
            reaction: placed.reaction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(user: &str) -> PlacedPointer {
        PlacedPointer {
            user: UserId(user.into()),
            at: (40.0, 50.0),
            name: None,
            agent: false,
            activity: Some("Reading the notes".into()),
            chat: None,
            reaction: None,
        }
    }

    /// The pointer wears the colour and name presence gives the person, so
    /// it reads as the same person as their avatar.
    #[test]
    fn a_placed_pointer_wears_the_persons_colour_and_name() {
        let mut presence = PresenceCtx::empty();
        let ada = UserId("ada-device".into());
        presence.set_display_name(ada.clone(), "Ada");
        presence.set_color_override(ada.clone(), "#38b8f6");
        let cursor = presence.placed_cursor(placed("ada-device"));
        assert_eq!(
            cursor.label, "Ada",
            "a person's activity is not theirs to show"
        );
        assert_eq!(cursor.color, Color::from_rgb8(0x38, 0xb8, 0xf6));
        assert_eq!(cursor.pos, (40.0, 50.0));
        assert!(!cursor.is_agent);
    }

    #[test]
    fn an_agent_says_what_it_does_and_an_unnamed_member_is_not_a_raw_key() {
        let mut presence = PresenceCtx::empty();
        let bot = UserId("bot".into());
        presence.set_display_name(bot.clone(), "Scribe");
        presence.set_participant_kind(&bot, true);
        let cursor = presence.placed_cursor(placed("bot"));
        assert_eq!(cursor.label, "Scribe · Reading the notes");
        assert!(cursor.is_agent);

        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let stranger = presence.placed_cursor(placed(key));
        assert_eq!(stranger.label, "01234567");
        let named = presence.placed_cursor(PlacedPointer {
            name: Some("Ada".into()),
            agent: true,
            ..placed(key)
        });
        assert_eq!(
            named.label, "Ada · Reading the notes",
            "until the room names them, by their own presence"
        );
        assert!(named.is_agent);
        assert_eq!(
            stranger.color,
            {
                let (r, g, b) = super::super::user_color(key);
                Color::from_rgb8(r, g, b)
            },
            "without a colour from the room, the identity's own"
        );
    }
}
