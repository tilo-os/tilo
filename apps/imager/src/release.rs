//! Trusted Tilo release catalog and verified medium download/cache.
//!
//! The catalog is embedded into each platform-signed Imager package at build
//! time.  A release therefore cannot substitute its URL or digest without also
//! replacing the signed host application.  Both the compressed transport and
//! expanded disk image are SHA-256 checked before the image becomes selectable.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{Source, TiloRelease};

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
    #[serde(default)]
    hardware_profile: Option<String>,
    #[serde(default)]
    provenance: Option<MediaProvenance>,
    url: String,
    archive_sha256: String,
    archive_size: u64,
    image_sha256: String,
    image_size: u64,
}

#[derive(Debug, Deserialize)]
struct MediaProvenance {
    source_image_id: String,
    input_qcow2_sha256: String,
    hardware_profile_sha256: Option<String>,
    target_update_ref: Option<String>,
}

impl MediaProvenance {
    fn validate(&self) -> Result<(), String> {
        let source_digest = self
            .source_image_id
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                "The release catalog contains an invalid source image identity.".to_string()
            })?;
        validate_digest(source_digest)?;
        validate_digest(&self.input_qcow2_sha256)?;
        let profile_digest = self.hardware_profile_sha256.as_deref().ok_or_else(|| {
            "The release catalog is missing a hardware profile digest.".to_string()
        })?;
        validate_digest(profile_digest)?;
        let origin = self.target_update_ref.as_deref().ok_or_else(|| {
            "The release catalog is missing a hardware update origin.".to_string()
        })?;
        let valid_origin =
            if let Some(digest) = origin.strip_prefix("code.podesta.ai/qos/tilo@sha256:") {
                validate_digest(digest).is_ok()
            } else if let Some(tag) = origin.strip_prefix("code.podesta.ai/qos/tilo:") {
                !tag.is_empty()
                    && tag.len() <= 128
                    && tag
                        .bytes()
                        .next()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && tag.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
                    })
            } else {
                false
            };
        if !valid_origin {
            return Err("The release catalog contains an untrusted hardware update origin.".into());
        }
        Ok(())
    }
}

/// A local, non-replicated update from the verified release fetch worker.
#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Downloading { received: u64, total: u64 },
    Verifying,
    Expanding { written: u64, total: u64 },
    Finished(Result<Source, String>),
}

/// Cancellation narrows the authority of this process-local network job. The
/// partial archive remains in the cache so a later attempt resumes safely.
#[derive(Clone, Debug, Default)]
pub struct DownloadControl(Arc<AtomicBool>);

impl DownloadControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct DownloadJob {
    pub events: Receiver<DownloadEvent>,
    pub control: DownloadControl,
}

/// Releases authenticated by the signature on this Imager build, filtered to
/// media for the current host CPU architecture. Board compatibility still
/// requires selecting the explicitly labeled target hardware profile.
///
/// # Errors
///
/// Returns an error when the embedded catalog is malformed or contains an
/// unsupported schema, untrusted URL, invalid digest, or invalid size, or when
/// the native host architecture cannot be detected.
pub fn bundled_releases() -> Result<Vec<TiloRelease>, String> {
    releases_for_host(BUNDLED_CATALOG, crate::host::architecture())
}

