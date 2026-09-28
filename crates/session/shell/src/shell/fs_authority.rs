//! Asynchronous publication of workspace file capabilities to `quick-fs`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

const DEFAULT_FS_AUTHORITY_CACHE_DIR: &str = "/var/cache/quick-fs";
const DEFAULT_FS_AUTHORITY_PUBLISHER: &str = "/usr/bin/fs";
const MAX_FS_AUTHORITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// Recent publisher attempts kept so a stalled publication can say why.
const MAX_RECORDED_FS_AUTHORITY_ATTEMPTS: usize = 16;
/// Tail of the publisher's stderr kept per recorded attempt.
const MAX_RECORDED_FS_AUTHORITY_STDERR: usize = 2048;

pub fn permission_profile_document_key(name: &str) -> String {
    blake3::hash(name.trim().to_lowercase().as_bytes())
        .to_hex()
        .to_string()
}

#[derive(Clone, Debug)]
struct FsAuthorityPublication {
    acl: PathBuf,
    cache_dir: PathBuf,
    /// Personal identity directory whose user root signs the authority.
    identity_dir: PathBuf,
    publisher: PathBuf,
}

#[derive(Debug)]
struct FsAuthorityPublicationAttempt {
    at: Instant,
    acl: PathBuf,
    outcome: String,
}

#[derive(Default)]
struct FsAuthorityPublicationQueue {
    pending: Option<FsAuthorityPublication>,
    running: bool,
    attempts: VecDeque<FsAuthorityPublicationAttempt>,
}

impl FsAuthorityPublicationQueue {
    fn record(&mut self, acl: PathBuf, outcome: String) {
        if self.attempts.len() == MAX_RECORDED_FS_AUTHORITY_ATTEMPTS {
            self.attempts.pop_front();
        }
        self.attempts.push_back(FsAuthorityPublicationAttempt {
            at: Instant::now(),
            acl,
            outcome,
        });
    }

    fn report(&self) -> String {
        let now = Instant::now();
        let mut report = format!(
            "running={} pending={}; recent publisher attempts (oldest first):",
            self.running,
            self.pending.as_ref().map_or_else(
                || "none".to_string(),
                |pending| pending.acl.display().to_string()
            ),
        );
        if self.attempts.is_empty() {
            report.push_str(" none");
        }
        for attempt in &self.attempts {
            let _ = write!(
                report,
                "\n  {:?} ago, acl {}: {}",
                now.saturating_duration_since(attempt.at),
                attempt.acl.display(),
                attempt.outcome
            );
        }
        report
    }
}

fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr
        .len()
        .saturating_sub(MAX_RECORDED_FS_AUTHORITY_STDERR);
    String::from_utf8_lossy(stderr.get(start..).unwrap_or_default())
        .trim()
        .to_string()
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
    let identity_dir = match runtime_identity::identity_dir_from_env() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "no personal identity directory to sign filesystem authority");
            return;
        }
    };
    let publication = FsAuthorityPublication {
        acl,
        cache_dir: fs_authority_cache_dir(),
        identity_dir,
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
            // Capture stderr so a failing publisher explains itself in the
            // journal and in the attempt history, not only by exit status.
            let result = std::process::Command::new(&publication.publisher)
                .arg("publish-authority")
                .arg("--cache-dir")
                .arg(&publication.cache_dir)
                .arg("--acl")
                .arg(&publication.acl)
                .arg("--identity-dir")
                .arg(&publication.identity_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            let (succeeded, outcome) = match result {
                Ok(output) if output.status.success() => (true, "published".to_string()),
                Ok(output) => {
                    let stderr = stderr_tail(&output.stderr);
                    tracing::warn!(
                        status = ?output.status,
                        %stderr,
                        acl = %publication.acl.display(),
                        "filesystem authority publication failed; will retry"
                    );
                    (false, format!("{}; stderr: {stderr:?}", output.status))
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        publisher = %publication.publisher.display(),
                        "filesystem authority publisher could not start; will retry"
                    );
                    (
                        false,
                        format!(
                            "publisher {} could not start: {error}",
                            publication.publisher.display()
                        ),
                    )
                }
            };
            if let Ok(mut queue) = fs_authority_publication_queue().lock() {
                queue.record(publication.acl.clone(), outcome);
            }
            if succeeded {
                break;
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
/// `timeout` passes. The worker's retirement is signalled, so this returns as
/// soon as publication settles.
///
/// # Errors
///
/// On timeout, or if the queue is poisoned, returns a report of the queue
/// state and the recent publisher attempts, including their stderr.
pub fn wait_for_fs_authority_publication_idle(timeout: std::time::Duration) -> Result<(), String> {
    let Ok(queue) = fs_authority_publication_queue().lock() else {
        return Err("filesystem authority publication queue is poisoned".into());
    };
    let Ok((queue, _)) =
        fs_authority_publication_retired().wait_timeout_while(queue, timeout, |queue| {
            queue.running || queue.pending.is_some()
        })
    else {
        return Err("filesystem authority publication queue is poisoned".into());
    };
    if queue.running || queue.pending.is_some() {
        return Err(format!(
            "filesystem authority publication did not settle within {timeout:?}: {}",
            queue.report()
        ));
    }
    Ok(())
}
