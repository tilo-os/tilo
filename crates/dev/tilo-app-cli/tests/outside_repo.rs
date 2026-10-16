//! The journey an external developer takes, from an empty directory with no
//! Tilo workspace around it to a release waiting for the registry owner's
//! review: `tilo-app new`, `check`, `test`, `build`, `package` and `publish`.
//!
//! `tilo-app new` must carry everything Cargo only reads from the root
//! workspace (the SDK's `[patch.crates-io]`, the toolchain, a lock file with
//! tested versions); the app's own tests must drive it headlessly, its
//! accessibility and action audit must catch a control bound to nothing, and
//! the signed package must verify and go through `apply`, owner approval and
//! `publish` into a registry's review queue without becoming public.
//!
//! The registry is a real `registry-server` on loopback, run in this process
//! so the owner's approval and the namespace proof need no account or DNS:
//! the only parts of publishing to `apps.tilo.so` this does not exercise are
//! the public DNS lookup and the owner's review itself.
//!
//! It is a cold build of the whole SDK graph in a fresh directory, so it is
//! ignored by default. CI's SDK lane runs it:
//!
//! ```sh
//! cargo test -p tilo-app-cli --test it outside_repo -- --ignored --nocapture
//! ```
//!
//! `TILO_SDK_SOURCE=git` takes the SDK from this checkout as a git
//! repository (`file://`, pinned to `HEAD`) instead of by path: the form a
//! developer without a checkout uses. `HEAD` must be committed.

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

/// A throwaway publisher key: the hex seed `tilo-app` reads from
/// `$TILO_PUBLISHER_KEY`, so no machine secret store is touched.
#[cfg(test)]
const PUBLISHER_SEED: &str = "0909090909090909090909090909090909090909090909090909090909090909";

#[cfg(test)]
const APP_ID: &str = "com.example.hello";

#[test]
#[ignore = "cold build of the SDK graph outside the workspace; run in CI's SDK lane"]
fn a_generated_app_goes_from_new_to_a_release_awaiting_review() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("checkout root")
        .canonicalize()
        .expect("canonical checkout");
    // A fixed directory, not a random one: the compiler cache keys every
    // unit by its working directory and build-script output paths, so a new
    // path each run would rebuild the whole graph uncached.
    let parent = OutsideDir::create(std::env::temp_dir().join("tilo-outside-repo"));
    let parent_path = parent.0.canonicalize().expect("canonical temp dir");
    assert!(
        !parent_path.starts_with(&checkout),
        "the temporary directory must be outside the checkout"
    );
    let app = parent_path.join("hello");
    scaffold(&checkout, &parent_path, &app);

    let host = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    // The full gate the generated CI's first job runs: structure, format,
    // Clippy, the app's tests (undo, persistence, a viewer's refused edit,
    // collaboration, the native contract), certification of the production
    // root without a window, and the audited headless scenario.
    run(tilo_app(&app).arg("check"), "tilo-app check");
    an_unbound_control_fails_the_audit(&app);
    run(
        tilo_app(&app).args(["build", "--target", host]).arg(&app),
        "tilo-app build (native)",
    );
    let built = app.join("target/tilo-app").join(host).join("hello");
    assert!(built.is_dir(), "no artifact under {}", built.display());
    run(
        tilo_app(&app).args(["check", "--target", "web"]).arg(&app),
        "tilo-app check --target web (wasm32)",
    );

    let package = package(&app);
    let public_key = verify_package(&app, &package);
    publish_for_review(&app, &package, &public_key);
}

/// `tilo-app new` from the checkout, by path or as a git repository.
#[cfg(test)]
fn scaffold(checkout: &Path, parent: &Path, app: &Path) {
    let mut new = tilo_app(parent);
    new.args(["new", "hello", "--id", APP_ID, "--title", "Hello"])
        .args(["--template", "starter", "--directory"])
        .arg(app);
    // `--sdk-git` and `--ui-path` are alternative SDK sources.
    if std::env::var("TILO_SDK_SOURCE").as_deref() == Ok("git") {
        new.arg("--sdk-git")
            .arg(format!("file://{}", checkout.display()));
    } else {
        new.arg("--ui-path")
            .arg(checkout.join("crates/sdk/ui-core"));
    }
    run(&mut new, "tilo-app new");

    let manifest = std::fs::read_to_string(app.join("Cargo.toml")).expect("generated manifest");
    assert!(manifest.contains("[patch.crates-io]"), "{manifest}");
    assert!(app.join("rust-toolchain.toml").is_file());
    assert!(app.join("Cargo.lock").is_file());
}

