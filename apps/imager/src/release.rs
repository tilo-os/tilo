//! Trusted qOS release catalog and verified medium download/cache.
//!
//! The catalog is embedded into each platform-signed Imager package at build
//! time.  A release therefore cannot substitute its URL or digest without also
//! replacing the signed host application.  Both the compressed transport and
//! expanded disk image are SHA-256 checked before the image becomes selectable.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{QosRelease, Source};

const BUNDLED_CATALOG: &str = include_str!("../release-catalog.json");

#[derive(Debug, Deserialize)]
struct Catalog {
    schema: u32,
    tag: String,
    releases: Vec<CatalogRelease>,
}

#[derive(Debug, Deserialize)]
struct CatalogRelease {
    name: String,
    version: String,
    channel: String,
    arch: String,
    url: String,
    archive_sha256: String,
    archive_size: u64,
    image_sha256: String,
    image_size: u64,
}

/// Releases authenticated by the signature on this Imager build, filtered to
/// media bootable by the current host architecture.
///
/// # Errors
///
/// Returns an error when the embedded catalog is malformed or contains an
/// unsupported schema, untrusted URL, invalid digest, or invalid size.
pub fn bundled_releases() -> Result<Vec<QosRelease>, String> {
    let catalog: Catalog = serde_json::from_str(BUNDLED_CATALOG)
        .map_err(|error| format!("The bundled release catalog is invalid: {error}"))?;
    if catalog.schema != 1 {
        return Err("The bundled release catalog uses an unsupported schema.".into());
    }
    if catalog.releases.is_empty() && catalog.tag == "unreleased" {
        return Ok(Vec::new());
    }
    if !catalog.tag.starts_with("imager-v") {
        return Err("The bundled release catalog has an invalid release tag.".into());
    }
    let host_arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return Ok(Vec::new()),
    };
    catalog
        .releases
        .into_iter()
        .filter(|release| release.arch == host_arch)
        .map(|release| {
            validate_digest(&release.archive_sha256)?;
            validate_digest(&release.image_sha256)?;
            if release.archive_size == 0 || release.image_size == 0 {
                return Err("The release catalog contains an invalid installer size.".into());
            }
            let url = reqwest::Url::parse(&release.url)
                .map_err(|_| "The release catalog contains an invalid URL.".to_string())?;
            if url.scheme() != "https" || url.host_str() != Some("code.podesta.ai") {
                return Err("The release catalog contains an untrusted download origin.".into());
            }
            Ok(QosRelease {
                name: release.name,
                version: release.version,
                size: release.image_size,
                channel: release.channel,
                local_path: String::new(),
                url: release.url,
                archive_sha256: release.archive_sha256,
                archive_size: release.archive_size,
                image_sha256: release.image_sha256,
            })
        })
        .collect()
}

fn validate_digest(digest: &str) -> Result<(), String> {
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("The release catalog contains an invalid SHA-256 digest.".into())
    }
}

fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("qos-imager")
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Could not open downloaded data: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not verify downloaded data: {error}"))?;
        if read == 0 {
            break;
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound downloaded data while verifying it.".to_string())?;
        hash.update(chunk);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verified_image(release: &QosRelease) -> Result<Source, String> {
    validate_digest(&release.archive_sha256)?;
    validate_digest(&release.image_sha256)?;
    if release.archive_size == 0 || release.size == 0 {
        return Err("The qOS installer has invalid download metadata.".into());
    }
    let root = cache_root().join(&release.image_sha256);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create the download cache: {error}"))?;
    let image = root.join("qos-installer.img");
    if image.is_file() {
        if sha256(&image)? == release.image_sha256 {
            return Ok(source_for(release, &image));
        }
        fs::remove_file(&image)
            .map_err(|error| format!("Could not replace an invalid cached installer: {error}"))?;
    }

    let archive_part = root.join("qos-installer.img.zst.part");
    let image_part = root.join("qos-installer.img.part");
    let response = reqwest::blocking::Client::builder()
        .user_agent("qOS-Imager/1")
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| format!("Could not initialize the downloader: {error}"))?
        .get(&release.url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("Could not download the qOS installer: {error}"))?;
    let final_url = response.url();
    if final_url.scheme() != "https" || final_url.host_str() != Some("code.podesta.ai") {
        return Err("The release download redirected to an untrusted origin.".into());
    }
    if response
        .content_length()
        .is_some_and(|size| size != release.archive_size)
    {
        return Err("The qOS installer download has an unexpected size.".into());
    }
    let mut archive = File::create(&archive_part)
        .map_err(|error| format!("Could not create the download cache: {error}"))?;
    let downloaded = std::io::copy(
        &mut response.take(release.archive_size.saturating_add(1)),
        &mut archive,
    )
    .map_err(|error| format!("The qOS installer download was interrupted: {error}"))?;
    archive
        .flush()
        .map_err(|error| format!("Could not save the qOS installer: {error}"))?;
    if downloaded != release.archive_size || sha256(&archive_part)? != release.archive_sha256 {
        let _ = fs::remove_file(&archive_part);
        return Err("The downloaded qOS installer did not match its signed catalog.".into());
    }

    let compressed = File::open(&archive_part)
        .map_err(|error| format!("Could not open the verified installer: {error}"))?;
    let decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|error| format!("Could not unpack the qOS installer: {error}"))?;
    let mut expanded = File::create(&image_part)
        .map_err(|error| format!("Could not create the expanded installer: {error}"))?;
    let expanded_size = std::io::copy(
        &mut decoder.take(release.size.saturating_add(1)),
        &mut expanded,
    )
    .map_err(|error| format!("Could not unpack the qOS installer: {error}"))?;
    expanded
        .flush()
        .map_err(|error| format!("Could not save the expanded installer: {error}"))?;
    if expanded_size != release.size
        || expanded.metadata().map_or(0, |meta| meta.len()) != release.size
        || sha256(&image_part)? != release.image_sha256
    {
        let _ = fs::remove_file(&image_part);
        return Err("The expanded qOS installer did not match its signed catalog.".into());
    }
    fs::rename(&image_part, &image)
        .map_err(|error| format!("Could not publish the verified installer cache: {error}"))?;
    let _ = fs::remove_file(&archive_part);
    Ok(source_for(release, &image))
}

fn source_for(release: &QosRelease, image: &Path) -> Source {
    let mut source = release.to_source();
    source.path = image.to_string_lossy().into_owned();
    source.identity = image
        .metadata()
        .ok()
        .and_then(|metadata| crate::model::SourceIdentity::from_metadata(&metadata));
    source
}

/// Download and verify without blocking the UI thread.
pub fn spawn_download(release: QosRelease) -> Receiver<Result<Source, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("qos-release-download".into())
        .spawn(move || {
            let _ = tx.send(verified_image(&release));
        })
        .ok();
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_catalog_is_well_formed() {
        assert!(bundled_releases().is_ok());
    }

    #[test]
    fn digest_validation_is_strict() {
        assert!(validate_digest(&"a".repeat(64)).is_ok());
        assert!(validate_digest(&"z".repeat(64)).is_err());
        assert!(validate_digest("abc").is_err());
    }

    #[test]
    fn verified_release_source_keeps_catalog_digest_and_file_identity() {
        let image = tempfile::NamedTempFile::new().unwrap();
        let release = QosRelease {
            name: "qOS".into(),
            version: "1.0".into(),
            size: 1,
            channel: "stable".into(),
            local_path: String::new(),
            url: "https://code.podesta.ai/qos/qos/releases/download/imager-v1.0.0/qos.img.zst"
                .into(),
            archive_sha256: "a".repeat(64),
            archive_size: 1,
            image_sha256: "b".repeat(64),
        };

        let source = source_for(&release, image.path());
        let expected = "b".repeat(64);
        assert_eq!(source.expected_sha256.as_deref(), Some(expected.as_str()));
        assert!(source.identity.is_some());
    }
}
