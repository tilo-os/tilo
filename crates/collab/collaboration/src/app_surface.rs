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

/// Name `bytes`, the committed surface of an app a host runs in its own
/// process (built into the host, no package of its own), for `app_id`,
/// unless a package already named one.
pub fn register_built_in(app_id: &str, bytes: &'static [u8]) {
    let hash = *blake3::hash(bytes).as_bytes();
    if let std::collections::btree_map::Entry::Vacant(entry) =
        registry().lock_recover().entry(app_id.to_owned())
    {
        entry.insert(Named { hash, bytes });
        NAMED.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Name `bytes`, the surface the signed package `app_id` ships, as the one
/// it runs with, replacing any its host held before (an update of the app).
pub fn register_package(app_id: &str, bytes: &[u8]) {
    let hash = *blake3::hash(bytes).as_bytes();
    let mut registry = registry().lock_recover();
    if registry.get(app_id).is_some_and(|named| named.hash == hash) {
        return;
    }
    // Readers below parse it on each use and take a surface that does not
    // parse as declaring nothing: say so once, here.
    if let Err(error) = collab_types::surface::SurfaceFile::parse(bytes) {
        tracing::warn!(app = app_id, %error, "package surface does not parse; it declares no records, reads or footprints");
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
    kinds()
        .lock_recover()
        .insert(kind.to_owned(), app_id.to_owned());
}

/// The app that keeps records of kind `kind`, when this process knows it: a
/// link naming the kind reaches the app that declares it without naming that
/// app itself.
#[must_use]
pub fn app_of_kind(kind: &str) -> Option<String> {
    kinds().lock_recover().get(kind).cloned()
}

/// The hash of the surface the changes to a record of kind `kind` are
/// decided against: its app's.
#[must_use]
pub fn hash_of_kind(kind: &str) -> Option<[u8; 32]> {
    let app = kinds().lock_recover().get(kind)?.clone();
    hash_of(&app)
}

/// The hash of the surface `app_id`'s changes are decided against.
#[must_use]
pub fn hash_of(app_id: &str) -> Option<[u8; 32]> {
    registry()
        .lock_recover()
        .get(app_id)
        .map(|named| named.hash)
}

/// The surface bytes of `app_id`, when this process carries them.
#[must_use]
pub fn bytes_of(app_id: &str) -> Option<&'static [u8]> {
    registry()
        .lock_recover()
        .get(app_id)
        .map(|named| named.bytes)
}

/// The person-record kinds `app_id`'s surface declares (its
/// `ResourceApp::records`): the records it may keep in, or read from, the
/// person's own room. Empty when this process holds no surface for it.
#[must_use]
pub fn person_record_kinds(app_id: &str) -> Vec<String> {
    bytes_of(app_id)
        // optional-decode: a held surface that does not parse was logged when it was named (`register_package`); it declares nothing here
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

/// Whether `reader`'s surface declares that it reads a resource kind of
/// `owner`'s (its `reads`): the read-only join to `owner`'s records an app
/// may hold. False when this process holds either surface not.
#[must_use]
pub fn reads_app(reader: &str, owner: &str) -> bool {
    let parse = |app: &str| {
        // optional-decode: a held surface that does not parse was logged when it was named (`register_package`); it declares nothing here
        bytes_of(app).and_then(|bytes| collab_types::surface::SurfaceFile::parse(bytes).ok())
    };
    match (parse(reader), parse(owner)) {
        (Some(reader), Some(owner)) => reader.reads_any_of(&owner),
        _ => false,
    }
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
        // optional-decode: a held surface that does not parse was logged when it was named (`register_package`); it declares nothing here
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

/// The claims of the changes `mutation` carries, a change this process made to
/// a record of `app_id`, each naming as units what its action may write
/// ([`with_footprints`]): what the op that carries it is signed with.
///
/// A change names what it is in its message. One store wrote a record's
/// first change with none: its schema stamp, a root change with no deps that
/// only sets the old schema version key (`tilo:schema_version`). That change
/// is the record's creation by what it is, so it is claimed as one
/// ([`collab_types::FrameworkChange::Genesis`]), as a store now names it when
/// it writes it: a record whose first change is such a stamp is handed to a
/// sealed room whole, with no change rewritten and no id changed.
///
/// # Errors
/// A change that names nothing and is no such stamp, or bytes that are no
/// changes.
pub fn claims_of(
    app_id: &str,
    mutation: &collab_types::DocumentMutation,
) -> Result<Vec<collab_types::ChangeClaim>, collab_types::ClaimError> {
    let changes = match mutation {
        collab_types::DocumentMutation::Snapshot(_) => return Ok(Vec::new()),
        collab_types::DocumentMutation::Incremental(bytes) => {
            collab_types::claim::parse_changes(bytes)?
        }
    };
    let claims = changes
        .iter()
        .map(|change| {
            if change.message().is_none() && is_schema_stamp(change) {
                return Ok(collab_types::ChangeClaim {
                    change: change.hash().0,
                    action: collab_types::ClaimedAction::Framework(
                        collab_types::FrameworkChange::Genesis,
                    ),
                    units: collab_types::record_unit(change),
                });
            }
            collab_types::claim::claims_of(std::slice::from_ref(change), collab_types::record_unit)
                .map(|mut one| one.remove(0))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(with_footprints(app_id, claims))
}

/// Whether `change` is a store's schema stamp as it was written before a
/// record's first change named itself: no deps, and every op a put of an
/// old schema version key (`tilo_resource::evolution::LEGACY_VERSION_KEYS`)
/// on the document's root.
fn is_schema_stamp(change: &automerge::Change) -> bool {
    use automerge::legacy::{Key, ObjectId};
    if !change.deps().is_empty() {
        return false;
    }
    let expanded = change.decode();
    !expanded.operations.is_empty()
        && expanded.operations.iter().all(|op| {
            op.obj == ObjectId::Root
                && !op.insert
                && matches!(&op.key, Key::Map(key)
                    if tilo_resource::evolution::LEGACY_VERSION_KEYS.contains(&key.as_str()))
                && matches!(op.action, automerge::legacy::OpType::Put(_))
        })
}

/// Why a room refuses `mutation`, a change this process made to `record` of
/// `app_id`, by what the app's surface publishes, if it does: the check every
/// receiver runs on its claims ([`collab_types::check_claims_by_kind`]), the
/// grants aside, which the broker decides. `None` when this process holds no
/// surface for the app: its receivers then hold the change until one arrives.
#[must_use]
pub fn refusal(
    record: &collab_types::Record<'_>,
    mutation: &collab_types::DocumentMutation,
) -> Option<String> {
    let claims = match claims_of(record.app, mutation) {
        Ok(claims) => claims,
        Err(error) => return Some(error.to_string()),
    };
    let file = bytes_of(record.app)
        // optional-decode: a held surface that does not parse was logged when it was named (`register_package`); it declares nothing here
        .and_then(|bytes| collab_types::surface::SurfaceFile::parse(bytes).ok())?;
    let surface_for = |kind: Option<&str>| {
        file.resource(kind).map(|resource| {
            std::sync::Arc::new(collab_types::surface::KindSurface(resource.clone()))
                as std::sync::Arc<dyn collab_types::ReceiptSurface>
        })
    };
    match collab_types::check_claims_by_kind(&claims, record, None, &surface_for, &|_, _| true) {
        Ok(collab_types::Receipt::Admit) => None,
        // The surface is here: what holds is a resource kind it does not publish, which no
        // receiver holding it admits.
        Ok(collab_types::Receipt::Hold) => {
            Some("the record's app publishes no resource the change names".to_owned())
        }
        Err(refusal) => Some(refusal.to_string()),
    }
}

/// Parse a lowercase hex hash, as [`SURFACE_HASH_ENV`] carries it.
#[must_use]
pub fn parse_hash(hex: &str) -> Option<[u8; 32]> {
    // optional-decode: callers report a hash that does not parse (`register_named` refuses with the text)
    let bytes = hex::decode(hex.trim()).ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a store's old schema stamp is claimed by what it is: a root change with no deps
    /// that only sets the schema version. Any other change that names nothing stays unclaimed.
    #[test]
    fn an_unnamed_schema_stamp_is_claimed_as_the_records_genesis_and_nothing_else_is() {
        use automerge::transaction::Transactable as _;
        let claimed = |doc: &mut automerge::AutoCommit, after: &[automerge::ChangeHash]| {
            claims_of(
                "test.stamps",
                &collab_types::DocumentMutation::Incremental(doc.save_after(after)),
            )
        };
        // The key a store wrote its first change with before records named theirs.
        let legacy_key = tilo_resource::evolution::LEGACY_VERSION_KEYS[0];
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, legacy_key, 1_u64).unwrap();
        let stamp = doc.commit().unwrap();
        let claims = claimed(&mut doc, &[]).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].change, stamp.0);
        assert_eq!(
            claims[0].action,
            collab_types::ClaimedAction::Framework(collab_types::FrameworkChange::Genesis)
        );
        // The same write on top of the record is no creation of it.
        doc.put(automerge::ROOT, legacy_key, 2_u64).unwrap();
        doc.commit();
        assert!(claimed(&mut doc, &[stamp]).is_err());
        // Nor is a first change that writes anything else.
        let mut other = automerge::AutoCommit::new();
        other.put(automerge::ROOT, legacy_key, 1_u64).unwrap();
        other.put(automerge::ROOT, "title", "x").unwrap();
        other.commit();
        assert!(claimed(&mut other, &[]).is_err());
    }

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
        let hash = blake3::hash(file).to_hex().to_string();

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
