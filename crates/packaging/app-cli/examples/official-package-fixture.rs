//! Prepare an independently pinned official Calculator fixture; never install it.
use app_cli::{client, config::RegistryConfig};
use serde_json::json;
use std::error::Error;
use std::fs::File;
use std::path::Path;
use tilo_app_package::{
    blobs::{BlobHash, DiscardBlobs},
    tapp,
    verify::{Verification, verify},
};
const OWNER: &str = "55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06";
const APP: &str = "so.tilo.calculator";
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn require(value: bool, message: &str) -> Result<()> {
    if value { Ok(()) } else { Err(message.into()) }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    require(
        args.len() == 2 || args.len() == 3,
        "usage: official-package-fixture OUTPUT [child]",
    )?;
    let output_arg = args.get(1).ok_or("missing output directory")?;
    let output = Path::new(output_arg);
    std::fs::create_dir_all(output)?;
    if args.len() == 2 {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([output_arg.as_os_str(), std::ffi::OsStr::new("child")])
            .env("TILO_STORE_CONFIG_DIR", output.join("private-checkpoint"))
            .status()?;
        return require(status.success(), "canonical fixture preparation failed");
    }
    prepare(output)
}
fn prepare(output: &Path) -> Result<()> {
    let registry = RegistryConfig {
        name: "Tilo main".into(),
        base_url: app_cli::config::DEFAULT_REGISTRY_BASE_URL.into(),
        owner_pubkey_hex: OWNER.into(),
        enabled: true,
        channel: tilo_app_package::version::ReleaseChannel::Stable,
    };
    app_cli::config::save_registries(&app_cli::config::RegistriesFile {
        registries: vec![registry.clone()],
    })?;
    save_snapshot(&registry, output)?;
    let entry = client::fetch_all_versions(&registry, APP)?;
    std::fs::write(
        output.join("verified-version-index.json"),
        serde_json::to_vec_pretty(&entry)?,
    )?;
    let offer = entry
        .entry
        .versions
        .iter()
        .filter(|version| {
            version.target.platform == registry_types::TILO_OS_PLATFORM
                && version.target.architecture == std::env::consts::ARCH
        })
        .max_by(|a, b| tilo_app_package::version::compare(&a.version, &b.version))
        .ok_or("no genuine native Calculator offer for this architecture")?;
    require(
        offer.tapp_size_bytes > 0
            && offer.tapp_size_bytes
                <= tilo_app_package::platform_size_limit(registry_types::TILO_OS_PLATFORM),
        "official app exceeds canonical size boundary",
    )?;
    let full = output.join("calculator-full.tapp");
    let size = app_cli::install::download(&offer.tapp_url, &full)?;
    require(
        size == offer.tapp_size_bytes,
        "official download size differs",
    )?;
    let (archive, _) = BlobHash::of_reader(&mut File::open(&full)?)?;
    require(
        archive.to_hex() == offer.tapp_blake3_hex,
        "official download archive hash differs",
    )?;
    let decoded = tapp::read(File::open(&full)?, &mut DiscardBlobs)?;
    let publisher: [u8; 32] = hex::decode(&entry.entry.publisher_pubkey_hex)?
        .try_into()
        .map_err(|_| "invalid publisher key")?;
    require(
        matches!(
            verify(&decoded.meta, &[publisher]),
            Verification::Trusted { .. }
        ),
        "official package signature refused",
    )?;
    require(
        hex::encode(decoded.meta.id.as_bytes()) == offer.tapp_root_hash_hex
            && decoded.is_complete(),
        "official package root/payload differs",
    )?;
    require(
        decoded.meta.manifest.package.id == APP
            && decoded.meta.manifest.package.version == offer.version
            && decoded.meta.manifest.package.runtime == offer.target.runtime,
        "signed manifest identity/runtime differs from offer",
    )?;
    let thin = output.join("calculator-thin.tapp");
    tapp::write(File::create(&thin)?, &decoded.meta, None)?;
    let thin_package = tapp::read(File::open(&thin)?, &mut DiscardBlobs)?;
    require(
        thin_package.meta.id == decoded.meta.id && thin_package.carried.is_empty(),
        "thin conversion changed signed identity",
    )?;
    let sdk_offer = offer
        .target
        .runtime
        .as_ref()
        .map(|runtime| app_cli::runtime_offer(runtime, &registry.name))
        .transpose()?;
    let receipt = json!({"package_id": APP, "independent_owner": OWNER, "publisher": entry.entry.publisher_pubkey_hex,
        "offer": offer, "full": {"path": full, "size": size, "archive_blake3": archive.to_hex(), "complete": true},
        "thin": {"path": thin, "root": hex::encode(thin_package.meta.id.as_bytes())},
        "required_sdk": offer.target.runtime, "sdk_offer": sdk_offer, "architecture": std::env::consts::ARCH,
        "verification": "canonical-owner-snapshot-and-index/publisher-signature/root/all-payload-blobs"});
    std::fs::write(
        output.join("verified-offer.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    println!(
        "verified official {} {} {}",
        APP,
        offer.version,
        std::env::consts::ARCH
    );
    Ok(())
}

fn save_snapshot(registry: &RegistryConfig, output: &Path) -> Result<()> {
    let snapshot = client::fetch_snapshot(registry)?;
    let bytes = match snapshot {
        registry_types::CatalogueSnapshot::Full(value) => serde_json::to_vec_pretty(&value)?,
        registry_types::CatalogueSnapshot::Compact(value) => serde_json::to_vec_pretty(&value)?,
    };
    std::fs::write(output.join("signed-snapshot.json"), bytes)?;
    Ok(())
}
