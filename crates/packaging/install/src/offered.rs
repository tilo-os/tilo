//! `tilo-install update` from a registry: the store client resolves the
//! package the way `app update` does, and the download is held to the offer.

use std::fmt::Write as _;

use super::{Error, TrustFlags};

/// A package a configured registry offers, downloaded and checked against
/// the signed offer, with what `check` must then hold it to.
pub(super) struct Offered {
    pub(super) staged: app_cli::install::StagedPackage,
    pub(super) source: String,
    pub(super) trust: TrustFlags,
    pub(super) payload: Option<app_registry::HttpBlobs>,
}

/// Resolve `package_id` the way `app update` does — through the store's
/// `registries.toml` and its verified catalogue — and download the offer.
///
/// The download is checked against the signed registry entry (length and
/// BLAKE3); `check` then also holds the package to the entry's publisher and
/// package root, on top of the trust flags the user passed.
pub(super) fn fetch_from_registries(
    package_id: &str,
    version_req: Option<&str>,
    trust: &TrustFlags,
) -> Result<Offered, Error> {
    let fetch_error = |detail: String| Error::Fetch {
        app_id: package_id.to_string(),
        detail,
    };
    let listing = app_cli::all_offers();
    let unreachable = || {
        listing
            .unreachable
            .iter()
            .fold(String::new(), |mut out, registry| {
                let _ = write!(out, "; {} unreachable: {}", registry.name, registry.detail);
                out
            })
    };
    let newest = listing
        .apps
        .iter()
        .find(|app| app.package_id == package_id)
        .ok_or_else(|| fetch_error(format!("no configured registry offers it{}", unreachable())))?;
    let offer = match version_req {
        None => newest.clone(),
        Some(req) => {
            let req = semver::VersionReq::parse(req)
                .map_err(|error| Error::Policy(format!("--version {req}: {error}")))?;
            app_cli::version_offers(package_id, &newest.registry)
                .map_err(fetch_error)?
                .into_iter()
                .find(|offer| semver::Version::parse(&offer.version).is_ok_and(|v| req.matches(&v)))
                .ok_or_else(|| {
                    fetch_error(format!(
                        "{} offers no version matching {req}",
                        newest.registry
                    ))
                })?
        }
    };
    let pinned = |flag: &str, given: Option<&String>, offered: &str| match given {
        Some(given) if !given.eq_ignore_ascii_case(offered) => Err(Error::Policy(format!(
            "{flag} {given} differs from the {offered} {} offers",
            offer.registry
        ))),
        _ => Ok(Some(offered.to_string())),
    };
    let trust = TrustFlags {
        require_package_root: pinned(
            "--require-package-root",
            trust.require_package_root.as_ref(),
            &offer.tapp_root_hash_hex,
        )?,
        require_publisher: pinned(
            "--require-publisher",
            trust.require_publisher.as_ref(),
            &offer.publisher_pubkey_hex,
        )?,
        ..trust.clone()
    };
    let staged = app_cli::install::stage(&offer, &mut |_| {})
        .map_err(|error| fetch_error(error.user_message()))?;
    Ok(Offered {
        staged,
        source: format!("{} (registry {})", offer.version, offer.registry),
        trust,
        payload: app_cli::install::payload_source(&offer).map(app_registry::HttpBlobs::new),
    })
}
