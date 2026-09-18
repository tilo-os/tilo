//! Trusted qOS release catalog and verified medium download/cache.
//!
//! The catalog is embedded into each platform-signed Imager package at build
//! time. In a genuinely signed distribution, changing catalog bytes requires
//! replacing that signed application. Local development embedding alone is not
//! cryptographic release evidence.  Both the compressed transport and
//! expanded disk image are SHA-256 checked before the image becomes selectable.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{QosRelease, Source};

const BUNDLED_CATALOG: &str = include_str!("../release-catalog.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    tag: String,
    releases: Vec<CatalogRelease>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogRelease {
    Media(MediaRelease),
    Internal(InternalMetadata),
}

#[derive(Debug, Deserialize)]
enum MediaPurpose {
    #[serde(rename = "removable-media")]
    RemovableMedia,
}
#[derive(Debug, Deserialize)]
enum MediaLayout {
    #[serde(rename = "whole-disk-image")]
    WholeDiskImage,
}
#[derive(Debug, Deserialize)]
enum InternalPurpose {
    #[serde(rename = "qos-internal-installer")]
    InternalInstaller,
}
#[derive(Debug, Deserialize)]
enum InternalLayout {
    #[serde(rename = "internal-bundle-v1")]
    BundleV1,
}

fn present_tag<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaRelease {
    #[serde(default, deserialize_with = "present_tag")]
    purpose: Option<MediaPurpose>,
    #[serde(default, deserialize_with = "present_tag")]
    layout: Option<MediaLayout>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InternalMetadata {
    purpose: InternalPurpose,
    layout: InternalLayout,
    release_id: String,
    arch: String,
    manifest_url: String,
    manifest_sha256: String,
    manifest_size: u64,
}

/// A selection obtained only from this build's embedded catalog. Authentication
/// depends on a genuinely signed build; development embedding is not a signature.
/// This type cannot become a whole-disk `Source` or a media download request.
#[derive(Debug)]
pub struct InternalRelease {
    metadata: InternalMetadata,
}

impl InternalRelease {
    pub fn release_id(&self) -> &str {
        &self.metadata.release_id
    }
    pub fn architecture(&self) -> &str {
        &self.metadata.arch
    }
    pub fn manifest_url(&self) -> &str {
        &self.metadata.manifest_url
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.metadata.manifest_sha256
    }

    #[cfg(test)]
    pub(crate) fn transport_fixture(bytes: &[u8], url: &str) -> Self {
        Self {
            metadata: InternalMetadata {
                purpose: InternalPurpose::InternalInstaller,
                layout: InternalLayout::BundleV1,
                release_id: "fixture-only".into(),
                arch: "x86_64".into(),
                manifest_url: url.into(),
                manifest_sha256: format!("{:x}", Sha256::digest(bytes)),
                manifest_size: bytes.len() as u64,
            },
        }
    }

    pub fn manifest_size(&self) -> u64 {
        self.metadata.manifest_size
    }

    /// Bind the exact received bytes to the embedded selection before parsing.
    /// A later downloader must retain existing origin/length rules; this method
    /// performs no transport, staging, execution or device access.
    /// # Errors
    /// Rejects byte length/digest mismatch, invalid schema or selection mismatch.
    pub fn verify_manifest(
        &self,
        bytes: &[u8],
    ) -> Result<crate::setup::internal_bundle::InternalBundle, String> {
        let expected = &self.metadata;
        if bytes.len() as u64 != expected.manifest_size
            || format!("{:x}", Sha256::digest(bytes)) != expected.manifest_sha256
        {
            return Err("Internal manifest does not match the embedded catalog bytes.".into());
        }
        let bundle = crate::setup::internal_bundle::validate_internal_bundle(bytes)?;
        let (InternalPurpose::InternalInstaller, InternalLayout::BundleV1) =
            (&expected.purpose, &expected.layout);
        if bundle.release_id() != expected.release_id
            || bundle.architecture() != expected.arch
            || bundle.purpose() != "qos-internal-installer"
        {
            return Err("Internal manifest does not match the selected release.".into());
        }
        Ok(bundle)
    }
}

fn trusted_catalog_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Invalid catalog URL.".to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some("code.podesta.ai")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("The release catalog contains an untrusted download origin.".into());
    }
    Ok(())
}

