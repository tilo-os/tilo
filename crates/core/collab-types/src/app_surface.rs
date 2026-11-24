//! Which app surface the changes an app makes are decided against.
//!
//! Every signed op names the content hash of its app's `surface.toml`
//! ([`crate::SignedOp::surface`]), the file the app's package ships at
//! its payload root. Whoever starts the app names that file
//! ([`SURFACE_FILE_ENV`]) and its hash ([`SURFACE_HASH_ENV`]): the launcher
//! the file in the package it runs, `tilolang run`/`dev` the app's committed
//! file. The app reads it, checks the hash and registers it
//! ([`register_named`]); an app's own binary does not carry the file. A host
//! that runs an app in its own process (the shell's built-in apps, the web
//! build) carries its committed file's JSON form instead
//! ([`register_built_in`]). Either way the surface is kept as one form
//! ([`crate::surface::canonical`]), whose hash the changes name; the bytes
//! are kept in the app's content store, from where they reach the devices
//! and relays that decide its changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use tilo_lock::MutexRecover as _;

/// The variable naming the path of the app's `surface.toml`.
pub const SURFACE_FILE_ENV: &str = "TILO_SURFACE";
/// The variable carrying the lowercase hex hash of that file, as whoever
/// started the app read it.
pub const SURFACE_HASH_ENV: &str = "TILO_SURFACE_HASH";

/// A surface this process was named: the bytes a change names by hash, and
/// those bytes parsed, `None` when they do not parse (and so decide nothing).
struct Named {
    hash: [u8; 32],
    bytes: &'static [u8],
    file: Option<Arc<crate::surface::SurfaceFile>>,
}

impl Named {
    fn new(
        app_id: &str,
        hash: [u8; 32],
        bytes: &'static [u8],
        parsed: Result<crate::surface::SurfaceFile, String>,
    ) -> Self {
        let file = match parsed {
            Ok(file) => Some(Arc::new(file)),
            Err(error) => {
                tracing::warn!(app = app_id, %error, "the app's surface does not parse; it declares no records, reads or footprints");
                None
            }
        };
        Self { hash, bytes, file }
    }
}

/// The app surfaces a device knows, by app: what the changes to its records are decided
/// against. A device knows its process's ([`Surfaces::process`]), which every free function of
/// this module reads and names into; a test that runs several devices in one process, or many
/// tests side by side, gives each device one of its own, so a surface one names is not a surface
/// another knows.
#[derive(Default)]
pub struct Surfaces {
    named: Mutex<BTreeMap<String, Named>>,
    /// Counts the surfaces named so far ([`Self::registered`]).
    count: std::sync::atomic::AtomicU64,
}

impl Surfaces {
    /// The surfaces this process knows: those the apps it runs named.
    #[must_use]
    pub fn process() -> &'static Arc<Self> {
        static PROCESS: OnceLock<Arc<Surfaces>> = OnceLock::new();
        PROCESS.get_or_init(Arc::default)
    }

    /// How many times an app's surface was named here: a change decided against a surface that
    /// was not here may be decided again once this moves.
    #[must_use]
    pub fn registered(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::Acquire)
    }

    /// A surface became readable other than by being named: an app's update installed it. A
    /// change decided against a surface that was not here may be decided again
    /// ([`Self::registered`] moves).
    pub fn arrived(&self) {
        self.count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Name `json`, the JSON form `tilo_resource::committed_surface!` wrote of the committed
    /// surface of an app a host runs in its own process (built into the host, no package of its
    /// own), for `app_id`, unless a package already named one.
    pub fn register_built_in(&self, app_id: &str, json: &'static str) {
        let Some(kept) = crate::surface::canonical_json(json) else {
            tracing::warn!(
                app = app_id,
                "the app's committed surface is not JSON; it declares no resources, reads or footprints"
            );
            return;
        };
        let hash = *blake3::hash(kept.as_bytes()).as_bytes();
        if let std::collections::btree_map::Entry::Vacant(entry) =
            self.named.lock_recover().entry(app_id.to_owned())
        {
            let bytes = if kept == json {
                json.as_bytes()
            } else {
                held_bytes(hash, kept.as_bytes())
            };
            let parsed = crate::surface::SurfaceFile::from_json(&kept);
            entry.insert(Named::new(app_id, hash, bytes, parsed));
            self.arrived();
        }
    }

    /// Name `bytes`, the surface file the signed package `app_id` ships, as the one it runs
    /// with, replacing any held before (an update of the app). A file that is not a surface
    /// names none.
    pub fn register_package(&self, app_id: &str, bytes: &[u8]) {
        let (file, kept) = match crate::surface::SurfaceFile::kept(bytes) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(
                    app = app_id,
                    %error,
                    "the app's surface file does not parse; it declares no resources, reads or footprints"
                );
                return;
            }
        };
        let hash = *blake3::hash(kept.as_bytes()).as_bytes();
        if self
            .named
            .lock_recover()
            .get(app_id)
            .is_some_and(|named| named.hash == hash)
        {
            return;
        }
        // Kept for the process's life, once per distinct surface.
        let bytes = held_bytes(hash, kept.as_bytes());
        self.named
            .lock_recover()
            .insert(app_id.to_owned(), Named::new(app_id, hash, bytes, Ok(file)));
        self.arrived();
    }

    /// The hash of the surface `app_id`'s changes are decided against.
    #[must_use]
    pub fn hash_of(&self, app_id: &str) -> Option<[u8; 32]> {
        self.named
            .lock_recover()
            .get(app_id)
            .map(|named| named.hash)
    }

    /// Every app's surface known here, parsed: a surface that does not parse carries none.
    #[must_use]
    pub fn every(&self) -> Vec<Arc<crate::surface::SurfaceFile>> {
        self.named
            .lock_recover()
            .values()
            .filter_map(|named| named.file.clone())
            .collect()
    }

    /// The surface bytes of `app_id` in the form it is kept by ([`crate::surface::canonical`]),
    /// when known here.
    #[must_use]
    pub fn bytes_of(&self, app_id: &str) -> Option<&'static [u8]> {
        self.named
            .lock_recover()
            .get(app_id)
            .map(|named| named.bytes)
    }

    /// `app_id`'s surface as parsed, once when it was named.
    fn surface_of(&self, app_id: &str) -> Option<Arc<crate::surface::SurfaceFile>> {
        self.named.lock_recover().get(app_id)?.file.clone()
    }
}

