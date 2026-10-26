//! Which app surface the changes an app makes are decided against.
//!
//! Every signed op names the content hash of its app's `surface.toml`
//! ([`collab_types::SignedOp::surface`]), the file the app's package ships at
//! its payload root. Whoever starts the app names that file
//! ([`SURFACE_FILE_ENV`]) and its hash ([`SURFACE_HASH_ENV`]): the launcher
//! the file in the package it runs, `tilo-app run`/`dev` the app's committed
//! file. The app reads it, checks the hash and registers it
//! ([`register_named`]); an app's own binary does not carry the file. A host
//! that runs an app in its own process (the shell's built-in apps, the web
//! build) carries its committed file instead ([`register_built_in`]). The bytes
//! are kept in the app's content store, from where they reach the devices and
//! relays that decide its changes.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use tilo_lock::MutexRecover as _;

/// The variable naming the path of the app's `surface.toml`.
pub const SURFACE_FILE_ENV: &str = "TILO_SURFACE";
/// The variable carrying the lowercase hex hash of that file, as whoever
/// started the app read it.
pub const SURFACE_HASH_ENV: &str = "TILO_SURFACE_HASH";

#[derive(Clone, Copy)]
struct Named {
    hash: [u8; 32],
    bytes: &'static [u8],
}

/// Counts the surfaces named so far ([`registered`]).
static NAMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times an app's surface was named in this process: a change
/// decided against a surface that was not here may be decided again once
/// this moves.
#[must_use]
pub fn registered() -> u64 {
    NAMED.load(std::sync::atomic::Ordering::Acquire)
}

