//! Ephemeral signed-model witness for the actual default-off image handler.
//! Runs only as mapped root in a one-user namespace, never on the host as root.
use ed25519_dalek::{Signer as _, SigningKey};
use local_model_service::catalog::{
    CATALOG_SIGNATURE_DOMAIN, CatalogPayload, SignedCatalog, VerifiedCatalog, catalog_signing_bytes,
};
use local_model_service::speech::worker;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::process::ExitCode;
use zeroize::Zeroizing;

const SIZE: usize = 16_919_407;
const SHA: &str = "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb";
const REVISION: &str = "b358ddadd89b7a713b5aa131f23032d3cca1b251";

fn fixture(root: &std::path::Path) -> Result<VerifiedCatalog, Box<dyn std::error::Error>> {
    let store = root.join("store");
    fs::create_dir_all(store.join("sha256"))?;
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(store.join("sha256"), fs::Permissions::from_mode(0o700))?;
    let mut bytes = Zeroizing::new(vec![0; SIZE]);
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/review/whistle.cact")?
        .read_exact(&mut bytes)?;
    if hex::encode(Sha256::digest(&bytes)) != SHA {
        return Err("public fixture receipt differs".into());
    }
    fs::write(store.join("sha256").join(SHA), &bytes)?;
    let release = serde_json::from_value(serde_json::json!({
        "id": "whistle-image-public-review", "label": "Public fixture",
        "description": "Isolated test only", "version": REVISION,
        "artifact": {"url": "https://example.invalid/public-fixture", "sha256": SHA, "size_bytes": SIZE},
        "artifact_format": "whistle_cact", "license_id": "Apache-2.0",
        "license_url": "https://example.invalid/LICENSE", "requires_acceptance": true,
        "installed_size_bytes": SIZE, "allowed_runtimes": [], "required_memory_mib": 1024,
        "required_accelerator_mib": 0, "context_window": 0,
        "vision": false, "face_recognition": false, "segmentation": false
    }))?;
    let state: local_model_service::ServiceState = serde_json::from_value(serde_json::json!({
        "models": {"whistle-image-public-review": {"model_sha256": SHA, "status": agent_provider_config::local::LocalModelStatus::Installed, "process": null,
            "in_process": {"revision": REVISION, "license_id": "Apache-2.0", "size_bytes": SIZE,
                "capabilities": ["speech_recognition"], "recorded_at_unix_ms": 1}}},
        "accepted_licenses": {"whistle-image-public-review": {"model_version": REVISION,
            "model_sha256": SHA, "license_id": "Apache-2.0"}},
    }))?;
    fs::write(store.join("state.json"), serde_json::to_vec(&state)?)?;
    // Public synthetic fixture key, private temporary namespace only. It never
    // enters an installed keyring, a person's store or the release catalogue.
    let key = SigningKey::from_bytes(&[5; 32]);
    let payload = CatalogPayload {
        schema: 1,
        models: vec![release],
        runtimes: vec![],
        recommendation: String::new(),
    };
    let signed = SignedCatalog {
        schema: 1,
        signature_domain: CATALOG_SIGNATURE_DOMAIN.into(),
        key_id: "isolated-test-only".into(),
        signature: key
            .sign(&catalog_signing_bytes(&payload)?)
            .to_bytes()
            .to_vec(),
        payload,
    };
    let catalog = root.join("catalog.json");
    let keys = root.join("keys.json");
    fs::write(&catalog, serde_json::to_vec(&signed)?)?;
    fs::write(
        &keys,
        serde_json::to_vec(
            &serde_json::json!({"keys": {"isolated-test-only": hex::encode(key.verifying_key().to_bytes())}}),
        )?,
    )?;
    Ok(VerifiedCatalog::load(&catalog, &keys)?)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let map = fs::read_to_string("/proc/self/uid_map")?;
    let numbers = map
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    // SAFETY: argument-free identity query. Require a single non-root host uid
    // mapped to namespace root; this helper cannot run with host root authority.
    if unsafe { libc::geteuid() } != 0
        || numbers.len() != 3
        || numbers.first() != Some(&0)
        || numbers.get(1).is_none_or(|uid| *uid == 0)
        || numbers.get(2) != Some(&1)
    {
        return Err("isolated mapped-root fixture required".into());
    }
    // SAFETY: fresh standalone single-threaded child; no descriptor owner above
    // the standard pipes survives this preparation boundary.
    unsafe {
        worker::prepare().map_err(|_| "worker preparation failed")?;
    }
    let root = tempfile::tempdir()?;
    let catalog = fixture(root.path())?;
    worker::serve(
        &root.path().join("store"),
        &catalog,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
    )
    .map_err(|_| "image speech review stopped")?;
    Ok(())
}
fn main() -> ExitCode {
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("image speech review stopped");
        ExitCode::FAILURE
    }
}
