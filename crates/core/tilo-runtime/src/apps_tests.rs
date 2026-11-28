use super::*;
use crate::device::workspaces::block_on_owned;
use ed25519_dalek::{Signer as _, SigningKey};
use package_store::{Catalog, InstalledPackage, Layout, PackageStore};
use tilo_action_policy::Grantee;
use tilo_app_package::{BlobHash, Collection, SignedRoot, SigningIdentity};
use tilo_permissions::caps::Access;
use tilo_permissions::caps::CapOverride;
use tilo_permissions::caps::Scope;
use tilo_test_env::SpawnUnderEnvLock as _;

const WORKSPACE: &str = "0b7c7a52-6f7e-4a55-9a51-1c2f4b3f7e10";
const VIEWER: &str = "so.tilo.viewer";

fn manifest(id: &str, entrypoint: &str, extra: &str) -> String {
    format!(
        r#"[package]
id = "{id}"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "probe"
type = "{entrypoint}"
{extra}"#
    )
}

/// A user store at `<base>/store`, as an install leaves it: each package's
/// payload tree beside its signed record, and one registry row each.
struct Store {
    root: PathBuf,
    registry: String,
}

impl Store {
    fn new(base: &Path) -> Self {
        let root = base.join("store");
        std::fs::create_dir_all(root.join("pkgs")).unwrap();
        Self {
            root,
            registry: String::new(),
        }
    }

    /// Install `files` as package `id` with `manifest`, signed by a fixed
    /// publisher key.
    fn install(&mut self, id: &str, manifest: &str, files: &[(&str, &[u8])]) -> InstalledPackage {
        self.install_signed(id, manifest, files, &SigningKey::from_bytes(&[7_u8; 32]))
    }

    /// Install a development build of `of` made by `identity`: the id its marking derives, the
    /// marking, and `identity`'s signature.
    fn install_dev(&mut self, of: &str, identity: &SigningKey) -> InstalledPackage {
        let key = identity.verifying_key().to_bytes();
        let id = tilo_app_package::dev_app_id(of, &key);
        let text = manifest(
            &id,
            "gui",
            &format!(
                "[package.dev]\ndevice = \"ed25519:{}\"\nof = {of:?}",
                hex::encode(key)
            ),
        );
        self.install_signed(&id, &text, &[("probe", b"binary")], identity)
    }

    /// [`Self::install`] signed by `identity`.
    fn install_signed(
        &mut self,
        id: &str,
        manifest: &str,
        files: &[(&str, &[u8])],
        identity: &SigningKey,
    ) -> InstalledPackage {
        let manifest_bytes = manifest.as_bytes().to_vec();
        let collection = Collection::from_entries(
            files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), BlobHash::of(bytes))),
        );
        let collection_bytes = collection.encode();
        let key = identity;
        let mut signed = SignedRoot::unsigned(
            BlobHash::of(&manifest_bytes),
            BlobHash::of(&collection_bytes),
            key.verifying_key().to_bytes(),
        );
        signed.signature = Some([0; 64]);
        signed.signature = Some(key.sign(&signed.signing_bytes()).to_bytes());
        let signed_bytes = signed.encode().to_vec();
        let id_hex = BlobHash::of(&signed_bytes).to_hex();
        let payload = self.root.join("pkgs").join(&id_hex);
        for (path, bytes) in files {
            let file = payload.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, bytes).unwrap();
        }
        std::fs::create_dir_all(&payload).unwrap();
        let meta = self.root.join("pkgs").join(format!("{id_hex}.pkg"));
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("signed-root"), &signed_bytes).unwrap();
        std::fs::write(meta.join("manifest.toml"), &manifest_bytes).unwrap();
        std::fs::write(meta.join("collection.qcl"), &collection_bytes).unwrap();
        let package = InstalledPackage {
            id: id.into(),
            version: "0.1.0".into(),
            package_id_hex: id_hex,
            payload_root: payload,
            manifest_path: meta.join("manifest.toml"),
            publisher_key_hex: hex_key(key),
            pinned: false,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: Vec::new(),
            runtime: String::new(),
            source: String::new(),
            held: false,
            payload_bytes: 0,
            system: false,
        };
        self.registry.push_str(&registry_row(&package));
        std::fs::write(Layout::new(&self.root).registry(), &self.registry).unwrap();
        package
    }

    fn catalog(&self) -> Catalog {
        Catalog::at(&self.root, None)
    }
}

fn registry_row(package: &InstalledPackage) -> String {
    format!(
        "[[entry]]\nid = {:?}\nversion = {:?}\npackage_id_hex = {:?}\npayload_root = {:?}\n\
         manifest_path = {:?}\npublisher_key_hex = {:?}\n\n",
        package.id,
        package.version,
        package.package_id_hex,
        package.payload_root.display().to_string(),
        package.manifest_path.display().to_string(),
        package.publisher_key_hex,
    )
}

fn hex_key(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn canonical_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    // Seatbelt and the identity store see resolved paths (macOS `/var`).
    let root = std::fs::canonicalize(temp.path()).unwrap();
    (temp, root)
}

/// The apps installed in `store`, launched into [`WORKSPACE`] with no grants.
fn apps(store: &Store, root: &Path, candidates: Vec<PathBuf>) -> Apps {
    let apps = installed(store, root, candidates);
    apps.keep_launch_grants(Arc::new(Granted::new(Vec::new())));
    apps
}

/// [`Apps::start`] waited for on a test's own thread — no runtime's, so the wait drives the app
/// runtime's own work — what a launch answers. The fixture is borrowed, which the start's future
/// carries, so this is no `block_on_owned` of its own.
fn launched(apps: &Apps, app: &str, workspace: &str) -> Result<Launch, String> {
    tilo_rt::app().handle().block_on(apps.start(app, workspace))
}

/// [`HostConnections::start`] waited for on a test's own thread: what a device's turn of a held
/// connection answers.
fn connection_started(
    apps: &Apps,
    workspace: &str,
    app: &str,
) -> Result<(), tilo_workspace_service::work::StartRefusal> {
    use tilo_workspace_service::work::HostConnections;
    tilo_rt::app()
        .handle()
        .block_on(HostConnections::start(apps, workspace, app))
}

/// The apps installed in `store`, before the workspace service answers what they are granted.
fn installed(store: &Store, root: &Path, candidates: Vec<PathBuf>) -> Apps {
    Apps::new(
        Some(store.catalog()),
        root,
        crate::daemon::Sockets::in_dir(root),
        root.join("support"),
        candidates,
    )
}

/// The same manifest, declaring `mode` for `hosts` instead of no network.
fn reachable_manifest(id: &str, mode: &str, hosts: &[&str]) -> String {
    let hosts = hosts
        .iter()
        .map(|host| format!("{host:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    manifest(id, "gui", "").replace(
        "[sandbox.net]\nmode = \"off\"",
        &format!("[sandbox.net]\nmode = \"{mode}\"\nhosts = [{hosts}]"),
    )
}

/// What the workspace service answers about launches into a workspace: the
/// person's grants, in place of the service that holds the governance, and whether the
/// workspace lets an app keep its own records there.
struct Granted {
    caps: Vec<CapOverride>,
    /// Whether the workspace runs the app: [`Self::keeps_records`] says so of the one app these
    /// tests start.
    records: bool,
}

impl Granted {
    /// The grants of a workspace that runs the app whose records it keeps.
    #[must_use]
    fn new(caps: Vec<CapOverride>) -> Self {
        Self {
            caps,
            records: true,
        }
    }

    /// The same grants in a workspace that runs no version of the app, or one whose members took
    /// its grant back: nothing the app keeps by itself is its to keep there.
    #[must_use]
    fn keeps_no_records(self) -> Self {
        Self {
            records: false,
            ..self
        }
    }
}

impl LaunchGrantsSource for Granted {
    fn launch_grants(&self, _workspace: &str) -> tilo_permissions::caps::LaunchGrants {
        tilo_permissions::caps::LaunchGrants {
            sandbox: self.caps.clone(),
        }
    }

    fn serves(&self, workspace: &str) -> bool {
        workspace == WORKSPACE
    }

    fn app_keeps_its_records(&self, workspace: &str, _: &str) -> bool {
        workspace == WORKSPACE && self.records
    }

    fn package_trust(&self, _: &str) -> crate::device::package_runs::PackageTrust {
        crate::device::package_runs::PackageTrust::default()
    }
}

tilo_test_env::scenario! {
    fn a_launch_reaches_what_the_manifest_declares_and_the_workspace_granted;

    phase a_granted_app_reaches_its_declared_hosts_and_un_granted_ones_stay_shut;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            VIEWER,
            &reachable_manifest(VIEWER, "allowlist", &["mail.example.com", "calendar.example.com"]),
            &[("probe", b"binary")],
        );
        let apps = Arc::new(installed(&store, &root, Vec::new()));
        let granted = CapOverride::grant(
            Grantee::person("person-7"),
            Scope::NetHost("mail.example.com".into()),
            Access::Write,
        )
        .through(VIEWER);
        apps.keep_launch_grants(Arc::new(Granted::new(vec![granted])));
        let app = apps.launchable(VIEWER).unwrap();
        let manifest = launcher::manifest::read_manifest(&app.manifest_path).unwrap();
        let (plan, egress) = apps.launch_capabilities(&app, WORKSPACE, &manifest);
        assert_eq!(plan.net_hosts, vec!["mail.example.com".to_owned()]);
        let policy = egress
            .policy()
            .expect("a granted app reaches the hosts it declares");
        assert!(policy.permits("mail.example.com"));
        // Declared but not granted: nobody reaches it.
        assert!(!policy.permits("calendar.example.com"));
    }

    phase an_app_nobody_granted_reaches_no_network;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            VIEWER,
            &reachable_manifest(VIEWER, "allowlist", &["mail.example.com"]),
            &[("probe", b"binary")],
        );
        let apps = apps(&store, &root, Vec::new());
        let app = apps.launchable(VIEWER).unwrap();
        let manifest = launcher::manifest::read_manifest(&app.manifest_path).unwrap();
        let (_, egress) = apps.launch_capabilities(&app, WORKSPACE, &manifest);
        assert!(
            matches!(egress, launcher::egress_proxy::EgressDecision::Blocked),
            "a launch without a grant reaches nothing, got {egress:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn the_catalogue_offers_what_is_installed_and_a_launch_stays_in_the_served_workspace;

    phase the_catalogue_offers_installed_windows_and_their_declared_access;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            VIEWER,
            &manifest(VIEWER, "gui", "\n[actions]\nquery = true\ninvoke = true\n"),
            &[("probe", b"binary")],
        );
        store.install(
            "so.tilo.tool",
            &manifest("so.tilo.tool", "cli", ""),
            &[("probe", b"binary")],
        );
        let apps = Arc::new(apps(&store, &root, Vec::new()));

        let offered = apps.applications();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].package_id, VIEWER);
        assert_eq!(offered[0].display_name, "Viewer");

        let resolver = apps.access_resolver();
        assert_eq!(
            resolver(VIEWER),
            AppActionAccess {
                query: true,
                invoke: true
            }
        );
        // An app that declares nothing, and one that is not installed, may
        // neither read the catalogue nor invoke anything.
        let nothing = AppActionAccess {
            query: false,
            invoke: false,
        };
        assert_eq!(resolver("so.tilo.tool"), nothing);
        assert_eq!(resolver("so.tilo.unknown"), nothing);
        assert_eq!(apps.display_name("so.tilo.unknown-app"), "Unknown App");

        let err = launched(&apps, "so.tilo.tool", WORKSPACE)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("no window"), "{err}");
        let err = launched(&apps, "so.tilo.unknown", WORKSPACE)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("not installed"), "{err}");

        let empty = Apps::new(
            None,
            &root,
            crate::daemon::Sockets::in_dir(&root),
            PathBuf::new(),
            Vec::new(),
        );
        assert!(empty.applications().is_empty());
    }

    phase a_launch_into_a_workspace_this_runtime_does_not_serve_is_refused;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            VIEWER,
            &manifest(VIEWER, "gui", ""),
            &[("probe", b"binary")],
        );
        let apps = apps(&store, &root, Vec::new());
        let err = launched(&apps, VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21")
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("not served by this runtime"), "{err}");
    }
}

