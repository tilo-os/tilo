//! Which app surface the changes an app makes are decided against.
//!
//! Every signed op names the content hash of its app's `surface.toml`
//! ([`collab_types::SignedOp::surface`]), the file the app's package ships.
//! A launched package's launcher hands the hash to the app (`TILO_SURFACE`);
//! an app run from its build carries its committed file
//! ([`tilo_resource::committed_surface`]), whose hash is the one its package
//! would name. The bytes are kept in the app's content store, from where
//! they reach the devices and relays that decide its changes.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
struct Named {
    hash: [u8; 32],
    bytes: Option<&'static [u8]>,
}

fn registry() -> &'static Mutex<BTreeMap<String, Named>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Named>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Name `bytes`, the app's committed surface, for `app_id`, unless its
/// launcher already named one.
pub fn register_committed(app_id: &str, bytes: Option<&'static [u8]>) {
    let Some(bytes) = bytes else {
        return;
    };
    let hash = *blake3::hash(bytes).as_bytes();
    if let Ok(mut registry) = registry().lock() {
        registry.entry(app_id.to_owned()).or_insert(Named {
            hash,
            bytes: Some(bytes),
        });
    }
}

/// Name `bytes`, the surface the signed package `app_id` ships, as the one
/// it runs with: a launched package's, which replaces any its host held
/// before (an update of the app).
pub fn register_package(app_id: &str, bytes: &[u8]) {
    let hash = *blake3::hash(bytes).as_bytes();
    let Ok(mut registry) = registry().lock() else {
        return;
    };
    if registry
        .get(app_id)
        .is_some_and(|named| named.hash == hash && named.bytes.is_some())
    {
        return;
    }
    // Kept for the process's life, once per distinct surface.
    let bytes = held_bytes(hash, bytes);
    registry.insert(
        app_id.to_owned(),
        Named {
            hash,
            bytes: Some(bytes),
        },
    );
}

/// The bytes of surface `hash`, kept once however many apps or launches
/// name it.
fn held_bytes(hash: [u8; 32], bytes: &[u8]) -> &'static [u8] {
    static HELD: OnceLock<Mutex<BTreeMap<[u8; 32], &'static [u8]>>> = OnceLock::new();
    let mut held = HELD
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.entry(hash)
        .or_insert_with(|| Box::leak(bytes.to_vec().into_boxed_slice()))
}

/// Name the surface the launcher handed `app_id`, by its hash. The
/// committed bytes stay with it when they are those.
pub fn register_launched(app_id: &str, hash: [u8; 32]) {
    if let Ok(mut registry) = registry().lock() {
        let bytes = registry
            .get(app_id)
            .and_then(|named| named.bytes)
            .filter(|bytes| blake3::hash(bytes).as_bytes() == &hash);
        registry.insert(app_id.to_owned(), Named { hash, bytes });
    }
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
    registry()
        .lock()
        .ok()?
        .get(app_id)
        .and_then(|named| named.bytes)
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

/// Whether `app_id`'s surface declares the person's records of `kind`.
#[must_use]
pub fn keeps_person_record(app_id: &str, kind: &str) -> bool {
    person_record_kinds(app_id)
        .iter()
        .any(|known| known == kind)
}

/// Parse a lowercase hex hash, as `TILO_SURFACE` carries it.
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
        register_committed("test.person-kinds", Some(FILE));
        assert_eq!(person_record_kinds("test.person-kinds"), ["test.history"]);
        assert!(keeps_person_record("test.person-kinds", "test.history"));
        assert!(!keeps_person_record("test.person-kinds", "test.own"));
        assert!(!keeps_person_record("test.unknown-app", "test.history"));
    }

    #[test]
    fn a_launched_package_surface_replaces_the_one_held() {
        static BUILT: &[u8] = b"[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 1\n";
        register_committed("test.package-surface", Some(BUILT));
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
    fn a_launched_surface_wins_and_keeps_matching_bytes() {
        static FILE: &[u8] = b"[[resources]]\n";
        register_committed("test.surface-a", Some(FILE));
        let hash = hash_of("test.surface-a").unwrap();
        assert_eq!(hash, *blake3::hash(FILE).as_bytes());
        assert_eq!(bytes_of("test.surface-a"), Some(FILE));
        register_launched("test.surface-a", hash);
        assert_eq!(bytes_of("test.surface-a"), Some(FILE), "the same file");
        register_launched("test.surface-a", [1; 32]);
        assert_eq!(hash_of("test.surface-a"), Some([1; 32]));
        assert_eq!(bytes_of("test.surface-a"), None, "another package's file");
        register_committed("test.surface-a", Some(FILE));
        assert_eq!(
            hash_of("test.surface-a"),
            Some([1; 32]),
            "the launcher's stays"
        );
        assert_eq!(parse_hash(&hex::encode([7; 32])), Some([7; 32]));
    }
}
