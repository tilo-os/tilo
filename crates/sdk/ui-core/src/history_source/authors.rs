//! Resolving an automerge actor id to the person behind it.
//!
//! `tilo_resource::history::Version` hands back an `actor_hex` — a raw hex string,
//! unreadable on its own. This module is the join.
//!
//! Three shapes occur in documents: a raw 32-byte public key (the
//! only cryptographically meaningful one, joining to a
//! `collab_types::Identity`); a self-declared handle
//! (`ActorId::from(app_id.as_bytes())`, the same key `tilo_ui::presence::user_color`
//! and the per-character `author` marks already use, but **unverified**); and
//! a random id (`ActorId::random()`, which resolves to nobody).
//!
//! For an unknown actor, use [`UNKNOWN_AUTHOR_NAME`] as the name, the first
//! eight hex characters as the label, and the same
//! [`crate::presence::user_color`] / [`crate::presence::PEER_PALETTE`] slot
//! presence uses for that identity, so history and presence cannot disagree.

use std::collections::HashMap;

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
/// Registrations win over inference: the collaboration layer resolves peer
/// identity and the local process resolves the user, so both call
/// [`AuthorDirectory::register`]. Everything else falls through the
/// handle-decode and then the unknown-participant path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthorDirectory {
    by_actor: HashMap<String, HistoryAuthor>,
    /// A person's own colour (`Collaborator::cursor_color`, local
    /// `avatar_color`). Wins over [`crate::presence::user_color`] for
    /// that identity, matching [`crate::presence::PresenceCtx::color_for`].
    color_overrides: HashMap<String, (u8, u8, u8)>,
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

    /// Bind `actor_hex` to the local user, read from the Tilo identity on disk.
    /// A `false` return is not an error: an app run outside a Tilo session has
    /// no identity, and the caller should leave the actor to resolve through
    /// the handle path.
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
        let registered = self
            .by_actor
            .get(&key)
            .or_else(|| handle_actor_hex(&key).and_then(|handle| self.by_actor.get(&handle)));
        if let Some(a) = registered {
            let mut presented = a.clone();
            presented.name = participant_display_label(&presented.name);
            let override_key = actor_handle(&key).unwrap_or_else(|| key.clone());
            if let Some(rgb) = self.color_overrides.get(&override_key) {
                presented = presented.with_rgb(*rgb);
            }
            return presented;
        }
        let short = short_id(&key);
        match actor_handle(&key) {
            Some(handle) => {
                self.author_from_identity(participant_display_label(&handle), &handle, short)
            }
            None => self.author_from_identity(UNKNOWN_AUTHOR_NAME, &key, short),
        }
    }

    /// Seed or replace this identity's colour so history agrees with
    /// [`crate::presence::PresenceCtx::color_for`].
    pub fn set_color_override(&mut self, identity: impl Into<String>, rgb: (u8, u8, u8)) {
        self.color_overrides.insert(identity.into(), rgb);
    }

    /// The colour history paints `identity` in: the person's own colour
    /// when one was seeded, otherwise the shared palette slot.
    pub fn colour_for(&self, identity: &str) -> (u8, u8, u8) {
        self.color_overrides
            .get(identity)
            .copied()
            .unwrap_or_else(|| crate::presence::user_color(identity))
    }

    fn author_from_identity(
        &self,
        name: impl Into<String>,
        identity: &str,
        identity_short: impl Into<String>,
    ) -> HistoryAuthor {
        author_coloured(name, identity_short, self.colour_for(identity))
    }

    /// True when this actor has an explicitly registered identity, as opposed
    /// to a decoded handle or the unknown-participant fallback. The chrome
    /// uses this to decide whether a row's author is trustworthy enough to,
    /// say, offer a "message them" affordance.
    pub fn is_registered(&self, actor_hex: &str) -> bool {
        self.by_actor.contains_key(&actor_hex.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.by_actor.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_actor.is_empty()
    }
}

