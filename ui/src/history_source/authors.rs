//! Resolving an automerge actor id to the person behind it.
//!
//! `qdoc::history::VersionMeta` hands back an `actor_hex` — a raw hex string.
//! On its own that is unreadable, and every row of an activity timeline built
//! from it says the same nothing. This module is the join.
//!
//! # Where an actor id comes from in qOS
//!
//! There are three shapes in the tree today, and the directory handles all
//! three because they all show up in a real document:
//!
//! 1. **A raw 32-byte public key.** `WorkspaceDirectory::new` does
//!    `set_actor(ActorId::from(device_public_key.as_slice()))`, so the hex is
//!    the key. This is the shape that joins straight to a
//!    `collab_types::Identity` or to the on-disk qOS identity, and it is the
//!    only one that is cryptographically meaningful.
//! 2. **A self-declared handle.** The apps set
//!    `ActorId::from(app_id.as_bytes())` where `app_id` is `QUICK_APP_ID` /
//!    `QOS_PACKAGE_ID` — the hex decodes back to the literal string the
//!    process chose for itself. It is the same key `ui::presence::user_color`
//!    and the per-character `author` marks are already keyed on, so honouring
//!    it keeps the timeline consistent with the carets and the highlight
//!    colours the user already sees. It is **not** verified: anyone can pick
//!    any handle. The row therefore always also carries the short actor hex.
//! 3. **A random id.** `app_state::DocStore` mints `ActorId::random()` and
//!    persists it per install directory. Nothing can resolve that to a person.
//!
//! # What happens for an actor we have never seen
//!
//! Nothing is invented. The author's name becomes
//! [`UNKNOWN_AUTHOR_NAME`], the identity label becomes the first eight hex
//! characters of the actor id, and the avatar hue is derived deterministically
//! from the actor id so the same stranger keeps the same colour across
//! sessions, processes, and peers. A stable colour plus an honest label is
//! genuinely useful — you can still see that rows 3, 7 and 11 were all the
//! same person — where a fabricated name would be a lie.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::widget::history::HistoryAuthor;

/// Display name used for an actor with no registered identity and no
/// self-declared handle. Deliberately not a person's name.
pub const UNKNOWN_AUTHOR_NAME: &str = "Unknown participant";

/// Number of hex characters of the actor id shown next to a name. Eight is the
/// convention `HistoryAuthor::identity_short` documents.
const SHORT_LEN: usize = 8;

/// Longest self-declared handle we will render. Anything longer is almost
/// certainly not a handle (it is a key, a blob, or an attempt to blow out the
/// row) and falls through to the unknown-participant path.
const MAX_HANDLE_LEN: usize = 64;

/// Maps automerge actor ids to the people the chrome should name.
///
/// Registrations win over inference: the collaboration layer knows who a peer
/// actually is, and the local process knows who the user is, so both call
/// [`AuthorDirectory::register`]. Everything else falls through the
/// handle-decode and then the unknown-participant path.
#[derive(Debug, Clone, Default)]
pub struct AuthorDirectory {
    by_actor: HashMap<String, HistoryAuthor>,
}

impl AuthorDirectory {
    /// An empty directory. Every actor resolves to an unknown participant
    /// until something registers it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an actor id to a known person. Later registrations replace earlier
    /// ones for the same actor — a peer that re-announces with a corrected
    /// display name should win.
    pub fn register(&mut self, actor_hex: &str, author: HistoryAuthor) {
        self.by_actor.insert(actor_hex.to_lowercase(), author);
    }

    /// Bind `actor_hex` to the local user, read from the qOS identity on disk.
    ///
    /// Tries the launcher-provided path first (`QOS_USER_IDENTITY`, which is
    /// what the sandbox bind-mounts), then the user's own
    /// `$XDG_CONFIG_HOME/qos/identity/primary.json`, then
    /// `$HOME/.config/qos/identity/primary.json` — the file
    /// `shell::firstboot::ensure_primary_identity` writes at first boot.
    ///
    /// Returns `true` when an identity was found and registered. A `false`
    /// return is not an error: an app run outside a qOS session has no
    /// identity, and the caller should leave the actor to resolve through the
    /// handle path.
    pub fn register_local_identity(&mut self, actor_hex: &str) -> bool {
        let Some(author) = local_identity_author() else {
            return false;
        };
        self.register(actor_hex, author);
        true
    }

    /// Look up an actor. Never fails, never invents a name — see the module
    /// docs for the fallback ladder.
    pub fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let key = actor_hex.to_lowercase();
        if let Some(a) = self.by_actor.get(&key) {
            return a.clone();
        }
        let short = short_id(&key);
        match actor_handle(&key) {
            Some(handle) => HistoryAuthor::new(handle, stable_hue(&key), short),
            None => HistoryAuthor::new(UNKNOWN_AUTHOR_NAME, stable_hue(&key), short),
        }
    }

    /// True when this actor has an explicitly registered identity, as opposed
    /// to a decoded handle or the unknown-participant fallback. The chrome
    /// uses this to decide whether a row's author is trustworthy enough to,
    /// say, offer a "message them" affordance.
    pub fn is_registered(&self, actor_hex: &str) -> bool {
        self.by_actor.contains_key(&actor_hex.to_lowercase())
    }

    /// Number of explicitly registered actors.
    pub fn len(&self) -> usize {
        self.by_actor.len()
    }

    /// True when nothing has been registered.
    pub fn is_empty(&self) -> bool {
        self.by_actor.is_empty()
    }
}

