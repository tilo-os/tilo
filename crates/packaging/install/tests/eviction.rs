//! The size cap the collection after every change applies, on a store past
//! it: the app a change just installed survives the sweep and opens, an app
//! being opened is not evicted under its launch, and the use a launch
//! records when the app closes decides which app goes first.

use std::time::{Duration, UNIX_EPOCH};

use install::gc::{Report, Request, collect, written_since};
use package_store::{Admission, Catalog, InstalledPackage, PackageStore};
use runtime_bundle::gc::BundleRoot;
use tilo_app_package::SigningIdentity;

/// One small app's payload.
const APP_BYTES: usize = 64 * 1024;
/// A payload larger than the cap the tests set on its own.
const LARGE_BYTES: usize = 4 * APP_BYTES;

/// When a launch took its lease, long ago.
fn long_ago() -> std::time::SystemTime {
    UNIX_EPOCH.checked_add(Duration::from_secs(10)).unwrap()
}

/// A store, and the publisher of everything installed into it.
struct Fixture {
    dir: tempfile::TempDir,
    store: PackageStore,
    identity: SigningIdentity,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path().join("store"));
        std::fs::create_dir_all(dir.path().join("runtimes")).unwrap();
        Self {
            dir,
            store,
            identity: SigningIdentity::generate(),
        }
    }

    /// Install `id` at `version` with a payload of `len` bytes of `seed`.
    fn install(&self, id: &str, version: &str, seed: u8, len: usize) -> InstalledPackage {
        let payload = self.dir.path().join(format!("payload-{id}-{version}"));
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), vec![seed; len]).unwrap();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "{id}"
version = "{version}"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(self.identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(&payload, &manifest, &self.identity).unwrap();
        let tapp = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        self.store
            .install(tapp.as_slice(), |_| Ok(Admission::default()), None)
            .unwrap()
            .package
    }

    /// Apps the person installed long ago and last opened at the times
    /// given, least recently used first.
    fn fill(&self, apps: &[(&str, u64)]) {
        for (seed, (id, last_used)) in (1u8..).zip(apps) {
            self.install(id, "1.0.0", seed, APP_BYTES);
            self.store
                .update_row(id, |row| {
                    row.installed_at = 1;
                    row.last_used = *last_used;
                })
                .unwrap();
        }
    }

    /// The collection `tilo-install` and the app host run after a change,
    /// with the store's cap at `cap` bytes.
    fn collect(&self, cap: u64, keep: Vec<String>) -> Report {
        let catalog = Catalog::at(self.store.layout().root(), None);
        let runtimes = BundleRoot::new(self.dir.path().join("runtimes"));
        let request = Request::after_change(cap).keeping(keep);
        collect(&self.store, &catalog, &runtimes, request).unwrap()
    }

    /// What a launch checks before it opens `id`: the app is installed, its
    /// tree can be held for the launch, and the tree is whole by its signed
    /// record.
    fn opens(&self, id: &str) -> bool {
        let Some(row) = self.store.registry().unwrap().find(id).cloned() else {
            return false;
        };
        let held = package_store::lease::hold_payload_in(self.store.layout(), &row.payload_root)
            .unwrap()
            .is_some();
        held && row.payload_root.is_dir()
            && package_store::integrity::verify(&row, &row.payload_root)
                .is_ok_and(|report| report.is_intact())
    }

    fn installed(&self, id: &str) -> bool {
        self.store.registry().unwrap().find(id).is_some()
    }
}

/// A cap that holds `apps` small apps.
fn cap(apps: u64) -> u64 {
    apps.saturating_mul(u64::try_from(APP_BYTES).unwrap())
}

/// The person keeps more than the cap, and the app they download next is
/// larger than the cap on its own. The sweep after its install evicts every
/// other app it may and still cannot get under the cap: the new app must not
/// be the last thing it evicts.
#[test]
fn a_large_app_installed_into_a_full_store_survives_the_sweep_and_opens() {
    const NEW: &str = "so.tilo.eviction-large";
    let fx = Fixture::new();
    fx.fill(&[
        ("so.tilo.eviction-a", 10),
        ("so.tilo.eviction-b", 20),
        ("so.tilo.eviction-c", 30),
        ("so.tilo.eviction-d", 40),
    ]);
    assert!(
        fx.store.disk_usage().unwrap().total_bytes > cap(3),
        "past the cap"
    );
    let installed = fx.install(NEW, "1.0.0", 99, LARGE_BYTES);

    // The app host keeps what its install just wrote.
    let report = fx.collect(cap(3), vec![installed.id]);

    assert_eq!(
        report.evicted,
        [
            "so.tilo.eviction-a",
            "so.tilo.eviction-b",
            "so.tilo.eviction-c",
            "so.tilo.eviction-d"
        ],
        "every other app goes, least recently used first"
    );
    assert!(fx.opens(NEW), "the app just installed opens");
    assert!(
        fx.store.disk_usage().unwrap().total_bytes > cap(3),
        "the store stays over the cap rather than evict the app being opened"
    );
}

