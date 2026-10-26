//! Resolving an installed package's update through the configured registries.
//!
//! The catalogue is the one the Store and `app update` read: owner-signed
//! snapshots and version indexes from `app_cli`, with the person's channel and
//! this host's architecture deciding which signed build is offered. Payload
//! blobs come from the registry that served the entry, over HTTPS or from its
//! directory.

use app_registry::RegistryBlobs;

use crate::{Error, TrustFlags};

/// The person's trust flags plus the exact root and publisher the signed
/// catalogue entry names, for checking the bytes the registry served.
///
/// An explicit pin on the command line is never replaced: a different one is
/// refused rather than silently narrowed to the registry's answer.
pub(crate) fn trust_named_by_registry(
    request: &TrustFlags,
    fetched: &FetchedPackage,
) -> Result<TrustFlags, Error> {
    let mut trust = request.clone();
    match &trust.require_publisher {
        Some(required) if !required.eq_ignore_ascii_case(&fetched.publisher_hex) => {
            return Err(Error::PublisherMismatch {
                required: required.clone(),
                found: fetched.publisher_hex.clone(),
            });
        }
        Some(_) => {}
        None => trust.require_publisher = Some(fetched.publisher_hex.clone()),
    }
    match &trust.require_package_root {
        Some(required) if !required.eq_ignore_ascii_case(&fetched.package_root_hex) => {
            return Err(Error::PackageRootMismatch {
                required: required.clone(),
                found: fetched.package_root_hex.clone(),
            });
        }
        Some(_) => {}
        None => trust.require_package_root = Some(fetched.package_root_hex.clone()),
    }
    Ok(trust)
}

/// A package a configured registry offered, with the signed identities the
/// installer checks it against and where its payload blobs come from.
pub(crate) struct FetchedPackage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) source: String,
    pub(crate) payload: RegistryBlobs,
    pub(crate) package_root_hex: String,
    pub(crate) publisher_hex: String,
}

/// Resolve `app_id` through the configured registries, newest first, and
/// download the chosen build.
///
/// Every registry is read through the same owner-signed snapshot and version
/// index the Store and `app update` use, on the person's configured channel.
/// A registry that cannot be read, or does not list the package, is skipped so
/// a later one can serve it; bytes that fail the signed entry's length, BLAKE3
/// or package identity abort the update rather than falling back.
pub(crate) fn fetch_from_registries(
    app_id: &str,
    version_req: Option<&str>,
) -> Result<FetchedPackage, Error> {
    let requirement = version_req
        .map(semver::VersionReq::parse)
        .transpose()
        .map_err(|error| Error::Fetch {
            app_id: app_id.to_string(),
            detail: format!("bad version requirement: {error}"),
        })?;
    let registries = app_cli::config::load_registries();
    let mut first_error = None;
    for registry in registries.enabled() {
        let verified = match app_cli::client::fetch_all_versions(registry, app_id) {
            Ok(verified) => verified,
            Err(error) => {
                first_error.get_or_insert_with(|| error.to_string());
                continue;
            }
        };
        let offers = app_cli::offers_for_entry(&verified, &registry.name, registry.channel);
        let matching: Vec<&app_cli::CatalogueApp> = offers
            .iter()
            .filter(|offer| {
                requirement.as_ref().is_none_or(|requirement| {
                    semver::Version::parse(&offer.version)
                        .is_ok_and(|version| requirement.matches(&version))
                })
            })
            .collect();
        let Some(newest) = matching.first().copied() else {
            continue;
        };
        // A device that lacks the exact runtime an app links would download it
        // only to refuse it. Prefer a newer signed build it can run now; the
        // next `app update` moves it on with its runtime.
        let chosen = app_cli::prefer_installed_runtime(
            matching.iter().copied(),
            &newest.version,
            app_cli::runtime_installed,
        )
        .unwrap_or(newest);
        let bytes = app_cli::install::verified_bytes(chosen, &mut |_| {}).map_err(|error| {
            Error::Fetch {
                app_id: app_id.to_string(),
                detail: error.user_message(),
            }
        })?;
        let payload = RegistryBlobs::parse(&registry.base_url)
            .map_err(|error| Error::PayloadSource(format!("{}: {error}", registry.name)))?;
        return Ok(FetchedPackage {
            bytes,
            source: format!("{} (registry {})", chosen.version, registry.name),
            payload,
            package_root_hex: chosen.tapp_root_hash_hex.clone(),
            publisher_hex: chosen.publisher_pubkey_hex.clone(),
        });
    }
    Err(Error::Fetch {
        app_id: app_id.to_string(),
        detail: first_error.map_or_else(
            || format!("no configured registry offers {app_id} for this host"),
            |first| format!("no configured registry offers {app_id} for this host ({first})"),
        ),
    })
}

/// Where `--payload-from` names payload blobs: an HTTP(S) registry server or a
/// registry directory on this machine.
pub(crate) fn payload_blobs(raw: &str) -> Result<app_registry::RegistryBlobs, Error> {
    RegistryBlobs::parse(raw).map_err(|error| Error::PayloadSource(error.to_string()))
}
