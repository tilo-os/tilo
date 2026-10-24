//! A person's presence colour: one of twelve hues they choose, or the one
//! their identity derives until they do.
//!
//! A colour is part of someone's identity, not of a theme, so it travels as
//! this choice (and as the sRGB colour [`identity_rgb`] gives it) and each
//! viewer's toolkit paints it at its own theme's lightness and chroma.

use std::collections::HashMap;

use tilo_sort::CompactSort as _;

/// How many hues the presence palette has.
pub const PRESENCE_LEN: usize = 12;

/// The presence palette's hues, in OKLCH degrees: twelve evenly spaced, so
/// neighbours sit 30° apart.
pub const PRESENCE_HUES: [f32; PRESENCE_LEN] = [
    25.0, 55.0, 85.0, 115.0, 145.0, 175.0, 205.0, 235.0, 265.0, 295.0, 325.0, 355.0,
];

/// One of the twelve presence hues: a person's colour, as they choose it.
///
/// A colour is part of someone's identity, not of a theme: it is stored as
/// this choice and painted through whatever theme each viewer uses
/// (the toolkit's presence palette), so it reads at that theme's presence
/// lightness and chroma everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceHue {
    Red,
    Orange,
    Amber,
    Olive,
    Green,
    Teal,
    Cyan,
    Sky,
    Blue,
    Violet,
    Magenta,
    Pink,
}

impl PresenceHue {
    /// Every hue, in [`PRESENCE_HUES`] order.
    pub const ALL: [Self; PRESENCE_LEN] = [
        Self::Red,
        Self::Orange,
        Self::Amber,
        Self::Olive,
        Self::Green,
        Self::Teal,
        Self::Cyan,
        Self::Sky,
        Self::Blue,
        Self::Violet,
        Self::Magenta,
        Self::Pink,
    ];

    /// The palette slot, `0..PRESENCE_LEN`.
    #[must_use]
    pub fn slot(self) -> usize {
        Self::ALL.iter().position(|h| *h == self).unwrap_or(0)
    }

    /// The hue at `slot`, wrapping past [`PRESENCE_LEN`].
    #[must_use]
    pub fn from_slot(slot: usize) -> Self {
        Self::ALL
            .get(slot % PRESENCE_LEN)
            .copied()
            .unwrap_or(Self::Blue)
    }

    #[must_use]
    pub fn degrees(self) -> f32 {
        PRESENCE_HUES.get(self.slot()).copied().unwrap_or(265.0)
    }

    /// The stored name (`"violet"`), as settings and actions spell it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Amber => "amber",
            Self::Olive => "olive",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Cyan => "cyan",
            Self::Sky => "sky",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Magenta => "magenta",
            Self::Pink => "pink",
        }
    }

    /// The name a person reads in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Amber => "Amber",
            Self::Olive => "Olive",
            Self::Green => "Green",
            Self::Teal => "Teal",
            Self::Cyan => "Cyan",
            Self::Sky => "Sky",
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Magenta => "Magenta",
            Self::Pink => "Pink",
        }
    }

    /// Parse [`Self::id`].
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    /// The hue nearest to `degrees` (an OKLCH hue) on the ring. The toolkit
    /// reads a colour's hue and decides first whether it has one at all; a
    /// grey does not.
    #[must_use]
    pub fn nearest_to_degrees(degrees: f32) -> Self {
        let distance = |h: Self| {
            let d = (h.degrees() - degrees).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        Self::ALL
            .into_iter()
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
            .unwrap_or(Self::Blue)
    }

    /// The colour someone wears until they choose one: a pure function of
    /// their identity, so every host paints them the same without asking.
    /// The fixture names `me`, `alice`, `bob` and `charlie` are pinned so
    /// demos and tests stay recognisable.
    #[must_use]
    pub fn of_identity(id: &str) -> Self {
        match id {
            "me" => Self::Blue,
            "alice" => Self::Pink,
            "bob" => Self::Orange,
            "charlie" => Self::Green,
            _ => {
                // FNV-1a 64-bit: cheap, deterministic, no RNG state.
                let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
                for byte in id.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x0100_0000_01b3);
                }
                let slot = hash.checked_rem(PRESENCE_LEN as u64).unwrap_or(0);
                Self::from_slot(usize::try_from(slot).unwrap_or(0))
            }
        }
    }

    /// The person's stored choice in a settings document
    /// (`identity.colour`), if they made one.
    #[must_use]
    pub fn chosen_in(settings: &serde_json::Value) -> Option<Self> {
        settings
            .get("identity")?
            .get("colour")?
            .as_str()
            .and_then(Self::from_id)
    }
}

