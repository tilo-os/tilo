use super::*;
use collab_types::caps::{Access, CapOverride, Scope};
use ed25519_dalek::{Signer as _, SigningKey};
use package_store::{Catalog, InstalledPackage, Layout};
use tilo_action_policy::Grantee;
use tilo_app_package::{BlobHash, Collection, SignedRoot};
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
    apps.keep_launch_grants(Arc::new(Granted(Vec::new())));
    apps
}

/// The apps installed in `store`, before the workspace service answers what they are granted.
fn installed(store: &Store, root: &Path, candidates: Vec<PathBuf>) -> Apps {
    Apps::new(
        Some(store.catalog()),
        root,
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
/// person's grants, in place of the service that holds the governance.
struct Granted(Vec<CapOverride>);

impl LaunchGrantsSource for Granted {
    fn launch_grants(&self, _workspace: &str) -> collab_types::caps::LaunchGrants {
        collab_types::caps::LaunchGrants {
            author: false,
            sandbox: self.0.clone(),
        }
    }

    fn serves(&self, workspace: &str) -> bool {
        workspace == WORKSPACE
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
        apps.keep_launch_grants(Arc::new(Granted(vec![granted])));
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

        let err = apps
            .start("so.tilo.tool", WORKSPACE)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("no window"), "{err}");
        let err = apps
            .start("so.tilo.unknown", WORKSPACE)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("not installed"), "{err}");

        let empty = Apps::new(None, &root, PathBuf::new(), Vec::new());
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
        let err = apps
            .start(VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21")
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
        let err = apps.start(VIEWER, WORKSPACE).map(|_| ()).unwrap_err();
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
                "[[resources]]\nkind = \"{kind}.page\"\nlabel = \"Page\"\nversion = 1\n\n\
             [[resources]]\nkind = \"{kind}.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n"
            )
        };
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let keeper = surface("person-keeper");
        store.install(
            KEEPER,
            &manifest(KEEPER, "gui", ""),
            &[("probe", b"binary"), ("surface.toml", keeper.as_bytes())],
        );
        let tampered = store.install(
            TAMPERED,
            &manifest(TAMPERED, "gui", ""),
            &[
                ("probe", b"binary"),
                ("surface.toml", surface("person-tampered").as_bytes()),
            ],
        );
        std::fs::write(
            tampered.payload_root.join("surface.toml"),
            surface("person-forged"),
        )
        .unwrap();
        let declared = |app: &str| tilo_workspace_service::apps::SurfacePersonDocuments.person_resource_types(app);
        assert!(declared(KEEPER).is_empty(), "nothing before it is launched");

        // This builder has no launcher: the launch stops there, after the
        // runtime took the package's surface.
        let apps = apps(&store, &root, vec![root.join("missing/tilo-launcher")]);
        let _ = apps.start(KEEPER, WORKSPACE);
        let _ = apps.start(TAMPERED, WORKSPACE);
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
            .windowless(VIEWER, WORKSPACE, None)
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
            .windowless(VIEWER, WORKSPACE, None)
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
            .windowless(VIEWER, WORKSPACE, Some(first))
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
            .windowless(VIEWER, WORKSPACE, Some(second))
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
            apps.windowless("so.tilo.unknown", WORKSPACE, None).unwrap().is_none(),
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
            .windowless(VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21", None)
            .map(|_| ())
            .unwrap_err();
        assert!(refused.contains("not served by this runtime"), "{refused}");
        assert!(apps.launched.0.lock().unwrap().is_empty(), "nothing started");
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
        let launch = apps.start(&dev.id, WORKSPACE).unwrap();
        assert_eq!(
            apps.attested(launch.pid),
            Some((dev.id.clone(), WORKSPACE.to_owned())),
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
        apps.keep_launch_grants(Arc::new(Granted(vec![to_the_app])));
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
        apps.keep_launch_grants(Arc::new(Granted(vec![to_the_dev])));
        assert_eq!(
            apps.app_grants(&dev_app, WORKSPACE).sandbox.len(),
            1,
            "the grant the person gave the dev build reaches it"
        );
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
        let start = |apps: &Apps| HostConnections::start(apps, WORKSPACE, BRIDGE);

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
            apps.start(BRIDGE, WORKSPACE).is_err(),
            "it has no window to open"
        );
        HostConnections::start(&apps, WORKSPACE, BRIDGE)
            .expect("its connection starts with no window");
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
        HostConnections::start(&apps, WORKSPACE, BRIDGE)
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

/// What a stand-in wrote to `path`, once it wrote it: a launch's own arguments and environment
/// are the launcher's to record, and the process runs on until the test ends.
#[cfg(target_os = "linux")]
fn wait_for(path: &str) -> String {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
    verified::verify_installed(&store.catalog(), &installed()).unwrap();

    // phase: a_changed_payload_file
    std::fs::write(package.payload_root.join("probe"), b"tampered").unwrap();
    let err = verified::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("payload changed"), "{err}");
    std::fs::write(package.payload_root.join("probe"), b"binary").unwrap();

    // phase: a_file_the_package_did_not_ship
    std::fs::write(package.payload_root.join("extra.dylib"), b"?").unwrap();
    let err = verified::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("did not ship"), "{err}");
    std::fs::remove_file(package.payload_root.join("extra.dylib")).unwrap();

    // phase: a_missing_signed_record
    let record = package.manifest_path.parent().unwrap().join("signed-root");
    let signed = std::fs::read(&record).unwrap();
    std::fs::remove_file(&record).unwrap();
    let err = verified::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    let apps = apps(&store, &root, Vec::new());
    let err = apps.start(VIEWER, WORKSPACE).map(|_| ()).unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    assert!(apps.launched.0.lock().unwrap().is_empty());
    std::fs::write(&record, signed).unwrap();
    verified::verify_installed(&store.catalog(), &installed()).unwrap();
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
        assert!(launched.record_showing(pid, VIEWER, WORKSPACE, Shows::Window, None));
        assert!(launched.attest(pid, VIEWER, WORKSPACE));
        assert!(!launched.attest(pid, "so.tilo.other", WORKSPACE));
        assert!(!launched.attest(pid, VIEWER, "another-workspace"));
        assert!(!launched.attest(pid.wrapping_add(100_000), VIEWER, WORKSPACE));

        // A record of an earlier process that held this pid does not attest the
        // one holding it now.
        launched.0.lock().unwrap().get_mut(&pid).unwrap().start_time += 1;
        assert!(!launched.attest(pid, VIEWER, WORKSPACE));
        launched.forget(pid);
        assert!(launched.record_showing(pid, VIEWER, WORKSPACE, Shows::Window, None));

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
        let apps = Apps::new(None, &root, PathBuf::new(), Vec::new());
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn_locked()
            .unwrap();
        let ours = std::process::id();
        assert!(apps.launched.record_showing(ours, VIEWER, WORKSPACE, Shows::Window, None));
        assert!(apps.launched.record_showing(child.id(), VIEWER, WORKSPACE, Shows::Window, None));
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
    // recorded by its pid, so a connection from it is admitted as that app. Only that process
    // is: a process it did not start, below a recorded one included, is no app.
    {
        let root = std::env::temp_dir();
        let apps = Apps::new(None, &root, PathBuf::new(), Vec::new());
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
        // A helper below the app is a process of its own: nothing an app starts inherits what
        // the daemon gave the app.
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

    let collab = UnixListener::bind(root.join(COLLAB_SOCKET)).unwrap();
    let _identity = UnixListener::bind(root.join(IDENTITY_SOCKET)).unwrap();
    let apps = apps(&store, &root, Vec::new());
    let launch = apps.start(VIEWER, WORKSPACE).unwrap();
    assert!(apps.attest_process(launch.pid, VIEWER, WORKSPACE));
    assert!(!apps.attest_process(launch.pid, "so.tilo.other", WORKSPACE));
    // The app's connection is queued, never accepted; closing the listener
    // ends it, and the probe's read with it.
    drop(collab);
    assert_eq!(launch.wait(), Some(0));
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
        .connect(VIEWER, WORKSPACE, 7, Shows::View(visitor.clone()))
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
        .connect(VIEWER, WORKSPACE, 8, Shows::Nothing)
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
        .open_view(&visitor, VIEWER, WORKSPACE, None, None)
        .unwrap_err();
    assert!(refused.contains("no tilolang app"), "{refused}");
    assert!(
        apps.windowless(VIEWER, WORKSPACE, None).unwrap().is_none(),
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

/// A launch starts the package this computer has installed, through the launcher and with the
/// sandbox, grants and program its manifest declares — never a program the caller names.
#[test]
fn a_launch_asks_the_launcher_for_the_installed_packages_own_program() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    let installed = store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
    let asked = root.join("launched");
    // A stand-in launcher: it records what it was asked, reads its launch hold to the end, and
    // ends, as the connector test's does.
    let launcher = tilo_test_env::stand_in(
        root.join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" > '{asked}'\nexec cat >/dev/null\n",
            asked = asked.display()
        ),
    )
    .unwrap();
    let apps = apps(&store, &root, vec![launcher.path().to_owned()]);
    // The windowless start, as a device with no display takes it: the launch is the package's,
    // through the same launcher a window goes through.
    let launch = apps
        .start_showing(VIEWER, WORKSPACE, Shows::Nothing, None)
        .expect("the installed app starts");
    let _ = launch.wait();
    let asked = std::fs::read_to_string(&asked).unwrap();
    assert!(
        asked.starts_with("run ")
            && asked.contains(&format!("--manifest {}", installed.manifest_path.display()))
            && asked.contains(&format!("--payload-root {}", installed.payload_root.display()))
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
}