fn releases_for_host(
    catalog_json: &str,
    host_arch: Result<Option<&str>, String>,
) -> Result<Vec<TiloRelease>, String> {
    let host_arch = host_arch?;
    let catalog: Catalog = serde_json::from_str(catalog_json)
        .map_err(|error| format!("The bundled release catalog is invalid: {error}"))?;
    if !matches!(catalog.schema, 1 | 2) {
        return Err("The bundled release catalog uses an unsupported schema.".into());
    }
    if catalog.releases.is_empty() && catalog.tag == "unreleased" {
        return Ok(Vec::new());
    }
    if !catalog.tag.starts_with("imager-v") {
        return Err("The bundled release catalog has an invalid release tag.".into());
    }
    let mut identities = std::collections::HashSet::new();
    for release in &catalog.releases {
        let profile = match (catalog.schema, release.hardware_profile.as_deref()) {
            (1, None | Some("generic")) => "generic",
            (2, Some(profile)) => profile,
            _ => {
                return Err(
                    "The release catalog has an invalid hardware profile for its schema.".into(),
                );
            }
        };
        if !matches!(release.arch.as_str(), "x86_64" | "aarch64")
            || !valid_hardware_profile(profile)
        {
            return Err(
                "The release catalog contains an unsupported architecture and hardware profile."
                    .into(),
            );
        }
        if !identities.insert((release.arch.as_str(), profile)) {
            return Err(
                "The release catalog contains duplicate architecture and hardware profiles.".into(),
            );
        }
    }
    for release in &catalog.releases {
        if release
            .hardware_profile
            .as_deref()
            .is_some_and(|profile| profile != "generic")
        {
            release
                .provenance
                .as_ref()
                .ok_or_else(|| {
                    "The release catalog is missing hardware media provenance.".to_string()
                })?
                .validate()?;
        }
    }
    let Some(host_arch) = host_arch else {
        return Ok(Vec::new());
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
            Ok(TiloRelease {
                name: release.name,
                version: release.version,
                size: release.image_size,
                channel: release.channel,
                architecture: release.arch,
                hardware_profile: release.hardware_profile.unwrap_or_else(|| "generic".into()),
                local_path: String::new(),
                url: release.url,
                archive_sha256: release.archive_sha256,
                archive_size: release.archive_size,
                image_sha256: release.image_sha256,
            })
        })
        .collect()
}

fn valid_hardware_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 64
        && profile
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && profile
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
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
        .join("tilo-imager")
}