/// A button with an imperative callback and no action behind it is
/// invisible to scripts, agents and assistive tools: `tilo-app test` must
/// fail on it and name it, and pass again once it is gone.
#[cfg(test)]
fn an_unbound_control_fails_the_audit(app: &Path) {
    let source = app.join("src/lib.rs");
    let original = std::fs::read_to_string(&source).expect("app source");
    let anchor = "            clear,\n        ])";
    assert!(
        original.contains(anchor),
        "the starter's view changed shape; update this test's anchor:\n{original}"
    );
    let unbound = original.replace(
        anchor,
        "            clear,\n            Button::new(\"forget\", \"starter-clear\").on_click(|| {}),\n        ])",
    );
    std::fs::write(&source, unbound).expect("write the unbound control");
    let output = tilo_app(app)
        .arg("test")
        .output()
        .expect("run tilo-app test");
    std::fs::write(&source, &original).expect("restore the app source");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "tilo-app test passed with a control bound to nothing:\n{text}"
    );
    assert!(
        text.contains("initial_native_app_contract_is_certified")
            && text.contains("\"forget\"")
            && text.contains("legacy-callback"),
        "the failure must be the audit naming the control:\n{text}"
    );
}

/// `tilo-app package` for this host: the Tilo OS thin app on Linux (what a
/// bare `tilo-app package` builds), the macOS build on macOS.
///
/// The thin package is signed with the throwaway key and never shipped, so
/// it is optimized lightly, as CI's other unshipped thin packages are. The
/// macOS build keeps the release profile: the toolkit is linked into it, and
/// only that profile keeps it under the platform's size limit.
#[cfg(test)]
fn package(app: &Path) -> PathBuf {
    let mut package = tilo_app(app);
    package.arg("package");
    if cfg!(target_os = "macos") {
        package.args(["--platform", "macos"]);
    }
    package
        .env("CARGO_PROFILE_THIN_APP_OPT_LEVEL", "1")
        .env("CARGO_PROFILE_THIN_APP_CODEGEN_UNITS", "16")
        .env("CARGO_PROFILE_THIN_APP_DEBUG", "0");
    run(&mut package, "tilo-app package");
    let mut packages: Vec<PathBuf> = std::fs::read_dir(app.join("dist"))
        .expect("tilo-app package writes dist/")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "tapp")
        })
        .collect();
    assert_eq!(packages.len(), 1, "one package in dist/: {packages:?}");
    packages.pop().expect("the package")
}