/// How many times an app's surface was named in this process: a change
/// decided against a surface that was not here may be decided again once
/// this moves.
#[must_use]
pub fn registered() -> u64 {
    Surfaces::process().registered()
}

/// A surface became readable other than by being named: an app's update
/// installed it. A change decided against a surface that was not here may
/// be decided again ([`registered`] moves).
pub fn arrived() {
    Surfaces::process().arrived();
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

/// Name `json`, the JSON form `tilo_resource::committed_surface!` wrote of
/// the committed surface of an app a host runs in its own process (built
/// into the host, no package of its own), for `app_id`, unless a package
/// already named one.
pub fn register_built_in(app_id: &str, json: &'static str) {
    Surfaces::process().register_built_in(app_id, json);
}

/// Name `bytes`, the surface file the signed package `app_id` ships, as the
/// one it runs with, replacing any its host held before (an update of the
/// app). A file that is not a surface names none.
pub fn register_package(app_id: &str, bytes: &[u8]) {
    Surfaces::process().register_package(app_id, bytes);
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
    Surfaces::process().hash_of(app_id)
}

/// Every app's surface this process carries, parsed: a surface that does not
/// parse carries none.
#[must_use]
pub fn every() -> Vec<Arc<crate::surface::SurfaceFile>> {
    Surfaces::process().every()
}

/// The surface bytes of `app_id` in the form it is kept by
/// ([`crate::surface::canonical`]), when this process carries them.
#[must_use]
pub fn bytes_of(app_id: &str) -> Option<&'static [u8]> {
    Surfaces::process().bytes_of(app_id)
}

/// `app_id`'s surface as parsed, once when it was named: every check of a
/// change reads it, and a surface is tens of kilobytes. `None` when this
/// process holds no surface for the app, or it does not parse (logged when it
/// was named).
fn surface_of(app_id: &str) -> Option<Arc<crate::surface::SurfaceFile>> {
    Surfaces::process().surface_of(app_id)
}

/// The kind of `app_id`'s own record of a workspace, the first its surface declares: the resource
/// whose `open` shows an item of that record. `None` when this process holds no surface for it.
#[must_use]
pub fn primary_kind(app_id: &str) -> Option<String> {
    surface_of(app_id)?
        .own_resource()
        .map(|surface| surface.kind.clone())
}

/// Whether `action` of `app_id` shows something in a window of the app, on
/// whatever resource it is invoked, as its surface declares
/// ([`tilo_actions::surface::Surface::shows`]). False when this process holds
/// no surface for it.
#[must_use]
pub fn shows(app_id: &str, action: &str) -> bool {
    surface_of(app_id).is_some_and(|file| {
        file.resources
            .iter()
            .any(|surface| surface.shows.iter().any(|shown| shown == action))
    })
}