fn sha256_controlled(path: &Path, control: &DownloadControl) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Could not open downloaded data: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    loop {
        if control.is_cancelled() {
            return Err("Tilo installer download cancelled.".into());
        }
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

fn same_origin(actual: &reqwest::Url, trusted: &reqwest::Url) -> bool {
    actual.scheme() == trusted.scheme()
        && actual.host_str() == trusted.host_str()
        && actual.port_or_known_default() == trusted.port_or_known_default()
}

fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn copy_download_observed(
    response: reqwest::blocking::Response,
    archive: &mut File,
    limit: u64,
    already_received: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<u64, String> {
    let mut input = response.take(limit.saturating_add(1));
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    let mut downloaded = 0_u64;
    let result = loop {
        if control.is_cancelled() {
            break Err("Tilo installer download cancelled.".to_string());
        }
        let read = input
            .read(&mut buffer)
            .map_err(|error| format!("The Tilo installer download was interrupted: {error}"))?;
        if read == 0 {
            break Ok(downloaded);
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound the Tilo installer response.".to_string())?;
        archive
            .write_all(chunk)
            .map_err(|error| format!("Could not save the Tilo installer: {error}"))?;
        downloaded = downloaded.saturating_add(read as u64);
        notify(DownloadEvent::Downloading {
            received: already_received.saturating_add(downloaded),
            total: already_received.saturating_add(limit),
        });
    };
    // Make successfully received chunks durable even when the connection ends
    // early, so the next attempt can ask only for the missing suffix.
    archive
        .flush()
        .map_err(|error| format!("Could not save the Tilo installer: {error}"))?;
    result
}

fn append_range_observed(
    response: reqwest::blocking::Response,
    archive_part: &Path,
    partial_size: u64,
    expected_size: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<bool, String> {
    let parsed_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(content_range);
    let correct_range = parsed_range.is_some_and(|(start, end, total)| {
        start == partial_size && end >= start && end < expected_size && total == expected_size
    });
    let response_size = parsed_range.map_or(0, |(start, end, _)| {
        end.saturating_sub(start).saturating_add(1)
    });
    let correct_length = response
        .content_length()
        .is_none_or(|length| length == response_size);
    if !correct_range || !correct_length {
        fs::remove_file(archive_part)
            .map_err(|error| format!("Could not restart a mismatched partial download: {error}"))?;
        return Ok(false);
    }

    let mut archive = OpenOptions::new()
        .append(true)
        .open(archive_part)
        .map_err(|error| format!("Could not resume the download cache: {error}"))?;
    let downloaded = copy_download_observed(
        response,
        &mut archive,
        response_size,
        partial_size,
        control,
        notify,
    )?;
    let total = archive
        .metadata()
        .map_or(u64::MAX, |metadata| metadata.len());
    if downloaded == response_size && total == expected_size {
        return Ok(true);
    }
    if downloaded > response_size || total > expected_size {
        let _ = fs::remove_file(archive_part);
        return Err("The Tilo installer range response exceeded its declared boundary.".into());
    }
    Err("The Tilo installer download was interrupted before it completed.".into())
}

#[cfg(test)]
fn download_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    trusted_origin: &reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
) -> Result<(), String> {
    download_archive_observed(
        client,
        url,
        trusted_origin,
        archive_part,
        expected_size,
        &DownloadControl::default(),
        &mut |_| {},
    )
}

fn download_archive_observed(
    client: &reqwest::blocking::Client,
    url: &str,
    trusted_origin: &reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<(), String> {
    let mut partial_size = archive_part.metadata().map_or(0, |metadata| metadata.len());
    if partial_size > expected_size {
        fs::remove_file(archive_part)
            .map_err(|error| format!("Could not replace an oversized partial download: {error}"))?;
        partial_size = 0;
    }
    if partial_size == expected_size {
        return Ok(());
    }

    // One ranged attempt may be followed by one clean full request when the
    // server ignores Range or returns a response that cannot safely be joined.
    for _ in 0..2 {
        let resume = partial_size > 0;
        let mut request = client.get(url);
        if resume {
            request = request.header(reqwest::header::RANGE, format!("bytes={partial_size}-"));
        }
        let response = request
            .send()
            .map_err(|error| format!("Could not download the Tilo installer: {error}"))?;
        if !same_origin(response.url(), trusted_origin) {
            return Err("The release download redirected to an untrusted origin.".into());
        }

        if resume && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            if append_range_observed(
                response,
                archive_part,
                partial_size,
                expected_size,
                control,
                notify,
            )? {
                return Ok(());
            }
            partial_size = 0;
            continue;
        }

        if resume && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // The remote object no longer agrees with the cached offset. Retry
            // once from zero; the final digest still authenticates the result.
            fs::remove_file(archive_part)
                .map_err(|error| format!("Could not restart the partial download: {error}"))?;
            partial_size = 0;
            continue;
        }

        if resume && response.status() != reqwest::StatusCode::OK {
            // Preserve a valid partial across transient server errors. No bytes
            // from an unrecognized response are ever appended to it.
            return response
                .error_for_status()
                .map(|_| ())
                .map_err(|error| format!("Could not download the Tilo installer: {error}"));
        }

        let response = response
            .error_for_status()
            .map_err(|error| format!("Could not download the Tilo installer: {error}"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("The Tilo installer server returned an invalid full response.".into());
        }
        if response
            .content_length()
            .is_some_and(|length| length != expected_size)
        {
            if resume {
                let _ = fs::remove_file(archive_part);
            }
            return Err("The Tilo installer download has an unexpected size.".into());
        }
        // A 200 response to a ranged request means Range was ignored. Truncate
        // first, then safely consume that response as the new complete file.
        let mut archive = File::create(archive_part)
            .map_err(|error| format!("Could not create the download cache: {error}"))?;
        let downloaded =
            copy_download_observed(response, &mut archive, expected_size, 0, control, notify)?;
        let total = archive
            .metadata()
            .map_or(u64::MAX, |metadata| metadata.len());
        if downloaded == expected_size && total == expected_size {
            return Ok(());
        }
        if total > expected_size {
            let _ = fs::remove_file(archive_part);
        }
        return Err("The Tilo installer download was interrupted before it completed.".into());
    }

    Err("The Tilo installer server did not honor a safe download restart.".into())
}

#[allow(
    clippy::too_many_lines,
    reason = "the authenticated download, expansion, and atomic cache publication form one fail-closed transaction"
)]
fn verified_image_observed(
    release: &TiloRelease,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<Source, String> {
    validate_digest(&release.archive_sha256)?;
    validate_digest(&release.image_sha256)?;
    if release.archive_size == 0 || release.size == 0 {
        return Err("The Tilo installer has invalid download metadata.".into());
    }
    let root = cache_root().join(&release.image_sha256);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create the download cache: {error}"))?;
    let image = root.join("tilo-installer.img");
    if image.is_file() {
        notify(DownloadEvent::Verifying);
        if sha256_controlled(&image, control)? == release.image_sha256 {
            return Ok(source_for(release, &image));
        }
        fs::remove_file(&image)
            .map_err(|error| format!("Could not replace an invalid cached installer: {error}"))?;
    }

    let archive_part = root.join("tilo-installer.img.zst.part");
    let image_part = root.join("tilo-installer.img.part");
    let client = reqwest::blocking::Client::builder()
        .user_agent("Tilo-Imager/1")
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| format!("Could not initialize the downloader: {error}"))?;
    let trusted_origin = reqwest::Url::parse("https://code.podesta.ai/")
        .map_err(|error| format!("Could not initialize the trusted release origin: {error}"))?;
    notify(DownloadEvent::Downloading {
        received: archive_part.metadata().map_or(0, |metadata| metadata.len()),
        total: release.archive_size,
    });
    download_archive_observed(
        &client,
        &release.url,
        &trusted_origin,
        &archive_part,
        release.archive_size,
        control,
        notify,
    )?;
    notify(DownloadEvent::Verifying);
    if sha256_controlled(&archive_part, control)? != release.archive_sha256 {
        let _ = fs::remove_file(&archive_part);
        return Err("The downloaded Tilo installer did not match its signed catalog.".into());
    }

    let compressed = File::open(&archive_part)
        .map_err(|error| format!("Could not open the verified installer: {error}"))?;
    let mut decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|error| format!("Could not unpack the Tilo installer: {error}"))?;
    let mut expanded = File::create(&image_part)
        .map_err(|error| format!("Could not create the expanded installer: {error}"))?;
    let mut expanded_size = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    while expanded_size <= release.size {
        if control.is_cancelled() {
            let _ = fs::remove_file(&image_part);
            return Err("Tilo installer download cancelled.".into());
        }
        let allowed = release
            .size
            .saturating_add(1)
            .saturating_sub(expanded_size)
            .min(buffer.len() as u64) as usize;
        if allowed == 0 {
            break;
        }
        let writable = buffer
            .get_mut(..allowed)
            .ok_or_else(|| "Could not bound the expanded Tilo installer.".to_string())?;
        let read = decoder
            .read(writable)
            .map_err(|error| format!("Could not unpack the Tilo installer: {error}"))?;
        if read == 0 {
            break;
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound the expanded Tilo installer.".to_string())?;
        expanded
            .write_all(chunk)
            .map_err(|error| format!("Could not unpack the Tilo installer: {error}"))?;
        expanded_size = expanded_size.saturating_add(read as u64);
        notify(DownloadEvent::Expanding {
            written: expanded_size,
            total: release.size,
        });
    }
    expanded
        .flush()
        .map_err(|error| format!("Could not save the expanded installer: {error}"))?;
    notify(DownloadEvent::Verifying);
    if expanded_size != release.size
        || expanded.metadata().map_or(0, |meta| meta.len()) != release.size
        || sha256_controlled(&image_part, control)? != release.image_sha256
    {
        let _ = fs::remove_file(&image_part);
        return Err("The expanded Tilo installer did not match its signed catalog.".into());
    }
    fs::rename(&image_part, &image)
        .map_err(|error| format!("Could not publish the verified installer cache: {error}"))?;
    let _ = fs::remove_file(&archive_part);
    Ok(source_for(release, &image))
}

fn source_for(release: &TiloRelease, image: &Path) -> Source {
    let mut source = release.to_source();
    source.path = image.to_string_lossy().into_owned();
    source.identity = fs::File::open(image)
        .ok()
        .and_then(|file| crate::model::SourceIdentity::from_file(&file));
    source
}

/// Download and verify without blocking the UI thread.
pub fn spawn_download(release: TiloRelease) -> DownloadJob {
    let (tx, rx) = mpsc::channel();
    let control = DownloadControl::default();
    let worker_control = control.clone();
    std::thread::Builder::new()
        .name("tilo-release-download".into())
        .spawn(move || {
            let result = verified_image_observed(&release, &worker_control, &mut |event| {
                let _ = tx.send(event);
            });
            let _ = tx.send(DownloadEvent::Finished(result));
        })
        .ok();
    DownloadJob {
        events: rx,
        control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct HttpResponse {
        status: &'static str,
        headers: Vec<String>,
        body: Vec<u8>,
        advertised_size: Option<usize>,
    }

    fn serve(
        responses: Vec<HttpResponse>,
    ) -> (
        String,
        reqwest::Url,
        Arc<Mutex<Vec<String>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                seen.lock().unwrap().push(request);
                let size = response.advertised_size.unwrap_or(response.body.len());
                let mut head = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {size}\r\nConnection: close\r\n",
                    response.status
                );
                for header in response.headers {
                    head.push_str(&header);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&response.body).unwrap();
            }
        });
        (
            format!("http://{address}/installer.img.zst"),
            reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            requests,
            handle,
        )
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes).unwrap()
    }

    fn test_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder().build().unwrap()
    }

    #[test]
    fn development_catalog_is_well_formed() {
        assert!(bundled_releases().is_ok());
    }

    fn catalog_with_both_architectures() -> String {
        let releases = ["x86_64", "aarch64"].map(|arch| {
            serde_json::json!({
                "name": format!("Tilo {arch}"),
                "version": "1.0.0",
                "channel": "stable",
                "arch": arch,
                "url": format!("https://code.podesta.ai/qos/releases/releases/download/imager-v1.0.0/tilo-{arch}.img.zst"),
                "archive_sha256": "a".repeat(64),
                "archive_size": 32,
                "image_sha256": "b".repeat(64),
                "image_size": 64,
            })
        });
        serde_json::json!({
            "schema": 1,
            "tag": "imager-v1.0.0",
            "releases": releases,
        })
        .to_string()
    }

    #[test]
    fn native_arm64_host_selects_only_arm64_media_regardless_of_imager_build() {
        let releases =
            releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].name, "Tilo aarch64");
        assert!(releases[0].url.ends_with("tilo-aarch64.img.zst"));
        assert_eq!(releases[0].image_sha256, "b".repeat(64));
    }

    #[test]
    fn native_x86_64_host_selects_only_x86_64_media() {
        let releases =
            releases_for_host(&catalog_with_both_architectures(), Ok(Some("x86_64"))).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].name, "Tilo x86_64");
    }

    fn catalog_with_hardware_variant() -> serde_json::Value {
        let mut catalog: serde_json::Value =
            serde_json::from_str(&catalog_with_both_architectures()).unwrap();
        catalog["schema"] = 2.into();
        for release in catalog["releases"].as_array_mut().unwrap() {
            release["hardware_profile"] = "generic".into();
        }
        let mut board = catalog["releases"][1].clone();
        board["hardware_profile"] = "lenovo-83ed".into();
        board["image_sha256"] = "c".repeat(64).into();
        board["provenance"] = serde_json::json!({
            "source_image_id": format!("sha256:{}", "d".repeat(64)),
            "input_qcow2_sha256": "e".repeat(64),
            "hardware_profile_sha256": "f".repeat(64),
            "target_update_ref": "code.podesta.ai/qos/tilo:board-fixture",
        });
        catalog["releases"].as_array_mut().unwrap().push(board);
        catalog
    }

    #[test]
    fn hardware_variants_keep_distinct_authenticated_selection_and_source_labels() {
        let releases = releases_for_host(
            &catalog_with_hardware_variant().to_string(),
            Ok(Some("aarch64")),
        )
        .unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].hardware_profile, "generic");
        assert_eq!(releases[1].hardware_profile, "lenovo-83ed");
        assert_ne!(releases[0].selection_id(), releases[1].selection_id());
        let board = releases[1].to_source();
        assert!(board.name.contains("lenovo-83ed only"));
        assert!(board.label.contains("lenovo-83ed only"));
        assert_eq!(
            board.expected_sha256.as_deref(),
            Some("c".repeat(64).as_str())
        );
        assert!(!releases[0].matches_source(&board));
        assert!(releases[1].matches_source(&board));
    }

    #[test]
    fn legacy_catalogs_mean_generic_and_cannot_silently_claim_a_board() {
        let legacy =
            releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
        assert_eq!(legacy[0].hardware_profile, "generic");
        let mut catalog = catalog_with_hardware_variant();
        catalog["schema"] = 1.into();
        assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
    }

    #[test]
    fn generic_media_provenance_does_not_require_a_board_profile_payload() {
        let mut catalog: serde_json::Value =
            serde_json::from_str(&catalog_with_both_architectures()).unwrap();
        catalog["releases"][1]["provenance"] = serde_json::json!({
            "source_image_id": format!("sha256:{}", "d".repeat(64)),
            "input_qcow2_sha256": "e".repeat(64),
            "hardware_profile_sha256": null,
            "target_update_ref": null,
        });
        let releases = releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).unwrap();
        assert_eq!(releases[0].hardware_profile, "generic");
    }

    #[test]
    fn v2_requires_safe_explicit_unique_profiles_and_bound_provenance() {
        for invalid in [
            serde_json::Value::Null,
            "".into(),
            "../board".into(),
            "Board".into(),
        ] {
            let mut catalog = catalog_with_hardware_variant();
            catalog["releases"][2]["hardware_profile"] = invalid;
            assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
        }
        for field in ["hardware_profile", "provenance"] {
            let mut catalog = catalog_with_hardware_variant();
            catalog["releases"][2]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
        }
        for field in [
            "source_image_id",
            "hardware_profile_sha256",
            "input_qcow2_sha256",
            "target_update_ref",
        ] {
            let mut catalog = catalog_with_hardware_variant();
            catalog["releases"][2]["provenance"][field] = "untrusted".into();
            assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
        }
        let mut catalog = catalog_with_hardware_variant();
        let duplicate = catalog["releases"][2].clone();
        catalog["releases"].as_array_mut().unwrap().push(duplicate);
        assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
    }

    #[test]
    fn hardware_profile_ids_are_data_driven_not_a_model_allowlist() {
        let mut catalog = catalog_with_hardware_variant();
        catalog["releases"][2]["hardware_profile"] = "another-reviewed-board".into();
        let releases = releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).unwrap();
        assert!(
            releases[1]
                .to_source()
                .name
                .contains("another-reviewed-board only")
        );
    }

    #[test]
    fn native_arm64_host_does_not_fall_back_when_only_x86_media_is_published() {
        let mut catalog: serde_json::Value =
            serde_json::from_str(&catalog_with_both_architectures()).unwrap();
        catalog["releases"]
            .as_array_mut()
            .unwrap()
            .retain(|release| release["arch"] == "x86_64");
        assert!(
            releases_for_host(&catalog.to_string(), Ok(Some("aarch64")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unknown_or_failed_native_detection_never_offers_media() {
        let catalog = catalog_with_both_architectures();
        assert!(releases_for_host(&catalog, Ok(None)).unwrap().is_empty());
        assert_eq!(
            releases_for_host(&catalog, Err("host detection failed".into())).unwrap_err(),
            "host detection failed"
        );
    }

    #[test]
    fn arm64_selection_preserves_catalog_metadata_validation() {
        let catalog = catalog_with_both_architectures();
        for invalid in [
            catalog.replace("code.podesta.ai", "untrusted.example"),
            catalog.replace(&"a".repeat(64), "invalid-digest"),
            catalog.replace("\"image_size\":64", "\"image_size\":0"),
        ] {
            assert!(releases_for_host(&invalid, Ok(Some("aarch64"))).is_err());
        }
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
        let release = TiloRelease {
            name: "Tilo".into(),
            version: "1.0".into(),
            size: 1,
            channel: "stable".into(),
            architecture: "aarch64".into(),
            hardware_profile: "generic".into(),
            local_path: String::new(),
            url:
                "https://code.podesta.ai/qos/releases/releases/download/imager-v1.0.0/tilo.img.zst"
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

    #[test]
    fn interrupted_download_keeps_bytes_and_resumes_with_a_valid_range() {
        let payload = b"signed installer archive bytes";
        let split = 9;
        let (url, origin, requests, server) = serve(vec![
            HttpResponse {
                status: "200 OK",
                headers: vec![],
                body: payload[..split].to_vec(),
                advertised_size: Some(payload.len()),
            },
            HttpResponse {
                status: "206 Partial Content",
                headers: vec![format!(
                    "Content-Range: bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )],
                body: payload[split..].to_vec(),
                advertised_size: None,
            },
        ]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        let client = test_client();

        assert!(download_archive(&client, &url, &origin, &part, payload.len() as u64).is_err());
        assert_eq!(fs::read(&part).unwrap(), payload[..split]);
        download_archive(&client, &url, &origin, &part, payload.len() as u64).unwrap();
        assert_eq!(fs::read(&part).unwrap(), payload);
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(!requests[0].contains("Range:"));
        assert!(requests[1].contains(&format!("range: bytes={split}-")));
        drop(requests);
    }

    #[test]
    fn ignored_range_restarts_without_duplicating_the_partial() {
        let payload = b"complete archive from a range-ignorant server";
        let (url, origin, requests, server) = serve(vec![HttpResponse {
            status: "200 OK",
            headers: vec![],
            body: payload.to_vec(),
            advertised_size: None,
        }]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        fs::write(&part, &payload[..8]).unwrap();

        download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
        assert_eq!(fs::read(&part).unwrap(), payload);
        server.join().unwrap();
        assert!(requests.lock().unwrap()[0].contains("range: bytes=8-"));
    }

    #[test]
    fn mismatched_content_range_is_discarded_before_a_clean_restart() {
        let payload = b"archive with authenticated final digest";
        let split = 7;
        let (url, origin, requests, server) = serve(vec![
            HttpResponse {
                status: "206 Partial Content",
                headers: vec![format!(
                    "Content-Range: bytes {}-{}/{}",
                    split + 1,
                    payload.len() - 1,
                    payload.len()
                )],
                body: payload[split..].to_vec(),
                advertised_size: None,
            },
            HttpResponse {
                status: "200 OK",
                headers: vec![],
                body: payload.to_vec(),
                advertised_size: None,
            },
        ]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        fs::write(&part, &payload[..split]).unwrap();

        download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
        assert_eq!(fs::read(&part).unwrap(), payload);
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains(&format!("range: bytes={split}-")));
        assert!(!requests[1].contains("Range:"));
        assert!(!requests[1].contains("range:"));
        drop(requests);
    }

    #[test]
    fn oversized_partial_is_removed_before_requesting_the_full_archive() {
        let payload = b"small complete archive";
        let (url, origin, requests, server) = serve(vec![HttpResponse {
            status: "200 OK",
            headers: vec![],
            body: payload.to_vec(),
            advertised_size: None,
        }]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        fs::write(&part, vec![b'x'; payload.len() + 1]).unwrap();

        download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
        assert_eq!(fs::read(&part).unwrap(), payload);
        server.join().unwrap();
        assert!(!requests.lock().unwrap()[0].contains("range:"));
    }

    #[test]
    fn transient_http_failure_does_not_destroy_a_partial_archive() {
        let payload = b"partial archive survives";
        let (url, origin, requests, server) = serve(vec![HttpResponse {
            status: "503 Service Unavailable",
            headers: vec![],
            body: b"later".to_vec(),
            advertised_size: None,
        }]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        fs::write(&part, &payload[..6]).unwrap();

        assert!(
            download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).is_err()
        );
        assert_eq!(fs::read(&part).unwrap(), payload[..6]);
        server.join().unwrap();
        assert!(requests.lock().unwrap()[0].contains("range: bytes=6-"));
    }

    #[test]
    fn observed_download_reports_bounded_byte_progress() {
        let payload = b"installer bytes with progress";
        let (url, origin, _requests, server) = serve(vec![HttpResponse {
            status: "200 OK",
            headers: vec![],
            body: payload.to_vec(),
            advertised_size: None,
        }]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        let mut events = Vec::new();

        download_archive_observed(
            &test_client(),
            &url,
            &origin,
            &part,
            payload.len() as u64,
            &DownloadControl::default(),
            &mut |event| events.push(event),
        )
        .unwrap();

        server.join().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::Downloading { received, total }
                if *received == payload.len() as u64 && *total == payload.len() as u64
        )));
    }

    #[test]
    fn cancellation_preserves_the_resumable_partial_archive() {
        let payload = b"more authenticated installer bytes";
        let split = 8;
        let (url, origin, _requests, server) = serve(vec![HttpResponse {
            status: "206 Partial Content",
            headers: vec![format!(
                "Content-Range: bytes {split}-{}/{}",
                payload.len() - 1,
                payload.len()
            )],
            body: payload[split..].to_vec(),
            advertised_size: None,
        }]);
        let cache = tempfile::tempdir().unwrap();
        let part = cache.path().join("installer.part");
        fs::write(&part, &payload[..split]).unwrap();
        let control = DownloadControl::default();
        control.cancel();

        let error = download_archive_observed(
            &test_client(),
            &url,
            &origin,
            &part,
            payload.len() as u64,
            &control,
            &mut |_| {},
        )
        .unwrap_err();

        server.join().unwrap();
        assert!(error.contains("cancelled"));
        assert_eq!(fs::read(&part).unwrap(), payload[..split]);
    }

    #[test]
    fn trusted_redirect_check_includes_scheme_host_and_port() {
        let trusted = reqwest::Url::parse("https://code.podesta.ai/").unwrap();
        assert!(same_origin(
            &reqwest::Url::parse("https://code.podesta.ai/releases/file").unwrap(),
            &trusted
        ));
        assert!(!same_origin(
            &reqwest::Url::parse("https://code.podesta.ai:444/releases/file").unwrap(),
            &trusted
        ));
        assert!(!same_origin(
            &reqwest::Url::parse("http://code.podesta.ai/releases/file").unwrap(),
            &trusted
        ));
    }
}