/// The package decodes completely, carries every payload blob, is signed by
/// the publisher key, and names the app. A desktop build carries the toolkit,
/// so its executable is unpacked and certified headlessly as it ships; a thin
/// Tilo OS build links the toolkit only the OS provides. Returns the key's
/// public half.
#[cfg(test)]
fn verify_package(app: &Path, package: &Path) -> String {
    let public_key = json_result(
        tilo_app(app).args(["keys", "show", "--json"]),
        "tilo-app keys show",
    )["public_key"]
        .as_str()
        .expect("public key")
        .to_owned();
    let file = std::fs::File::open(package).expect("open the package");
    let mut blobs = std::collections::BTreeMap::new();
    let tapp = tilo_app_package::tapp::read(std::io::BufReader::new(file), &mut blobs)
        .expect("the package decodes and its hashes hold");
    assert!(
        tapp.missing().is_empty(),
        "a full package carries its payload"
    );
    let key: [u8; 32] = hex::decode(&public_key)
        .expect("hex key")
        .try_into()
        .expect("32-byte key");
    match tilo_app_package::verify(&tapp.meta, &[key]) {
        tilo_app_package::Verification::Trusted { manifest } => {
            assert_eq!(manifest.package.id, APP_ID);
        }
        other => panic!("the package does not verify under its publisher key: {other:?}"),
    }
    if cfg!(target_os = "macos") {
        let unpacked = app.join("target/unpacked");
        let _ = std::fs::remove_dir_all(&unpacked);
        tilo_app_package::unpack(&tapp.meta, &blobs, &unpacked).expect("unpack the payload");
        let executable = unpacked.join(tapp.meta.executable_path().expect("an entrypoint"));
        let audit = Command::new(&executable)
            .arg("--native-audit")
            .output()
            .expect("run the packaged executable");
        let report = String::from_utf8_lossy(&audit.stdout);
        assert!(
            audit.status.success(),
            "the packaged app fails its headless audit:\n{report}\n{}",
            String::from_utf8_lossy(&audit.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_str(&report).expect("the audit is a JSON report");
        assert_eq!(report["issues"], serde_json::json!([]), "{report}");
    }
    public_key
}

/// `apply`, the owner's approval, `publish`: the release reaches the review
/// queue and nothing is public.
#[cfg(test)]
fn publish_for_review(app: &Path, package: &Path, public_key: &str) {
    let registry = registry::Local::start(&app.join("target/registry"));
    let url = registry.url();
    let publish = || {
        let mut publish = tilo_app(app);
        publish.args(["publish", "--json", "--registry", &url]);
        publish
    };

    let refused = publish().output().expect("run tilo-app publish");
    assert!(
        !refused.status.success(),
        "a key with no approved namespace published"
    );

    registry.prove("_tilo-publisher.example.com", public_key);
    let applied = json_result(
        tilo_app(app).args([
            "apply",
            "--json",
            "--domain",
            "example.com",
            "--contact",
            "dev@example.com",
            "--registry",
            &url,
        ]),
        "tilo-app apply",
    );
    assert_eq!(applied["status"], "pending", "{applied}");
    registry.approve(public_key);
    let status = json_result(
        tilo_app(app).args(["apply", "--status", "--json", "--registry", &url]),
        "tilo-app apply --status",
    );
    assert_eq!(status["status"], "approved", "{status}");

    let published = json_result(&mut publish(), "tilo-app publish");
    let builds = published["published"].as_array().expect("published builds");
    let [build] = builds.as_slice() else {
        panic!("one build published: {published}");
    };
    assert_eq!(build["package_id"], APP_ID, "{published}");
    assert_eq!(build["entry"], "add", "{published}");
    assert_eq!(
        build["package"].as_str().map(Path::new),
        Some(package),
        "publish takes what dist/ holds: {published}"
    );
    registry.awaits_review(APP_ID, &std::fs::read(package).expect("package bytes"));
}

/// `tilo-app` run in `dir` with the throwaway publisher key and no inherited
/// target directory: the app has its own.
#[cfg(test)]
fn tilo_app(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-app"));
    command
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("TILO_REGISTRY")
        .env("TILO_PUBLISHER_KEY", PUBLISHER_SEED);
    command
}

#[cfg(test)]
fn run(command: &mut Command, what: &str) {
    eprintln!("[outside-repo] {what}: {command:?}");
    let status = command.status().expect(what);
    assert!(status.success(), "{what} failed with {status}");
}

/// The `result` of a `--json` command that must succeed; stdout holds only
/// that one document.
#[cfg(test)]
fn json_result(command: &mut Command, what: &str) -> serde_json::Value {
    eprintln!("[outside-repo] {what}: {command:?}");
    let output = command.output().expect(what);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{what} failed with {}:\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut document: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("{what}: stdout is not one JSON document ({error}):\n{stdout}")
    });
    assert_eq!(document["ok"], true, "{document}");
    document["result"].take()
}

/// A directory removed before use and when the test ends.
#[cfg(test)]
struct OutsideDir(PathBuf);

#[cfg(test)]
impl OutsideDir {
    fn create(path: PathBuf) -> Self {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove {}: {error}", path.display()),
        }
        std::fs::create_dir_all(&path).expect("outside directory");
        Self(path)
    }
}

