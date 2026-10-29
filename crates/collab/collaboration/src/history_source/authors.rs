//! Who wrote a change, as data: an automerge actor id decoded to the handle
//! its process chose, the actors one device writes as, and the author a
//! history names, with the colour presence paints them in.
//!
//! Three shapes of actor occur in documents: a raw 32-byte public key (the
//! only cryptographically meaningful one); a self-declared handle
//! (`ActorId::from(app_id.as_bytes())`, **unverified**), read back by
//! [`actor_handle`]; and a random id, which resolves to nobody. Naming
//! someone in the reader's words (the toolkit's `AuthorDirectory`) is
//! presentation and stays above; a revert asks for names through
//! [`ResolveAuthor`].

/// Resolves an actor to the author history names: what a revert says when
/// somebody's later edits held part of it back.
pub trait ResolveAuthor {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor;
}

/// Display name used for an actor with no registered identity and no
/// self-declared handle. Deliberately not a person's name.
pub const UNKNOWN_AUTHOR_NAME: &str = "Unknown participant";

/// Number of hex characters of the actor id shown next to a name. Eight is the
/// convention `HistoryAuthor::identity_short` documents.
#[doc(hidden)]
pub const SHORT_LEN: usize = 8;

/// Longest self-declared handle we will render. Anything longer is almost
/// certainly not a handle (it is a key, a blob, or an attempt to blow out the
/// row) and falls through to the unknown-participant path.
const MAX_HANDLE_LEN: usize = 64;

/// History row whose colour is already decided (a person's own colour).
pub fn author_coloured(
    name: impl Into<String>,
    identity_short: impl Into<String>,
    rgb: (u8, u8, u8),
) -> HistoryAuthor {
    HistoryAuthor::new(name, rgb_to_hue([rgb.0, rgb.1, rgb.2]), identity_short).with_rgb(rgb)
}

/// Decode an actor id back to the handle the authoring process chose for
/// itself, when it did. `ActorId::from(&[u8])` stores the bytes verbatim, so
/// `set_actor(ActorId::from(app_id.as_bytes()))` leaves the UTF-8 of `app_id`
/// in the hex. Returns `None` for anything not a plausible handle (odd-length
/// /non-hex input, non-UTF-8, control chars, empty, or too long); a 32-byte
/// public key almost never survives this filter.
pub fn actor_handle(actor_hex: &str) -> Option<String> {
    // optional-decode: a heuristic over actor bytes; anything not a plausible handle is a key, by design
    let bytes = hex::decode(actor_hex).ok()?;
    // optional-decode: a heuristic over actor bytes; anything not a plausible handle is a key, by design
    let s = String::from_utf8(bytes).ok()?;
    let handle = s.split(DEVICE_SEPARATOR).next().unwrap_or_default();
    if handle.is_empty() || handle.len() > MAX_HANDLE_LEN || handle.chars().any(char::is_control) {
        return None;
    }
    let trimmed = handle.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Separates the handle of a [`device_actor`] from its device part.
const DEVICE_SEPARATOR: char = '\u{1f}';

/// The actor `handle` writes a document as from the device whose record
/// writer is `device`: the handle, which [`actor_handle`] reads back, and a
/// part only that device has. Automerge numbers each actor's changes, so
/// one person writing from two devices under the bare handle would make two
/// different changes with the same number, which no replica can merge.
pub fn device_actor(handle: &str, device: &automerge::ActorId) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(device.to_bytes());
    let mut actor = format!("{handle}{DEVICE_SEPARATOR}").into_bytes();
    actor.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
    actor
}

/// The actors a device writes as, to tell its changes apart: its record
/// writer, and the [`device_actor`] of any handle on it.
#[derive(Clone, Debug)]
pub struct DeviceActors {
    writer: String,
    suffix: Vec<u8>,
}

impl DeviceActors {
    /// The actors of the device whose record writer is `device`.
    pub fn of(device: &automerge::ActorId) -> Self {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(device.to_bytes());
        let mut suffix = vec![DEVICE_SEPARATOR as u8];
        suffix.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
        Self {
            writer: device.to_hex_string(),
            suffix,
        }
    }

    pub fn wrote(&self, actor_hex: &str) -> bool {
        actor_hex == self.writer
            || hex::decode(actor_hex).is_ok_and(|bytes| bytes.ends_with(&self.suffix))
    }
}

/// The bare handle's actor of a [`device_actor`], hex-encoded.
#[doc(hidden)]
pub fn handle_actor_hex(actor_hex: &str) -> Option<String> {
    // optional-decode: a probe for a device actor; anything else names no bare handle
    let bytes = hex::decode(actor_hex).ok()?;
    let at = bytes.iter().position(|&b| b == DEVICE_SEPARATOR as u8)?;
    Some(hex::encode(bytes.get(..at)?))
}

/// First `SHORT_LEN` hex characters of an actor id, for the dim monospace
/// label next to the name. Short ids are returned whole.
pub fn short_id(actor_hex: &str) -> String {
    actor_hex.chars().take(SHORT_LEN).collect()
}

/// Deterministic avatar hue for an actor with no registered colour: FNV-1a
/// over the actor hex, folded into `[0, 360)`. Same actor, same hue in every
/// process and on every peer.
pub fn stable_hue(actor_hex: &str) -> f32 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, actor_hex.as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    // `hash % 360` is always `0..359`, which fits a `u16` exactly and so
    // converts to `f32` without any precision loss.
    let bucket = u16::try_from(hash % 360).unwrap_or(0);
    f32::from(bucket)
}

