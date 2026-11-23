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
        io::{Read, Seek, SeekFrom},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt as StdOpenOptionsExt},
        sync::atomic::AtomicU64,
        time::Duration,
    };
    use tilo_net::http::fetch;

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

    /// The downloader for this package's own origin. The caller validated the
    /// package URL's scheme and host; every redirect is checked against it.
    fn downloader(url: &reqwest::Url) -> Result<fetch::Downloader, String> {
        let mut origin = url.clone();
        origin.set_path("/");
        origin.set_query(None);
        origin.set_fragment(None);
        fetch::Downloader::new(
            origin,
            Duration::from_secs(60),
            Some("Tilo-Desktop-Setup/1"),
        )
        .map_err(|error| format!("Could not initialize Desktop downloader: {error}"))
    }

    fn fetch_error(error: fetch::Error) -> String {
        match error {
            fetch::Error::Cancelled => "Desktop package download cancelled.".into(),
            fetch::Error::Digest => "The Desktop package failed SHA-256 verification.".into(),
            fetch::Error::Size { .. } => "Desktop package response has an unexpected size.".into(),
            fetch::Error::Incomplete(_) | fetch::Error::Paused => {
                "Desktop package transfer remained incomplete after bounded retries.".into()
            }
            fetch::Error::Trusted(_) => "Untrusted Desktop package response origin.".into(),
            fetch::Error::Io(error) => io_error(error),
            fetch::Error::Protocol(detail) | fetch::Error::Hook(detail) => {
                format!("Desktop package response was not a valid download: {detail}")
            }
            fetch::Error::Client(detail) => {
                format!("Could not initialize Desktop downloader: {detail}")
            }
        }
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
        url: reqwest::Url,
    ) -> Result<PathBuf, String> {
        cancelled(cancel)?;
        let dir = open_cache(cache)?;
        let name = format!("{}.{}", release.sha256, release.package_format.extension());
        if let Some(mut existing) = open_cached(&dir, &name)? {
            verify(&mut existing, release, cancel)?;
            return checked_path(&dir, cache, &name);
        }
        let mut staging = Staging::create(&dir)?;
        downloader(&url)?
            .fetch(
                fetch::Request {
                    url,
                    size: release.byte_size,
                    sha256: Some(&release.sha256),
                    cancel,
                    pause: None,
                },
                &mut staging.file,
                &mut (),
                &mut |_| {},
            )
            .map_err(fetch_error)?;
        verify(&mut staging.file, release, cancel)?;
        staging.file.sync_all().map_err(io_error)?;
        cancelled(cancel)?;
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
            account_credentials: None,
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
        stop.store(true, Ordering::Release);
        handle.join().unwrap();
        let observed = requests.lock().unwrap().clone();
        (root, result, observed)
    }

    tilo_test_env::scenario! {
        fn a_desktop_download_keeps_only_verified_private_bytes;

        phase desktop_download_rejects_bad_objects_and_transports;
        {
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

        phase desktop_download_cancellation_and_interruption_discard_staging;
        {
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

        phase desktop_download_cache_is_private_and_rejects_links_and_swaps;
        {
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
}
