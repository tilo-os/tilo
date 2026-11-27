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

#[cfg(unix)]
pub fn download_package(
    release: &DesktopRelease,
    cache: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    release.validate_metadata()?;
    let origin = trusted_url(&release.url)?;
    unix::download(release, cache, cancel, origin)
}

#[cfg(not(unix))]
pub fn download_package(_: &DesktopRelease, _: &Path, _: &AtomicBool) -> Result<PathBuf, String> {
    Err("Desktop package installation is unavailable on this platform.".into())
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
    unix::download(release, cache, cancel, url)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::verified_cache::{Cache, downloader, fetch_error, message as cache_message};
    use std::time::Duration;
    use tilo_net::http::fetch;

    pub(super) fn download(
        release: &DesktopRelease,
        cache: &Path,
        cancel: &AtomicBool,
        url: reqwest::Url,
    ) -> Result<PathBuf, String> {
        cancelled(cancel)?;
        let noun = "Desktop package";
        let cache = Cache::open(cache).map_err(|error| cache_message(noun, error))?;
        let name = format!("{}.{}", release.sha256, release.package_format.extension());
        if let Some(path) = cache
            .verified(&name, release.byte_size, &release.sha256, cancel)
            .map_err(|error| cache_message(noun, error))?
        {
            return Ok(path);
        }
        let mut staging = cache
            .staging()
            .map_err(|error| cache_message(noun, error))?;
        let mut origin = url.clone();
        origin.set_path("/");
        origin.set_query(None);
        origin.set_fragment(None);
        downloader(origin, Duration::from_secs(60), "Tilo-Desktop-Setup/1")?
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
            .map_err(|error| fetch_error(noun, error))?;
        cache
            .publish(staging, &name, release.byte_size, &release.sha256, cancel)
            .map_err(|error| cache_message(noun, error))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::setup::desktop_catalog::{DesktopRelease, PackageFormat};
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use test_http::{Reply, Responder};

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
            verified_runtime: None,
        }
    }
    fn ok(body: &[u8]) -> Reply {
        Reply::status(200).body(body)
    }
    fn package_url(responder: &Responder) -> String {
        format!("{}/package", responder.url())
    }
    fn run(replies: Vec<Reply>) -> (tempfile::TempDir, Result<PathBuf, String>, Responder) {
        let root = tempfile::tempdir().unwrap();
        let responder = Responder::start(replies);
        let result = download_for_test(
            &release(package_url(&responder)),
            &root.path().join("cache"),
            &AtomicBool::new(false),
        );
        (root, result, responder)
    }

    tilo_test_env::scenario! {
        fn a_desktop_download_keeps_only_verified_private_bytes;

        phase desktop_download_rejects_bad_objects_and_transports;
        {
            {
                for reply in [
                    ok(b"an oversized package that must fail"),
                    ok(&vec![b'x'; BYTES.len()]),
                    Reply::status(200)
                        .without_content_length()
                        .body(b"an oversized package that must fail"),
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
                let responder = Responder::start(vec![ok(BYTES)]);
                let metadata = release(package_url(&responder));
                let cache = root.path().join("cache");
                let path = download_for_test(&metadata, &cache, &AtomicBool::new(false)).unwrap();
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
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let responder = Responder::serve(move |peer| {
                    peer.request();
                    signal.store(true, Ordering::Release);
                    peer.reply(&ok(BYTES));
                });
                let cache = root.path().join("cache");
                assert!(
                    download_for_test(&release(package_url(&responder)), &cache, &cancel)
                        .unwrap_err()
                        .contains("cancelled")
                );
                assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
            }

            {
                let replies = (0..3).map(|_| ok(BYTES).cut_short(true)).collect();
                let (root, result, responder) = run(replies);
                assert!(result.is_err());
                assert_eq!(responder.drain_requests().len(), 3);
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
                let responder = Responder::start(vec![ok(BYTES)]);
                let metadata = release(package_url(&responder));
                let cache = root.path().join("cache");
                let path = download_for_test(&metadata, &cache, &AtomicBool::new(false)).unwrap();
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
                let old = cache.clone();
                let responder = Responder::serve(move |peer| {
                    peer.request();
                    std::fs::rename(&old, &moved).unwrap();
                    std::fs::create_dir(&old).unwrap();
                    peer.reply(&ok(BYTES));
                });
                let result =
                    download_for_test(&release(package_url(&responder)), &cache, &AtomicBool::new(false));
                assert!(
                    result.is_err(),
                    "must not return a pathname in a substituted cache directory"
                );
            }
        }
    }
}
