//! Asynchronous publication of workspace file capabilities to `quick-fs`.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};

const DEFAULT_FS_AUTHORITY_CACHE_DIR: &str = "/var/cache/quick-fs";
const DEFAULT_FS_AUTHORITY_PUBLISHER: &str = "/usr/bin/fs";
const MAX_FS_AUTHORITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub fn permission_profile_document_key(name: &str) -> String {
    blake3::hash(name.trim().to_lowercase().as_bytes())
        .to_hex()
        .to_string()
}

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

/// Signalled whenever the publication worker retires, so a waiter observes
/// the idle transition itself instead of sampling the queue on a timer.
fn fs_authority_publication_retired() -> &'static Condvar {
    static RETIRED: OnceLock<Condvar> = OnceLock::new();
    RETIRED.get_or_init(Condvar::new)
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
pub fn schedule_fs_authority_publication(acl: PathBuf) {
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
        fs_authority_publication_retired().notify_all();
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
            fs_authority_publication_retired().notify_all();
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

/// Block until the publication worker has retired with nothing queued, or
/// `timeout` passes. Returns whether it retired.
pub fn wait_for_fs_authority_publication_idle(timeout: std::time::Duration) -> bool {
    let Ok(queue) = fs_authority_publication_queue().lock() else {
        return false;
    };
    fs_authority_publication_retired()
        .wait_timeout_while(queue, timeout, |queue| {
            queue.running || queue.pending.is_some()
        })
        .is_ok_and(|(queue, _)| !queue.running && queue.pending.is_none())
}