tilo_test_env::scenario! {
    #[cfg(target_os = "linux")]
    fn a_linux_launch_goes_through_the_launcher_with_the_signed_package_as_installed;

    phase a_linux_launch_without_tilo_launcher_is_refused;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            VIEWER,
            &manifest(VIEWER, "gui", ""),
            &[("probe", b"binary")],
        );
        let apps = apps(&store, &root, vec![root.join("missing/tilo-launcher")]);
        let err = launched(&apps, VIEWER, WORKSPACE).map(|_| ()).unwrap_err();
        assert!(err.contains("tilo-launcher was not found"), "{err}");
        assert!(apps.launched.0.lock().unwrap().is_empty());
    }

    phase an_original_launch_refuses_a_changed_or_missing_signed_manifest;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let source = manifest(
            VIEWER,
            "gui",
            "\n[resources]\nkinds = [\"viewer.document\"]\n",
        );
        let installed = store.install(VIEWER, &source, &[("probe", b"binary")]);
        let apps = apps(&store, &root, Vec::new());
        let app = apps.launchable(VIEWER).unwrap();
        apps.original_launch(&app).unwrap();
        std::fs::write(
            &installed.manifest_path,
            source.replace("viewer.document", "viewer.all"),
        )
        .unwrap();
        let error = apps
            .original_launch(&app)
            .err()
            .expect("changed declarations must be refused");
        assert!(
            error.contains("recorded manifest does not match the hash the signature covers"),
            "{error}"
        );
        std::fs::write(&installed.manifest_path, &source).unwrap();
        std::fs::remove_file(
            installed
                .manifest_path
                .parent()
                .unwrap()
                .join("signed-root"),
        )
        .unwrap();
        let error = apps
            .original_launch(&app)
            .err()
            .expect("unsigned launch must be refused");
        assert!(error.contains("original signed package"), "{error}");
        assert!(apps.launched.0.lock().unwrap().is_empty());
    }

    phase original_launch_hash_is_checked_by_the_actual_launcher_before_spawn;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let installed = store.install(
            VIEWER,
            &manifest(VIEWER, "gui", ""),
            &[("probe", b"binary")],
        );
        let apps = apps(&store, &root, Vec::new());
        let original = apps
            .original_launch(&apps.launchable(VIEWER).unwrap())
            .unwrap();
        assert_eq!(original.package_hash, installed.package_id_hex);
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment
            .remove("TILO_APPS_SYSTEM_DIR")
            .set("TILO_APPS_DATA_DIR", &store.root);
        let options = launcher::run::LaunchOptions {
            expected_package_hash: Some(original.package_hash),
            app_id: Some(VIEWER.into()),
            ..Default::default()
        };
        let verify = |options| {
            launcher::run::launch_manifest(&installed.manifest_path, &installed.payload_root, options)
        };
        assert_eq!(verify(&options).unwrap().package_id, VIEWER);
        let mut wrong = options.clone();
        wrong.expected_package_hash = Some("0".repeat(64));
        assert!(
            verify(&wrong).is_err(),
            "a replacement root cannot use original declarations"
        );
        std::fs::write(installed.payload_root.join("probe"), b"tampered").unwrap();
        assert!(
            verify(&options).is_err(),
            "the production launcher rejects a changed executable before spawn"
        );
        assert!(apps.launched.0.lock().unwrap().is_empty());
    }

    phase a_launched_package_declares_the_persons_records_its_signed_surface_names;
    // A launched package keeps the person's records it declares in its
    // signed surface as a built-in app does: the runtime takes the surface
    // from the signed package as it launches it and hands it to its identity
    // service and broker. A surface changed after install declares nothing.
    {
        use tilo_identity::broker::PersonResourceTypes as _;

        const KEEPER: &str = "so.tilo.person-keeper";
        const TAMPERED: &str = "so.tilo.person-tampered";
        let surface = |kind: &str| {
            format!(
                r#"{{"resources":[{{"kind":"{kind}.page","label":"Page","version":1}},{{"kind":"{kind}.history","label":"History","person":true,"version":1}}]}}"#
            )
        };
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let keeper = surface("person-keeper");
        store.install(
            KEEPER,
            &manifest(KEEPER, "gui", ""),
            &[("probe", b"binary"), ("surface.json", keeper.as_bytes())],
        );
        let tampered = store.install(
            TAMPERED,
            &manifest(TAMPERED, "gui", ""),
            &[
                ("probe", b"binary"),
                ("surface.json", surface("person-tampered").as_bytes()),
            ],
        );
        std::fs::write(
            tampered.payload_root.join("surface.json"),
            surface("person-forged"),
        )
        .unwrap();
        let declared = |app: &str| tilo_app_host::SurfacePersonDocuments.person_resource_types(app);
        assert!(declared(KEEPER).is_empty(), "nothing before it is launched");

        // This builder has no launcher: the launch stops there, after the
        // runtime took the package's surface.
        let apps = apps(&store, &root, vec![root.join("missing/tilo-launcher")]);
        let _ = launched(&apps, KEEPER, WORKSPACE);
        let _ = launched(&apps, TAMPERED, WORKSPACE);
        assert_eq!(declared(KEEPER), ["person-keeper.history"]);
        assert!(
            declared(TAMPERED).is_empty(),
            "a surface that is not the signed one declares nothing"
        );
    }
}

// A call that shows nothing starts a Rust app's own process with no window, through the same
// launch a window goes through, and the process that runs takes the next call: the daemon hands
// the broker a launch it attests as one with no window, so it is never handed an `open`.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_rust_app_runs_with_no_window_and_takes_its_calls_there;

    phase a_call_that_shows_nothing_starts_the_apps_process_and_the_next_call_reuses_it;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        // The toolkit's loop is what runs the app's code, even drawing nothing: it is handed
        // the display, as a window's launch is.
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let said = root.join("said");
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!(
                "printf '%s\\n' \"$@\" > '{said}.args'\nenv > '{said}.env'\nhold\n",
                said = said.display()
            ),
        )
        .unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);

        let mut start = apps
            .windowless(VIEWER, WORKSPACE, None, None)
            .unwrap()
            .expect("a Rust app installed here runs its own process with no window");
        start
            .try_recv()
            .expect("the start is answered once the process runs")
            .expect("the process started");
        assert_eq!(
            apps.launched.0.lock().unwrap().len(),
            1,
            "one process runs it"
        );
        let (pid, no_window) = apps
            .launched
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, record)| (*pid, record.no_window))
            .next()
            .unwrap();
        assert!(no_window, "the launch shows no window");

        let handed = wait_for(&format!("{}.args", said.display()));
        assert!(
            handed.contains("--app-id") && handed.contains(VIEWER),
            "the launcher starts the app's package: {handed}"
        );
        assert!(
            handed.contains("--wayland-socket"),
            "the loop that runs its code is handed the display: {handed}"
        );
        let env = wait_for(&format!("{}.env", said.display()));
        let idle = format!("TILO_NO_WINDOW_IDLE={}", crate::IDLE.as_secs());
        assert!(
            env.lines().any(|line| line == "TILO_NO_WINDOW=1"),
            "the process takes its actions and draws nothing: {env}"
        );
        assert!(
            env.lines().any(|line| line == idle.as_str()),
            "it ends after the idle time a tilolang process does: {env}"
        );

        // The broker asks who the process behind a connection is, and is told it shows no
        // window: nothing hands it an `open`.
        let launch = apps
            .authenticated_launch(pid, VIEWER, WORKSPACE, None)
            .unwrap()
            .expect("the process is the launch this device made");
        assert!(launch.windowless, "it shows no window");
        assert!(
            apps.attest_process(pid, VIEWER, WORKSPACE),
            "the app's connection is its own launch's"
        );

        // The next call finds the process that runs: nothing starts.
        let mut again = apps
            .windowless(VIEWER, WORKSPACE, None, None)
            .unwrap()
            .expect("the running process takes the next call");
        again
            .try_recv()
            .expect("a call that finds it running is answered at once")
            .expect("nothing failed");
        assert_eq!(
            apps.launched.0.lock().unwrap().len(),
            1,
            "the running process is reused, not started again"
        );
    }

    // A process serves the one record its launch named: a call on another record is not answered
    // by it, and starts a process of its own.
    phase a_call_on_another_record_starts_a_second_process;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let launcher = tilo_test_env::stand_in(root.join("tilo-launcher"), "hold\n").unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);

        let first = "uuid:00000000-0000-0000-0000-000000000001";
        let second = "uuid:00000000-0000-0000-0000-000000000002";
        let mut start = apps
            .windowless(VIEWER, WORKSPACE, Some(first), None)
            .unwrap()
            .expect("a call on a record starts the app's process with no window");
        start
            .try_recv()
            .expect("the start is answered once the process runs")
            .expect("the process started");
        assert_eq!(
            apps.no_window_processes(VIEWER, WORKSPACE, Some(first)),
            1,
            "one process serves the record the call named"
        );

        let mut other = apps
            .windowless(VIEWER, WORKSPACE, Some(second), None)
            .unwrap()
            .expect("a call on another record starts a process of its own");
        other
            .try_recv()
            .expect("the second start is answered once its process runs")
            .expect("the process started");
        assert_eq!(
            apps.launched.0.lock().unwrap().len(),
            2,
            "a process serves one record, so a call on another starts a second"
        );
    }

    phase an_app_that_is_not_installed_starts_nothing_without_a_window;
    {
        let (_temp, root) = canonical_temp();
        let store = Store::new(&root);
        let apps = apps(&store, &root, Vec::new());
        assert!(
            apps.windowless("so.tilo.unknown", WORKSPACE, None, None).unwrap().is_none(),
            "an app that is not installed runs nowhere"
        );
        assert!(apps.launched.0.lock().unwrap().is_empty(), "nothing started");
    }

    phase a_call_into_a_workspace_this_runtime_does_not_serve_is_refused;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let apps = apps(&store, &root, Vec::new());
        let refused = apps
            .windowless(VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21", None, None)
            .map(|_| ())
            .unwrap_err();
        assert!(refused.contains("not served by this runtime"), "{refused}");
        assert!(apps.launched.0.lock().unwrap().is_empty(), "nothing started");
    }
}