/// The person-record kinds `app_id`'s surface declares (its
/// `ResourceApp::PERSON_RESOURCES`, as `PersonKind::profile`): the records it
/// may keep in, or read from, the person's own workspace. Empty when this
/// process holds no surface for it.
#[must_use]
pub fn person_resource_types(app_id: &str) -> Vec<String> {
    surface_of(app_id)
        .map(|file| {
            file.resources
                .iter()
                .filter(|surface| surface.person)
                .map(|surface| surface.kind.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn keeps_person_resource(app_id: &str, kind: &str) -> bool {
    (app_id == "so.tilo.lang.views" && kind == "tl.so.tilo.lang.views")
        || person_resource_types(app_id)
            .iter()
            .any(|known| known == kind)
}

/// Whether `reader`'s surface declares that it reads a resource kind of
/// `owner`'s (its `reads`): the read-only join to `owner`'s records an app
/// may hold. False when this process holds either surface not.
#[must_use]
pub fn reads_app(reader: &str, owner: &str) -> bool {
    match (surface_of(reader), surface_of(owner)) {
        (Some(reader), Some(owner)) => reader.reads_any_of(&owner),
        _ => false,
    }
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

    tilo_test_env::scenario! {
        /// The surfaces this process holds, each case under an app id no other case names.
        fn a_registered_surface_is_held_under_its_app_and_decides_what_it_names;

        phase only_the_kinds_a_surface_marks_are_the_persons;
        {
            static FILE: &[u8] = b"[[resources]]\nkind = \"test.shared\"\nlabel = \"Shared\"\nversion = 1\n\n[[resources]]\nkind = \"test.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n";
            register_package("test.person-kinds", FILE);
            assert_eq!(person_resource_types("test.person-kinds"), ["test.history"]);
            assert!(keeps_person_resource("test.person-kinds", "test.history"));
            assert!(!keeps_person_resource("test.person-kinds", "test.shared"));
            assert!(!keeps_person_resource("test.unknown-app", "test.history"));
        }

        phase a_launched_package_surface_replaces_the_one_held;
        {
            register_package(
                "test.package-surface",
                b"[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 1\n",
            );
            let package = b"[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 2\n\n[[resources]]\nkind = \"test.pkg-history\"\nlabel = \"History\"\nperson = true\nversion = 1\n".to_vec();
            register_package("test.package-surface", &package);
            let (_, kept) = crate::surface::SurfaceFile::kept(&package).expect("a surface");
            assert_eq!(
                hash_of("test.package-surface"),
                Some(*blake3::hash(kept.as_bytes()).as_bytes())
            );
            assert_eq!(bytes_of("test.package-surface"), Some(kept.as_bytes()));
            assert_eq!(
                person_resource_types("test.package-surface"),
                ["test.pkg-history"]
            );
        }

        phase a_named_surface_registers_only_when_it_hashes_to_its_name;
        {
            let file = b"[[resources]]\nkind = \"test.named\"\nlabel = \"Named\"\nversion = 1\n";
            let hash = blake3::hash(file).to_hex().to_string();

            let other = hex::encode([1; 32]);
            assert!(register_named("test.surface-a", file, &other).is_err());
            assert!(register_named("test.surface-a", file, "not hex").is_err());
            assert_eq!(hash_of("test.surface-a"), None, "nothing named");

            register_named("test.surface-a", file, &hash).unwrap();
            let (_, kept) = crate::surface::SurfaceFile::kept(file).expect("a surface");
            assert_eq!(
                hash_of("test.surface-a"),
                Some(*blake3::hash(kept.as_bytes()).as_bytes())
            );
            assert_eq!(bytes_of("test.surface-a"), Some(kept.as_bytes()));
            assert_eq!(parse_hash(&hex::encode([7; 32])), Some([7; 32]));
        }

        phase a_built_in_surface_never_replaces_a_packages;
        {
            static BUILT_JSON: &str =
                r#"{"resources":[{"kind":"test.built","label":"Built","version":1}]}"#;
            register_built_in("test.built-in", BUILT_JSON);
            assert_eq!(bytes_of("test.built-in"), Some(BUILT_JSON.as_bytes()));
            assert_eq!(
                primary_kind("test.built-in"),
                Some("test.built".to_owned()),
                "the committed form is what decides, without the file here"
            );
            assert!(
                every()
                    .iter()
                    .any(|file| file.resources.first().is_some_and(|r| r.kind == "test.built")),
                "every surface this process carries is here, parsed"
            );
            let package = b"[[resources]]\nkind = \"test.built\"\nlabel = \"Built\"\nversion = 1\n".to_vec();
            register_package("test.built-in", &package);
            register_built_in("test.built-in", BUILT_JSON);
            let (_, kept) = crate::surface::SurfaceFile::kept(&package).expect("a surface");
            assert_eq!(
                bytes_of("test.built-in"),
                Some(kept.as_bytes()),
                "the package's stays"
            );
        }
    }
}
