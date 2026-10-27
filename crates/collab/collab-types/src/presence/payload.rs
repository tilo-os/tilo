//! What a presence update says, and who is in a room: the typed payloads apps
//! publish under a [`PresenceKey`](super::PresenceKey), how available a person
//! says they are, and a roster row.

use web_time::Duration;

use super::hue::{PresenceHue, identity_rgb};

#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: usize,
    },
    Selection {
        start: usize,
        end: usize,
    },
    /// The local user just inserted `start..end` under scope `key`; peers
    /// render a fading "just typed" swipe (see
    /// `ui::presence::PresenceCtx::record_peer_typing`). Unlike
    /// `Caret`/`Selection` this is an *event*, published once and decayed
    /// on the receiver.
    Typing {
        start: usize,
        end: usize,
    },
    Arrived,
    Left,
    /// A room-scoped, transient follow relationship announced by the
    /// authenticated sender. It is a visual presence cue only; the local
    /// Follow command remains private to the follower's session.
    Following {
        target: Option<String>,
    },
    /// Truthful action/resource activity. Identity is supplied by the
    /// authenticated event envelope; this payload never describes a mouse or
    /// keyboard gesture and therefore cannot create a synthetic cursor.
    Interaction(Box<tilo_actions::SemanticInteraction>),
    /// Short room-scoped intention. `automatic` distinguishes a local-model
    /// suggestion from participant-authored words; empty text clears it.
    Status {
        text: String,
        automatic: bool,
    },
    /// How available the authenticated sender is, and the voice channel
    /// they are in, if any. Desktop presence: the shell shows it on faces
    /// and in the roster; it never reaches an app ([`Self::reaches_apps`]).
    Availability {
        state: Availability,
        call: Option<String>,
    },
}

impl PresencePayload {
    /// Whether apps receive this: everything but the desktop's own
    /// presence (availability), which only the shell shows.
    #[must_use]
    pub const fn reaches_apps(&self) -> bool {
        !matches!(self, Self::Availability { .. })
    }
}

/// How available a person says they are, beside their face. Emoji-free
/// presets; idle and away also come on their own (no input for
/// [`IDLE_AFTER`], a locked screen).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Availability {
    /// Here and open to being talked to.
    #[default]
    Available,
    /// Here, heads down.
    Focusing,
    /// Not at the computer: chosen, or the screen is locked.
    Away,
    /// No input for a while: the face dims.
    Idle,
}

/// No input for this long and a person shows as idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(5 * 60);

impl Availability {
    /// What others see, from what the person chose and what their machine
    /// knows: a locked screen is away, and a quiet one is idle unless they
    /// chose otherwise.
    #[must_use]
    pub fn shown(chosen: Self, locked: bool, quiet_for: Duration) -> Self {
        if locked {
            Self::Away
        } else if chosen == Self::Available && quiet_for >= IDLE_AFTER {
            Self::Idle
        } else {
            chosen
        }
    }

    /// Whether the person's face dims.
    #[must_use]
    pub const fn dims(self) -> bool {
        matches!(self, Self::Away | Self::Idle)
    }

    /// The stored and wire word.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Focusing => "focusing",
            Self::Away => "away",
            Self::Idle => "idle",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "available" => Some(Self::Available),
            "focusing" => Some(Self::Focusing),
            "away" => Some(Self::Away),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    /// The colour this participant wears, as they chose it or as their
    /// stable identity derives it. `None` when the room has not heard it;
    /// they are then painted in the colour their id derives
    /// ([`user_color`](super::user_color)).
    pub colour: Option<PresenceHue>,
    /// Whether this participant is an agent rather than a person.
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    /// Apps can't derive this themselves: `id` is the address *other*
    /// machines use, and a machine never announces its own to its own apps.
    pub is_self: bool,
    /// When the room admitted this participant, Unix milliseconds, as its
    /// membership records it; `None` for an agent or a member admitted
    /// before rooms recorded it. People who share a name are numbered in
    /// this order (`ui::presence::ParticipantLabels::by_joining`).
    pub joined_at: Option<u64>,
}

impl Collaborator {
    /// [`Self::joined_at`] as the roster's wire carries it: `u64::MAX` for
    /// none.
    #[must_use]
    pub fn packed_joined_at(&self) -> u64 {
        self.joined_at.unwrap_or(u64::MAX)
    }

    /// The admission time a roster row's packed value names: `None` for
    /// `u64::MAX`.
    #[must_use]
    pub const fn joined_at_from_packed(packed: u64) -> Option<u64> {
        if packed == u64::MAX {
            None
        } else {
            Some(packed)
        }
    }

    /// [`Self::colour`] as the roster's wire carries it: the presence
    /// colour's sRGB packed `0x00RRGGBB`, `0` for none (no presence colour
    /// is black).
    #[must_use]
    pub fn packed_colour(&self) -> u32 {
        self.colour.map_or(0, |hue| {
            let (r, g, b) = identity_rgb(hue);
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
    }
}