// With no Tilo session, the daemon opens a tilolang app's window itself: the spare window process a
// session starts, connected to the person's Wayland display and bound over its link to the app and
// the instance its tilolang process runs, as a session binds one.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_linux_daemon_opens_a_tilolang_apps_window_with_no_session;

    phase the_spare_window_starts_on_the_display_bound_to_the_instance;
    {
        use runtime_portal::process_link::{Packet as _, ToProcess};
        use std::os::unix::fs::FileExt as _;
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let surface = b"{\"surface\":1}";
        store.install(
            VIEWER,
            &manifest(VIEWER, "gui", "runtime = \"tilolang/1.0.0\"\n").replace(
                "kind = \"app\"\n",
                "kind = \"app\"\nruntime = \"tilo-runtime/2026.1\"\n",
            ),
            &[
                ("probe", b"binary"),
                ("app.wasm", b"module"),
                ("surface.json", surface),
            ],
        );
        // The person's display: a compositor stand-in that takes the window's connection.
        let display = root.join("wayland-0");
        let compositor = std::os::unix::net::UnixListener::bind(&display).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let said = root.join("said");
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!("printf '%s\\n' \"$@\" > '{said}.args'\nhold\n", said = said.display()),
        )
        .unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
        let app = apps.launchable(VIEWER).unwrap();
        let original = apps.original_launch(&app).unwrap();
        let manifest = launcher::manifest::read_manifest(&app.manifest_path).unwrap();
        let (window_end, _host_end) = runtime_portal::process_link::pair().unwrap();
        let mut launch = linux::window(
            &apps,
            (&app, WORKSPACE),
            (&manifest, &original),
            instances::HostedWindow {
                instance: 7,
                patch: window_end,
            },
        )
        .unwrap();

        let handed = wait_for(&format!("{}.args", said.display()));
        let handed: Vec<&str> = handed.lines().collect();
        assert_eq!(
            handed,
            [
                launcher::run::tilolang_window::SUBCOMMAND,
                "--runtime",
                "tilolang/1.0.0",
                "--app-runtime",
                "tilo-runtime/2026.1",
                "--workspace-id",
                WORKSPACE,
            ],
            "the launcher starts the spare window, in its own sandbox"
        );
        compositor
            .accept()
            .expect("the window's display connection reached the person's compositor");

        // What the launcher hands the window with its release: its display, its patch connection
        // and the app it is bound to.
        let link = launch.window_link.take().expect("the window's link");
        assert!(matches!(
            ToProcess::receive(&link),
            Ok(Some(ToProcess::Display { .. }))
        ));
        assert!(matches!(
            ToProcess::receive(&link),
            Ok(Some(ToProcess::Patch { instance: 7, .. }))
        ));
        let Ok(Some(ToProcess::Bind {
            binding,
            module,
            shell_ui,
            surface: Some(sent_surface),
        })) = ToProcess::receive(&link)
        else {
            panic!("the window is bound to the app");
        };
        assert_eq!(
            (binding.app.as_str(), binding.workspace.as_str(), binding.window.instance),
            (VIEWER, WORKSPACE, 7)
        );
        assert_eq!(
            binding.surface_hash.as_deref(),
            Some(blake3::hash(surface).to_hex().as_str())
        );
        assert!(shell_ui.is_none(), "no shell runs beside it");
        let read = |fd: runtime_portal::process_link::Descriptor, len: usize| {
            let mut bytes = vec![0; len];
            std::fs::File::from(fd).read_exact_at(&mut bytes, 0).unwrap();
            bytes
        };
        assert_eq!(read(module, 6), b"module", "its signed module");
        assert_eq!(read(sent_surface, surface.len()), surface, "its signed surface");
        launch.kill();
    }
}

// A development build is a package of its own, marked with the device that built it and the app
// it builds: it launches through the same path a window does, is attested under its own id, and
// starts with none of the installed app's grants.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_development_build_runs_as_itself_with_no_grants;

    phase a_dev_build_launches_attested_under_its_own_id;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let device = SigningKey::from_bytes(&[7_u8; 32]);
        let of = "org.example.notes";
        store.install(of, &manifest(of, "gui", ""), &[("probe", b"binary")]);
        let dev = store.install_dev(of, &device);
        // The toolkit's loop is what runs the app's code, even drawing nothing: it is handed the
        // display, as a window's launch is.
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let launcher = tilo_test_env::stand_in(root.join("tilo-launcher"), "sleep 600\n").unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
        let app = apps
            .launchable(&dev.id)
            .expect("the device launches the build it made");
        assert_eq!(app.app_id.as_str(), dev.id, "it runs under its own id");
        assert_ne!(app.app_id.as_str(), of);
        let launch = launched(&apps, &dev.id, WORKSPACE).unwrap();
        assert_eq!(
            apps.attested(launch.pid),
            Some((dev.id, WORKSPACE.to_owned())),
            "the launch is attested as the dev build, not as the app it builds"
        );
        assert!(
            apps.installed_here(of).is_ok(),
            "the installed app is still the installed app"
        );
    }

    // A dev build's grants start empty; the person's grant to the installed app never reaches it,
    // and a grant it is given reaches it through the same ask, keyed by its own id.
    phase a_dev_build_gets_the_grants_asked_for_under_its_own_id_and_no_others;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let device = SigningKey::from_bytes(&[7_u8; 32]);
        let of = "org.example.notes";
        store.install(of, &reachable_manifest(of, "allowlist", &["mail.example.com"]), &[("probe", b"binary")]);
        let dev = store.install_dev(of, &device);
        let apps = installed(&store, &root, Vec::new());
        let installed_app = apps.launchable(of).unwrap();
        let dev_app = apps.launchable(&dev.id).unwrap();
        // What the workspace granted the installed app.
        let to_the_app = CapOverride::grant(
            Grantee::person("person-7"),
            Scope::NetHost("mail.example.com".into()),
            Access::Write,
        )
        .through(of);
        apps.keep_launch_grants(Arc::new(Granted::new(vec![to_the_app])));
        assert!(
            !apps.app_grants(&installed_app, WORKSPACE).sandbox.is_empty(),
            "the installed app keeps its grant"
        );
        assert!(
            apps.app_grants(&dev_app, WORKSPACE).sandbox.is_empty(),
            "a dev build starts with none of the installed app's grants"
        );
        // The person answers the dev build's own ask.
        let apps = installed(&store, &root, Vec::new());
        let to_the_dev = CapOverride::grant(
            Grantee::person("person-7"),
            Scope::NetHost("mail.example.com".into()),
            Access::Write,
        )
        .through(dev.id.as_str());
        apps.keep_launch_grants(Arc::new(Granted::new(vec![to_the_dev])));
        assert_eq!(
            apps.app_grants(&dev_app, WORKSPACE).sandbox.len(),
            1,
            "the grant the person gave the dev build reaches it"
        );
    }
}

// The tool's own staging of a build, installed through the cache's action the device runs: the
// manifest and payload go in unsigned, the device packs and signs them under the id its marking
// derives, and the app starts from there as itself, with none of the grants of the app it builds.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_staged_development_build_is_installed_and_starts_under_the_device_id;

    phase a_staged_build_installs_under_the_device_id_and_starts_with_no_grants;
    {
        let (_temp, root) = canonical_temp();
        let store = Store::new(&root);
        let device = SigningIdentity::from_signing_key(SigningKey::from_bytes(&[7_u8; 32]));
        let of = "org.example.dev";
        // The app as its sources write it: a value a person changes, shown in its window.
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("app.ids"), "").unwrap();
        std::fs::write(
            source.join("app.tl"),
            r#"app org.example.dev "Dev build" (category: .utility)

/// What the app shows.
type Value {
    /// The value.
    value: Int merge counter
}

root Value

default view value for Value

/// Show the value.
view value(v: Value) {
    std.page(v) {
        ui.Text("{v.value}", role: .title, numeric: true)
    }
}
"#,
        )
        .unwrap();
        let dev_id = tilo_app_package::dev_app_id(of, &device.public_key_bytes());
        let options = tilolang_cli::package::BuildOptions {
            dir: source,
            search: Vec::new(),
            out: root.join("stage"),
            tilo_runtime: tilolang_cli::package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_runtime: tilolang_cli::package::default_tilolang_runtime(),
            version: "0.1.0".to_owned(),
            locked: false,
        };
        let staged =
            tilolang_cli::package::stage_dev(&options, &[(of.to_owned(), dev_id.clone())])
            .unwrap()
            .pop()
            .expect("the app is staged");
        assert_eq!(staged.app, of, "the manifest names the app it builds");
        assert_eq!(
            staged.meta.app, dev_id,
            "the module is the device's own development build, which it runs as"
        );
        assert_eq!(
            staged.dev, dev_id,
            "the staging names the id it was compiled for"
        );
        // The runtimes the app names, as a session bakes them: the cache refuses an app whose
        // runtime this computer does not have.
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let runtimes = root.join("runtimes");
        for package in [
            tilolang_cli::package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_cli::package::default_tilolang_runtime(),
        ] {
            let version = package.split_once('/').unwrap().1;
            std::fs::create_dir_all(runtimes.join(version)).unwrap();
            std::fs::write(
                runtimes
                    .join(version)
                    .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
                "",
            )
            .unwrap();
        }
        environment.set(runtime_bundle::RUNTIME_DIR_ENV, &runtimes);
        let unsigned =
            tilo_app_package::Manifest::from_toml_str(
                &std::fs::read_to_string(&staged.manifest).unwrap(),
            )
            .unwrap();
        assert!(
            unsigned.publisher.signature.is_empty() && unsigned.package.dev.is_none(),
            "the tool stages it unsigned: the device signs and marks it"
        );
        let report = package_actions::apply(
            &package_actions::Target {
                store: &PackageStore::at(&store.root),
                catalog: &store.catalog(),
                max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
                device: Some(&device),
            },
            package_actions::Change::InstallDev {
                manifest: package_actions::Uploaded {
                    source: staged.manifest.display().to_string(),
                },
                payload: staged
                    .payload
                    .iter()
                    .map(|(path, file)| package_actions::HostFile {
                        path: path.clone(),
                        file: package_actions::Uploaded {
                            source: file.display().to_string(),
                        },
                    })
                    .collect(),
            },
            &package_actions::Entry::Here,
        )
        .unwrap_or_else(|error| panic!("the device installs the staged build: {error}"));
        let id = tilo_app_package::dev_app_id(of, &device.public_key_bytes());
        assert_eq!(
            report.installed.as_deref(),
            Some(id.as_str()),
            "the device answers the id it installed: {:?}",
            report.lines
        );
        let dev_package = store
            .catalog()
            .find(&id)
            .unwrap()
            .expect("installed under the device's dev id");
        let manifest = tilo_app_package::Manifest::from_toml_str(
            &std::fs::read_to_string(&dev_package.manifest_path).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.package.dev.as_ref().map(|dev| dev.of.as_str()),
            Some(of),
            "the device's marking travels with the installed build"
        );
        assert!(
            manifest
                .resources
                .kinds
                .iter()
                .any(|kind| kind == &format!("tl.{id}.Value")),
            "the build keeps its records under its own kinds: {:?}",
            manifest.resources.kinds
        );
        assert!(
            !manifest
                .resources
                .kinds
                .iter()
                .any(|kind| kind == &format!("tl.{of}.Value")),
            "and never under the kinds of the app it builds: {:?}",
            manifest.resources.kinds
        );
        assert!(
            store.catalog().find(of).unwrap().is_none(),
            "the app it builds is not the app it ran as"
        );
        // The build is a tilolang app: a window of it runs its instance in the app's own process,
        // which needs this device's person and its tilolang host, and this fixture has neither.
        // What starts here is the installed package's own program with no window, as a device
        // with no display starts it.
        let launcher = tilo_test_env::stand_in(root.join("tilo-launcher"), "hold\n").unwrap();
        let apps = Arc::new(installed(
            &store,
            &root,
            vec![launcher.path().to_owned()],
        ));
        // What the workspace granted the app this is a build of.
        let to_the_app = CapOverride::grant(
            Grantee::person("person-7"),
            Scope::NetHost("mail.example.com".into()),
            Access::Write,
        )
        .through(of);
        apps.keep_launch_grants(Arc::new(Granted::new(vec![to_the_app])));
        let dev = apps
            .launchable(&id)
            .expect("the device launches the build it installed");
        let launch = block_on_owned({
            let (apps, app) = (Arc::clone(&apps), id.clone());
            async move {
                let launch = apps.start_showing(&app, WORKSPACE, Shows::Nothing, None, &Runs::default())?;
                apps.started(launch, &app).await
            }
        })
        .expect("the staged build starts");
        assert_eq!(
            apps.attested(launch.pid),
            Some((id, WORKSPACE.to_owned())),
            "it runs under its own id, not the app's"
        );
        assert!(
            apps.app_grants(&dev, WORKSPACE).sandbox.is_empty(),
            "it starts with no grants of its own, and none of the app it builds reach it"
        );
        launch.kill();
    }
}