fn parse_catalog(raw: &str) -> Result<Catalog, String> {
    if raw.len() > 1024 * 1024 {
        return Err("Release catalog exceeds size limit.".into());
    }
    // Derived record/enum deserializers admit sequence records and object
    // unit-enum tags. Constrain the published JSON shapes before decoding the
    // ORIGINAL bytes, preserving duplicate-key checks and authenticated input.
    {
        let shape: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| error.to_string())?;
        let valid = shape.is_object()
            && shape
                .get("releases")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|entries| {
                    entries.iter().all(|entry| {
                        entry.is_object()
                            && ["purpose", "layout"]
                                .iter()
                                .all(|key| entry.get(*key).is_none_or(serde_json::Value::is_string))
                    })
                });
        if !valid {
            return Err(
                "Catalog records must be JSON objects with string purpose/layout tags.".into(),
            );
        }
    }
    let catalog: Catalog = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if catalog.schema != 1 {
        return Err("Unsupported release catalog schema.".into());
    }
    if catalog.releases.is_empty() && catalog.tag == "unreleased" {
        return Ok(catalog);
    }
    if !catalog.tag.starts_with("imager-v") || catalog.releases.len() > 256 {
        return Err("Invalid release catalog tag or entry count.".into());
    }
    let mut internal_ids = std::collections::HashSet::new();
    // Validate every entry before any architecture filtering.
    for entry in &catalog.releases {
        let arch = match entry {
            CatalogRelease::Media(media) => {
                if media.purpose.is_some() != media.layout.is_some() {
                    return Err("Incomplete media purpose/layout.".into());
                }
                validate_digest(&media.archive_sha256)?;
                validate_digest(&media.image_sha256)?;
                if media.archive_size == 0 || media.image_size == 0 {
                    return Err("Invalid media size.".into());
                }
                trusted_catalog_url(&media.url)?;
                &media.arch
            }
            CatalogRelease::Internal(internal) => {
                use crate::setup::internal_bundle::{
                    MAX_MANIFEST_BYTES, canonical_digest, identifier,
                };
                if !identifier(&internal.release_id)
                    || !canonical_digest(&internal.manifest_sha256, 64)
                    || internal.manifest_size == 0
                    || internal.manifest_size > MAX_MANIFEST_BYTES as u64
                    || !internal_ids.insert((&internal.release_id, &internal.arch))
                {
                    return Err("Invalid internal bundle metadata.".into());
                }
                trusted_catalog_url(&internal.manifest_url)?;
                &internal.arch
            }
        };
        if !matches!(arch.as_str(), "x86_64" | "aarch64") {
            return Err("Unsupported release architecture.".into());
        }
    }
    Ok(catalog)
}