#[cfg(test)]
impl Drop for OutsideDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod registry {
    use std::collections::BTreeMap;
    use std::io::{Read as _, Write as _};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use registry_server::applications::{self, ProofSource};
    use registry_server::serve::ServeOptions;
    use registry_server::{AppState, ServerIdentity, Storage, bind_and_run};

    /// The DNS TXT records a namespace proof reads, as the owner of
    /// `example.com` would publish them.
    #[derive(Clone, Default)]
    struct Records(Arc<Mutex<BTreeMap<String, Vec<String>>>>);

    impl ProofSource for Records {
        fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
            Ok(self
                .0
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .get(name)
                .cloned()
                .unwrap_or_default())
        }

        fn https_text(&self, _url: &str) -> Result<String, String> {
            Err("404".into())
        }
    }

    /// A registry on loopback, served from a thread of its own until the
    /// test process ends.
    pub struct Local {
        address: SocketAddr,
        storage: Storage,
        records: Records,
    }

    impl Local {
        pub fn start(dir: &Path) -> Self {
            let _ = std::fs::remove_dir_all(dir);
            let storage = Storage::open(dir).expect("registry storage");
            let records = Records::default();
            let state = AppState::with_proof_source(
                storage.clone(),
                ServerIdentity {
                    // The owner's key only signs what review approves.
                    owner_pubkey_hex: "01".repeat(32),
                    name: "journey registry".into(),
                    description: "a registry the developer journey publishes to".into(),
                    base_url: "https://apps.example".into(),
                },
                ServeOptions::default(),
                Arc::new(records.clone()),
            );
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("registry runtime");
                runtime.block_on(async move {
                    let bound =
                        bind_and_run(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), state)
                            .await
                            .expect("bind the registry");
                    sender.send(bound).expect("report the address");
                    std::future::pending::<()>().await;
                });
            });
            let address = receiver.recv().expect("the registry starts");
            Self {
                address,
                storage,
                records,
            }
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.address)
        }

        /// Publish `tilo-publisher=<key>` at the TXT record `name`.
        pub fn prove(&self, name: &str, public_key: &str) {
            self.records.0.lock().expect("records").insert(
                name.to_owned(),
                vec![registry_types::application::proof_token(public_key)],
            );
        }

        /// The owner approves the key's application, on the server.
        pub fn approve(&self, public_key: &str) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs();
            applications::approve(&self.storage, public_key, "owner", now, &self.records)
                .expect("the owner approves the application");
        }

        /// One request waits for review, naming exactly `package`; the
        /// catalogue does not list the app and the upload is not served.
        pub fn awaits_review(&self, package_id: &str, package: &[u8]) {
            let pending = self.storage.load_pending_submissions().expect("pending");
            let [submission] = pending.as_slice() else {
                panic!("expected one request awaiting review, got {pending:?}");
            };
            assert_eq!(submission.change.entry.package_id, package_id);
            let reviewed = registry_server::review_entry_bytes(
                &self.storage,
                &submission.change.entry,
                registry_server::InspectionPolicy::default(),
            )
            .expect("review reads the uploaded package");
            assert!(
                reviewed
                    .iter()
                    .any(|(_, bytes)| bytes.as_slice() == package),
                "review reads the exact package dist/ holds"
            );
            let client = registry_client::RegistryClient::new(&self.url()).expect("client");
            assert!(
                client
                    .entry(package_id)
                    .expect("read the catalogue")
                    .is_none(),
                "nothing is public before the owner reviews it"
            );
            let digest = blake3::hash(package).to_hex();
            assert_eq!(
                self.status(&format!("/artifacts/{digest}.tapp")),
                404,
                "the upload is not served before review"
            );
        }

        /// The HTTP status an anonymous GET of `path` answers with.
        fn status(&self, path: &str) -> u16 {
            let mut stream = TcpStream::connect(self.address).expect("connect");
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                self.address
            )
            .expect("request");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .unwrap_or_else(|| panic!("no HTTP status in {response:?}"))
        }
    }
}