// An app that keeps a long-lived connection says so in its signed manifest: the daemon offers its
// turn with the capabilities it requires, starts it with no window and no idle end when this
// device holds the lease, recognises the process by its launch record, and ends exactly that
// process when the device lets the turn go.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn an_apps_connection_runs_with_no_window_until_the_device_stops_it;

    phase the_connection_starts_runs_and_stops;
    {
        // `Apps` has a launch of its own called `start`, and its installed list: the
        // connection host's are named through the trait.
        use tilo_workspace_service::work::HostConnections;
        let start = |apps: &Apps| connection_started(apps, WORKSPACE, BRIDGE);

        const BRIDGE: &str = "so.tilo.bridge";
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            BRIDGE,
            &manifest(
                BRIDGE,
                "gui",
                "connection = true\n\n[capabilities]\nrequired = [\"network.http\", \"secrets\"]\n",
            ),
            &[("probe", b"binary")],
        );
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let said = root.join("said");
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!("env > '{said}.env'\nhold\n", said = said.display()),
        )
        .unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);

        assert_eq!(
            apps.connections(),
            [(
                BRIDGE.to_owned(),
                vec!["network.http".to_owned(), "secrets".to_owned()]
            )],
            "only the app that says it keeps a connection has a connection turn"
        );
        assert!(apps.gives(WORKSPACE, BRIDGE, "secrets"));
        assert!(!apps.gives(WORKSPACE, BRIDGE, "camera"), "nothing it does not declare");
        assert!(
            HostConnections::installed(&apps, BRIDGE)
                && !HostConnections::installed(&apps, "so.tilo.unknown")
        );
        assert!(!apps.runs(WORKSPACE, BRIDGE), "nothing runs before the device holds it");

        start(&apps).expect("the connection's process starts");
        assert!(apps.runs(WORKSPACE, BRIDGE), "it runs, recognised by its launch record");
        let env = wait_for(&format!("{}.env", said.display()));
        assert!(
            env.lines().any(|line| line == "TILO_NO_WINDOW=1"),
            "it draws nothing: {env}"
        );
        assert!(
            !env.contains("TILO_NO_WINDOW_IDLE"),
            "it has no idle end, unlike a process started for a call: {env}"
        );
        start(&apps).expect("starting it again is no second process");
        assert_eq!(apps.launched.0.lock().unwrap().len(), 1, "one process keeps it");
        // The daemon knows the process as the app's held connection, which a vault item may be
        // released to.
        let pid = *apps.launched.0.lock().unwrap().keys().next().unwrap();
        assert_eq!(
            apps.process_of(pid),
            Some(super::ProcessOf::Launched {
                app: BRIDGE.to_owned(),
                workspace: WORKSPACE.to_owned(),
                connection: true,
            })
        );

        apps.stop(WORKSPACE, BRIDGE);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while apps.runs(WORKSPACE, BRIDGE) {
            assert!(std::time::Instant::now() < until, "the stopped process ends");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            apps.launched.0.lock().unwrap().is_empty(),
            "its reaper forgets it once it ended"
        );
    }
}

// A bridge has no window: its manifest says `type = "cli"`, and its held connection still starts,
// with no window and no idle end.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn an_app_with_no_window_keeps_its_connection;

    phase a_cli_entrypoint_keeps_its_connection;
    {
        use tilo_workspace_service::work::HostConnections;
        const BRIDGE: &str = "so.tilo.bridge";
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(
            BRIDGE,
            &manifest(BRIDGE, "cli", "connection = true\n"),
            &[("probe", b"binary")],
        );
        let said = root.join("said");
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!("env > '{said}.env'\nhold\n", said = said.display()),
        )
        .unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
        assert!(
            launched(&apps, BRIDGE, WORKSPACE).is_err(),
            "it has no window to open"
        );
        connection_started(&apps, WORKSPACE, BRIDGE).expect("its connection starts with no window");
        assert!(apps.runs(WORKSPACE, BRIDGE));
        let env = wait_for(&format!("{}.env", said.display()));
        assert!(env.lines().any(|line| line == "TILO_NO_WINDOW=1"), "{env}");
        assert!(!env.contains("TILO_NO_WINDOW_IDLE"), "{env}");
        apps.stop(WORKSPACE, BRIDGE);
    }
}

// A package that carries a program (`[entrypoint] script`) beside the module its actions and views
// are: the device runs the program as the app's held connection, in its own process, and its code
// still runs in the app's tilolang host, so its actions answer.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_package_that_carries_a_program_keeps_its_own_connection;

    phase its_code_runs_in_a_tilolang_host_and_its_program_is_the_connection;
    {
        use crate::device::package_runs;
        use tilo_workspace_service::work::HostConnections;
        const BRIDGE: &str = "so.tilo.bridge";
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let package = format!(
            "[package]\nid = \"{BRIDGE}\"\nversion = \"0.1.0\"\nkind = \"app\"\n\
             runtime = \"tilo-runtime/2026.07\"\n\
             optional-runtimes = [\"tilolang/1.0.0\", \"bun/1.4.5+bun\"]\n\n\
             [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\n\
             [sandbox]\nnamespaces = [\"user\", \"pid\", \"uts\", \"ipc\", \"mount\", \"net\"]\n\
             seccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n\
             [sandbox.net]\nmode = \"allowlist\"\nhosts = [\"gateway.example\"]\n\n\
             [capabilities]\nrequired = [\"network.http\", \"secrets\"]\n\n\
             [entrypoint]\nbin = \"bin/bun\"\ntype = \"cli\"\nruntime = \"bun/1.4.5+bun\"\n\
             script = \"main.js\"\nconnection = true\n",
            "00".repeat(32)
        );
        store.install(
            BRIDGE,
            &package,
            &[
                ("app.wasm", b"module"),
                ("main.js", b"program"),
                ("src/tilo.js", b"calls"),
            ],
        );
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment
            .remove("TILO_APPS_SYSTEM_DIR")
            .set("TILO_APPS_DATA_DIR", &store.root);
        // The runtimes its manifest names are installed, so the launch it stands in for is
        // not refused for them.
        let bundles = root.join("runtimes");
        for version in ["2026.07", "1.0.0", "1.4.5+bun"] {
            std::fs::create_dir_all(bundles.join(version)).unwrap();
            std::fs::write(bundles.join(version).join("bundle.toml"), "").unwrap();
        }
        environment.set("TILO_RUNTIME_BUNDLE_DIR", &bundles);
        assert!(
            package_runs::runs_in_process(BRIDGE),
            "its payload carries the module its actions are"
        );
        assert_eq!(
            package_runs::origin_of(BRIDGE).unwrap(),
            None,
            "it reaches no outside service through a connector"
        );
        assert!(
            !package_runs::runs_in_process(VIEWER),
            "an app that carries no module runs no code of its own"
        );

        let said = root.join("said");
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!("env > '{said}.env'\nhold\n", said = said.display()),
        )
        .unwrap();
        let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
        assert_eq!(
            apps.connections()
                .into_iter()
                .map(|(app, _)| app)
                .collect::<Vec<String>>(),
            [BRIDGE.to_owned()],
            "it is the app's held connection"
        );
        connection_started(&apps, WORKSPACE, BRIDGE)
            .expect("its program is started as its own process, not its origin's connector");
        assert!(apps.runs(WORKSPACE, BRIDGE), "the program runs");
        let env = wait_for(&format!("{}.env", said.display()));
        assert!(
            env.lines().any(|line| line == "TILO_NO_WINDOW=1"),
            "it draws nothing: {env}"
        );
        assert!(!env.contains("TILO_NO_WINDOW_IDLE"), "{env}");
        apps.stop(WORKSPACE, BRIDGE);
    }
}

