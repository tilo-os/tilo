//! Asynchronous publication of workspace file capabilities to `quick-fs`.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const DEFAULT_FS_AUTHORITY_CACHE_DIR: &str = "/var/cache/quick-fs";
const DEFAULT_FS_AUTHORITY_PUBLISHER: &str = "/usr/bin/fs";
const MAX_FS_AUTHORITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug)]
struct FsAuthorityPublication {
    acl: PathBuf,
    cache_dir: PathBuf,
    publisher: PathBuf,
}

#[derive(Default)]
struct FsAuthorityPublicationQueue {
    pending: Option<FsAuthorityPublication>,
    running: bool,
}

fn fs_authority_publication_queue() -> &'static Mutex<FsAuthorityPublicationQueue> {
    static QUEUE: OnceLock<Mutex<FsAuthorityPublicationQueue>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(FsAuthorityPublicationQueue::default()))
}

fn fs_authority_publisher() -> PathBuf {
    std::env::var_os("TILO_FS_AUTHORITY_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || PathBuf::from(DEFAULT_FS_AUTHORITY_PUBLISHER),
            PathBuf::from,
        )
}

fn fs_authority_cache_dir() -> PathBuf {
    std::env::var_os("TILO_FS_AUTHORITY_CACHE_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || PathBuf::from(DEFAULT_FS_AUTHORITY_CACHE_DIR),
            PathBuf::from,
        )
}

/// Coalesce exports and submit them outside the compositor's mutation path.
/// The filesystem authority socket may be unavailable while its service is
/// starting; capped-backoff retries cover that normal boot ordering and a
/// later capability mutation always replaces the queued projection.
pub(super) fn schedule_fs_authority_publication(acl: PathBuf) {
    // Unit tests exercise this only with an explicit harmless publisher;
    // never let an isolated tree submit a fixture ACL to a real service.
    if cfg!(test) && std::env::var_os("TILO_FS_AUTHORITY_PUBLISHER").is_none() {
        return;
    }
    let publication = FsAuthorityPublication {
        acl,
        cache_dir: fs_authority_cache_dir(),
        publisher: fs_authority_publisher(),
    };
    let Ok(mut queue) = fs_authority_publication_queue().lock() else {
        tracing::warn!("filesystem authority publication queue is unavailable");
        return;
    };
    queue.pending = Some(publication);
    if queue.running {
        return;
    }
    queue.running = true;
    let spawned = std::thread::Builder::new()
        .name("tilo-fs-authority-publish".to_string())
        .spawn(run_fs_authority_publication_queue);
    if let Err(error) = spawned {
        queue.running = false;
        tracing::warn!(%error, "could not start filesystem authority publisher");
    }
}

fn run_fs_authority_publication_queue() {
    loop {
        let publication = match fs_authority_publication_queue().lock() {
            Ok(mut queue) => queue.pending.take(),
            Err(error) => {
                tracing::warn!(%error, "filesystem authority publication queue is poisoned");
                return;
            }
        };
        let Some(mut publication) = publication else {
            if let Ok(mut queue) = fs_authority_publication_queue().lock() {
                queue.running = false;
            }
            return;
        };
        let mut retry_delay = std::time::Duration::from_millis(100);
        loop {
            let result = std::process::Command::new(&publication.publisher)
                .arg("publish-authority")
                .arg("--cache-dir")
                .arg(&publication.cache_dir)
                .arg("--acl")
                .arg(&publication.acl)
                .status();
            match result {
                Ok(status) if status.success() => break,
                Ok(status) => tracing::warn!(
                    ?status,
                    acl = %publication.acl.display(),
                    "filesystem authority publication failed; will retry"
                ),
                Err(error) => tracing::warn!(
                    %error,
                    publisher = %publication.publisher.display(),
                    "filesystem authority publisher could not start; will retry"
                ),
            }
            // A new mutation supersedes the failed projection immediately;
            // otherwise retain this exact latest ACL until the daemon is
            // ready. The delay is bounded, while the background worker is
            // deliberately persistent rather than silently losing a grant.
            std::thread::sleep(retry_delay);
            retry_delay = retry_delay
                .saturating_mul(2)
                .min(MAX_FS_AUTHORITY_RETRY_DELAY);
            if let Ok(mut queue) = fs_authority_publication_queue().lock()
                && let Some(newer) = queue.pending.take()
            {
                publication = newer;
                retry_delay = std::time::Duration::from_millis(100);
            }
        }
    }
}

#[cfg(test)]
pub(super) fn wait_for_fs_authority_publication_idle(timeout: std::time::Duration) -> bool {
    let Some(deadline) = std::time::Instant::now().checked_add(timeout) else {
        return false;
    };
    loop {
        if fs_authority_publication_queue()
            .lock()
            .is_ok_and(|queue| !queue.running && queue.pending.is_none())
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