/// Turn a durable collaboration principal into copy suitable for people-facing
/// history and audit UI.
///
/// Agent principals deliberately remain stored and transported as
/// `agent:<id>`: the prefix is part of the authorization boundary. Rendering
/// that wire identifier verbatim, however, makes the UI look unfinished and
/// obscures the important fact that the actor was software. This projection
/// removes only the protocol prefix and adds a persistent, readable kind
/// label. Human names and other identifiers pass through unchanged.
pub fn participant_display_label(principal: &str) -> String {
    match agent_principal_name(principal) {
        Some(name) => agent_display_label(&name),
        None => principal.trim().to_string(),
    }
}

/// The readable name of an `agent:<id>` principal, without the kind label:
/// the id, or the localized unknown-agent name when it is empty. `None` for
/// any principal that is not an agent. For surfaces that already mark the
/// actor as software, such as an agent badge.
pub fn agent_principal_name(principal: &str) -> Option<String> {
    let name = principal.trim().strip_prefix("agent:")?.trim();
    Some(if name.is_empty() {
        crate::i18n::widget_format("participant-unknown-agent", &[])
    } else {
        name.to_string()
    })
}

/// `name` with the localized agent kind label, as every agent is shown in
/// history, presence and activity.
pub fn agent_display_label(name: &str) -> String {
    crate::i18n::widget_format("participant-agent", &[("name", name.into())])
}

/// Build a [`HistoryAuthor`] from the local identity
/// ([`crate::local_identity`]), if there is one.
/// Public because the shell wants the same answer for its own chrome without
/// having to own an [`AuthorDirectory`].
pub fn local_identity_author() -> Option<HistoryAuthor> {
    let id = crate::local_identity::load()?;
    let key_hex = hex::encode(id.group_public_key);
    let name = if id.display_name.is_empty() {
        // First boot writes an empty display name and the user names
        // themselves later. An empty string would render as a blank row, so
        // fall back to the stable label rather than to a guess.
        UNKNOWN_AUTHOR_NAME.to_string()
    } else {
        id.display_name
    };
    Some(author_coloured(
        name,
        short_id(&key_hex),
        (id.avatar_color[0], id.avatar_color[1], id.avatar_color[2]),
    ))
}

/// History row for `identity`: the shared palette slot, same RGB
/// [`crate::presence::user_color`] / [`crate::presence::PresenceCtx::color_for`]
/// pick when no override is set.
pub fn author_for_identity(name: impl Into<String>, identity: &str) -> HistoryAuthor {
    author_coloured(
        name,
        short_id(identity),
        crate::presence::user_color(identity),
    )
}

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
    let bytes = hex::decode(actor_hex).ok()?;
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