// The computer's own daemon runs as the device's account (root, in an image whose daemon is the
// system's own), and an app is an untrusted package: its held connection, which runs with no person
// signed in, is started by the device's app host (`tilo-launcher host`, which systemd runs as an
// account of its own; `runtime_portal::app_host`'s tests hold that account apart), through the same
// launcher command, and recognised by the pidfd the host pinned it with. The daemon starts no
// process of the app itself, and still stops it. It hands the launch every connection it makes,
// each bound to the launch it recorded: the app reaches no socket of the daemon's by path.
#[cfg(target_os = "linux")]
#[test]
fn a_held_connection_runs_at_the_app_host() {
    use tilo_workspace_service::work::HostConnections;
    const BRIDGE: &str = "so.tilo.bridge";
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        BRIDGE,
        &manifest(
            BRIDGE,
            "cli",
            "connection = true\n\n[sandbox.services]\nnative = \"required@58\"\n\
             collab = \"required@1\"\n",
        ),
        &[("probe", b"binary")],
    );
    // The daemon's own services, which only it reaches.
    let sockets = crate::daemon::Sockets::in_dir(&root);
    let _collab = std::os::unix::net::UnixListener::bind(&sockets.collab).unwrap();
    let _identity = std::os::unix::net::UnixListener::bind(&sockets.identity).unwrap();
    // The launcher the host runs.
    let said = root.join("said");
    std::fs::create_dir(&said).unwrap();
    let launcher = saying_launcher(&root, &said);
    let socket = root.join("app-host.sock");
    let hosts = app_host_at(&socket);
    let apps = apps(&store, &root, vec![launcher])
        .ordering_of_the_seat()
        .hosting_connections_at(socket);
    let lines = own_lines_made(&apps);

    connection_started(&apps, WORKSPACE, BRIDGE).expect("its connection starts at the app host");
    assert!(
        apps.runs(WORKSPACE, BRIDGE),
        "it runs, recognised by its pidfd"
    );
    let pid = *apps.launched.0.lock().unwrap().keys().next().unwrap();
    assert_eq!(
        apps.process_of(pid),
        Some(super::ProcessOf::Launched {
            app: BRIDGE.to_owned(),
            workspace: WORKSPACE.to_owned(),
            connection: true,
        }),
        "a connection from it is admitted as the app's held connection"
    );
    assert_eq!(
        *lines.lock().unwrap(),
        [(pid, BRIDGE.to_owned(), WORKSPACE.to_owned())],
        "its native line is answered as the launch the daemon recorded"
    );
    let launch = wait_for(&said.join("launch").display().to_string());
    let mut lines = launch.lines();
    let parent: u32 = lines.next().unwrap().parse().unwrap();
    let host_pids: Vec<u32> = hosts
        .lock()
        .unwrap()
        .iter()
        .map(std::process::Child::id)
        .collect();
    assert_eq!(
        host_pids,
        [parent],
        "the app host started it, never the daemon itself: {launch}"
    );
    let rest: Vec<&str> = lines.collect();
    assert_eq!(
        rest.first().copied(),
        Some("run"),
        "it is the launcher's own run of the app, in the sandbox every app runs in: {launch}"
    );
    assert!(
        rest.windows(2).any(|pair| pair == ["--app-id", BRIDGE]),
        "{launch}"
    );
    assert!(
        rest.contains(&"TILO_NO_WINDOW=1"),
        "it draws nothing: {launch}"
    );
    handed_its_connections(&apps, (&rest, &root), (host_pids[0], pid), BRIDGE);
    let runtime = rest
        .iter()
        .find_map(|line| line.strip_prefix("XDG_RUNTIME_DIR="))
        .expect("its runtime directory");
    assert!(
        !Path::new(runtime).starts_with(&root),
        "its runtime directory is the host's, not the daemon's: {runtime}"
    );

    apps.stop(WORKSPACE, BRIDGE);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while apps.runs(WORKSPACE, BRIDGE) {
        assert!(
            std::time::Instant::now() < until,
            "the stopped connection ends"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for mut host in hosts.lock().unwrap().drain(..) {
        let status = host.wait().unwrap();
        assert!(
            status.success(),
            "the host ends with the app it ran: {status}"
        );
    }
}

/// A launcher stand-in that waits at its launch hold as the real one does, says into `said` who
/// started it and what it was handed, then keeps running until it is stopped.
#[cfg(target_os = "linux")]
fn saying_launcher(root: &Path, said: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let launcher = root.join("tilo-launcher");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n\
             head -c 1 >/dev/null\n\
             {{ echo \"$PPID\"\n\
             printf '%s\\n' \"$@\"\n\
             env\n\
             }} > '{said}/launch.tmp' && mv '{said}/launch.tmp' '{said}/launch'\n\
             exec sleep 600\n",
            said = said.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    launcher
}

/// The daemon's lines `apps` hands each launch, each answered as the launch it names: the
/// launches named, in order.
#[cfg(target_os = "linux")]
fn own_lines_made(apps: &Apps) -> Arc<std::sync::Mutex<Vec<(u32, String, String)>>> {
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let made = Arc::clone(&lines);
    apps.serve_own_lines(Arc::new(move |pid, app: &str, workspace: &str| {
        made.lock()
            .unwrap()
            .push((pid, app.to_owned(), workspace.to_owned()));
        let (_ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        Ok(theirs.into())
    }));
    lines
}

/// The hosted launch `pid` of `app`, started by the host `host` with the command lines `rest`,
/// takes every connection the daemon made from its release and is told the path of none of the
/// daemon's sockets in `root`, in its command or its environment. The collaboration connection the daemon made names the launch, and is admitted
/// once, as that launch, from the daemon alone: a request from any other process naming the app
/// is no launch of the daemon's.
#[cfg(target_os = "linux")]
fn handed_its_connections(
    apps: &Apps,
    (rest, root): (&[&str], &Path),
    (host, pid): (u32, u32),
    app: &str,
) {
    let launch = rest.join("\n");
    let handed: Vec<&str> = rest
        .windows(2)
        .filter(|pair| pair[0] == "--handed-service")
        .map(|pair| pair[1])
        .collect();
    assert_eq!(
        handed,
        ["native", "collab", "identity"],
        "it takes each connection the daemon made from its release, in order: {launch}"
    );
    let sockets = crate::daemon::Sockets::in_dir(root);
    for socket in [
        &sockets.native,
        &sockets.collab,
        &sockets.identity,
        &sockets.blobs,
    ] {
        let socket = socket.to_str().unwrap();
        assert!(
            !rest.iter().any(|line| line.contains(socket)),
            "it is told no path to {socket}: {launch}"
        );
    }
    assert!(
        apps.authenticated_launch(host, app, WORKSPACE, None)
            .unwrap()
            .is_none(),
        "a process that is not the daemon gets no launch's connection"
    );
    let admitted = apps
        .authenticated_launch(std::process::id(), app, WORKSPACE, None)
        .unwrap()
        .expect("the connection the daemon made is the launch's");
    assert_eq!(admitted.launch_ticket, apps.launched.ticket(pid));
    assert!(
        admitted.launch_ticket.is_some(),
        "it carries the launch's ticket"
    );
    assert!(admitted.windowless, "a held connection has no window");
    assert!(
        apps.authenticated_launch(std::process::id(), app, WORKSPACE, None)
            .unwrap()
            .is_none(),
        "it is admitted once"
    );
}

/// The app host, as systemd runs it for each connection to `socket`: `tilo-launcher host`, the
/// connection its standard input. The hosts it started so far.
#[cfg(target_os = "linux")]
fn app_host_at(socket: &Path) -> Arc<std::sync::Mutex<Vec<std::process::Child>>> {
    let listener = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::bind(
        &listener,
        &rustix::net::SocketAddrUnix::new(socket).unwrap(),
    )
    .unwrap();
    rustix::net::listen(&listener, 4).unwrap();
    let host = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let hosts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = Arc::clone(&hosts);
    std::thread::spawn(move || {
        while let Ok(link) = rustix::net::accept_with(&listener, rustix::net::SocketFlags::CLOEXEC)
        {
            let child = std::process::Command::new(&host)
                .arg("host")
                .stdin(std::process::Stdio::from(link))
                .spawn_locked()
                .expect("the app host starts");
            started.lock().unwrap().push(child);
        }
    });
    hosts
}

/// What a stand-in wrote to `path`, once it wrote it: a launch's own arguments and environment
/// are the launcher's to record, and the process runs on until the test ends.
#[cfg(target_os = "linux")]
fn wait_for(path: &str) -> String {
    let until = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(30))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if let Ok(written) = std::fs::read_to_string(path) {
            return written;
        }
        assert!(
            std::time::Instant::now() < until,
            "the launcher wrote {path} in time"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_payload_that_does_not_verify_is_refused() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    let package = store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let installed = || {
        app_catalogue::apps::read(&store.catalog())
            .unwrap()
            .into_iter()
            .find(|app| app.app_id == VIEWER)
            .unwrap()
    };
    verified::verify_installed(
        &store.catalog(),
        &installed(),
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap();

    // phase: a_changed_payload_file
    std::fs::write(package.payload_root.join("probe"), b"tampered").unwrap();
    let err = verified::verify_installed(
        &store.catalog(),
        &installed(),
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap_err();
    assert!(err.contains("payload changed"), "{err}");
    std::fs::write(package.payload_root.join("probe"), b"binary").unwrap();

    // phase: a_file_the_package_did_not_ship
    std::fs::write(package.payload_root.join("extra.dylib"), b"?").unwrap();
    let err = verified::verify_installed(
        &store.catalog(),
        &installed(),
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap_err();
    assert!(err.contains("did not ship"), "{err}");
    std::fs::remove_file(package.payload_root.join("extra.dylib")).unwrap();

    // phase: a_missing_signed_record
    let record = package.manifest_path.parent().unwrap().join("signed-root");
    let signed = std::fs::read(&record).unwrap();
    std::fs::remove_file(&record).unwrap();
    let err = verified::verify_installed(
        &store.catalog(),
        &installed(),
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    let apps = apps(&store, &root, Vec::new());
    let err = launched(&apps, VIEWER, WORKSPACE).map(|_| ()).unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    assert!(apps.launched.0.lock().unwrap().is_empty());
    std::fs::write(&record, signed).unwrap();
    verified::verify_installed(
        &store.catalog(),
        &installed(),
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap();
}

tilo_test_env::scenario! {
    fn a_launched_process_is_attested_and_named_by_its_own_launch;

    phase attestation_holds_only_for_the_launched_process_itself;
    {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        let pid = child.id();
        let launched = Launched::default();
        assert!(
            !launched.attest(pid, VIEWER, WORKSPACE),
            "unknown until recorded"
        );
        assert!(launched.record_showing(pid, VIEWER, WORKSPACE, Shows::Window, None, 1));
        assert!(launched.attest(pid, VIEWER, WORKSPACE));
        assert!(!launched.attest(pid, "so.tilo.other", WORKSPACE));
        assert!(!launched.attest(pid, VIEWER, "another-workspace"));
        assert!(!launched.attest(pid.wrapping_add(100_000), VIEWER, WORKSPACE));

        // A record of an earlier process that held this pid does not attest the
        // one holding it now.
        launched.0.lock().unwrap().get_mut(&pid).unwrap().process = super::Pinned::Started(u64::MAX);
        assert!(!launched.attest(pid, VIEWER, WORKSPACE));
        launched.forget(pid);
        assert!(launched.record_showing(pid, VIEWER, WORKSPACE, Shows::Window, None, 1));

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            !launched.attest(pid, VIEWER, WORKSPACE),
            "a reaped process is no longer the one launched"
        );
        launched.forget(pid);
        assert!(!launched.attest(pid, VIEWER, WORKSPACE));
    }

    phase a_connecting_process_names_the_launch_that_started_it;
    // Each launch's process connects with the ticket of its own launch, so a
    // workspace `open` learns which instance it started.
    {
        let root = std::env::temp_dir();
        let apps = Apps::new(
            None,
            &root,
            crate::daemon::Sockets::in_dir(&root),
            PathBuf::new(),
            Vec::new(),
        );
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        let ours = std::process::id();
        assert!(apps.launched.record_showing(ours, VIEWER, WORKSPACE, Shows::Window, None, 1));
        assert!(apps.launched.record_showing(child.id(), VIEWER, WORKSPACE, Shows::Window, None, 2));
        let ticket = |pid| {
            apps.authenticated_launch(pid, VIEWER, WORKSPACE, None)
                .unwrap()
                .and_then(|launch| launch.launch_ticket)
        };
        let (first, second) = (ticket(ours), ticket(child.id()));
        assert!(first.is_some() && second.is_some());
        assert_ne!(
            first, second,
            "two launches of one app hold their own tickets"
        );
        assert!(
            apps.authenticated_launch(ours, "so.tilo.other", WORKSPACE, None)
                .unwrap()
                .is_none(),
            "a ticket is presented only for the launch's own app"
        );
        // A process below a launch the daemon recorded is that launch's, as the ancestry read
        // the workspace serves admits it: the attestation names it and presents its ticket.
        let mut below = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        assert_eq!(
            ticket(below.id()),
            first,
            "a descendant presents the launch's ticket"
        );
        assert!(
            apps.launched.attest(below.id(), VIEWER, WORKSPACE),
            "a descendant is attested as the launch it descends from"
        );
        assert_eq!(
            apps.attested(below.id()),
            Some((VIEWER.to_owned(), WORKSPACE.to_owned())),
            "and it is the app the device serves"
        );
        assert!(!apps.launched.attest(below.id(), "so.tilo.other", WORKSPACE));
        below.kill().unwrap();
        below.wait().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(ticket(child.id()), None, "a reaped process presents none");
    }

    phase a_process_start_time_is_stable_and_absent_for_no_process;
    {
        let own = std::process::id();
        let first = process_start_time(own).unwrap();
        assert_eq!(process_start_time(own), Some(first));
        assert_eq!(process_start_time(u32::MAX), None);
    }

    phase a_process_the_daemon_started_is_the_app_it_runs_and_no_other_process_is;
    // What the daemon starts for a tilolang app — an instance, a connector, a task's host — is
    // recorded by its pid, so a connection from it is admitted as that app. A process it did not
    // start, and that descends from none it did, is no app.
    {
        let root = std::env::temp_dir();
        let apps = Apps::new(
            None,
            &root,
            crate::daemon::Sockets::in_dir(&root),
            PathBuf::new(),
            Vec::new(),
        );
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        let pid = child.id();
        assert_eq!(apps.attested(pid), None, "unknown until recorded");
        apps.record_started(pid, VIEWER, WORKSPACE);
        assert_eq!(
            apps.attested(pid),
            Some((VIEWER.to_owned(), WORKSPACE.to_owned())),
            "the process is the app it was started as"
        );
        assert_eq!(
            apps.process_of(pid),
            Some(super::ProcessOf::Started {
                app: VIEWER.to_owned(),
                workspace: WORKSPACE.to_owned(),
                own: false,
            }),
            "a connector or a task's host is not the app's own tilolang process"
        );
        assert!(apps.launched.record_own_tilolang(pid, VIEWER, WORKSPACE));
        assert!(
            matches!(
                apps.process_of(pid),
                Some(super::ProcessOf::Started { own: true, .. })
            ),
            "the process of the app's own instances is told apart"
        );
        // A process beside the app, started by this test and not by it, is a process of its own.
        let mut below = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        assert_eq!(
            apps.attested(below.id()),
            None,
            "a process the daemon did not start itself is no app"
        );
        below.kill().unwrap();
        below.wait().unwrap();
        apps.forget_started(pid);
        assert_eq!(apps.attested(pid), None, "an ended process is no app");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

/// A real launch through `launcher-macos`: a signed, verified package whose
/// probe checks it inherited its service descriptors and its file chooser,
/// then waits on the
/// collaboration connection until the runtime goes away.
#[cfg(target_os = "macos")]
#[test]
fn a_verified_app_launches_confined_with_its_services_on_macos() {
    use std::os::unix::net::UnixListener;
    use std::process::Command;

    const PROBE: &str = r#"
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>
static int fd_of(const char *name) {
    const char *value = getenv(name);
    if (!value) return -1;
    int fd = atoi(value);
    return fcntl(fd, F_GETFD) < 0 ? -1 : fd;
}
int main(void) {
    int collab = fd_of("TILO_SVC_COLLAB_FD");
    if (collab < 0 || fd_of("TILO_SVC_IDENTITY_FD") < 0) return 3;
    if (fd_of("TILO_SVC_CAPTURE_FD") < 0) return 5;
    if (fd_of("TILO_FILE_PICKER_FD") < 0) return 4;
    char byte;
    (void)read(collab, &byte, 1);
    return 0;
}
"#;
    let (_temp, root) = canonical_temp();
    let source = root.join("probe.c");
    std::fs::write(&source, PROBE).unwrap();
    let binary = root.join("probe");
    for command in [
        Command::new("cc")
            .arg("-O0")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .status_locked(),
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&binary)
            .status_locked(),
    ] {
        assert!(command.unwrap().success());
    }
    let bytes = std::fs::read(&binary).unwrap();
    let mut store = Store::new(&root);
    let package = store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", &bytes)]);
    let probe = package.payload_root.join("probe");
    std::fs::set_permissions(&probe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let collab = UnixListener::bind(root.join(crate::COLLAB_SOCKET)).unwrap();
    let _identity = UnixListener::bind(root.join(crate::IDENTITY_SOCKET)).unwrap();
    let apps = apps(&store, &root, Vec::new());
    let launch = launched(&apps, VIEWER, WORKSPACE).unwrap();
    assert!(apps.attest_process(launch.pid, VIEWER, WORKSPACE));
    assert!(!apps.attest_process(launch.pid, "so.tilo.other", WORKSPACE));
    // The app's connection is queued, never accepted; closing the listener
    // ends it, and the probe's read with it.
    drop(collab);
    let status = launch.wait().expect("the probe's process ended");
    assert!(status.success(), "the probe ended as it ran: {status}");
}

/// A connection this daemon makes for an instance it starts itself names that instance, and the
/// collaboration service attests it as exactly that, once, from this process alone: opened for the
/// caller its view was opened for (a visitor at a page here), or showing nothing. The connection is
/// the instance's; who acts is the caller it carries. An app that is no tilolang app installed here
/// opens no view, and nothing starts for it. A window the daemon launched itself is attested as
/// that window: shown, and opened for the member at it rather than for a caller.
#[cfg(unix)]
#[test]
fn the_daemons_own_instances_are_attested_as_the_view_they_were_opened_for() {
    use super::instances::Shows;
    use std::io::Read as _;
    use tilo_actions::remote::Caller;
    let (_temp, root) = canonical_temp();
    let store = Store::new(&root);
    let listener = std::os::unix::net::UnixListener::bind(root.join(crate::COLLAB_SOCKET)).unwrap();
    let apps = apps(&store, &root, Vec::new());
    let ours = std::process::id();
    let visitor = Caller::visitor([9; 32]).in_workspace(WORKSPACE);

    let _view = apps
        .instances
        .connect(
            VIEWER,
            WORKSPACE,
            7,
            Shows::View(Box::new((visitor.clone(), None))),
        )
        .unwrap();
    let (mut broker_end, _) = listener.accept().unwrap();
    let mut named = [0; 256];
    let read = broker_end.read(&mut named).unwrap();
    let named = String::from_utf8_lossy(&named[..read]);
    assert!(
        named.contains(VIEWER) && named.contains("\"window_instance\":7"),
        "the connection names the instance it is for: {named}"
    );
    assert!(
        apps.authenticated_launch(1, VIEWER, WORKSPACE, Some(7))
            .unwrap()
            .is_none(),
        "another process naming the instance is no launch"
    );
    let launch = apps
        .authenticated_launch(ours, VIEWER, WORKSPACE, Some(7))
        .unwrap()
        .expect("the instance's connection is attested");
    assert_eq!(launch.continuation_instance, Some(7));
    assert!(!launch.windowless, "it shows a view");
    assert_eq!(
        launch.opened_for,
        Some(visitor.clone()),
        "opened for the visitor"
    );
    assert!(
        apps.authenticated_launch(ours, VIEWER, WORKSPACE, Some(7))
            .unwrap()
            .is_none(),
        "a second connection naming it is no launch"
    );

    let _no_view = apps
        .instances
        .connect(VIEWER, WORKSPACE, 8, Shows::Nothing(None))
        .unwrap();
    let launch = apps
        .authenticated_launch(ours, VIEWER, WORKSPACE, Some(8))
        .unwrap()
        .expect("the instance that shows nothing is attested");
    assert!(launch.windowless, "it shows nothing");
    assert_eq!(launch.opened_for, None, "and was opened for nobody");

    let _window = apps
        .instances
        .connect(VIEWER, WORKSPACE, 9, Shows::Window)
        .unwrap();
    let launch = apps
        .authenticated_launch(ours, VIEWER, WORKSPACE, Some(9))
        .unwrap()
        .expect("the instance of the daemon's window is attested");
    assert_eq!(launch.continuation_instance, Some(9));
    assert!(!launch.windowless, "it shows the window");
    assert_eq!(
        launch.opened_for, None,
        "the member at the window acts in it"
    );

    let refused = apps
        .open_view((&visitor, None), VIEWER, WORKSPACE, None, None)
        .unwrap_err();
    assert!(refused.contains("no tilolang app"), "{refused}");
    assert!(
        apps.windowless(VIEWER, WORKSPACE, None, None)
            .unwrap()
            .is_none(),
        "an app that is not installed here starts nothing without a window"
    );
}

/// A daemon nobody is signed in at starts an app's origin connector itself: the app's own launch
/// through the launcher, with the origin named and no display, handed the connector's socket
/// through its launch hold. An app it may not run there is refused, saying why, and starts
/// nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_daemon_with_no_session_starts_an_apps_origin_connector_and_refuses_one_it_may_not_run() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let asked = root.join("asked");
    // Records what it was asked, reads its launch hold to the end, and ends.
    let launcher = tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" > '{asked}'\nexec cat >/dev/null\n",
            asked = asked.display()
        ),
    )
    .unwrap();
    let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
    let (_ours, socket) = runtime_portal::origin_connector::pair().unwrap();

    let mut connector = apps
        .start_connector(VIEWER, WORKSPACE, "imap", &socket)
        .unwrap();
    assert!(connector.wait().unwrap().success());
    let asked = std::fs::read_to_string(&asked).unwrap();
    assert!(
        asked.starts_with("run ")
            && asked.contains(&format!("--app-id {VIEWER}"))
            && asked.contains(&format!("--workspace-id {WORKSPACE}"))
            && asked.contains("--tilolang-connector imap"),
        "the launcher starts the app's connector for its origin: {asked}"
    );
    assert!(
        !asked.contains("--wayland-socket"),
        "a connector draws nothing: {asked}"
    );

    for (app, workspace, why) in [
        (VIEWER, "another-workspace", "not served"),
        ("so.tilo.unknown", WORKSPACE, "not installed"),
    ] {
        let refused = apps
            .start_connector(app, workspace, "imap", &socket)
            .map(drop)
            .unwrap_err();
        assert!(refused.contains(why), "{refused}");
    }
}

/// A workspace that runs no version of the app, or whose members took the grant it keeps its
/// records under back, refuses every change an app's origin connector makes. The connector
/// starts nowhere there, so it makes none: the launcher is never asked for it, and nothing is
/// refused on every frame.
#[cfg(target_os = "linux")]
#[test]
fn an_apps_origin_connector_starts_nowhere_the_workspace_will_not_let_it_keep_its_records() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let asked = root.join("asked");
    let launcher = tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" > '{asked}'\nexec cat >/dev/null\n",
            asked = asked.display()
        ),
    )
    .unwrap();
    let apps = installed(&store, &root, vec![launcher.path().to_owned()]);
    apps.keep_launch_grants(Arc::new(Granted::new(Vec::new()).keeps_no_records()));
    let (_ours, socket) = runtime_portal::origin_connector::pair().unwrap();

    let refused = apps
        .start_connector(VIEWER, WORKSPACE, "imap", &socket)
        .map(drop)
        .unwrap_err();
    assert!(
        refused.contains("keeps no records of its own"),
        "the app's own records are what its connector keeps: {refused}"
    );
    assert!(
        !asked.exists(),
        "the launcher starts no connector where the workspace refuses what it writes"
    );
}

/// A launch starts the package this computer has installed, through the launcher and with the
/// sandbox, grants and program its manifest declares — never a program the caller names.
#[cfg(target_os = "linux")]
#[test]
fn a_launch_asks_the_launcher_for_the_installed_packages_own_program() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    let installed = store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let asked = root.join("launched");
    // A stand-in launcher: it records what it was asked and keeps running, as a launcher released
    // to run its app does. One that ended at once would be a start that ran no app.
    let launcher = tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" > '{asked}'\nhold\n",
            asked = asked.display()
        ),
    )
    .unwrap();
    let apps = Arc::new(apps(&store, &root, vec![launcher.path().to_owned()]));
    // The windowless start, as a device with no display takes it: the launch is the package's,
    // through the same launcher a window goes through.
    let launch = block_on_owned({
        let apps = Arc::clone(&apps);
        async move {
            let launch =
                apps.start_showing(VIEWER, WORKSPACE, Shows::Nothing, None, &Runs::default())?;
            apps.started(launch, VIEWER).await
        }
    })
    .expect("the installed app starts");
    let asked = wait_for(&asked.display().to_string());
    assert!(
        asked.starts_with("run ")
            && asked.contains(&format!("--manifest {}", installed.manifest_path.display()))
            && asked.contains(&format!(
                "--payload-root {}",
                installed.payload_root.display()
            ))
            && asked.contains(&format!("--app-id {VIEWER}"))
            && asked.contains(&format!("--workspace-id {WORKSPACE}")),
        "the launcher starts the installed package's own program: {asked}"
    );
    assert!(
        !asked.contains("/bin/sh"),
        "no program of the caller's is started: {asked}"
    );

    let refused = apps.installed_here("so.tilo.passwords").unwrap_err();
    assert!(
        refused
            .contains("so.tilo.passwords is not installed on this computer; install it first with `tilo app install so.tilo.passwords`"),
        "{refused}"
    );
    launch.kill();
}

// A start whose program ends as it starts has run no app: it answers the status and the
// program's own words, never that it started.
#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn a_start_that_ends_as_it_starts_answers_why;

    phase the_launch_answers_the_programs_own_words;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let device = SigningKey::from_bytes(&[7_u8; 32]);
        let of = "org.example.notes";
        store.install(of, &manifest(of, "gui", ""), &[("probe", b"binary")]);
        let dev = store.install_dev(of, &device);
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            "echo 'the sandbox refused this app' >&2\nexit 3\n",
        )
        .unwrap();
        let apps =
            apps(&store, &root, vec![launcher.path().to_owned()]).expecting_connection();
        let Err(error) = launched(&apps, &dev.id, WORKSPACE) else {
            panic!("a start that ran no app is refused");
        };
        assert!(error.contains("ended as it started"), "{error}");
        assert!(error.contains("the sandbox refused this app"), "{error}");
    }

    // On a device's daemon a start answers only once the app is up: a launcher that keeps
    // running and then ends without connecting ends the start, with its own words, rather than
    // answering that it started.
    phase a_start_that_ends_before_it_connects_answers_why;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let device = SigningKey::from_bytes(&[7_u8; 32]);
        let of = "org.example.notes";
        store.install(of, &manifest(of, "gui", ""), &[("probe", b"binary")]);
        let dev = store.install_dev(of, &device);
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let launcher = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            "sleep 0.3\necho 'the app could not load its module' >&2\nexit 4\n",
        )
        .unwrap();
        let apps =
            apps(&store, &root, vec![launcher.path().to_owned()]).expecting_connection();
        let Err(error) = launched(&apps, &dev.id, WORKSPACE) else {
            panic!("a start that ended before connecting is refused");
        };
        assert!(error.contains("ended as it started"), "{error}");
        assert!(error.contains("the app could not load its module"), "{error}");
    }
}

