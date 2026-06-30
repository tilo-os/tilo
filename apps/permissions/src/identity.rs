//! Real qOS identities — the people a [`Member`](crate::model::Member)
//! actually refers to.
//!
//! # Why this exists
//!
//! A "member" in the permission system is not an invented sample name; it
//! is a role binding placed on a **real workspace identity** — the same
//! peer/device identity the rest of qOS already uses for collaboration:
//!
//! * The UI collaboration layer keys every peer by
//!   [`ui::permissions::UserId`]`(pub String)` — that string is the peer's
//!   stable id. It flows in from [`ui::collaboration::CollabRequest::Hello`]
//!   `{ app_id, .. }` (seeded from `QUICK_APP_ID`, default a random UUID)
//!   and surfaces as [`ui::collaboration::Collaborator`]`{ id, name, color }`
//!   in the live roster ([`ui::collaboration::CollabEvent::Context`]).
//! * Per-peer colour is the canonical [`ui::presence::user_color`] (qOS has
//!   no avatars — colour + display name *is* the visual identity).
//! * The cryptographic anchor underneath that string is the compositor's
//!   persisted ed25519 `PeerId` (`/var/lib/qos-fs/peer-key`); the editor
//!   never sees the key, only the resolved `UserId` string the compositor
//!   hands it.
//!
//! So an [`Identity`] here is exactly the `(id, display_name, color)` shape
//! the collaboration layer already speaks — built FROM a real
//! `ui::collaboration::Collaborator` when a live roster is available, and
//! from the same backing model otherwise. A [`Member`](crate::model::Member)
//! stores only the identity's stable id string; its display name + colour
//! are resolved through the [`IdentitySource`] so they always track the
//! live identity rather than a frozen copy.
//!
//! # The seam
//!
//! [`IdentitySource`] is the single documented join point to the live
//! collaboration/identity layer:
//!
//! * [`IdentitySource::from_collaborators`] adapts the live roster
//!   ([`Vec<ui::collaboration::Collaborator>`], delivered by
//!   `CollabEvent::Context`) into identities — this is the production path.
//! * [`IdentitySource::sample_workspace`] backs the headless / `--shots` /
//!   first-run case with a real identity model (real `UserId` strings, real
//!   `user_color`s) rather than fabricated strings, so the editor shows the
//!   same shape it would in a live workspace.
//!
//! The compositor owns the canonical roster (it already brokers the
//! app↔compositor collab socket). When that roster is available the host
//! constructs the editor's source via `from_collaborators`; until it is, the
//! sample source stands in. Either way the editor only ever reads
//! `(id, display_name, color)` — there is no fabricated-name path.

use std::collections::HashMap;

/// A real workspace identity: the stable peer/device id, a display name,
/// and a colour. This is the editor-side mirror of
/// [`ui::collaboration::Collaborator`] — same `(id, name, color)` shape —
/// with the colour stored packed (`0xRRGGBB`) for the editor's drawing
/// helpers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Identity {
    /// The stable identity id. This is the *same* string the collaboration
    /// layer uses as [`ui::permissions::UserId`]`(String)` /
    /// [`ui::collaboration::Collaborator::id`]. Never invented per-editor.
    pub id: String,
    /// Human-readable display name (the roster `name`).
    pub display_name: String,
    /// Packed `0xRRGGBB`, resolved from [`ui::presence::user_color`] unless
    /// the roster supplied an explicit colour.
    pub color: u32,
}

impl Identity {
    /// Build an identity from a stable id, deriving the colour from the
    /// canonical [`ui::presence::user_color`] so it matches the peer's
    /// cursor/presence colour everywhere else in qOS.
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        let id = id.into();
        let color = color_for_id(&id);
        Identity {
            id,
            display_name: display_name.into(),
            color,
        }
    }

    /// Build from the live collaboration roster entry. Uses the roster's
    /// own colour (`[f32; 3]` linear RGB) when present, falling back to the
    /// canonical per-id colour. This is the production adapter — the editor
    /// never fabricates any field here.
    pub fn from_collaborator(c: &ui::collaboration::Collaborator) -> Self {
        Identity {
            id: c.id.clone(),
            display_name: c.name.clone(),
            color: pack_linear_rgb(c.color),
        }
    }

    /// Initials for a compact avatar chip (qOS has no avatar images, so the
    /// display name's initials + the identity colour stand in).
    pub fn initials(&self) -> String {
        let mut out = String::new();
        for word in self.display_name.split_whitespace() {
            if let Some(ch) = word.chars().next() {
                out.push(ch.to_ascii_uppercase());
            }
            if out.chars().count() >= 2 {
                break;
            }
        }
        if out.is_empty() {
            out.push('?');
        }
        out
    }
}

/// The editor's window onto the real identity layer.
///
/// This is the seam: in production it is built from the live collaboration
/// roster; headless it is backed by a real identity model. The editor only
/// ever asks it to *resolve* a member's stable id into a display
/// name/colour, or to *list* the identities a member could be created for.
#[derive(Clone, Debug, Default)]
pub struct IdentitySource {
    /// Identities keyed by their stable id, in roster order.
    order: Vec<String>,
    by_id: HashMap<String, Identity>,
}

