//! Verified, unprivileged Desktop package cache. No installation happens here.

use super::desktop_catalog::{DesktopRelease, trusted_url};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[cfg(unix)]
use std::sync::atomic::Ordering;

#[cfg(unix)]
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Desktop package download cancelled.".into())
    } else {
        Ok(())
    }
}

/// Download and authenticate a package into a private, content-addressed cache.
/// The caller supplies a cache directory whose parent already exists. No elevation occurs.
/// A returned pathname is not privileged authorization: the install helper must verify again.
///
/// # Errors
/// Refuses invalid metadata, untrusted transport, unsafe cache objects, cancellation,
/// incomplete transfers, and size/digest mismatches. Staging is removed on failure.
pub fn download_package(
    release: &DesktopRelease,
    cache: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    release.validate_metadata()?;
    let origin = trusted_url(&release.url)?;
    download(release, cache, cancel, origin)
}

#[cfg(not(unix))]
fn download(
    _: &DesktopRelease,
    _: &Path,
    _: &AtomicBool,
    _: reqwest::Url,
) -> Result<PathBuf, String> {
    Err("Desktop package installation is unavailable on this platform.".into())
}

#[cfg(unix)]
fn download(
    release: &DesktopRelease,
    cache: &Path,
    cancel: &AtomicBool,
    origin: reqwest::Url,
) -> Result<PathBuf, String> {
    unix::download(release, cache, cancel, origin)
}