// A start whose app is slow to connect waits for it without holding the runtime it runs on:
// another task there goes on while it waits, and the app's connection is what ends it.
#[cfg(target_os = "linux")]
#[test]
fn a_start_that_waits_for_its_app_leaves_the_runtime_it_runs_on_going() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    let device = SigningKey::from_bytes(&[7_u8; 32]);
    let of = "org.example.notes";
    store.install(of, &manifest(of, "gui", ""), &[("probe", b"binary")]);
    let dev = store.install_dev(of, &device);
    let display = root.join("wayland-0");
    std::fs::write(&display, b"").unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("WAYLAND_DISPLAY", &display);
    // A launcher that runs on, and an app that connects only once the test says so.
    let launcher = tilo_test_env::stand_in(root.join("tilo-launcher"), "sleep 5\n").unwrap();
    let apps = std::sync::Arc::new(
        apps(&store, &root, vec![launcher.path().to_owned()]).expecting_connection(),
    );
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let (waited, looked) = runtime.block_on(local.run_until(async {
        let waiting = tokio::task::spawn_local({
            let apps = std::sync::Arc::clone(&apps);
            let app = dev.id.clone();
            async move { apps.start(&app, WORKSPACE).await }
        });
        let ticking = tokio::task::spawn_local({
            let (apps, progress) = (
                std::sync::Arc::clone(&apps),
                std::sync::Arc::clone(&progress),
            );
            async move {
                for _ in 0..4 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    progress.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let pid = loop {
                    let recorded = apps.launched.0.lock().unwrap().keys().next().copied();
                    if let Some(pid) = recorded {
                        break pid;
                    }
                    assert!(std::time::Instant::now() < until, "the launch is recorded");
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                apps.launched.connects(pid);
            }
        });
        tokio::join!(waiting, ticking)
    }));
    looked.unwrap();
    let launch = waited
        .unwrap()
        .expect("the app's connection answers its start");
    assert!(
        progress.load(std::sync::atomic::Ordering::SeqCst) >= 4,
        "the runtime went on with other work while the start waited"
    );
    assert!(apps.attested(launch.pid).is_some(), "the app runs");
}