/// Build a [`HistoryAuthor`] from the qOS identity on disk, if there is one.
///
/// Public because the shell wants the same answer for its own chrome without
/// having to own an [`AuthorDirectory`].
pub fn local_identity_author() -> Option<HistoryAuthor> {
    let id = runtime_identity::load()
        .ok()
        .or_else(|| {
            config_identity_path().and_then(|p| runtime_identity::load_from_path(p).ok())
        })?;
    let key_hex = hex_lower(&id.group_public_key());
    let name = if id.display_name().is_empty() {
        // First boot writes an empty display name and the user names
        // themselves later. An empty string would render as a blank row, so
        // fall back to the honest label rather than to a guess.
        UNKNOWN_AUTHOR_NAME.to_string()
    } else {
        id.display_name().to_string()
    };
    Some(HistoryAuthor::new(
        name,
        rgb_to_hue(id.avatar_color()),
        short_id(&key_hex),
    ))
}

/// `$XDG_CONFIG_HOME/qos/identity/primary.json`, else
/// `$HOME/.config/qos/identity/primary.json`. Mirrors the resolution in
/// `shell::firstboot::resolve_identity_dir`, which is what writes the file.
fn config_identity_path() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
    };
    let p = base.join("qos").join("identity").join("primary.json");
    p.is_file().then_some(p)
}

