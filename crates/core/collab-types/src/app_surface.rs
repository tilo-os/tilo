//! Which app surface the changes an app makes are decided against.
//!
//! Every signed op names the content hash of its app's compiled surface
//! (`surface.json`, [`crate::surface::SurfaceFile`]): the build tool compiles
//! it from the `surface.toml` an author writes, and that compiled form is what
//! an author's package ships at its payload root and a built-in app carries.
//! Whoever starts the app names that file ([`SURFACE_FILE_ENV`]) and its hash
//! ([`SURFACE_HASH_ENV`]): the launcher the file in the package it runs,
//! `tilolang run`/`dev` the app's committed file. The app reads it, checks the
//! hash and registers it ([`register_named`]); an app's own binary does not
//! carry the file. A host that runs an app in its own process (the shell's
//! built-in apps, the web build) carries its compiled file's text instead
//! ([`register_built_in`]). Either way the surface is kept as one form
//! ([`crate::surface::canonical`]), whose hash the changes name; the bytes
//! are kept in the app's content store, from where they reach the devices
//! and relays that decide its changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use tilo_actions::surface::Surface;
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
    /// The workspace records the system keeps for apps ([`crate::WORKSPACE_DOCUMENTS`]), by the
    /// app that keeps each: declared from their types as the device starts
    /// ([`Self::declare_system`]), whether or not the app is installed here.
    system: Mutex<BTreeMap<String, Vec<Surface>>>,
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

    /// Name `json`, the compiled surface of an app a host runs in its own process (built into the
    /// host, no package of its own), for `app_id`, unless a package already named one.
    pub fn register_built_in(&self, app_id: &str, json: &'static str) {
        if crate::surface::canonical_json(json).as_deref() != Some(json) {
            tracing::warn!(
                app = app_id,
                "the app's built-in surface is not the canonical compiled form; it declares no resources, reads or footprints"
            );
            return;
        }
        let hash = *blake3::hash(json.as_bytes()).as_bytes();
        if let std::collections::btree_map::Entry::Vacant(entry) =
            self.named.lock_recover().entry(app_id.to_owned())
        {
            let parsed = crate::surface::SurfaceFile::from_json(json);
            entry.insert(Named::new(app_id, hash, json.as_bytes(), parsed));
            self.arrived();
        }
    }

    /// Name `bytes`, the compiled surface (`surface.json`) the signed package `app_id` ships, as
    /// the one it runs with, replacing any held before (an update of the app). Bytes that are not
    /// the canonical compiled form name none.
    ///
    /// A package that declares a record the system keeps ([`Self::declare_system`]) names none:
    /// its type is the system's on every device, and a package's copy of it would judge that
    /// record's writes by another.
    pub fn register_package(&self, app_id: &str, bytes: &[u8]) {
        let file = match crate::surface::SurfaceFile::compiled(bytes) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    app = app_id,
                    %error,
                    "the app's surface is not the compiled form; it declares no resources, reads or footprints"
                );
                return;
            }
        };
        if let Some(kept) = file
            .resources
            .iter()
            .find(|resource| self.keeps_system(&resource.kind))
        {
            tracing::warn!(
                app = app_id,
                kind = kept.kind,
                "the package declares a record the system keeps; its surface is refused"
            );
            return;
        }
        let hash = *blake3::hash(bytes).as_bytes();
        if self
            .named
            .lock_recover()
            .get(app_id)
            .is_some_and(|named| named.hash == hash)
        {
            return;
        }
        // Kept for the process's life, once per distinct surface.
        let bytes = held_bytes(hash, bytes);
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

    /// Declare `resource`, the type of a workspace record the system keeps for `app`
    /// ([`crate::WORKSPACE_DOCUMENTS`]), on this device: every device keeps and judges that
    /// record's writes by it, whether or not the app is installed here.
    ///
    /// # Panics
    /// The table names no such record of `app`: a kind the system keeps is listed there first.
    pub fn declare_system(&self, app: &str, resource: Surface) {
        assert!(
            crate::WORKSPACE_DOCUMENTS
                .iter()
                .any(|kept| kept.app == app && kept.kind == resource.kind),
            "{app}'s {} is no workspace record the system keeps",
            resource.kind
        );
        let mut system = self.system.lock_recover();
        let kept = system.entry(app.to_owned()).or_default();
        kept.retain(|known| known.kind != resource.kind);
        kept.push(resource);
        drop(system);
        self.arrived();
    }

    /// Whether `kind` is a workspace record the system keeps ([`Self::declare_system`]).
    #[must_use]
    pub fn keeps_system(&self, kind: &str) -> bool {
        self.system
            .lock_recover()
            .values()
            .flatten()
            .any(|resource| resource.kind == kind)
    }

    /// Every resource `app_id` keeps whose type this device knows: those its surface declares,
    /// and the records the system keeps for it ([`Self::declare_system`]). What the kinds of an
    /// app, the store's types and the reads of another app's records all read.
    #[must_use]
    pub fn resources_of(&self, app_id: &str) -> Vec<Surface> {
        let mut resources = self
            .surface_of(app_id)
            .map_or_else(Vec::new, |surface| surface.resources.clone());
        resources.extend(
            self.system
                .lock_recover()
                .get(app_id)
                .into_iter()
                .flatten()
                .cloned(),
        );
        resources
    }

    /// Every resource known here ([`Self::resources_of`]), by the app it is of.
    #[must_use]
    pub fn resources(&self) -> Vec<(String, Surface)> {
        let mut apps: std::collections::BTreeSet<String> = self
            .named
            .lock_recover()
            .iter()
            .filter(|(_, named)| named.file.is_some())
            .map(|(app, _)| app.clone())
            .collect();
        apps.extend(self.system.lock_recover().keys().cloned());
        apps.into_iter()
            .flat_map(|app| {
                self.resources_of(&app)
                    .into_iter()
                    .map(move |resource| (app.clone(), resource))
            })
            .collect()
    }

    /// Every resource kind `app_id` keeps whose type this device knows
    /// ([`Self::resources_of`]): the kinds of the records it keeps.
    #[must_use]
    pub fn kinds_of(&self, app_id: &str) -> Vec<String> {
        self.resources_of(app_id)
            .into_iter()
            .map(|resource| resource.kind)
            .collect()
    }

    /// Every resource kind known here ([`Self::resources`]), by the app it is of.
    #[must_use]
    pub fn kinds(&self) -> Vec<(String, String)> {
        self.resources()
            .into_iter()
            .map(|(app, resource)| (app, resource.kind))
            .collect()
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

/// Name `bytes`, the compiled surface whoever started the app named, for
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

/// Name `json`, the compiled surface an app a host runs in its own process
/// (built into the host, no package of its own) carries, for `app_id`,
/// unless a package already named one.
pub fn register_built_in(app_id: &str, json: &'static str) {
    Surfaces::process().register_built_in(app_id, json);
}

/// Name `bytes`, the compiled surface (`surface.json`) the signed package
/// `app_id` ships, as the one it runs with, replacing any its host held
/// before (an update of the app). Bytes that are not the compiled form name
/// none.
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

/// The name an app publishes the verb `id` under, when a surface this process knows declares
/// it: a tilolang app offers its actions by the names its code declares, while a change of one
/// claims the id the declaration keeps, and a caller's signed invocation names the offered one.
#[must_use]
pub fn published_name(id: &str) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    every().into_iter().find_map(|file| {
        file.resources.iter().find_map(|resource| {
            resource
                .verbs
                .iter()
                .find(|verb| verb.id == id)
                .map(|verb| verb.offered().to_owned())
        })
    })
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

/// What the surface of the app that declares `kind` says of it, when this process knows that
/// surface.
#[must_use]
pub fn resource_surface(kind: &str) -> Option<Surface> {
    Surfaces::process()
        .resources()
        .into_iter()
        .map(|(_, resource)| resource)
        .find(|resource| resource.kind == kind)
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
/// may hold. `owner`'s kinds are its surface's resources and the workspace
/// records it keeps ([`crate::WORKSPACE_DOCUMENTS`]). False when this process
/// holds no surface of `reader`.
#[must_use]
pub fn reads_app(reader: &str, owner: &str) -> bool {
    let Some(reader) = surface_of(reader) else {
        return false;
    };
    let kinds = Surfaces::process().kinds_of(owner);
    reader.reads.iter().any(|read| {
        kinds.contains(read)
            || crate::WORKSPACE_DOCUMENTS
                .iter()
                .any(|kept| kept.app == owner && kept.kind == read)
    })
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

    /// The compiled form of the authored surface `toml`, as the build tools write it.
    fn compiled(toml: &str) -> String {
        tilo_surface_compile::compile(toml).expect("a surface")
    }

    tilo_test_env::scenario! {
        /// The surfaces this process holds, each case under an app id no other case names.
        fn a_registered_surface_is_held_under_its_app_and_decides_what_it_names;

        phase only_the_kinds_a_surface_marks_are_the_persons;
        {
            let file = compiled(
                "[[resources]]\nkind = \"test.shared\"\nlabel = \"Shared\"\nversion = 1\n\n[[resources]]\nkind = \"test.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n",
            );
            register_package("test.person-kinds", file.as_bytes());
            assert_eq!(person_resource_types("test.person-kinds"), ["test.history"]);
            assert!(keeps_person_resource("test.person-kinds", "test.history"));
            assert!(!keeps_person_resource("test.person-kinds", "test.shared"));
            assert!(!keeps_person_resource("test.unknown-app", "test.history"));
        }

        phase a_launched_package_surface_replaces_the_one_held;
        {
            let first = compiled(
                "[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 1\n",
            );
            register_package("test.package-surface", first.as_bytes());
            let package = compiled(
                "[[resources]]\nkind = \"test.pkg\"\nlabel = \"Pkg\"\nversion = 2\n\n[[resources]]\nkind = \"test.pkg-history\"\nlabel = \"History\"\nperson = true\nversion = 1\n",
            );
            register_package("test.package-surface", package.as_bytes());
            assert_eq!(
                hash_of("test.package-surface"),
                Some(*blake3::hash(package.as_bytes()).as_bytes())
            );
            assert_eq!(bytes_of("test.package-surface"), Some(package.as_bytes()));
            assert_eq!(
                person_resource_types("test.package-surface"),
                ["test.pkg-history"]
            );
        }

        phase a_named_surface_registers_only_when_it_hashes_to_its_name;
        {
            let file = compiled(
                "[[resources]]\nkind = \"test.named\"\nlabel = \"Named\"\nversion = 1\n",
            );
            let hash = blake3::hash(file.as_bytes()).to_hex().to_string();

            let other = hex::encode([1; 32]);
            assert!(register_named("test.surface-a", file.as_bytes(), &other).is_err());
            assert!(register_named("test.surface-a", file.as_bytes(), "not hex").is_err());
            assert_eq!(hash_of("test.surface-a"), None, "nothing named");

            register_named("test.surface-a", file.as_bytes(), &hash).unwrap();
            assert_eq!(
                hash_of("test.surface-a"),
                Some(*blake3::hash(file.as_bytes()).as_bytes())
            );
            assert_eq!(bytes_of("test.surface-a"), Some(file.as_bytes()));
            assert_eq!(parse_hash(&hex::encode([7; 32])), Some([7; 32]));
        }

        phase a_surface_in_the_authored_form_names_none;
        {
            let toml = b"[[resources]]\nkind = \"test.toml\"\nlabel = \"Toml\"\nversion = 1\n";
            let hash = blake3::hash(toml).to_hex().to_string();
            register_named("test.surface-toml", toml, &hash)
                .expect("it hashes to its name");
            assert_eq!(
                hash_of("test.surface-toml"),
                None,
                "the authored form is not a surface a change is decided against"
            );
            register_package("test.surface-toml", toml);
            assert_eq!(bytes_of("test.surface-toml"), None);
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
                "the compiled form is what decides, without the file here"
            );
            assert!(
                every()
                    .iter()
                    .any(|file| file.resources.first().is_some_and(|r| r.kind == "test.built")),
                "every surface this process carries is here, parsed"
            );
            let package = compiled(
                "[[resources]]\nkind = \"test.built\"\nlabel = \"Built\"\nversion = 1\n",
            );
            register_package("test.built-in", package.as_bytes());
            register_built_in("test.built-in", BUILT_JSON);
            assert_eq!(
                bytes_of("test.built-in"),
                Some(package.as_bytes()),
                "the package's stays"
            );
        }
    }
}