/// A tilolang app that keeps a long-lived connection to an outside service through its
/// origin's connector, on `runtime`.
fn connecting_manifest(id: &str, runtime: &str) -> String {
    format!(
        r#"[package]
id = "{id}"
version = "0.1.0"
kind = "app"
optional-runtimes = ["{runtime}"]

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "probe"
type = "gui"
runtime = "{runtime}"
origins = ["imap"]
connection = true
"#
    )
}

/// A bundle directory naming `runtime` installed, and the environment set to read it: a start
/// that names it runs on this test's own files.
#[cfg(target_os = "linux")]
fn installing_runtime(environment: &mut tilo_test_env::ScopedEnv, root: &Path, runtime: &str) {
    let version = runtime
        .split_once('/')
        .expect("a runtime names its version")
        .1;
    let bundles = root.join("runtimes");
    std::fs::create_dir_all(bundles.join(version)).expect("the bundle directory");
    std::fs::write(bundles.join(version).join("bundle.toml"), "").expect("a bundle");
    environment.set("TILO_RUNTIME_BUNDLE_DIR", &bundles);
}

/// A launcher standing in for `tilo-launcher`: it records each start in `started`, takes its
/// launch hold, writes `said` to its error output, and ends as a launch that cannot start does.
#[cfg(target_os = "linux")]
fn failing_launcher(root: &Path, started: &Path, said: &str) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             dd bs=1 count=1 <&0 >/dev/null 2>&1\n\
             printf '%s\\n' '{said}' >&2\n\
             exit 1\n",
            started = started.display(),
            said = said,
        ),
    )
    .expect("the launcher stands in")
}

/// A connection's start that keeps failing the same way is retried a bounded number of times,
/// then stops: the app's row says why, in its launcher's words, and nothing of the app runs, so
/// no process of it publishes anything the action catalogue would list.
#[cfg(target_os = "linux")]
#[test]
fn a_connection_that_exits_as_it_starts_is_retried_then_stops_with_its_reason() {
    const FAILING: &str = "so.tilo.failing";
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        FAILING,
        &connecting_manifest(FAILING, "tilolang/0.0.1-test"),
        &[("probe", b"binary")],
    );
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment
        .remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", &store.root);
    installing_runtime(&mut environment, &root, "tilolang/0.0.1-test");
    let started = root.join("started");
    let said = "bwrap: no such source path /var/tmp/apps/pkgs/abc: Permission denied";
    let launcher = failing_launcher(&root, &started, said);
    let mut apps = apps(&store, &root, vec![launcher.path().to_owned()]);
    // What a daemon hands every connector before a reader: the connection to its dispatch.
    apps.hand_actions(Box::new(|_, _, _, _| Ok(())));
    // No wait between attempts: the test asks again as soon as a start may run.
    apps.retry = Retry {
        first: std::time::Duration::ZERO,
        cap: std::time::Duration::ZERO,
        settled: std::time::Duration::from_secs(60),
        identical: 3,
    };
    let failures = apps.failures();
    let cache = crate::device::packages::PackageCache::new(
        &SigningKey::from_bytes(&[7; 32]),
        false,
        &store.root,
        Arc::new(crate::device::control::DeviceContent::at(
            &root.join("content"),
        )),
    );
    cache.watch_app_failures(Arc::clone(&failures));

    let mut tries = Vec::new();
    for _ in 0..8 {
        let refusal = connection_started(&apps, WORKSPACE, FAILING)
            .expect_err("the app's connection never comes up");
        tries.push(refusal);
    }

    let spawns = std::fs::read_to_string(&started).map_or(0, |written| written.lines().count());
    assert_eq!(spawns, 3, "three starts, then this device stops: {tries:?}");
    let last = tries.last().expect("a refusal");
    assert!(
        last.reason.contains(said),
        "the reason is the launcher's own words: {last:?}"
    );
    assert!(
        tries
            .windows(2)
            .all(|pair| pair[1].reason == pair[0].reason),
        "every refusal says the same failure: {tries:?}"
    );

    // The app's row says why it is not running, and still that it is installed.
    let rows = cache.rows_for_test().expect("the cache's rows");
    let row = rows
        .iter()
        .find(|row| row.app == FAILING)
        .expect("the app has a row");
    assert_eq!(
        row.failed(),
        Some(last.reason.as_str()),
        "the row says why it stopped"
    );
    assert!(row.installed(), "it is installed still: {row:?}");

    // Nothing of the app ran: no process of it published a surface or a directory root for the
    // action catalogue to list, which is what keeps a listing's revision still across attempts.
    assert_eq!(
        apps.instances.no_view_running(FAILING, WORKSPACE),
        0,
        "the app's code never ran"
    );
    assert!(
        !apps.instances.started_a_process(),
        "no tilolang process of the app ran to publish anything"
    );
}

