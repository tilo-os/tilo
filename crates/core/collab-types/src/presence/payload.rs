//! What a presence update says: the typed payloads apps
//! publish under a [`PresenceKey`](super::PresenceKey).

use super::{Availability, Payload};

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
    /// Where the sender is in a document a view draws, in the document's own units
    /// ([`Spot`]): the point they work at, and what they selected around it. The app
    /// declares it; the document's surface draws it.
    Spot(Spot),
}

/// Where a member is in a document a view draws: a point in the document's own units, and the
/// rectangle of what they selected around it.
///
/// The units are the app's own — artwork units on a canvas, cells on a sheet, seconds on a
/// timeline — because the app is the only one that knows them. The surface that draws the
/// document is the only one that knows how to place them, so the point travels as declared and
/// is placed by the surface, never re-derived from pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spot {
    /// Across the document, in its own units.
    pub x: f32,
    /// Down the document, in its own units.
    pub y: f32,
    /// How wide what they selected is, in the same units; zero with `tall` for a point.
    pub wide: f32,
    /// How tall what they selected is, in the same units.
    pub tall: f32,
}

impl Spot {
    /// Whether it marks a rectangle rather than a point.
    #[must_use]
    pub fn is_rect(&self) -> bool {
        self.wide != 0.0 || self.tall != 0.0
    }

    /// Whether its numbers are ones a document can be placed at.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        [self.x, self.y, self.wide, self.tall]
            .iter()
            .all(|value| value.is_finite())
    }
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
            Self::Spot(spot) => Payload::Spot {
                x: spot.x,
                y: spot.y,
                wide: spot.wide,
                tall: spot.tall,
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
            Payload::Spot { x, y, wide, tall } => {
                let spot = Spot { x, y, wide, tall };
                if !spot.is_finite() {
                    return Err("a spot is not a place a document has".to_owned());
                }
                Self::Spot(spot)
            }
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