/// Stable identity colour for a collaborator who has not chosen one: the
/// presence hue [`PresenceHue::of_identity`] derives from
/// their id, as the sRGB colour in [`PEER_PALETTE`]. A pure function of the
/// id alone: two hosts painting the same room see different peer sets
/// (everyone but themselves), so a colour derived from who's present
/// couldn't agree across machines.
///
/// This is the identity as it travels; paint it through
/// `theme.tokens.colors.presence.of_rgb(..)`, which carries the hue at the
/// active theme's presence lightness and chroma.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    identity_rgb(PresenceHue::of_identity(user_id))
}

/// A presence hue as the identity colour it travels as.
pub fn identity_rgb(hue: PresenceHue) -> (u8, u8, u8) {
    PEER_PALETTE.get(hue.slot()).copied().unwrap_or_default()
}

/// Every colour a peer can be drawn in:the design language's twelve
/// presence hues ([`PRESENCE_HUES`], 30° apart) at the dark
/// material's presence lightness and chroma (OKLCH L 0.74, C 0.14, gamut
/// mapped), as sRGB. Equal lightness and chroma, so no participant looks
/// more important than another; a closed ring rather than a hash over all
/// 360 hues, so two people never land a few degrees apart.
pub const PEER_PALETTE: [(u8, u8, u8); 12] = [
    (0xf7, 0x85, 0x7d), // 25°  red
    (0xed, 0x91, 0x4c), // 55°  orange — pinned for "bob"
    (0xd3, 0xa3, 0x29), // 85°  amber
    (0xa8, 0xb5, 0x40), // 115° olive
    (0x6e, 0xc2, 0x72), // 145° green — pinned for "charlie"
    (0x00, 0xc7, 0xa6), // 175° teal
    (0x00, 0xc1, 0xd1), // 205° cyan
    (0x38, 0xb8, 0xf6), // 235° sky
    (0x81, 0xa8, 0xff), // 265° blue — pinned for "me"
    (0xb2, 0x98, 0xf9), // 295° violet
    (0xd7, 0x8a, 0xdb), // 325° magenta
    (0xee, 0x83, 0xaf), // 355° pink — pinned for "alice"
];

/// Give everyone in a room a colour nobody else in it is wearing.
///
/// [`user_color`] alone can't promise this — it's a hash, so `carol` lands
/// on `alice`'s pink. Whoever collides steps to the next free ring entry,
/// walking ids in sorted order so every host agrees despite seeing a
/// different membership (everyone but itself). `reserved` colours are
/// already spoken for by an explicit roster choice.
pub fn assign_peer_colors(
    ids: &[&str],
    reserved: &[(u8, u8, u8)],
) -> HashMap<String, (u8, u8, u8)> {
    let mut sorted: Vec<&str> = ids.to_vec();
    sorted.compact_sort();
    sorted.dedup();

    let mut taken = [false; PEER_PALETTE.len()];
    for c in reserved {
        if let Some(i) = PEER_PALETTE.iter().position(|p| p == c)
            && let Some(slot) = taken.get_mut(i)
        {
            *slot = true;
        }
    }

    let mut out = HashMap::with_capacity(sorted.len());
    for id in sorted {
        let natural = user_color(id);
        let start = PEER_PALETTE
            .iter()
            .position(|p| *p == natural)
            // Always a ring member; 0 just keeps a draw path total rather than panicking.
            .unwrap_or(0);
        let mut slot = start;
        for step in 0..PEER_PALETTE.len() {
            // `PEER_PALETTE.len()` (24) is a nonzero compile-time constant,
            // so the remainder can never divide by zero; `start`/`step` are
            // both < 24, so the sum can't realistically overflow either.
            let i = start
                .saturating_add(step)
                .checked_rem(PEER_PALETTE.len())
                .unwrap_or(0);
            if !taken.get(i).copied().unwrap_or(false) {
                slot = i;
                break;
            }
            // More people than colours: share the natural entry; the name still says who is who.
            slot = start;
        }
        if let Some(s) = taken.get_mut(slot) {
            *s = true;
        }
        out.insert(
            id.to_string(),
            PEER_PALETTE.get(slot).copied().unwrap_or_default(),
        );
    }
    out
}