/// A runtime the app needs that this device does not have is refused at once, without starting
/// anything, and reported once: the app's row says why, and asking again changes nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_connection_needing_a_runtime_that_is_missing_is_refused_once_without_starts() {
    const NEEDS_BUN: &str = "so.tilo.needs-bun";
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        NEEDS_BUN,
        &connecting_manifest(NEEDS_BUN, "bun/0.0.1-test"),
        &[("probe", b"binary")],
    );
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment
        .remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", &store.root);
    // The bundle root names nothing installed: `bun/0.0.1-test` is not here.
    let bundles = root.join("runtimes");
    std::fs::create_dir_all(&bundles).expect("the bundle root");
    environment.set("TILO_RUNTIME_BUNDLE_DIR", &bundles);
    let started = root.join("started");
    let launcher = failing_launcher(&root, &started, "the app is never started");
    let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
    let failures = apps.failures();
    let cache = crate::device::packages::PackageCache::new(
        &SigningKey::from_bytes(&[7; 32]),
        false,
        &store.root,
        Arc::new(crate::device::control::DeviceContent::at(
            &root.join("content"),
        )),
    );
    cache.watch_app_failures(Arc::clone(&failures));

    let mut reported = failures.watch();
    let refusal = connection_started(&apps, WORKSPACE, NEEDS_BUN)
        .expect_err("a runtime it needs is not installed");
    assert!(
        refusal
            .reason
            .contains("needs the runtime `bun/0.0.1-test`, which is not installed"),
        "{}",
        refusal.reason
    );
    assert!(
        reported.has_changed().unwrap_or(false),
        "the first refusal is reported"
    );
    let reason = *reported.borrow_and_update();
    for _ in 0..4 {
        let again = connection_started(&apps, WORKSPACE, NEEDS_BUN)
            .expect_err("it is refused until the runtime is installed");
        assert_eq!(again.reason, refusal.reason);
    }
    assert!(
        !reported.has_changed().unwrap_or(false),
        "a refusal already reported is not reported again"
    );
    assert_eq!(*reported.borrow(), reason);
    assert!(
        !started.exists(),
        "nothing is started for an app whose runtime is missing"
    );

    let rows = cache.rows_for_test().expect("the cache's rows");
    let row = rows
        .iter()
        .find(|row| row.app == NEEDS_BUN)
        .expect("the app has a row");
    assert_eq!(row.failed(), Some(refusal.reason.as_str()));
}

/// A connector that says it is up and then ends at once: the test binary itself, started as the
/// connector ([`readying_launcher`]), so a crash right after a healthy start is exercised
/// through the real socket and the real reader.
#[cfg(target_os = "linux")]
const CONNECTOR_HELPER_ENV: &str = "TILO_TEST_CONNECTOR_HELPER";
#[cfg(target_os = "linux")]
const CONNECTOR_HELPER_TEST: &str = "apps::tests::a_connector_helper_says_ready_and_ends";

#[cfg(target_os = "linux")]
#[test]
fn a_connector_helper_says_ready_and_ends() {
    if std::env::var_os(CONNECTOR_HELPER_ENV).is_none() {
        return;
    }
    let (released, mut fds) = runtime_portal::fd_transport::recv_with_fds(std::io::stdin())
        .expect("the launch hold carries the connector's socket");
    assert_eq!(released, vec![1], "the launch is released before it runs");
    let socket = fds
        .pop()
        .expect("the connector's socket came with the hold");
    runtime_portal::origin_connector::say_ready(&socket).expect("the connector says it is up");
    // Stay on the socket a moment, so the connections the device hands a connector are sent
    // before this ends and nothing it writes meets a closed socket.
    std::thread::sleep(std::time::Duration::from_millis(100));
    eprintln!("the connector ended as it started");
}

/// A launcher standing in for `tilo-launcher` for an app whose connection is its origin's
/// connector: it records each start in `started`, and on a connector launch runs the test
/// binary as the connector ([`a_connector_helper_says_ready_and_ends`]), which says it is up
/// and ends at once.
#[cfg(target_os = "linux")]
fn readying_launcher(root: &Path, started: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             case \"$1\" in\n\
               run) export {env}=1; exec '{exe}' {test} --exact --test-threads 1 --nocapture ;;\n\
               *) hold ;;\n\
             esac\n",
            started = started.display(),
            env = CONNECTOR_HELPER_ENV,
            exe = std::env::current_exe()
                .expect("the test binary is known")
                .display(),
            test = CONNECTOR_HELPER_TEST,
        ),
    )
    .expect("the launcher stands in")
}

/// Wait for the connector of `app` that ran to end, as the turn that keeps it does.
#[cfg(target_os = "linux")]
fn wait_for_connector_end(apps: &Apps, app: &str) {
    use tilo_workspace_service::work::HostConnections;
    let until = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);
    while HostConnections::runs(apps, WORKSPACE, app) {
        assert!(std::time::Instant::now() < until, "the connector ends");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A connector that comes up and then ends at once is backed off exactly as a start that never
/// came up is: its wait doubles, and after three such ends it stops with its own last words as
/// the reason, and its row says so.
#[cfg(target_os = "linux")]
#[test]
fn a_connector_that_ends_soon_after_it_comes_up_backs_off_then_stops_with_its_reason() {
    use tilo_workspace_service::work::HostConnections;
    const APP: &str = "so.tilo.ending";
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        APP,
        &connecting_manifest(APP, "tilolang/0.0.1-test"),
        &[("probe", b"binary")],
    );
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment
        .remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", &store.root);
    installing_runtime(&mut environment, &root, "tilolang/0.0.1-test");
    let started = root.join("started");
    let launcher = readying_launcher(&root, &started);
    let mut apps = apps(&store, &root, vec![launcher.path().to_owned()]);
    apps.hand_actions(Box::new(|_, _, _, _| Ok(())));
    let first = std::time::Duration::from_millis(50);
    let cap = std::time::Duration::from_secs(300);
    apps.retry = Retry {
        first,
        cap,
        settled: std::time::Duration::from_secs(60),
        identical: 3,
    };
    let failures = apps.failures();
    let cache = crate::device::packages::PackageCache::new(
        &SigningKey::from_bytes(&[7; 32]),
        false,
        &store.root,
        Arc::new(crate::device::control::DeviceContent::at(
            &root.join("content"),
        )),
    );
    cache.watch_app_failures(Arc::clone(&failures));

    let mut waits = Vec::new();
    for _ in 0..3 {
        let origin = apps
            .origin_connection(APP, WORKSPACE)
            .expect("the connector comes up");
        assert!(origin.is_some(), "the app reaches its origin");
        wait_for_connector_end(&apps, APP);
        let ended = HostConnections::ended(&apps, WORKSPACE, APP).expect("it ended on its own");
        assert!(
            ended.reason.contains("the connector ended as it started"),
            "its last words are the reason: {}",
            ended.reason
        );
        waits.push(ended.again);
        if ended.again < cap {
            std::thread::sleep(
                ended
                    .again
                    .saturating_add(std::time::Duration::from_millis(10)),
            );
        }
    }
    assert_eq!(
        waits,
        [first, first.saturating_mul(2), cap],
        "the wait doubles, then it stops"
    );

    // It stopped: the supervisor's start is refused, with its reason, and starts nothing more.
    let refused = connection_started(&apps, WORKSPACE, APP).expect_err("it stopped here");
    assert!(
        refused.reason.contains("the connector ended as it started"),
        "{}",
        refused.reason
    );
    let spawns = std::fs::read_to_string(&started).map_or(0, |written| written.lines().count());
    assert_eq!(spawns, 3, "three starts, then this device stops it");

    let rows = cache.rows_for_test().expect("the cache's rows");
    let row = rows
        .iter()
        .find(|row| row.app == APP)
        .expect("the app has a row");
    assert_eq!(row.failed(), Some(refused.reason.as_str()));
}

/// The action catalogue's revision stands still across a failing app's attempts: nothing of the
/// app comes up, so no surface or directory root of it is ever published or withdrawn.
#[cfg(target_os = "linux")]
#[test]
fn a_failing_apps_attempts_move_no_catalogue_revision() {
    use crate::RuntimeServices as _;
    const TEST: &str = "apps::tests::a_failing_apps_attempts_move_no_catalogue_revision";
    const FAILING: &str = "so.tilo.failing";
    let Some(root) = crate::device::services::tests::isolated_root(TEST) else {
        return;
    };
    let mut store = Store::new(&root);
    store.install(
        FAILING,
        &connecting_manifest(FAILING, "tilolang/0.0.1-test"),
        &[("probe", b"binary")],
    );
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment
        .remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", &store.root);
    installing_runtime(&mut environment, &root, "tilolang/0.0.1-test");
    let started = root.join("started");
    let launcher = failing_launcher(
        &root,
        &started,
        "bwrap: no such source path /var/tmp/apps/pkgs/abc: Permission denied",
    );
    environment.set("TILO_LAUNCHER_BIN", launcher.path());
    let daemon = std::sync::Arc::new(
        crate::device::services::Daemon::start_reporting(&root, &mut |_| {})
            .expect("the daemon starts"),
    );
    let workspace = daemon
        .workspace()
        .expect("the daemon serves the person's workspace");
    // The connection every connector is handed before a reader: what its session start hands
    // it in a real daemon.
    daemon.apps().hand_actions(Box::new(|_, _, _, _| Ok(())));
    let broker = daemon
        .app_action_broker()
        .expect("the daemon runs a broker");
    let revision = || {
        (
            broker.state().published_surfaces_generation(),
            broker
                .state()
                .action_directory_root_page(None, 0)
                .generation,
        )
    };
    let before = revision();
    let mut answers = Vec::new();
    for _ in 0..5 {
        match connection_started(daemon.apps(), &workspace, FAILING) {
            Ok(()) => answers.push("it came up".to_owned()),
            Err(refusal) => answers.push(refusal.reason),
        }
    }
    assert!(
        started.exists(),
        "its start was tried, and failed, before this: {answers:?}"
    );
    assert_eq!(
        revision(),
        before,
        "an app that never came up moved the catalogue's revision"
    );
    assert_eq!(
        daemon.apps().instances.no_view_running(FAILING, &workspace),
        0,
        "nothing of the app ran to publish anything"
    );
}

#[cfg(target_os = "linux")]
#[path = "apps_executor_tests.rs"]
mod executor_tests;