fn media_releases(catalog: Catalog, host_arch: &str) -> Vec<QosRelease> {
    catalog
        .releases
        .into_iter()
        .filter_map(|entry| {
            let CatalogRelease::Media(release) = entry else {
                return None;
            };
            (release.arch == host_arch).then(|| QosRelease {
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

/// Media selections only. Authentication depends on the signature of the build.
/// # Errors
/// Rejects invalid metadata in any entry, including foreign architectures.
pub fn bundled_releases() -> Result<Vec<QosRelease>, String> {
    Ok(media_releases(
        parse_catalog(BUNDLED_CATALOG)?,
        std::env::consts::ARCH,
    ))
}

/// Internal selections stay separate from the ordinary media downloader.
/// No released entries exist until actual reviewed artifacts are published.
/// # Errors
/// Rejects any invalid embedded catalog entry before host filtering.
pub fn bundled_internal_releases() -> Result<Vec<InternalRelease>, String> {
    Ok(parse_catalog(BUNDLED_CATALOG)?
        .releases
        .into_iter()
        .filter_map(|entry| match entry {
            CatalogRelease::Internal(metadata) if metadata.arch == std::env::consts::ARCH => {
                Some(InternalRelease { metadata })
            }
            _ => None,
        })
        .collect())
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

fn copy_download(
    response: reqwest::blocking::Response,
    archive: &mut File,
    limit: u64,
) -> Result<u64, String> {
    let result = std::io::copy(&mut response.take(limit.saturating_add(1)), archive);
    // Make successfully received chunks durable even when the connection ends
    // early, so the next attempt can ask only for the missing suffix.
    archive
        .flush()
        .map_err(|error| format!("Could not save the qOS installer: {error}"))?;
    result.map_err(|error| format!("The qOS installer download was interrupted: {error}"))
}

fn append_range(
    response: reqwest::blocking::Response,
    archive_part: &Path,
    partial_size: u64,
    expected_size: u64,
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
    let downloaded = copy_download(response, &mut archive, response_size)?;
    let total = archive
        .metadata()
        .map_or(u64::MAX, |metadata| metadata.len());
    if downloaded == response_size && total == expected_size {
        return Ok(true);
    }
    if downloaded > response_size || total > expected_size {
        let _ = fs::remove_file(archive_part);
        return Err("The qOS installer range response exceeded its declared boundary.".into());
    }
    Err("The qOS installer download was interrupted before it completed.".into())
}

fn download_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    trusted_origin: &reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
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
            .map_err(|error| format!("Could not download the qOS installer: {error}"))?;
        if !same_origin(response.url(), trusted_origin) {
            return Err("The release download redirected to an untrusted origin.".into());
        }

        if resume && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            if append_range(response, archive_part, partial_size, expected_size)? {
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
                .map_err(|error| format!("Could not download the qOS installer: {error}"));
        }

        let response = response
            .error_for_status()
            .map_err(|error| format!("Could not download the qOS installer: {error}"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("The qOS installer server returned an invalid full response.".into());
        }
        if response
            .content_length()
            .is_some_and(|length| length != expected_size)
        {
            if resume {
                let _ = fs::remove_file(archive_part);
            }
            return Err("The qOS installer download has an unexpected size.".into());
        }
        // A 200 response to a ranged request means Range was ignored. Truncate
        // first, then safely consume that response as the new complete file.
        let mut archive = File::create(archive_part)
            .map_err(|error| format!("Could not create the download cache: {error}"))?;
        let downloaded = copy_download(response, &mut archive, expected_size)?;
        let total = archive
            .metadata()
            .map_or(u64::MAX, |metadata| metadata.len());
        if downloaded == expected_size && total == expected_size {
            return Ok(());
        }
        if total > expected_size {
            let _ = fs::remove_file(archive_part);
        }
        return Err("The qOS installer download was interrupted before it completed.".into());
    }

    Err("The qOS installer server did not honor a safe download restart.".into())
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
    let client = reqwest::blocking::Client::builder()
        .user_agent("qOS-Imager/1")
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| format!("Could not initialize the downloader: {error}"))?;
    let trusted_origin = reqwest::Url::parse("https://code.podesta.ai/")
        .map_err(|error| format!("Could not initialize the trusted release origin: {error}"))?;
    download_archive(
        &client,
        &release.url,
        &trusted_origin,
        &archive_part,
        release.archive_size,
    )?;
    if sha256(&archive_part)? != release.archive_sha256 {
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

#[cfg(test)]
mod internal_catalog_tests {
    use super::*;
    use serde_json::{Value, json};
    fn media() -> Value {
        json!({"name":"qOS","version":"test","channel":"test","arch":"x86_64",
            "url":"https://code.podesta.ai/test.img.zst","archive_sha256":"a".repeat(64),
            "archive_size":42,"image_sha256":"b".repeat(64),"image_size":84})
    }
    fn catalog(entry: Value) -> String {
        json!({"schema":1,"tag":"imager-vtest","releases":[entry]}).to_string()
    }
    #[test]
    fn legacy_media_still_parses() {
        assert!(parse_catalog(&catalog(media())).is_ok());
    }
    #[test]
    fn explicit_internal_purpose_never_falls_through_as_media() {
        let mut value = media();
        value["purpose"] = json!("qos-internal-installer");
        value["layout"] = json!("internal-bundle-v1");
        assert!(parse_catalog(&catalog(value)).is_err());
    }
    #[test]
    fn catalog_rejects_unknown_fields_and_bad_foreign_entries() {
        let mut value = media();
        value["unexpected"] = json!(true);
        assert!(parse_catalog(&catalog(value)).is_err());
        for (field, bad) in [
            ("archive_sha256", json!("invalid")),
            ("url", json!("https://evil.example/file")),
            ("image_size", json!(0)),
        ] {
            let mut value = media();
            value["arch"] = json!("aarch64");
            value[field] = bad;
            assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
        }
    }
    fn internal(manifest: &[u8]) -> Value {
        json!({"purpose":"qos-internal-installer","layout":"internal-bundle-v1",
            "release_id":"fixture-only","arch":"x86_64",
            "manifest_url":"https://code.podesta.ai/fixture/manifest.json",
            "manifest_sha256":format!("{:x}",Sha256::digest(manifest)),"manifest_size":manifest.len()})
    }
    fn selection(value: Value) -> InternalRelease {
        match parse_catalog(&catalog(value))
            .unwrap()
            .releases
            .pop()
            .unwrap()
        {
            CatalogRelease::Internal(metadata) => InternalRelease { metadata },
            _ => panic!("internal entry became media"),
        }
    }
    #[test]
    fn internal_entries_remain_separate_and_bind_exact_manifest_bytes() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        let value = internal(&bytes);
        assert!(
            media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64").is_empty()
        );
        let release = selection(value);
        assert!(release.verify_manifest(&bytes).is_ok());
        let mut longer = bytes.clone();
        longer.push(b' ');
        assert!(release.verify_manifest(&longer).is_err());
        let mut changed_manifest = crate::setup::internal_bundle::tests::fixture();
        changed_manifest["boot_binding"]["uki_sha256"] = json!("c".repeat(64));
        let changed = crate::setup::internal_bundle::tests::bytes(&changed_manifest);
        assert_eq!(changed.len(), bytes.len());
        let valid = crate::setup::internal_bundle::validate_internal_bundle(&changed).unwrap();
        assert_eq!(valid.release_id(), release.release_id());
        assert_eq!(valid.architecture(), release.architecture());
        assert!(release.verify_manifest(&changed).is_err());
    }
    #[test]
    fn internal_selection_rejects_release_and_architecture_mismatch() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        for (key, value) in [
            ("release_id", json!("other-release")),
            ("arch", json!("aarch64")),
        ] {
            let mut entry = internal(&bytes);
            entry[key] = value;
            assert!(
                selection(entry).verify_manifest(&bytes).is_err(),
                "accepted {key}"
            );
        }
    }

    #[test]
    fn explicit_media_pair_and_internal_catalog_profiles_are_strict() {
        let mut tagged = media();
        tagged["purpose"] = json!("removable-media");
        tagged["layout"] = json!("whole-disk-image");
        assert_eq!(
            media_releases(parse_catalog(&catalog(tagged.clone())).unwrap(), "x86_64").len(),
            1
        );
        for bad in [Value::Null, json!("unknown"), json!(true)] {
            let mut value = tagged.clone();
            value["purpose"] = bad;
            assert!(parse_catalog(&catalog(value)).is_err());
        }
        tagged.as_object_mut().unwrap().remove("layout");
        assert!(parse_catalog(&catalog(tagged)).is_err());
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        for (field, bad) in [
            ("layout", json!("whole-disk-image")),
            ("manifest_size", json!(65537)),
            ("manifest_sha256", json!("A".repeat(64))),
            ("purpose", json!("removable-media")),
            (
                "manifest_url",
                json!("https://code.podesta.ai:444/manifest"),
            ),
        ] {
            let mut value = internal(&bytes);
            value[field] = bad;
            assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
        }
        assert!(bundled_internal_releases().unwrap().is_empty());
        assert!(bundled_releases().unwrap().is_empty());
        let raw = catalog(media()).replacen("\"arch\":", "\"arch\":\"x86_64\",\"arch\":", 1);
        assert!(parse_catalog(&raw).is_err());
    }
    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
    #[test]
    fn representation_catalog_must_be_an_object() {
        let raw = catalog(media());
        assert!(parse_catalog(&raw).is_ok());
        let value: Value = serde_json::from_str(&raw).unwrap();
        let array = positional_record(&value, "schema tag releases");
        assert!(parse_catalog(&array.to_string()).is_err());
    }
    #[test]
    fn representation_media_entry_must_be_an_object() {
        let mut value = media();
        value["purpose"] = json!("removable-media");
        value["layout"] = json!("whole-disk-image");
        assert!(parse_catalog(&catalog(value.clone())).is_ok());
        let array = positional_record(
            &value,
            "purpose layout name version channel arch url archive_sha256 archive_size image_sha256 image_size",
        );
        assert!(parse_catalog(&catalog(array)).is_err());
    }
    #[test]
    fn representation_internal_entry_must_be_an_object() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        let value = internal(&bytes);
        assert!(parse_catalog(&catalog(value.clone())).is_ok());
        let array = positional_record(
            &value,
            "purpose layout release_id arch manifest_url manifest_sha256 manifest_size",
        );
        assert!(parse_catalog(&catalog(array)).is_err());
    }
    #[test]
    fn representation_internal_purpose_and_layout_must_be_strings() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        let entry = internal(&bytes);
        assert!(parse_catalog(&catalog(entry.clone())).is_ok());
        let accepted: Vec<_> = ["purpose", "layout"]
            .into_iter()
            .filter(|field| {
                let mut value = entry.clone();
                value[*field] = json!({entry[*field].as_str().unwrap():null});
                parse_catalog(&catalog(value)).is_ok()
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "accepted object-shaped tags: {accepted:?}"
        );
    }
    #[test]
    fn representation_media_purpose_and_layout_must_be_strings() {
        let mut entry = media();
        entry["purpose"] = json!("removable-media");
        entry["layout"] = json!("whole-disk-image");
        assert!(parse_catalog(&catalog(entry.clone())).is_ok());
        let accepted: Vec<_> = ["purpose", "layout"]
            .into_iter()
            .filter(|field| {
                let mut value = entry.clone();
                value[*field] = json!({entry[*field].as_str().unwrap():null});
                parse_catalog(&catalog(value)).is_ok()
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "accepted object-shaped tags: {accepted:?}"
        );
    }
    #[test]
    fn representation_checks_do_not_collapse_duplicate_catalog_keys() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );
        for entry in [media(), internal(&bytes)] {
            let raw = catalog(entry);
            assert!(parse_catalog(&raw).is_ok());
            for (marker, replacement) in [
                ("\"schema\":1", "\"schema\":1,\"schema\":1"),
                (
                    "\"arch\":\"x86_64\"",
                    "\"arch\":\"x86_64\",\"arch\":\"x86_64\"",
                ),
            ] {
                let changed = raw.replacen(marker, replacement, 1);
                assert_ne!(changed, raw);
                assert!(parse_catalog(&changed).is_err());
            }
        }
    }
}