/// RGB → hue in degrees. Saturation/value are fixed by
/// [`hue_to_rgb`], so only the hue survives —
/// deliberate, it keeps the dots a coherent family.
#[doc(hidden)]
pub fn rgb_to_hue(rgb: [u8; 3]) -> f32 {
    let red = f32::from(rgb[0]) / 255.0;
    let green = f32::from(rgb[1]) / 255.0;
    let blue = f32::from(rgb[2]) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let range = max - min;
    if range <= f32::EPSILON {
        // Grey has no hue. First boot writes [128, 128, 128], so this is the
        // common case until the user picks a colour: give it a fixed,
        // recognisable slot rather than a random one.
        return 0.0;
    }
    // `max` is always the literal value of one of `red`/`green`/`blue` (never
    // a computed value of its own), so an epsilon compare here agrees with
    // exact equality in every case and only avoids the float-cmp lint.
    let hue = if (max - red).abs() <= f32::EPSILON {
        ((green - blue) / range).rem_euclid(6.0)
    } else if (max - green).abs() <= f32::EPSILON {
        (blue - red) / range + 2.0
    } else {
        (red - green) / range + 4.0
    };
    (hue * 60.0).rem_euclid(360.0)
}

/// Not crypto material — just enough metadata to render a per-author
/// colour dot, display name, and a short identity hash for
/// disambiguation when two authors share a name.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryAuthor {
    pub name: String,
    /// Degrees `[0, 360)`. Kept for fixtures and any surface that still
    /// reads hue; paint uses [`HistoryAuthor::rgb`].
    pub avatar_hue: f32,
    /// Short hex fragment of the identity pubkey; the producer decides
    /// the length, the widget treats it as an opaque label.
    pub identity_short: String,
    /// The colour presence paints this identity in. Defaults to
    /// [`hue_to_rgb`] of [`Self::avatar_hue`] so existing constructors
    /// keep their look; [`Self::with_rgb`] and identity resolution set
    /// the person's colour or the shared presence palette slot
    /// (`ui::presence::PEER_PALETTE`).
    avatar_rgb: (u8, u8, u8),
    /// Who the author is to whoever reads the history: themselves, another
    /// person, or software acting for someone.
    pub relation: AuthorRelation,
    /// Who acted and for whom, when the author is an agent, a Shortcut or an
    /// app acting for a person: the same facts [`Self::name`] reads as a
    /// sentence, kept as data for the action surface and assistive tech.
    pub attribution: Option<HistoryAttribution>,
}

/// An author that acted for someone: an agent's change is the agent's, on
/// behalf of its person, in every history (an app's, the Files history, and
/// what the history actions return).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAttribution {
    /// The acting principal as it was authenticated (`agent:scribe`).
    pub actor: String,
    /// What the actor is.
    pub kind: tilo_actions::ActorKind,
    /// The actor's display name, without the person it acted for.
    pub actor_name: String,
    /// The principal of the person it acted for, when it acted for one.
    pub on_behalf_of: Option<String>,
    /// That person's display name.
    pub on_behalf_of_name: Option<String>,
}

/// Who an author is to the person reading the history. Rows draw the
/// reader's own changes, a collaborator's and an agent's differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorRelation {
    /// The reader's own change.
    Reader,
    /// Another person's.
    #[default]
    Person,
    /// An agent's, a Shortcut's or an app's, acting for someone.
    Software,
}

impl HistoryAuthor {
    pub fn new(
        name: impl Into<String>,
        avatar_hue: f32,
        identity_short: impl Into<String>,
    ) -> Self {
        let avatar_hue = avatar_hue.rem_euclid(360.0);
        Self {
            name: name.into(),
            avatar_hue,
            identity_short: identity_short.into(),
            avatar_rgb: hue_to_rgb(avatar_hue),
            relation: AuthorRelation::Person,
            attribution: None,
        }
    }

    #[must_use]
    pub fn with_relation(mut self, relation: AuthorRelation) -> Self {
        self.relation = relation;
        self
    }

    /// Record who acted and for whom.
    #[must_use]
    pub fn with_attribution(mut self, attribution: HistoryAttribution) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Use this exact RGB for the avatar, cursor and history row.
    #[must_use]
    pub fn with_rgb(mut self, rgb: (u8, u8, u8)) -> Self {
        self.avatar_rgb = rgb;
        self
    }

    pub fn rgb(&self) -> (u8, u8, u8) {
        self.avatar_rgb
    }
}

/// Rounds a normalized `[0, 1]` colour channel to a `u8` byte. The `clamp`
/// makes the truncation and sign-loss the lint warns about impossible —
/// clippy just can't see through it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0.0, 255.0] immediately above, so this can only \
              produce a value in u8's range"
)]
fn channel_u8(unit: f32) -> u8 {
    (unit * 255.0).round().clamp(0.0, 255.0) as u8
}

/// `HSV(h, 0.65, 0.95)` → RGB. Saturation and value held constant so
/// generated author colours stay visually coherent (no neon outliers,
/// no muddy ones).
pub fn hue_to_rgb(hue_deg: f32) -> (u8, u8, u8) {
    let sector = hue_deg.rem_euclid(360.0) / 60.0;
    let sat = 0.65_f32;
    let val = 0.95_f32;
    let chroma = val * sat;
    let x2 = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let min_add = val - chroma;
    let (red, green, blue) = if sector < 1.0 {
        (chroma, x2, 0.0)
    } else if sector < 2.0 {
        (x2, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, x2)
    } else if sector < 4.0 {
        (0.0, x2, chroma)
    } else if sector < 5.0 {
        (x2, 0.0, chroma)
    } else {
        (chroma, 0.0, x2)
    };
    (
        channel_u8(red + min_add),
        channel_u8(green + min_add),
        channel_u8(blue + min_add),
    )
}