/// Decode an actor id back to the handle the authoring process chose for
/// itself, when it did.
///
/// `ActorId::from(&[u8])` stores the bytes verbatim, so an app that did
/// `set_actor(ActorId::from(app_id.as_bytes()))` leaves the UTF-8 of `app_id`
/// in the hex. Returns `None` for anything that is not a plausible handle:
/// odd-length or non-hex input, non-UTF-8 bytes, control characters, an empty
/// string, or something longer than a name could reasonably be. A 32-byte
/// public key almost never survives this filter, and when it does the caller
/// still sees the short hex alongside it.
pub fn actor_handle(actor_hex: &str) -> Option<String> {
    let bytes = decode_hex(actor_hex)?;
    if bytes.is_empty() || bytes.len() > MAX_HANDLE_LEN {
        return None;
    }
    let s = String::from_utf8(bytes).ok()?;
    if s.chars().any(|c| c.is_control()) {
        return None;
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// First [`SHORT_LEN`] hex characters of an actor id, for the dim monospace
/// label next to the name. Short ids are returned whole.
pub fn short_id(actor_hex: &str) -> String {
    actor_hex.chars().take(SHORT_LEN).collect()
}

/// Deterministic avatar hue for an actor with no registered colour.
///
/// FNV-1a over the actor hex, folded into `[0, 360)`. The same actor gets the
/// same hue in every process and on every peer, which is what makes "these
/// three rows are the same stranger" readable without a name.
pub fn stable_hue(actor_hex: &str) -> f32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in actor_hex.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    (hash % 360) as f32
}

/// RGB → hue in degrees, so an identity's chosen avatar colour drives the
/// timeline dot. Saturation and value are fixed by
/// `crate::widget::history::hue_to_rgb`, so only the hue survives; that is
/// deliberate, it keeps the dots a coherent family.
fn rgb_to_hue(rgb: [u8; 3]) -> f32 {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d <= f32::EPSILON {
        // Grey has no hue. First boot writes [128, 128, 128], so this is the
        // common case until the user picks a colour: give it a fixed,
        // recognisable slot rather than a random one.
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The apps set their actor to the UTF-8 of `QUICK_APP_ID`. That has to
    /// come back out, because it is the same key the presence colours and the
    /// per-character author marks already use — a timeline that called the
    /// caret's owner something else would contradict the editor.
    #[test]
    fn handle_round_trips_the_app_id_an_app_set() {
        let actor = automerge::ActorId::from("alice".as_bytes());
        let hex = actor.to_hex_string();
        assert_eq!(actor_handle(&hex).as_deref(), Some("alice"));

        let dir = AuthorDirectory::new();
        assert_eq!(dir.resolve(&hex).name, "alice");
    }

    /// A 32-byte public key is not a handle. It must not be rendered as
    /// mojibake in the name slot.
    #[test]
    fn a_public_key_actor_is_not_mistaken_for_a_handle() {
        // A key with high bytes: not valid UTF-8, so the handle decode fails.
        let key: Vec<u8> = (0u8..32).map(|i| 0x80u8.wrapping_add(i)).collect();
        let hex = automerge::ActorId::from(key.as_slice()).to_hex_string();
        assert_eq!(actor_handle(&hex), None, "a raw key must not decode to a handle");

        let dir = AuthorDirectory::new();
        let a = dir.resolve(&hex);
        assert_eq!(a.name, UNKNOWN_AUTHOR_NAME);
        assert_eq!(a.identity_short.len(), SHORT_LEN);
        assert!(hex.starts_with(&a.identity_short), "short id prefixes the actor");
    }

    /// An unknown actor gets a stable colour, so the same stranger is
    /// recognisable across rows even with no name. Different actors get
    /// different colours.
    #[test]
    fn unknown_actors_get_stable_distinct_hues() {
        let a = "ff00ff00ff00ff00";
        let b = "00ff00ff00ff00ff";
        assert_eq!(stable_hue(a), stable_hue(a), "hue is deterministic");
        assert_ne!(stable_hue(a), stable_hue(b), "distinct actors, distinct hues");
        assert!((0.0..360.0).contains(&stable_hue(a)));
        assert!((0.0..360.0).contains(&stable_hue(b)));
    }

    /// An explicit registration beats the decoded handle — the collaboration
    /// layer knows the peer's real display name, the handle is only what the
    /// process called itself.
    #[test]
    fn registration_wins_over_the_decoded_handle() {
        let hex = automerge::ActorId::from("wp-1234".as_bytes()).to_hex_string();
        let mut dir = AuthorDirectory::new();
        assert_eq!(dir.resolve(&hex).name, "wp-1234");
        assert!(!dir.is_registered(&hex));

        dir.register(&hex, HistoryAuthor::new("Quinten", 210.0, "abcdef01"));
        let a = dir.resolve(&hex);
        assert_eq!(a.name, "Quinten");
        assert_eq!(a.avatar_hue, 210.0);
        assert_eq!(a.identity_short, "abcdef01");
        assert!(dir.is_registered(&hex));
    }

    /// Case-insensitive: `to_hex_string` is lowercase but callers that got the
    /// id from somewhere else must still hit the same entry.
    #[test]
    fn lookup_is_case_insensitive() {
        let mut dir = AuthorDirectory::new();
        dir.register("ABCDEF01", HistoryAuthor::new("Bob", 30.0, "abcdef01"));
        assert_eq!(dir.resolve("abcdef01").name, "Bob");
        assert_eq!(dir.resolve("AbCdEf01").name, "Bob");
    }

    /// The identity file's avatar colour drives the dot, so the timeline
    /// agrees with the colour the user picked at first boot.
    #[test]
    fn rgb_maps_onto_the_expected_hue_sextants() {
        assert!((rgb_to_hue([255, 0, 0]) - 0.0).abs() < 0.5, "red");
        assert!((rgb_to_hue([0, 255, 0]) - 120.0).abs() < 0.5, "green");
        assert!((rgb_to_hue([0, 0, 255]) - 240.0).abs() < 0.5, "blue");
        // First boot writes neutral grey; it has no hue and must not blow up.
        assert_eq!(rgb_to_hue([128, 128, 128]), 0.0);
    }

    /// Garbage in the actor slot must not produce a garbage name.
    #[test]
    fn malformed_actor_ids_fall_through_to_unknown() {
        let dir = AuthorDirectory::new();
        for bad in ["", "zz", "abc", "00", "0a0b0c0d0e0f1011"] {
            let a = dir.resolve(bad);
            assert!(
                a.name == UNKNOWN_AUTHOR_NAME || !a.name.is_empty(),
                "{bad:?} produced an empty name"
            );
        }
        // A NUL-containing "handle" is not a handle.
        assert_eq!(actor_handle("610062"), None, "control bytes are rejected");
    }

    /// A real on-disk identity file is read and turned into an author. This
    /// exercises the same `runtime_identity` parse the launcher uses, on the
    /// exact JSON shape `shell::firstboot` writes.
    #[test]
    fn local_identity_is_read_from_the_firstboot_json_shape() {
        let record = serde_json::json!({
            "display_name": "Quinten",
            "avatar_color": [0u8, 255u8, 0u8],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vec![7u8; 32],
                "threshold": 1u16,
                "total_shares": 1u16,
                "share_index": 1u16,
                "secret_bytes": vec![9u8; 32],
            },
            "created_at": "2026-07-28T00:00:00Z",
            "schema_version": 1u32,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        let id = runtime_identity::load_from_bytes(&bytes).expect("firstboot shape parses");
        assert_eq!(id.display_name(), "Quinten");
        let author = HistoryAuthor::new(
            id.display_name(),
            rgb_to_hue(id.avatar_color()),
            short_id(&hex_lower(&id.group_public_key())),
        );
        assert_eq!(author.name, "Quinten");
        assert!((author.avatar_hue - 120.0).abs() < 0.5, "green avatar colour");
        assert_eq!(author.identity_short, "07070707");
    }
}
