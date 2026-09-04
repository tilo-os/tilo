//! Trusted qOS release catalog and verified medium download/cache.
//!
//! The catalog is embedded into each platform-signed Imager package at build
//! time.  A release therefore cannot substitute its URL or digest without also
//! replacing the signed host application.  Both the compressed transport and
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
