//! Exact optional-runtime repair. The installed signed declaration, not Store
//! input or catalogue display metadata, admits the consuming application.

use serde::{Deserialize, Serialize};

/// Actual bounded installer progress; installation has no invented fraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeProgress {
    pub installing: bool,
    pub received: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOffer {
    pub registry: String,
    pub package_id: String,
    pub version: String,
    pub root: String,
    pub publisher: String,
    pub archive_hash: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReview {
    pub consumer_app: String,
    pub consumer_root: String,
    pub runtime: String,
    /// None means the exact signed bundle is verified locally, without fetching.
    pub offer: Option<RuntimeOffer>,
}

#[cfg(feature = "service")]
impl RuntimeOffer {
    fn from_offer(offer: &app_cli::CatalogueApp) -> Self {
        Self {
            registry: offer.registry.clone(),
            package_id: offer.package_id.clone(),
            version: offer.version.clone(),
            root: offer.tapp_root_hash_hex.clone(),
            publisher: offer.publisher_pubkey_hex.clone(),
            archive_hash: offer.tapp_blake3_hex.clone(),
            size_bytes: offer.tapp_size_bytes,
        }
    }
}

#[cfg(feature = "service")]
fn admit_consumer(
    catalog: &package_store::Catalog,
    app: &str,
    root: tilo_app_package::PackageId,
    runtime: &str,
) -> Result<(), String> {
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 128
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    };
    let Some((name, version)) = runtime.split_once('/') else {
        return Err("invalid exact runtime reference".into());
    };
    if name != "tilo-runtime" || !valid_part(version) || app.len() > 256 {
        return Err("invalid exact runtime reference or consuming application".into());
    }
    let package = catalog
        .find(app)
        .map_err(|e| e.to_string())?
        .ok_or("consuming application is not installed")?;
    if package.package_id() != Some(root) {
        return Err("consuming application was replaced; review again".into());
    }
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(&package),
        &package.id,
        &root.to_hex(),
    )
    .map_err(|e| format!("consuming application integrity: {e}"))?;
    if !hex::encode(record.root.publisher_identity).eq_ignore_ascii_case(&package.publisher_key_hex)
    {
        return Err("consuming application publisher differs from installed admission".into());
    }
    let current = std::fs::read(&package.manifest_path).map_err(|e| e.to_string())?;
    if current != record.manifest_bytes {
        return Err("consuming application manifest was replaced".into());
    }
    let text = std::str::from_utf8(&record.manifest_bytes).map_err(|e| e.to_string())?;
    let manifest = tilo_app_package::Manifest::from_toml_str(text).map_err(|e| e.to_string())?;
    if manifest.package.kind != tilo_app_package::id::Kind::App
        || manifest.package.id != app
        || manifest.package.version != package.version
        || !manifest
            .package
            .optional_runtimes
            .iter()
            .any(|declared| declared == runtime)
    {
        return Err("installed signed application does not declare this optional runtime".into());
    }
    Ok(())
}

#[cfg(feature = "service")]
pub(crate) fn inspect(
    catalog: &package_store::Catalog,
    app: &str,
    root: tilo_app_package::PackageId,
    runtime: &str,
) -> Result<RuntimeReview, String> {
    admit_consumer(catalog, app, root, runtime)?;
    let offer = if app_cli::runtime_installed(runtime) {
        None
    } else {
        let configured = app_cli::config::load_registries();
        let mut selected = None;
        let mut failures = Vec::new();
        for registry in configured.enabled() {
            match app_cli::runtime_offer(runtime, &registry.name) {
                Ok(Some(offer)) => {
                    selected = Some(RuntimeOffer::from_offer(&offer));
                    break;
                }
                Ok(None) => {}
                Err(error) => failures.push(error),
            }
        }
        Some(selected.ok_or_else(|| {
            format!(
                "exact optional runtime is unavailable from configured repositories: {}",
                failures.join("; ")
            )
        })?)
    };
    Ok(RuntimeReview {
        consumer_app: app.into(),
        consumer_root: root.to_hex(),
        runtime: runtime.into(),
        offer,
    })
}

#[cfg(feature = "service")]
pub(crate) fn acquire(
    catalog: &package_store::Catalog,
    review: &RuntimeReview,
    progress: &mut dyn FnMut(app_cli::install::DownloadProgress),
) -> Result<(), String> {
    let root = format!("tapp:{}", review.consumer_root)
        .parse()
        .map_err(|_| "invalid consuming root")?;
    admit_consumer(catalog, &review.consumer_app, root, &review.runtime)?;
    if app_cli::runtime_installed(&review.runtime) {
        return Ok(());
    }
    let expected = review
        .offer
        .as_ref()
        .ok_or("runtime is no longer present; review again")?;
    // runtime_offer accepts only an enabled, host-configured repository and
    // verifies its owner/publisher proofs. No URL/key from the review is used.
    let offer = app_cli::runtime_offer(&review.runtime, &expected.registry)?
        .ok_or("runtime offer was withdrawn; review again")?;
    if RuntimeOffer::from_offer(&offer) != *expected {
        return Err("runtime offer changed; review again before acquiring".into());
    }
    // Recheck the consumer after network metadata IO and before payload IO.
    admit_consumer(catalog, &review.consumer_app, root, &review.runtime)?;
    app_cli::install::acquire_admitted_runtime_reporting(&offer, &review.runtime, progress)
        .map_err(|e| e.to_string())
}