/// An update keeps the app's last use, so updating the app the person used
/// least recently makes it the first the sweep would evict. `tilo-install`
/// keeps every app its change wrote.
#[test]
fn an_updated_app_survives_the_sweep_its_update_runs_and_opens() {
    const OLDEST: &str = "so.tilo.eviction-oldest";
    let fx = Fixture::new();
    fx.fill(&[
        (OLDEST, 10),
        ("so.tilo.eviction-b", 20),
        ("so.tilo.eviction-c", 30),
    ]);
    let started = tilo_clock::unix_now();
    let updated = fx.install(OLDEST, "2.0.0", 77, APP_BYTES);
    assert_eq!(updated.last_used, 10, "an update is not a use");

    let written = written_since(&fx.store, started).unwrap();
    assert_eq!(written, [OLDEST], "only the update is new");
    let report = fx.collect(cap(2), written);

    assert_eq!(report.evicted, ["so.tilo.eviction-b"]);
    assert!(fx.opens(OLDEST), "the app just updated opens");
    assert_eq!(
        fx.store.registry().unwrap().find(OLDEST).unwrap().version,
        "2.0.0"
    );
}

/// A launch holds an app's tree from choosing it until its sandbox exits;
/// the sweep leaves an app being opened alone and evicts the next one.
#[test]
fn an_app_being_opened_is_not_evicted() {
    const OPENING: &str = "so.tilo.eviction-opening";
    let fx = Fixture::new();
    fx.fill(&[(OPENING, 10), ("so.tilo.eviction-idle", 20)]);
    let row = fx.store.registry().unwrap().find(OPENING).cloned().unwrap();
    let launch = package_store::lease::hold_payload_in(fx.store.layout(), &row.payload_root)
        .unwrap()
        .expect("a tree of the user's store is held");
    // The launch began long ago, so its lease records no recent use.
    launch.file().set_modified(long_ago()).unwrap();

    let report = fx.collect(cap(1), Vec::new());

    assert_eq!(report.evicted, ["so.tilo.eviction-idle"]);
    assert!(fx.installed(OPENING));
    assert!(row.payload_root.is_dir());
    drop(launch);
}

/// The person opened the first app weeks ago and kept it open until today;
/// the second they opened once, last week. Recorded at open alone, the
/// first looks older and goes; with the use recorded when it closed, the
/// second goes.
#[test]
fn the_use_recorded_when_an_app_closes_decides_what_is_evicted() {
    const KEPT_OPEN: &str = "so.tilo.eviction-kept-open";
    const OPENED_ONCE: &str = "so.tilo.eviction-opened-once";
    let fx = Fixture::new();
    fx.fill(&[(KEPT_OPEN, 10), (OPENED_ONCE, 20)]);
    let row = fx
        .store
        .registry()
        .unwrap()
        .find(KEPT_OPEN)
        .cloned()
        .unwrap();
    let launch = package_store::lease::hold_payload_in(fx.store.layout(), &row.payload_root)
        .unwrap()
        .expect("a tree of the user's store is held");
    launch.file().set_modified(long_ago()).unwrap();
    assert_eq!(
        fx.store.last_used(&row),
        10,
        "while open, its use is the open"
    );

    // The app closes: the launcher records the use as it releases the tree.
    package_store::lease::record_use(&launch).unwrap();
    drop(launch);
    assert!(fx.store.last_used(&row) > 20);

    let report = fx.collect(cap(1), Vec::new());

    assert_eq!(report.evicted, [OPENED_ONCE]);
    assert!(fx.opens(KEPT_OPEN));
}

/// The person opened an app long ago and kept it open; an update replaced
/// the build it runs on. Its row now names the new build, which nothing
/// runs or holds, but the app is open: the sweep after a later change must
/// evict the idle app, not the open one.
#[test]
fn an_app_updated_while_open_is_not_evicted() {
    const OPEN: &str = "so.tilo.eviction-open";
    const IDLE: &str = "so.tilo.eviction-idle";
    let fx = Fixture::new();
    fx.fill(&[(OPEN, 10), (IDLE, 20)]);
    let row = fx.store.registry().unwrap().find(OPEN).cloned().unwrap();
    let launch = package_store::lease::hold_payload_in(fx.store.layout(), &row.payload_root)
        .unwrap()
        .expect("a tree of the user's store is held");
    launch.file().set_modified(long_ago()).unwrap();
    let updated = fx.install(OPEN, "2.0.0", 77, APP_BYTES);
    assert_ne!(updated.payload_root, row.payload_root, "a new build");
    assert!(row.payload_root.is_dir(), "the open build stays");
    assert_eq!(fx.store.last_used(&updated), 10, "an update is not a use");

    let report = fx.collect(cap(1), Vec::new());

    assert_eq!(report.evicted, [IDLE]);
    assert!(fx.installed(OPEN));
    drop(launch);
}