/// A surface became readable other than by being named: an app's update
/// installed it. A change decided against a surface that was not here may
/// be decided again ([`registered`] moves).
pub fn arrived() {
    NAMED.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

fn registry() -> &'static Mutex<BTreeMap<String, Named>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Named>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Name `bytes`, the surface file whoever started the app named, for
/// `app_id`, when they hash to `hash`, the hash it was named with.
pub fn register_named(app_id: &str, bytes: &[u8], hash: &str) -> Result<(), String> {
    let expected =
        parse_hash(hash).ok_or_else(|| format!("{SURFACE_HASH_ENV} is not a hash: {hash:?}"))?;
    if *blake3::hash(bytes).as_bytes() != expected {
        return Err(format!(
            "the surface file {SURFACE_FILE_ENV} names does not hash to {SURFACE_HASH_ENV}"
        ));
    }
    register_package(app_id, bytes);
    Ok(())
}

/// The variables that name `bytes`, read from `path`, as an app's surface:
/// what a host that starts an app sets in its environment.
#[must_use]
pub fn launch_env(path: &std::path::Path, bytes: &[u8]) -> [(&'static str, String); 2] {
    [
        (SURFACE_FILE_ENV, path.display().to_string()),
        (SURFACE_HASH_ENV, blake3::hash(bytes).to_hex().to_string()),
    ]
}

/// Name `bytes`, the committed surface of an app a host runs in its own
/// process (built into the host, no package of its own), for `app_id`,
/// unless a package already named one.
pub fn register_built_in(app_id: &str, bytes: &'static [u8]) {
    let hash = *blake3::hash(bytes).as_bytes();
    if let Ok(mut registry) = registry().lock()
        && let std::collections::btree_map::Entry::Vacant(entry) = registry.entry(app_id.to_owned())
    {
        entry.insert(Named { hash, bytes });
        NAMED.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Name `bytes`, the surface the signed package `app_id` ships, as the one
/// it runs with, replacing any its host held before (an update of the app).
pub fn register_package(app_id: &str, bytes: &[u8]) {
    let hash = *blake3::hash(bytes).as_bytes();
    let Ok(mut registry) = registry().lock() else {
        return;
    };
    if registry.get(app_id).is_some_and(|named| named.hash == hash) {
        return;
    }
    // Kept for the process's life, once per distinct surface.
    let bytes = held_bytes(hash, bytes);
    registry.insert(app_id.to_owned(), Named { hash, bytes });
    NAMED.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// The bytes of surface `hash`, kept once however many apps or launches
/// name it.
fn held_bytes(hash: [u8; 32], bytes: &[u8]) -> &'static [u8] {
    static HELD: OnceLock<Mutex<BTreeMap<[u8; 32], &'static [u8]>>> = OnceLock::new();
    let mut held = HELD.get_or_init(Mutex::default).lock_recover();
    held.entry(hash)
        .or_insert_with(|| Box::leak(bytes.to_vec().into_boxed_slice()))
}

fn kinds() -> &'static Mutex<BTreeMap<String, String>> {
    static KINDS: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    KINDS.get_or_init(Mutex::default)
}

/// Records of kind `kind` belong to the app `app_id`.
pub fn register_kind(kind: &str, app_id: &str) {
    if let Ok(mut kinds) = kinds().lock() {
        kinds.insert(kind.to_owned(), app_id.to_owned());
    }
}

/// The app that keeps records of kind `kind`, when this process knows it: a
/// link naming the kind reaches the app that declares it without naming that
/// app itself.
#[must_use]
pub fn app_of_kind(kind: &str) -> Option<String> {
    kinds().lock().ok()?.get(kind).cloned()
}

/// The hash of the surface the changes to a record of kind `kind` are
/// decided against: its app's.
#[must_use]
pub fn hash_of_kind(kind: &str) -> Option<[u8; 32]> {
    let app = kinds().lock().ok()?.get(kind)?.clone();
    hash_of(&app)
}

/// The hash of the surface `app_id`'s changes are decided against.
#[must_use]
pub fn hash_of(app_id: &str) -> Option<[u8; 32]> {
    registry().lock().ok()?.get(app_id).map(|named| named.hash)
}

/// The surface bytes of `app_id`, when this process carries them.
#[must_use]
pub fn bytes_of(app_id: &str) -> Option<&'static [u8]> {
    registry().lock().ok()?.get(app_id).map(|named| named.bytes)
}

/// The person-record kinds `app_id`'s surface declares (its
/// `ResourceApp::records`): the records it may keep in, or read from, the
/// person's own room. Empty when this process holds no surface for it.
#[must_use]
pub fn person_record_kinds(app_id: &str) -> Vec<String> {
    bytes_of(app_id)
        .and_then(|bytes| collab_types::surface::SurfaceFile::parse(bytes).ok())
        .map(|file| {
            file.resources
                .into_iter()
                .filter(|surface| surface.person)
                .map(|surface| surface.kind)
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn keeps_person_record(app_id: &str, kind: &str) -> bool {
    person_record_kinds(app_id)
        .iter()
        .any(|known| known == kind)
}

/// `claims` naming as units what each action may write by `app_id`'s
/// surface: what a relay that cannot read a sealed change checks it against
/// (devices that open it check what it really wrote). Every host that signs a
/// change names its units this way; a claim left on the whole record is
/// refused wherever the action may only write some of its units (a comment,
/// a message). Unchanged when this process holds no surface for the app.
#[must_use]
pub fn with_footprints(
    app_id: &str,
    mut claims: Vec<collab_types::ChangeClaim>,
) -> Vec<collab_types::ChangeClaim> {
    use collab_types::ReceiptSurface as _;
    let Some(file) =
        bytes_of(app_id).and_then(|bytes| collab_types::surface::SurfaceFile::parse(bytes).ok())
    else {
        return claims;
    };
    for claim in &mut claims {
        let kind = claim
            .action
            .members()
            .into_iter()
            .find_map(|action| match action {
                collab_types::ClaimedAction::Verb { action, .. } => Some(action.kind.clone()),
                collab_types::ClaimedAction::Edit { kind, .. } if !kind.is_empty() => {
                    Some(kind.clone())
                }
                _ => None,
            });
        if let Some(resource) = file.resource(kind.as_deref()) {
            let surface = collab_types::surface::KindSurface(resource.clone());
            claim.units = surface.footprint(&claim.action);
        }
    }
    claims
}

/// Parse a lowercase hex hash, as [`SURFACE_HASH_ENV`] carries it.
#[must_use]
pub fn parse_hash(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex.trim()).ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_kinds_a_surface_marks_are_the_persons() {
        static FILE: &[u8] = b"[[resources]]\nkind = \"test.own\"\nlabel = \"Own\"\nversion = 1\n\n[[resources]]\nkind = \"test.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n";
        register_package("test.person-kinds", FILE);
        assert_eq!(person_record_kinds("test.person-kinds"), ["test.history"]);
        assert!(keeps_person_record("test.person-kinds", "test.history"));
        assert!(!keeps_person_record("test.person-kinds", "test.own"));
        assert!(!keeps_person_record("test.unknown-app", "test.history"));
    }

    /// A change to a part of a record its app keeps apart (a PDF's
    /// comments) claims only that part: claimed on the whole record, the
    /// room refuses it, since no action of it may write the whole record.
    #[test]
    fn a_claim_names_the_units_its_action_may_write() {
        use collab_types::{ChangeClaim, ClaimedAction, ReceiptRefusal};
        use tilo_actions::PathStep;
        static FILE: &[u8] = b"[[resources]]\nkind = \"test.paper\"\nlabel = \"Paper\"\nversion = 1\nfields = [\n  { path = \"title\", type = \"String\" },\n  { path = \"comments\", type = \"Keyed<Note>\", stored = \"keyed\", unit = true },\n  { path = \"comments.*.text\", type = \"String\" },\n  { path = \"comments.*.resolved\", type = \"bool\" },\n]\n";
        register_package("test.footprints", FILE);
        let edit = |field: &str| ClaimedAction::Edit {
            kind: "test.paper".into(),
            path: vec![
                PathStep::Field("comments".into()),
                PathStep::Item("c1".into()),
                PathStep::Field(field.into()),
            ],
        };
        let group = ClaimedAction::Group(vec![edit("text"), edit("resolved")]);
        let whole_record = vec![ChangeClaim {
            change: [7; 32],
            action: group,
            units: vec![Vec::new()],
        }];
        let file = collab_types::surface::SurfaceFile::parse(FILE).unwrap();
        let receiver = collab_types::surface::KindSurface(file.resource(None).unwrap().clone());
        let record = collab_types::Record {
            room: "room-1",
            app: "test.footprints",
            record: "paper-1",
        };
        let decide = |claims: &[ChangeClaim]| {
            collab_types::check_claims(claims, &record, None, None, Some(&receiver), &|_, _| true)
        };
        assert!(matches!(
            decide(&whole_record),
            Err(ReceiptRefusal::OutsideFootprint { .. })
        ));
        let claimed = with_footprints("test.footprints", whole_record);
        let comment = vec![
            PathStep::Field("comments".into()),
            PathStep::Item("c1".into()),
        ];
        assert_eq!(claimed[0].units, [comment.clone(), comment]);
        assert_eq!(decide(&claimed), Ok(collab_types::Receipt::Admit));
    }

    #[test]
    fn a_launched_package_surface_replaces_the_one_held() {
        register_package(
            "test.package-surface",
            b"[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 1\n",
        );
        let package = b"[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 2\n\n[[resources]]\nkind = \"test.pkg-history\"\nlabel = \"History\"\nperson = true\nversion = 1\n".to_vec();
        register_package("test.package-surface", &package);
        assert_eq!(
            hash_of("test.package-surface"),
            Some(*blake3::hash(&package).as_bytes())
        );
        assert_eq!(bytes_of("test.package-surface"), Some(package.as_slice()));
        assert_eq!(
            person_record_kinds("test.package-surface"),
            ["test.pkg-history"]
        );
    }

    #[test]
    fn a_named_surface_registers_only_when_it_hashes_to_its_name() {
        let file = b"[[resources]]\n";
        let path = std::path::Path::new("/payload/surface.toml");
        let [(file_key, named_path), (hash_key, hash)] = launch_env(path, file);
        assert_eq!(
            (file_key, named_path.as_str()),
            (SURFACE_FILE_ENV, "/payload/surface.toml")
        );
        assert_eq!(hash_key, SURFACE_HASH_ENV);

        let other = hex::encode([1; 32]);
        assert!(register_named("test.surface-a", file, &other).is_err());
        assert!(register_named("test.surface-a", file, "not hex").is_err());
        assert_eq!(hash_of("test.surface-a"), None, "nothing named");

        register_named("test.surface-a", file, &hash).unwrap();
        assert_eq!(
            hash_of("test.surface-a"),
            Some(*blake3::hash(file).as_bytes())
        );
        assert_eq!(bytes_of("test.surface-a"), Some(&file[..]));
        assert_eq!(parse_hash(&hex::encode([7; 32])), Some([7; 32]));
    }

    #[test]
    fn a_built_in_surface_never_replaces_a_packages() {
        static BUILT: &[u8] =
            b"[[resources]]\nkind = \"test.built\"\nlabel = \"Built\"\nversion = 1\n";
        register_built_in("test.built-in", BUILT);
        assert_eq!(bytes_of("test.built-in"), Some(BUILT));
        let package = b"[[resources]]\n".to_vec();
        register_package("test.built-in", &package);
        register_built_in("test.built-in", BUILT);
        assert_eq!(
            bytes_of("test.built-in"),
            Some(package.as_slice()),
            "the package's stays"
        );
    }
}