#[cfg(test)]
#[cfg(unix)]
fn download_for_test(
    release: &DesktopRelease,
    cache: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    release.validate_metadata()?;
    let url = reqwest::Url::parse(&release.url).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    download(release, cache, cancel, url)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::FollowSymlinks;
    use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File},
        io::{Read, Seek, SeekFrom, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt as StdOpenOptionsExt},
        sync::atomic::AtomicU64,
        time::Duration,
    };

    fn io_error(error: std::io::Error) -> String {
        format!("Desktop package cache: {error}")
    }

    fn open_cache(path: &Path) -> Result<Dir, String> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(io_error(error)),
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)
            .map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if metadata.uid() != tilo_process::effective_uid() || metadata.mode() & 0o777 != 0o700 {
            return Err(
                "The Desktop package cache must be owned by this user with mode 0700.".into(),
            );
        }
        Ok(Dir::from_std_file(file))
    }

    fn verify(
        file: &mut File,
        release: &DesktopRelease,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let metadata = file.metadata().map_err(io_error)?;
        // No executable, shared, hard-linked, or nonregular cache objects are accepted.
        if !metadata.is_file()
            || metadata.len() != release.byte_size
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.uid() != tilo_process::effective_uid()
        {
            return Err(
                "The cached Desktop package has invalid type, permissions, or size.".into(),
            );
        }
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let mut digest = Sha256::new();
        let mut remaining = release.byte_size;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            cancelled(cancel)?;
            let limit =
                usize::try_from(remaining.min(buffer.len() as u64)).map_err(|e| e.to_string())?;
            let count = file.read(&mut buffer[..limit]).map_err(io_error)?;
            if count == 0 {
                return Err("The cached Desktop package is truncated.".into());
            }
            digest.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if file.read(&mut buffer[..1]).map_err(io_error)? != 0
            || hex::encode(digest.finalize()) != release.sha256
        {
            return Err("The Desktop package failed SHA-256 verification.".into());
        }
        cancelled(cancel)
    }

    fn open_cached(dir: &Dir, name: &str) -> Result<Option<File>, String> {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        match dir.open_with(name, &options) {
            Ok(file) => Ok(Some(file.into_std())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    struct Staging<'a> {
        dir: &'a Dir,
        name: String,
        file: File,
    }
    impl Drop for Staging<'_> {
        fn drop(&mut self) {
            let _ = self.dir.remove_file(&self.name);
        }
    }
    impl<'a> Staging<'a> {
        fn create(dir: &'a Dir) -> Result<Self, String> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..100 {
                let name = format!(
                    ".download-{}-{}.part",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                let mut options = OpenOptions::new();
                options
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .follow(FollowSymlinks::No);
                match dir.open_with(&name, &options) {
                    Ok(file) => {
                        return Ok(Self {
                            dir,
                            name,
                            file: file.into_std(),
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => return Err(io_error(error)),
                }
            }
            Err("Could not allocate exclusive Desktop package staging.".into())
        }
    }

    fn same_origin(actual: &reqwest::Url, trusted: &reqwest::Url) -> bool {
        actual.scheme() == trusted.scheme()
            && actual.host_str() == trusted.host_str()
            && actual.port_or_known_default() == trusted.port_or_known_default()
            && actual.username().is_empty()
            && actual.password().is_none()
            && actual.fragment().is_none()
    }

    fn client(origin: reqwest::Url) -> Result<reqwest::blocking::Client, String> {
        tilo_net::http::blocking_client()
            .user_agent("Tilo-Desktop-Setup/1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 || !same_origin(attempt.url(), &origin) {
                    attempt.error(
                        "Desktop package redirect left its trusted origin or exceeded its limit",
                    )
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|error| format!("Could not initialize Desktop downloader: {error}"))
    }

    fn range_size(response: &reqwest::blocking::Response, offset: u64, total: u64) -> Option<u64> {
        let value = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)?
            .to_str()
            .ok()?
            .strip_prefix("bytes ")?;
        let (range, declared_total) = value.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        let (start, end, declared_total) = (
            start.parse::<u64>().ok()?,
            end.parse::<u64>().ok()?,
            declared_total.parse::<u64>().ok()?,
        );
        if start != offset || end < start || end >= total || declared_total != total {
            return None;
        }
        let count = end - start + 1;
        response
            .content_length()
            .is_none_or(|length| length == count)
            .then_some(count)
    }

    fn reset(file: &mut File) -> Result<(), String> {
        file.set_len(0).map_err(io_error)?;
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        Ok(())
    }

    fn transfer(
        release: &DesktopRelease,
        file: &mut File,
        cancel: &AtomicBool,
        origin: reqwest::Url,
    ) -> Result<(), String> {
        let client = client(origin.clone())?;
        // Bounded resume uses only bytes in this exclusive, held staging handle.
        // Terminal failures/cancellation discard staging; no editable partial file is trusted.
        for _ in 0..3 {
            cancelled(cancel)?;
            let offset = file.metadata().map_err(io_error)?.len();
            let mut request = client
                .get(&release.url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = request
                .send()
                .map_err(|error| format!("Desktop package request failed: {error}"))?;
            cancelled(cancel)?;
            if !same_origin(response.url(), &origin) {
                return Err("Untrusted Desktop package response origin.".into());
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|value| value != "identity")
            {
                return Err("Encoded Desktop package responses are unsupported.".into());
            }
            let expected = if offset > 0
                && response.status() == reqwest::StatusCode::PARTIAL_CONTENT
            {
                let Some(size) = range_size(&response, offset, release.byte_size) else {
                    reset(file)?;
                    continue;
                };
                size
            } else if offset > 0 && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
            {
                reset(file)?;
                continue;
            } else if response.status() == reqwest::StatusCode::OK {
                if response
                    .content_length()
                    .is_some_and(|size| size != release.byte_size)
                {
                    return Err("Desktop package response has an unexpected size.".into());
                }
                reset(file)?;
                release.byte_size
            } else {
                return Err(format!(
                    "Desktop package server returned {}.",
                    response.status()
                ));
            };
            let mut received = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                cancelled(cancel)?;
                let limit = usize::try_from((expected - received + 1).min(buffer.len() as u64))
                    .map_err(|e| e.to_string())?;
                let count = match response.read(&mut buffer[..limit]) {
                    Ok(count) => count,
                    Err(_) => break, // retry the safely bounded prefix below
                };
                cancelled(cancel)?;
                if count == 0 {
                    break;
                }
                if count as u64 > expected - received {
                    return Err("Desktop package response exceeded its size boundary.".into());
                }
                file.write_all(&buffer[..count]).map_err(io_error)?;
                received += count as u64;
            }
            if received == expected && file.metadata().map_err(io_error)?.len() == release.byte_size
            {
                return cancelled(cancel);
            }
        }
        Err("Desktop package transfer remained incomplete after bounded retries.".into())
    }

    fn checked_path(dir: &Dir, cache: &Path, name: &str) -> Result<PathBuf, String> {
        let pinned = dir.dir_metadata().map_err(io_error)?;
        let current = fs::symlink_metadata(cache).map_err(io_error)?;
        if !current.is_dir()
            || current.dev() != cap_fs_ext::MetadataExt::dev(&pinned)
            || current.ino() != cap_fs_ext::MetadataExt::ino(&pinned)
            || current.mode() & 0o777 != 0o700
        {
            return Err("The Desktop package cache directory changed during download.".into());
        }
        Ok(cache.join(name))
    }

    pub(super) fn download(
        release: &DesktopRelease,
        cache: &Path,
        cancel: &AtomicBool,
        origin: reqwest::Url,
    ) -> Result<PathBuf, String> {
        cancelled(cancel)?;
        let dir = open_cache(cache)?;
        let name = format!("{}.{}", release.sha256, release.package_format.extension());
        if let Some(mut existing) = open_cached(&dir, &name)? {
            verify(&mut existing, release, cancel)?;
            return checked_path(&dir, cache, &name);
        }
        let mut staging = Staging::create(&dir)?;
        transfer(release, &mut staging.file, cancel, origin)?;
        verify(&mut staging.file, release, cancel)?;
        staging.file.sync_all().map_err(io_error)?;
        cancelled(cancel)?;
        // hard_link publishes without replacing any concurrent winner or hostile object.
        match dir.hard_link(&staging.name, &dir, &name) {
            Ok(()) => {
                dir.remove_file(&staging.name).map_err(io_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(io_error(error)),
        }
        let mut published =
            open_cached(&dir, &name)?.ok_or("Desktop package publication disappeared.")?;
        verify(&mut published, release, cancel)?;
        checked_path(&dir, cache, &name)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::setup::desktop_catalog::{DesktopRelease, PackageFormat};
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };

    const BYTES: &[u8] = b"verified desktop package";

    fn release(url: String) -> DesktopRelease {
        DesktopRelease {
            id: "test".into(),
            version: "test".into(),
            distro_id: "fedora".into(),
            distro_version: "44".into(),
            architecture: "aarch64".into(),
            package_format: PackageFormat::Rpm,
            url,
            byte_size: BYTES.len() as u64,
            sha256: hex::encode(Sha256::digest(BYTES)),
            account_custody: None,
            verified_runtime: None,
        }
    }
    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        [
            format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
            body,
        ]
        .concat()
    }
    fn full(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            body,
        )
    }
    /// Serve `replies` in order until they run out or `stop` is set, as
    /// the client asks for them.
    fn server(
        replies: Vec<Vec<u8>>,
        stop: Arc<AtomicBool>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let handle = thread::spawn(move || {
            for reply in replies {
                let deadline = tilo_test_env::hang_deadline();
                let mut stream: TcpStream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) || std::time::Instant::now() >= deadline
                            {
                                return;
                            }
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                // Darwin can inherit O_NONBLOCK from the listener. A
                // temporary WouldBlock is not a failed request read.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                observed
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap().to_ascii_lowercase());
                let _ = stream.write_all(&reply);
            }
        });
        (url, requests, handle)
    }
    fn run(replies: Vec<Vec<u8>>) -> (tempfile::TempDir, Result<PathBuf, String>, Vec<String>) {
        let root = tempfile::tempdir().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (url, requests, handle) = server(replies, Arc::clone(&stop));
        let result = download_for_test(
            &release(url),
            &root.path().join("cache"),
            &AtomicBool::new(false),
        );
        // A reply the download did not ask for is never served.
        stop.store(true, Ordering::Release);
        handle.join().unwrap();
        let observed = requests.lock().unwrap().clone();
        (root, result, observed)
    }

    /// Range resume and restart.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_download_resumes_or_restarts_ranges_without_duplicate_bytes() {
        {
            let split = 8;
            let (_root, result, requests) = run(vec![
                response(
                    "200 OK",
                    &format!("Content-Length: {}\r\n", BYTES.len()),
                    &BYTES[..split],
                ),
                response(
                    "206 Partial Content",
                    &format!(
                        "Content-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n",
                        BYTES.len() - split,
                        split,
                        BYTES.len() - 1,
                        BYTES.len()
                    ),
                    &BYTES[split..],
                ),
            ]);
            assert_eq!(std::fs::read(result.unwrap()).unwrap(), BYTES);
            assert!(requests[1].contains("range: bytes=8-"));
        }

        {
            let (_root, result, requests) = run(vec![
                response(
                    "200 OK",
                    &format!("Content-Length: {}\r\n", BYTES.len()),
                    &BYTES[..8],
                ),
                full(BYTES),
            ]);
            assert_eq!(std::fs::read(result.unwrap()).unwrap(), BYTES);
            assert!(requests[1].contains("range: bytes=8-"));
        }

        {
            let (_root, result, requests) = run(vec![
                response(
                    "200 OK",
                    &format!("Content-Length: {}\r\n", BYTES.len()),
                    &BYTES[..8],
                ),
                response(
                    "206 Partial Content",
                    "Content-Range: bytes 7-23/24\r\nContent-Length: 17\r\n",
                    &BYTES[7..],
                ),
                full(BYTES),
            ]);
            assert_eq!(std::fs::read(result.unwrap()).unwrap(), BYTES);
            assert!(!requests[2].contains("range:"));
        }
    }
    /// Size, digest, cache revalidation, redirects and transport rejection.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_download_rejects_bad_objects_redirects_and_transports() {
        {
            for reply in [
                full(b"an oversized package that must fail"),
                full(&vec![b'x'; BYTES.len()]),
                response("200 OK", "", b"an oversized package that must fail"),
            ] {
                let (root, result, _) = run(vec![reply]);
                assert!(result.is_err());
                assert_eq!(
                    std::fs::read_dir(root.path().join("cache"))
                        .unwrap()
                        .count(),
                    0
                );
            }
        }

        {
            let root = tempfile::tempdir().unwrap();
            let (url, _, handle) = server(vec![full(BYTES)], Arc::default());
            let metadata = release(url);
            let cache = root.path().join("cache");
            let path = download_for_test(&metadata, &cache, &AtomicBool::new(false)).unwrap();
            handle.join().unwrap();
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                format!("{}.rpm", metadata.sha256)
            );
            assert_eq!(
                download_for_test(&metadata, &cache, &AtomicBool::new(false)).unwrap(),
                path
            );
            std::fs::write(&path, b"tampered").unwrap();
            assert!(download_for_test(&metadata, &cache, &AtomicBool::new(false)).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"tampered");
        }

        {
            let (_root, result, requests) = run(vec![
                response(
                    "302 Found",
                    "Location: /renamed\r\nContent-Length: 0\r\n",
                    b"",
                ),
                full(BYTES),
            ]);
            assert!(result.is_ok());
            assert!(requests[1].starts_with("get /renamed"));
            let (_root, result, _) = run(vec![response(
                "302 Found",
                "Location: http://localhost:1/escape\r\nContent-Length: 0\r\n",
                b"",
            )]);
            assert!(result.is_err());
        }

        {
            let root = tempfile::tempdir().unwrap();
            assert!(
                download_package(
                    &release("http://127.0.0.1:1/package".into()),
                    root.path(),
                    &AtomicBool::new(false)
                )
                .is_err()
            );
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_download_cancellation_and_interruption_discard_staging() {
        {
            let root = tempfile::tempdir().unwrap();
            assert!(
                download_for_test(
                    &release("http://127.0.0.1:1/package".into()),
                    root.path(),
                    &AtomicBool::new(true)
                )
                .is_err()
            );
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }

        {
            let root = tempfile::tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/package", listener.local_addr().unwrap());
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).unwrap();
                signal.store(true, Ordering::Release);
                let _ = stream.write_all(&full(BYTES));
            });
            let cache = root.path().join("cache");
            assert!(
                download_for_test(&release(url), &cache, &cancel)
                    .unwrap_err()
                    .contains("cancelled")
            );
            handle.join().unwrap();
            assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
        }

        {
            let replies = (0..3)
                .map(|_| {
                    response(
                        "200 OK",
                        &format!("Content-Length: {}\r\n", BYTES.len()),
                        &BYTES[..8],
                    )
                })
                .collect();
            let (root, result, requests) = run(replies);
            assert!(result.is_err());
            assert_eq!(requests.len(), 3);
            assert_eq!(
                std::fs::read_dir(root.path().join("cache"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
    /// Cache directory security.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_download_cache_is_private_and_rejects_links_and_swaps() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let root = tempfile::tempdir().unwrap();
            let (url, _, handle) = server(vec![full(BYTES)], Arc::default());
            let metadata = release(url);
            let cache = root.path().join("cache");
            let path = download_for_test(&metadata, &cache, &AtomicBool::new(false)).unwrap();
            handle.join().unwrap();
            assert_eq!(
                std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::remove_file(&path).unwrap();
            let external = root.path().join("external");
            std::fs::write(&external, BYTES).unwrap();
            symlink(&external, &path).unwrap();
            assert!(download_for_test(&metadata, &cache, &AtomicBool::new(false)).is_err());
            let linked = root.path().join("linked");
            symlink(&cache, &linked).unwrap();
            assert!(download_for_test(&metadata, &linked, &AtomicBool::new(false)).is_err());
        }

        {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let moved = root.path().join("moved");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/package", listener.local_addr().unwrap());
            let old = cache.clone();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).unwrap();
                std::fs::rename(&old, &moved).unwrap();
                std::fs::create_dir(&old).unwrap();
                let _ = stream.write_all(&full(BYTES));
            });
            let result = download_for_test(&release(url), &cache, &AtomicBool::new(false));
            handle.join().unwrap();
            assert!(
                result.is_err(),
                "must not return a pathname in a substituted cache directory"
            );
        }
    }
}
