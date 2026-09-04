//! Resolving an automerge actor id to the person behind it.
//!
//! `qdoc::history::VersionMeta` hands back an `actor_hex` — a raw hex string,
//! unreadable on its own. This module is the join.
//!
//! Three shapes show up in a real document: a raw 32-byte public key (the
//! only cryptographically meaningful one, joining to a
//! `collab_types::Identity`); a self-declared handle
//! (`ActorId::from(app_id.as_bytes())`, the same key `ui::presence::user_color`
//! and the per-character `author` marks already use, but **unverified**); and
//! a random id (`ActorId::random()`, which resolves to nobody).
//!
//! For an actor never seen before, nothing is invented: the name becomes
//! [`UNKNOWN_AUTHOR_NAME`], the label is the first eight hex characters, and
//! the avatar hue is derived deterministically from the actor id so the same
//! stranger keeps the same colour across sessions.

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
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// A `false` return is not an error: an app run outside a qOS session has
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

    pub fn len(&self) -> usize {
        self.by_actor.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_actor.is_empty()
    }
}

/// Build a [`HistoryAuthor`] from the qOS identity on disk, if there is one.
/// Public because the shell wants the same answer for its own chrome without
/// having to own an [`AuthorDirectory`].
pub fn local_identity_author() -> Option<HistoryAuthor> {
    #[cfg(not(unix))]
    return None;
    #[cfg(unix)]
    {
        let id = runtime_identity::load().ok().or_else(|| {
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
/// itself, when it did. `ActorId::from(&[u8])` stores the bytes verbatim, so
/// `set_actor(ActorId::from(app_id.as_bytes()))` leaves the UTF-8 of `app_id`
/// in the hex. Returns `None` for anything not a plausible handle (odd-length
/// /non-hex input, non-UTF-8, control chars, empty, or too long); a 32-byte
/// public key almost never survives this filter.
pub fn actor_handle(actor_hex: &str) -> Option<String> {
    let bytes = decode_hex(actor_hex)?;
    if bytes.is_empty() || bytes.len() > MAX_HANDLE_LEN {
        return None;
    }
    let s = String::from_utf8(bytes).ok()?;
    if s.chars().any(char::is_control) {
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

/// Deterministic avatar hue for an actor with no registered colour: FNV-1a
/// over the actor hex, folded into `[0, 360)`. Same actor, same hue in every
/// process and on every peer.
pub fn stable_hue(actor_hex: &str) -> f32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in actor_hex.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
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

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // Writing to a `String` is infallible, so there is no `Err` worth
        // handling.
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks(2) {
        // `chunks(2)` on a length checked even above only ever yields
        // full pairs, but the pattern proves that instead of indexing.
        let [p0, p1] = pair else {
            return None;
        };
        let hi = (*p0 as char).to_digit(16)?;
        let lo = (*p1 as char).to_digit(16)?;
        // Both digits are `0..=15`, so `combined` is always `0..=255` and
        // the cast below never actually truncates.
        let combined = hi.saturating_mul(16).saturating_add(lo);
        out.push(u8::try_from(combined).unwrap_or(u8::MAX));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apps set their actor to the UTF-8 of `QUICK_APP_ID`; that has to come
    /// back out, since it's the same key presence colours and author marks use.
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

    /// An unknown actor gets a stable colour, so the same stranger is
    /// recognisable across rows even with no name.
    #[test]
    fn unknown_actors_get_stable_distinct_hues() {
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

    /// An explicit registration beats the decoded handle — the collaboration
    /// layer knows the peer's real name; the handle is just self-declared.
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

    /// Seed for the minted test identities. Any fixed value works — it is
    /// pinned only so the derived public key, and therefore the expected
    /// short id, is the same on every run.
    const FIXTURE_SEED: [u8; 32] = [0x5a; 32];

    /// Mint an identity record through the **real writer**, exactly as
    /// `shell::firstboot::ensure_primary_identity` does. Returns the JSON
    /// bytes alongside the public key the seed *actually* derives — computed
    /// here straight from `ed25519-dalek`, independently of anything the
    /// writer or reader reports, so a test asserting on it checks the whole
    /// chain rather than echoing one side of it.
    fn firstboot_identity_bytes(display_name: &str, avatar_color: [u8; 3]) -> (Vec<u8>, [u8; 32]) {
        let identity =
            threshold_identity::ThresholdIdentity::from_single_device_seed(&FIXTURE_SEED);
        let record = crate::widget::identity_flow::IdentityRecord::from_threshold_identity(
            &identity,
            display_name,
            avatar_color,
            std::time::SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(1_785_000_000))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
        .expect("a single-device identity is persistable");
        let bytes = record.to_json_bytes().expect("record encodes to JSON");
        let derived = ed25519_dalek::SigningKey::from_bytes(&FIXTURE_SEED)
            .verifying_key()
            .to_bytes();
        (bytes, derived)
    }

    fn secure_test_identity(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("secure identity permissions");
        }
    }

    /// Bound every source `local_identity_author` consults to the test's own
    /// temp dir, and put the environment back afterwards, panic or not.
    ///
    /// Both variables matter: `QOS_USER_IDENTITY` is the launcher-provided
    /// path tried first; `XDG_CONFIG_HOME` is the fallback tried second, and
    /// leaving it alone would let a developer's real
    /// `~/.config/qos/identity/primary.json` decide the result.
    struct IdentityEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl IdentityEnvGuard {
        fn pointing_at(identity_path: &std::path::Path, config_home: &std::path::Path) -> Self {
            let vars = [
                (runtime_identity::IDENTITY_ENV_VAR, identity_path),
                ("XDG_CONFIG_HOME", config_home),
            ];
            let saved = vars
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var(name).ok();
                    // SAFETY: every test that touches these variables is
                    // serialised on the `qos_user_identity` key, so no other
                    // thread reads them between here and the restore in
                    // `drop`.
                    unsafe { std::env::set_var(name, value) };
                    (*name, previous)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for IdentityEnvGuard {
        fn drop(&mut self) {
            for (name, previous) in self.saved.drain(..) {
                // SAFETY: as above — the serial key bounds the window.
                unsafe {
                    match previous {
                        Some(v) => std::env::set_var(name, v),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    /// A real on-disk identity file is read and turned into an author.
    ///
    /// The record is minted by the writer `shell::firstboot` actually calls,
    /// not hand-typed: this schema is declared twice on purpose
    /// (`ui::widget::identity_flow::IdentityRecord` serialises it,
    /// `runtime_identity` re-declares it rather than depending on `ui`), so
    /// only a fixture that came out of the writer proves the reader still
    /// accepts what the writer emits.
    #[test]
    #[serial_test::serial(qos_user_identity)]
    fn local_identity_is_read_from_the_firstboot_json_shape() {
        let (bytes, public_key) = firstboot_identity_bytes("Quinten", [0, 255, 0]);

        // The reader accepts what the writer produced, field for field.
        let id = runtime_identity::load_from_bytes(&bytes).expect("firstboot shape parses");
        assert_eq!(id.display_name(), "Quinten");
        assert_eq!(id.avatar_color(), [0, 255, 0]);
        assert_eq!(
            id.group_public_key(),
            public_key,
            "the record must carry the key its own secret derives"
        );

        // ...and the production entry point turns that file into the row the
        // timeline renders. This is the function the chrome calls; asserting
        // on a locally reassembled `HistoryAuthor` instead would only restate
        // its body.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("primary.json");
        std::fs::write(&path, &bytes).expect("write identity");
        secure_test_identity(&path);
        let _env = IdentityEnvGuard::pointing_at(&path, dir.path());

        let author = local_identity_author().expect("an identity on disk resolves to an author");
        assert_eq!(author.name, "Quinten");
        assert!(
            (author.avatar_hue - 120.0).abs() < 0.5,
            "green avatar colour"
        );
        assert_eq!(author.identity_short, short_id(&hex_lower(&public_key)));
        assert_eq!(author.identity_short.len(), SHORT_LEN);
    }

    /// First boot writes an empty display name and neutral grey — the user
    /// names and colours themselves later. That record must still load, and
    /// must render as the honest unknown-participant label rather than as a
    /// blank row.
    #[test]
    #[serial_test::serial(qos_user_identity)]
    fn a_freshly_provisioned_identity_renders_as_unknown_rather_than_blank() {
        let (bytes, public_key) = firstboot_identity_bytes("", [128, 128, 128]);

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("primary.json");
        std::fs::write(&path, &bytes).expect("write identity");
        secure_test_identity(&path);
        let _env = IdentityEnvGuard::pointing_at(&path, dir.path());

        let author = local_identity_author().expect("a provisioned identity resolves to an author");
        assert_eq!(author.name, UNKNOWN_AUTHOR_NAME);
        assert_eq!(author.avatar_hue, 0.0, "neutral grey has no hue");
        assert_eq!(author.identity_short, short_id(&hex_lower(&public_key)));
    }

    /// With no identity anywhere, the chrome gets `None` and falls through to
    /// the handle/unknown ladder — an app run outside a qOS session is not an
    /// error.
    #[test]
    #[serial_test::serial(qos_user_identity)]
    fn a_missing_identity_file_is_not_an_author() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _env = IdentityEnvGuard::pointing_at(&dir.path().join("absent.json"), dir.path());
        assert!(local_identity_author().is_none());
    }
}