/// The bare handle's actor of a [`device_actor`], hex-encoded.
fn handle_actor_hex(actor_hex: &str) -> Option<String> {
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
/// `crate::widget::history::hue_to_rgb`, so only the hue survives —
/// deliberate, it keeps the dots a coherent family.
fn rgb_to_hue(rgb: [u8; 3]) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actors_are_named_by_registration_handle_or_agent_and_never_misread() {
        // phase: handle_round_trips_the_app_id_an_app_set
        // Apps set their actor to the UTF-8 of `QUICK_APP_ID`; that has to come
        // back out, since it's the same key presence colours and author marks use.
        {
            let actor = automerge::ActorId::from("alice".as_bytes());
            let hex = actor.to_hex_string();
            assert_eq!(actor_handle(&hex).as_deref(), Some("alice"));

            let dir = AuthorDirectory::new();
            assert_eq!(dir.resolve(&hex).name, "alice");
        }
        // phase: agent_principals_are_presented_as_named_agents_not_wire_ids
        {
            assert_eq!(participant_display_label("agent:scribe"), "scribe · Agent");
            assert_eq!(
                participant_display_label("agent:opaque-runtime-id"),
                "opaque-runtime-id · Agent"
            );
            assert_eq!(participant_display_label("agent:"), "Unknown agent · Agent");
            assert_eq!(participant_display_label("ada"), "ada");

            let actor = automerge::ActorId::from("agent:scribe".as_bytes());
            let resolved = AuthorDirectory::new().resolve(&actor.to_hex_string());
            assert_eq!(resolved.name, "scribe · Agent");
            assert!(!resolved.name.contains("agent:"));

            let mut registered = AuthorDirectory::new();
            registered.register(
                "aabbccdd",
                HistoryAuthor::new("agent:reviewer", 42.0, "aabbccdd"),
            );
            assert_eq!(registered.resolve("aabbccdd").name, "reviewer · Agent");
        }
        // phase: a_public_key_actor_is_not_mistaken_for_a_handle
        // A 32-byte public key is not a handle. It must not be rendered as
        // mojibake in the name slot.
        {
            // A key with high bytes: not valid UTF-8, so the handle decode fails.
            let key: Vec<u8> = (0u8..32).map(|i| 0x80u8.wrapping_add(i)).collect();
            let hex = automerge::ActorId::from(key.as_slice()).to_hex_string();
            assert_eq!(
                actor_handle(&hex),
                None,
                "a raw key must not decode to a handle"
            );

            let dir = AuthorDirectory::new();
            let a = dir.resolve(&hex);
            assert_eq!(a.name, UNKNOWN_AUTHOR_NAME);
            assert_eq!(a.identity_short.len(), SHORT_LEN);
            assert!(
                hex.starts_with(&a.identity_short),
                "short id prefixes the actor"
            );
        }
        // phase: registration_wins_over_the_decoded_handle
        // An explicit registration beats the decoded handle — the collaboration
        // layer knows the peer's registered name; the handle is self-declared.
        {
            let hex = automerge::ActorId::from("wp-1234".as_bytes()).to_hex_string();
            let mut dir = AuthorDirectory::new();
            assert_eq!(dir.resolve(&hex).name, "wp-1234");
            assert!(!dir.is_registered(&hex));

            dir.register(&hex, HistoryAuthor::new("ada", 210.0, "abcdef01"));
            let a = dir.resolve(&hex);
            assert_eq!(a.name, "ada");
            assert_eq!(a.avatar_hue, 210.0);
            assert_eq!(a.identity_short, "abcdef01");
            assert!(dir.is_registered(&hex));
        }
        // phase: lookup_is_case_insensitive
        // Case-insensitive: `to_hex_string` is lowercase but callers that got the
        // id from somewhere else must still hit the same entry.
        {
            let mut dir = AuthorDirectory::new();
            dir.register("ABCDEF01", HistoryAuthor::new("Bob", 30.0, "abcdef01"));
            assert_eq!(dir.resolve("abcdef01").name, "Bob");
            assert_eq!(dir.resolve("AbCdEf01").name, "Bob");
        }
        // phase: malformed_actor_ids_fall_through_to_unknown
        // Garbage in the actor slot must not produce a garbage name.
        {
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
        // phase: a_missing_identity_file_is_not_an_author
        // With no identity anywhere, the chrome gets `None` and falls through to
        // the handle/unknown ladder — an app run outside a Tilo session is not an
        // error.
        {
            let dir = tempfile::tempdir().expect("temp dir");
            let _env = IdentityEnvGuard::pointing_at(&dir.path().join("absent.json"), dir.path());
            assert!(local_identity_author().is_none());
        }
    }

    #[test]
    fn unknown_actors_get_stable_distinct_hues_in_the_right_sextants() {
        // phase: unknown_actors_get_stable_distinct_hues
        // An unknown actor gets a stable colour, so the same stranger is
        // recognisable across rows even with no name.
        {
            let a = "ff00ff00ff00ff00";
            let b = "00ff00ff00ff00ff";
            assert_eq!(stable_hue(a), stable_hue(a), "hue is deterministic");
            assert_ne!(
                stable_hue(a),
                stable_hue(b),
                "distinct actors, distinct hues"
            );
            assert!((0.0..360.0).contains(&stable_hue(a)));
            assert!((0.0..360.0).contains(&stable_hue(b)));
        }
        // phase: rgb_maps_onto_the_expected_hue_sextants
        // The identity file's avatar colour drives the dot, so the timeline
        // agrees with the colour the user picked at first boot.
        {
            assert!((rgb_to_hue([255, 0, 0]) - 0.0).abs() < 0.5, "red");
            assert!((rgb_to_hue([0, 255, 0]) - 120.0).abs() < 0.5, "green");
            assert!((rgb_to_hue([0, 0, 255]) - 240.0).abs() < 0.5, "blue");
            // First boot writes neutral grey; it has no hue and must not blow up.
            assert_eq!(rgb_to_hue([128, 128, 128]), 0.0);
        }
    }

    /// Seed for the minted test identities. Any fixed value works — it is
    /// pinned only so the derived public key, and therefore the expected
    /// short id, is the same on every run.
    const FIXTURE_SEED: [u8; 32] = [0x5a; 32];

    /// Mint an identity record through the same path as
    /// `shell::firstboot::ensure_primary_identity`. Return the JSON bytes and
    /// independently derive the public key with `ed25519-dalek` so the test
    /// checks both sides of the serialization chain.
    fn firstboot_identity_bytes(display_name: &str, avatar_color: [u8; 3]) -> (Vec<u8>, [u8; 32]) {
        let identity =
            threshold_identity::ThresholdIdentity::from_single_device_seed(&FIXTURE_SEED);
        let record = tilo_identity_ceremony::IdentityRecord::from_threshold_identity(
            &identity,
            display_name,
            avatar_color,
            crate::time::SystemTime::UNIX_EPOCH
                .checked_add(crate::time::Duration::from_secs(1_785_000_000))
                .unwrap_or(crate::time::SystemTime::UNIX_EPOCH),
        )
        .expect("a single-device identity is persistable");
        let bytes = record.to_json_bytes().expect("record encodes to JSON");
        let derived = ed25519_dalek::SigningKey::from_bytes(&FIXTURE_SEED)
            .verifying_key()
            .to_bytes();
        (bytes, derived)
    }

    fn secure_test_identity(_path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o600))
                .expect("secure identity permissions");
        }
    }

    /// Bound every source `local_identity_author` consults to the test's own
    /// temp dir, and put the environment back afterwards, panic or not.
    ///
    /// Both variables matter: `TILO_USER_IDENTITY` is the launcher-provided
    /// path tried first; `XDG_CONFIG_HOME` is the fallback tried second, and
    /// leaving it alone would let a developer's real
    /// `~/.config/tilo/identity/primary.json` decide the result.
    struct IdentityEnvGuard {
        _env: tilo_test_env::ScopedEnv,
    }

    impl IdentityEnvGuard {
        fn pointing_at(identity_path: &std::path::Path, config_home: &std::path::Path) -> Self {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_USER_IDENTITY", identity_path)
                .set("XDG_CONFIG_HOME", config_home);
            Self { _env: env }
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_local_identity_is_read_from_firstboot_json_or_left_unknown() {
        // phase: local_identity_is_read_from_the_firstboot_json_shape
        // An on-disk identity file is read and turned into an author.
        //
        // The record is minted by the writer `shell::firstboot` calls. This schema
        // is declared twice on purpose
        // (`tilo_identity_ceremony::IdentityRecord` serialises it,
        // `runtime_identity` re-declares it rather than depending on `ui`), so
        // so the fixture checks that the reader accepts the writer's output.
        {
            let (bytes, public_key) = firstboot_identity_bytes("ada", [0, 255, 0]);

            // The reader accepts what the writer produced, field for field.
            let id = runtime_identity::load_from_bytes(&bytes).expect("firstboot shape parses");
            assert_eq!(id.display_name(), "ada");
            assert_eq!(id.avatar_color(), [0, 255, 0]);
            assert_eq!(
                id.group_public_key(),
                public_key,
                "the record must carry the key its own secret derives"
            );

            // The production entry point turns the file into the row the timeline
            // renders.
            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("primary.json");
            std::fs::write(&path, &bytes).expect("write identity");
            secure_test_identity(&path);
            let _env = IdentityEnvGuard::pointing_at(&path, dir.path());

            let author =
                local_identity_author().expect("an identity on disk resolves to an author");
            assert_eq!(author.name, "ada");
            assert!(
                (author.avatar_hue - 120.0).abs() < 0.5,
                "green avatar colour"
            );
            assert_eq!(
                author.rgb(),
                (0, 255, 0),
                "the person's own colour wins, not hue_to_rgb of its hue"
            );
            assert_eq!(author.identity_short, short_id(&hex::encode(public_key)));
            assert_eq!(author.identity_short.len(), SHORT_LEN);
        }
        // phase: a_freshly_provisioned_identity_renders_as_unknown_rather_than_blank
        // First boot writes an empty display name and neutral grey — the user
        // names and colours themselves later. That record must still load, and
        // must render as the unknown-participant label rather than as a
        // blank row.
        {
            let (bytes, public_key) = firstboot_identity_bytes("", [128, 128, 128]);

            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("primary.json");
            std::fs::write(&path, &bytes).expect("write identity");
            secure_test_identity(&path);
            let _env = IdentityEnvGuard::pointing_at(&path, dir.path());

            let author =
                local_identity_author().expect("a provisioned identity resolves to an author");
            assert_eq!(author.name, UNKNOWN_AUTHOR_NAME);
            assert_eq!(author.avatar_hue, 0.0, "neutral grey has no hue");
            assert_eq!(author.identity_short, short_id(&hex::encode(public_key)));
        }
    }

    #[test]
    fn history_and_presence_pick_the_same_colour_for_the_same_identity() {
        use crate::permissions::UserId;
        use crate::presence::{PresenceCtx, user_color};

        for id in [
            "alice",
            "bob",
            "carol",
            "agent:scribe",
            "unknown-person-xyz",
        ] {
            let hex = automerge::ActorId::from(id.as_bytes()).to_hex_string();
            let history = AuthorDirectory::new().resolve(&hex);
            let presence = PresenceCtx::default().color_for(&UserId(id.to_string()));
            assert_eq!(
                history.rgb(),
                presence,
                "history and presence diverged for {id}"
            );
            assert_eq!(
                history.rgb(),
                user_color(id),
                "unoverridden identity must land on the shared palette slot for {id}"
            );
        }

        let unknown = "ff00ff00ff00ff00";
        let history = AuthorDirectory::new().resolve(unknown);
        assert_eq!(
            history.rgb(),
            PresenceCtx::default().color_for(&UserId(unknown.into())),
            "unknown people share the palette slot keyed by that identity"
        );

        let mut ctx = PresenceCtx::default();
        ctx.set_color_override(UserId("carol".into()), "#112233");
        let mut dir = AuthorDirectory::new();
        dir.set_color_override("carol", (0x11, 0x22, 0x33));
        let hex = automerge::ActorId::from("carol".as_bytes()).to_hex_string();
        assert_eq!(
            dir.resolve(&hex).rgb(),
            ctx.color_for(&UserId("carol".into())),
            "a person's own colour must win on history and presence together"
        );
        assert_eq!(dir.resolve(&hex).rgb(), (0x11, 0x22, 0x33));
        assert_ne!(
            user_color("carol"),
            (0x11, 0x22, 0x33),
            "the override must differ from the palette or the win is invisible"
        );
    }
}