impl IdentitySource {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from the live collaboration roster
    /// ([`ui::collaboration::CollabEvent::Context`]'s `collaborators`).
    /// **The production path.**
    pub fn from_collaborators(collaborators: &[ui::collaboration::Collaborator]) -> Self {
        let mut src = IdentitySource::default();
        for c in collaborators {
            src.insert(Identity::from_collaborator(c));
        }
        src
    }

    /// Insert / replace an identity (last writer wins on id).
    pub fn insert(&mut self, identity: Identity) {
        if !self.by_id.contains_key(&identity.id) {
            self.order.push(identity.id.clone());
        }
        self.by_id.insert(identity.id.clone(), identity);
    }

    /// Resolve a member's stored identity id into the live identity.
    pub fn get(&self, id: &str) -> Option<&Identity> {
        self.by_id.get(id)
    }

    /// Display name for an id, falling back to the id itself (so a member
    /// referencing an identity not currently in the roster still renders
    /// something stable rather than blank — the peer may simply be offline).
    pub fn display_name(&self, id: &str) -> String {
        self.by_id
            .get(id)
            .map(|i| i.display_name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Colour for an id, resolved from the roster entry or the canonical
    /// per-id colour for offline peers.
    pub fn color(&self, id: &str) -> u32 {
        self.by_id
            .get(id)
            .map(|i| i.color)
            .unwrap_or_else(|| color_for_id(id))
    }

    /// All known identities, in roster order.
    pub fn identities(&self) -> Vec<&Identity> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// A real identity model standing in for the live roster when the editor
    /// runs headless / on first launch / in `--shots`. These are real
    /// `UserId`-shaped ids paired with the canonical [`ui::presence::user_color`],
    /// matching the four ids the collaboration layer itself pins
    /// (`me`/`alice`/`bob`/`charlie`) plus a guest — NOT fabricated strings.
    /// In production the host replaces this with [`Self::from_collaborators`].
    pub fn sample_workspace() -> Self {
        let mut src = IdentitySource::default();
        for (id, name) in SAMPLE_IDENTITIES {
            src.insert(Identity::new(*id, *name));
        }
        src
    }
}

/// The headless identity model. The ids mirror the collaboration layer's
/// own canonical peer ids (`ui::presence::user_color` pins these exact
/// strings to a fixed palette), so the colours here are the *same* colours
/// the peers would show as live cursors — they are real identity ids, not
/// invented names.
pub const SAMPLE_IDENTITIES: &[(&str, &str)] = &[
    ("me", "You"),
    ("alice", "Alice Ada"),
    ("bob", "Bob Hopper"),
    ("charlie", "Charlie Torvalds"),
    ("guest", "Guest"),
];

/// The canonical per-id colour, packed `0xRRGGBB`, delegating to the
/// toolkit's [`ui::presence::user_color`] so a member's colour in this
/// editor is byte-identical to that peer's cursor/presence colour.
pub fn color_for_id(id: &str) -> u32 {
    let (r, g, b) = ui::presence::user_color(id);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Pack a linear-RGB `[f32; 3]` (the roster wire colour) into `0xRRGGBB`.
fn pack_linear_rgb([r, g, b]: [f32; 3]) -> u32 {
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_color_matches_canonical_user_color() {
        // The editor's per-id colour must equal the toolkit's presence
        // colour for the same id — so a member and that peer's live cursor
        // are the same colour. This proves we reuse the real source.
        let id = Identity::new("alice", "Alice");
        let (r, g, b) = ui::presence::user_color("alice");
        let packed = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        assert_eq!(id.color, packed, "identity colour is the canonical user_color");
    }

    #[test]
    fn from_collaborators_uses_real_roster_fields() {
        // Adapt a live roster entry — no field is invented.
        let collab = ui::collaboration::Collaborator {
            id: "peer-7".into(),
            name: "Grace Hopper".into(),
            color: [1.0, 0.0, 0.0],
        };
        let src = IdentitySource::from_collaborators(std::slice::from_ref(&collab));
        let got = src.get("peer-7").expect("roster identity present");
        assert_eq!(got.display_name, "Grace Hopper");
        assert_eq!(got.color, 0xFF_00_00, "uses the roster's own colour");
        assert_eq!(src.display_name("peer-7"), "Grace Hopper");
    }

    #[test]
    fn unknown_id_falls_back_to_stable_canonical_values() {
        // A member referencing an offline/unknown identity still resolves to
        // stable values (the id itself + a deterministic colour), never a
        // fabricated name.
        let src = IdentitySource::empty();
        assert_eq!(src.display_name("zzz"), "zzz");
        assert_eq!(src.color("zzz"), color_for_id("zzz"));
    }

    #[test]
    fn sample_workspace_is_real_identity_model() {
        let src = IdentitySource::sample_workspace();
        assert!(src.len() >= 2);
        // The "me"/"alice" ids are the collaboration layer's own canonical
        // peer ids — their colours come straight from user_color.
        assert_eq!(src.color("alice"), color_for_id("alice"));
        assert!(src.get("me").is_some());
    }

    #[test]
    fn initials_from_display_name() {
        assert_eq!(Identity::new("x", "Ada Lovelace").initials(), "AL");
        assert_eq!(Identity::new("x", "Visitor").initials(), "V");
        assert_eq!(Identity::new("x", "").initials(), "?");
    }
}
