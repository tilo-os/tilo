//! Test helper: build a .tapp-tar in memory so integration tests and
//! benchmarks can drive `install::install_tapp` with a real signed package.
//!
//! Mirrors `tilo-app-package-cli::write_tapp`, kept local so this test crate does
//! not depend on the `tilo-app-package-cli` binary crate.

#![allow(
    dead_code,
    reason = "each test and bench uses its own part of these helpers"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, PackageId, SigningIdentity, pack};

/// Same TOML shape as the pkg crate's fixtures.
pub const SAMPLE_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "so.tilo.editor"
version     = "1.4.2"
kind        = "app"
runtime     = "tilo-runtime/2025.06"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[[trust.attested_by]]
attestor    = "tilo-curated/2025.06"
claim       = "deadbeef"

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]
file-chooser = "via-host"

[sandbox.services]
collab      = "required@1"
tilo-fs      = "required@1"
spellcheck  = "optional@1"

[entrypoint]
bin             = "bin/editor"
splash-segments = ["bin/editor[0x0:0x300000]", "share/splash.png"]
splash-budget   = 100

[providers]
mime = ["application/vnd.oasis.opendocument.text"]
ext  = ["odt"]
"#;

pub fn render_template(template: &str, identity_hex: &str) -> String {
    template.replace("<HEX>", identity_hex)
}

pub fn write_app_payload(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::create_dir_all(dir.join("share"))?;
    std::fs::write(dir.join("bin/editor"), b"#!/notreal")?;
    std::fs::write(dir.join("share/splash.png"), b"PNG\x00\x00\x00")?;
    Ok(())
}

pub fn write_app_payload_with_bulk(dir: &std::path::Path, bulk_kib: usize) -> std::io::Result<()> {
    write_app_payload(dir)?;
    // Make the payload measurably-sized without stalling on IO —
    // fill a single "data" blob deterministically. Content is
    // non-repeating (so blob-store dedup can't skip the bytes).
    let bulk_bytes = bulk_kib.saturating_mul(1024);
    let mut bulk = Vec::with_capacity(bulk_bytes);
    for i in 0..bulk_bytes {
        // `i % 256` is always in 0..256, so this never falls back.
        let low_byte = u8::try_from(i % 256).unwrap_or(0);
        bulk.push(low_byte.wrapping_mul(31));
    }
    std::fs::write(dir.join("share/data.bin"), bulk)?;
    Ok(())
}

pub fn build_tapp(payload_kib: usize) -> (Vec<u8>, PackageId) {
    build_tapp_for("so.tilo.editor", payload_kib)
}

/// A package of app `app_id` whose payload carries a unique `payload_kib`
/// KiB blob (seeded by the app id, so two apps share no bulk bytes).
pub fn build_tapp_for(app_id: &str, payload_kib: usize) -> (Vec<u8>, PackageId) {
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    let src = render_template(SAMPLE_APP_MANIFEST_TEMPLATE, &pub_hex)
        .replace("\"so.tilo.editor\"", &format!("{app_id:?}"));
    let manifest = Manifest::from_toml_str(&src).expect("decode manifest");

    let payload_dir = tempfile::tempdir().unwrap();
    write_app_payload_with_bulk(payload_dir.path(), payload_kib).unwrap();
    std::fs::write(payload_dir.path().join("share/owner.txt"), app_id).unwrap();
    let seed = app_id.bytes().fold(0u8, u8::wrapping_add);
    let mut bulk: Vec<u8> = std::fs::read(payload_dir.path().join("share/data.bin"))
        .unwrap()
        .into_iter()
        .map(|byte| byte ^ seed)
        .collect();
    // The seed alone repeats (`bench19` and `bench91` sum alike), so two apps could share this
    // blob: led by its app id, each app's bulk is its own.
    for (byte, id) in bulk.iter_mut().zip(app_id.bytes()) {
        *byte = id;
    }
    std::fs::write(payload_dir.path().join("share/data.bin"), bulk).unwrap();

    let identity = SigningIdentity::from_signing_key(sk);
    let packed = pack(payload_dir.path(), &manifest, &identity).expect("pack");
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).expect("write tapp");
    (bytes, packed.id())
}

/// The Store's connection to the host listening at `socket`, past the
/// launcher's handoff, once the host accepts connections.
pub fn store_client(socket: &std::path::Path) -> package_service::client::BlockingClient {
    client_for(socket, tilo_app_id::apps::STORE)
}

/// A connection the launcher would hand Settings.
pub fn settings_client(socket: &std::path::Path) -> package_service::client::BlockingClient {
    client_for(socket, tilo_app_id::apps::SETTINGS)
}

/// A connection from the person's own terminal (`tilo-install`, `app`).
pub fn person_client(socket: &std::path::Path) -> package_service::client::BlockingClient {
    client_for(socket, package_service::wire::PERSON)
}

fn client_for(socket: &std::path::Path, app_id: &str) -> package_service::client::BlockingClient {
    let deadline = tilo_test_env::hang_deadline();
    let mut stream = loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(socket) {
            break stream;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the package service never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    package_service::wire::write_handoff(&mut stream, app_id).unwrap();
    package_service::client::BlockingClient::from_connected_fd(stream.into()).unwrap()
}

/// The daemon on a socket of its own, on a thread that ends with the test
/// process.
pub struct Harness {
    socket: std::path::PathBuf,
    store_root: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_language_packs(package_service::language_pack::LanguagePackInstalls::new())
    }

    pub fn with_language_packs(
        language_packs: std::sync::Arc<package_service::language_pack::LanguagePackInstalls>,
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("package-service.sock");
        let store_root = tmp.path().join("store");
        let store = std::sync::Arc::new(package_store::PackageStore::at(&store_root));
        // An empty image catalog: the host's own preinstalled packages must
        // not appear in what this store lists.
        let system_dir = tmp.path().join("system");
        std::fs::create_dir_all(&system_dir).unwrap();
        let catalog = std::sync::Arc::new(package_store::Catalog::at(
            &store_root,
            Some(package_store::SystemStore {
                data_dir: system_dir.clone(),
                payload_dir: system_dir,
                interfaces: None,
            }),
        ));
        let bound = socket.clone();
        // `serve` accepts forever; the thread ends with the test process.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                let listener = package_service::transport::bind(&bound).unwrap();
                package_service::transport::serve(listener, store, catalog, 0, language_packs).await
            })
        });
        Self {
            socket,
            store_root,
            _tmp: tmp,
        }
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn store_root(&self) -> &std::path::Path {
        &self.store_root
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
