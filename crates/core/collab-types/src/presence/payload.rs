//! What a presence update says, and who is in a workspace: the typed payloads apps
//! publish under a [`PresenceKey`](super::PresenceKey), how available a person
//! says they are, and a roster row.

use web_time::Duration;

use super::Payload;

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
    /// Truthful action/resource activity. Identity is supplied by the
    /// authenticated event envelope; this payload never describes a mouse or
    /// keyboard gesture and therefore cannot create a synthetic cursor.
    Interaction(Box<tilo_actions::SemanticInteraction>),
    /// Short workspace-scoped intention. `automatic` distinguishes a local-model
    /// suggestion from member-authored words; empty text clears it.
    Status {
        text: String,
        automatic: bool,
    },
    /// How available the authenticated sender is, and the voice channel
    /// they are in, if any. Shell presence: the shell shows it on faces
    /// and in the roster; it never reaches an app ([`Self::reaches_apps`]).
    Availability {
        state: Availability,
        call: Option<String>,
    },
    /// The sender is composing a message in the resource their key names (a
    /// channel, a direct conversation): a draft is being typed, never what it
    /// says. `composing` false says they stopped, so a reader who stopped
    /// typing and closed the window is not left "typing" forever. Ephemeral:
    /// a presence frame, never the record.
    Composing {
        composing: bool,
    },
}

impl PresencePayload {
    /// Whether apps receive this: everything but the shell's own
    /// presence (availability), which only the shell shows.
    #[must_use]
    pub const fn reaches_apps(&self) -> bool {
        !matches!(self, Self::Availability { .. })
    }

    /// What an app says, as its frame on the workspace's live session carries it: `None` for
    /// what no app says (the shell's availability) and for a position or interaction the frame
    /// cannot carry.
    #[must_use]
    pub fn to_live(&self) -> Option<Payload> {
        let at = |index: usize| u64::try_from(index).ok();
        Some(match self {
            Self::Pointer { x, y } => Payload::Pointer { x: *x, y: *y },
            Self::Caret { pos } => Payload::Caret { pos: at(*pos)? },
            Self::Selection { start, end } => Payload::Selection {
                start: at(*start)?,
                end: at(*end)?,
            },
            Self::Typing { start, end } => Payload::Typing {
                start: at(*start)?,
                end: at(*end)?,
            },
            Self::Arrived => Payload::Arrived,
            Self::Left => Payload::Left,
            Self::Interaction(update) => {
                Payload::InteractionJson(serde_json::to_string(update).ok()?)
            }
            Self::Status { text, automatic } => Payload::Status {
                text: text.clone(),
                automatic: *automatic,
            },
            Self::Composing { composing } => Payload::Composing {
                composing: *composing,
            },
            Self::Availability { .. } => return None,
        })
    }

    /// What an app shows of `payload`, a frame's word: `Ok(None)` for the desktop's own word,
    /// which no app shows, and an error for a word that does not decode.
    ///
    /// # Errors
    ///
    /// A position this device cannot address, or an interaction that does not decode or is not
    /// a valid one.
    pub fn from_live(payload: Payload) -> Result<Option<Self>, String> {
        let index = |name: &str, value: u64| {
            usize::try_from(value).map_err(|_| format!("{name} {value} is out of range"))
        };
        Ok(Some(match payload {
            Payload::Pointer { x, y } => Self::Pointer { x, y },
            Payload::Caret { pos } => Self::Caret {
                pos: index("caret", pos)?,
            },
            Payload::Selection { start, end } => Self::Selection {
                start: index("selection start", start)?,
                end: index("selection end", end)?,
            },
            Payload::Typing { start, end } => Self::Typing {
                start: index("typing start", start)?,
                end: index("typing end", end)?,
            },
            Payload::Arrived => Self::Arrived,
            Payload::Left => Self::Left,
            Payload::InteractionJson(json) => {
                let update: tilo_actions::SemanticInteraction = serde_json::from_str(&json)
                    .map_err(|error| format!("interaction does not decode: {error}"))?;
                update
                    .validate()
                    .map_err(|error| format!("invalid interaction: {error}"))?;
                Self::Interaction(Box::new(update))
            }
            Payload::Status { text, automatic } => Self::Status { text, automatic },
            Payload::Composing { composing } => Self::Composing { composing },
            Payload::Availability { .. }
            | Payload::Chat { .. }
            | Payload::Reaction { .. }
            | Payload::Wave { .. }
            | Payload::Following { .. }
            | Payload::Person { .. }
            | Payload::Profile { .. }
            | Payload::Bridged(_)
            | Payload::Shell(_) => return Ok(None),
        }))
    }
}

/// How available a person says they are, beside their face. Emoji-free
/// presets; idle and away also come on their own (no input for
/// [`IDLE_AFTER`], a locked screen).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
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
