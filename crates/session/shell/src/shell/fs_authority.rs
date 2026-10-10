//! Asynchronous publication of workspace file capabilities to `quick-fs`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

const DEFAULT_FS_AUTHORITY_PUBLISHER: &str = "/usr/bin/fs";
/// `fs publish-authority`'s exit status on a device without the workspace
/// governor key: a joined device never signs authority, so there is nothing
/// to publish and nothing to retry.
const PUBLISHER_NOT_GOVERNOR_EXIT: i32 = 77;
const MAX_FS_AUTHORITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// A publisher that is not installed will not appear within seconds; a host
/// session without `quick-fs` would otherwise spawn and warn every five.
const MAX_MISSING_PUBLISHER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(300);
/// Recent publisher attempts kept so a stalled publication can say why.
const MAX_RECORDED_FS_AUTHORITY_ATTEMPTS: usize = 16;
/// Tail of the publisher's stderr kept per recorded attempt.
const MAX_RECORDED_FS_AUTHORITY_STDERR: usize = 2048;

#[derive(Clone, Debug)]
struct FsAuthorityPublication {
    acl: PathBuf,
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

/// Run the publisher once. Its stderr is captured so a failing publisher
/// explains itself in the journal and in the attempt history, not only by
/// exit status.
fn run_publisher(publication: &FsAuthorityPublication) -> std::io::Result<std::process::Output> {
    std::process::Command::new(&publication.publisher)
        .arg("publish-authority")
        .arg("--acl")
        .arg(&publication.acl)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
}

fn run_fs_authority_publication_queue() {
    loop {
        // Taking the last publication and retiring happen under one lock: a
        // schedule in between would see `running` still set, leave its ACL
        // pending and return, and the retiring worker would drop it.
        let publication = match fs_authority_publication_queue().lock() {
            Ok(mut queue) => {
                let publication = queue.pending.take();
                if publication.is_none() {
                    queue.running = false;
                }
                publication
            }
            Err(error) => {
                tracing::warn!(%error, "filesystem authority publication queue is poisoned");
                return;
            }
        };
        let Some(mut publication) = publication else {
            fs_authority_publication_retired().notify_all();
            return;
        };
        let mut retry_delay = std::time::Duration::from_millis(100);
        // One warning per publication; the retries after it are the same story.
        let mut failures = 0_u32;
        loop {
            let result = run_publisher(&publication);
            let mut publisher_missing = false;
            let (succeeded, outcome) = match result {
                Ok(output) if output.status.success() => {
                    if failures > 0 {
                        tracing::info!(
                            failures,
                            acl = %publication.acl.display(),
                            "filesystem authority published after retrying"
                        );
                    }
                    (true, "published".to_string())
                }
                Ok(output) if output.status.code() == Some(PUBLISHER_NOT_GOVERNOR_EXIT) => {
                    tracing::debug!(
                        acl = %publication.acl.display(),
                        "this device does not govern the workspace; its authority is published by the primary device"
                    );
                    (
                        true,
                        "not the workspace governor; nothing to publish".to_string(),
                    )
                }
                Ok(output) => {
                    let stderr = stderr_tail(&output.stderr);
                    if failures == 0 {
                        tracing::warn!(
                            status = ?output.status,
                            %stderr,
                            acl = %publication.acl.display(),
                            "filesystem authority publication failed; retrying quietly"
                        );
                    } else {
                        tracing::debug!(status = ?output.status, %stderr, "filesystem authority publication failed again");
                    }
                    (false, format!("{}; stderr: {stderr:?}", output.status))
                }
                Err(error) => {
                    publisher_missing = error.kind() == std::io::ErrorKind::NotFound;
                    if failures == 0 {
                        tracing::warn!(
                            %error,
                            publisher = %publication.publisher.display(),
                            "filesystem authority publisher could not start; retrying quietly"
                        );
                    } else {
                        tracing::debug!(%error, "filesystem authority publisher could not start again");
                    }
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
            failures = failures.saturating_add(1);
            std::thread::sleep(retry_delay);
            retry_delay = next_retry_delay(retry_delay, publisher_missing);
            if let Ok(mut queue) = fs_authority_publication_queue().lock()
                && let Some(newer) = queue.pending.take()
            {
                publication = newer;
                retry_delay = std::time::Duration::from_millis(100);
                failures = 0;
            }
        }
    }
}

/// Doubling backoff, capped at five seconds for a publisher that ran and
/// failed and at five minutes for one that is not installed.
fn next_retry_delay(current: std::time::Duration, publisher_missing: bool) -> std::time::Duration {
    let cap = if publisher_missing {
        MAX_MISSING_PUBLISHER_RETRY_DELAY
    } else {
        MAX_FS_AUTHORITY_RETRY_DELAY
    };
    current.saturating_mul(2).min(cap)
}

/// Block until the publication worker has retired with nothing queued, or
/// `timeout` passes. The worker's retirement is signalled, so this returns as
/// soon as publication settles. Exports still queued on the state writer,
/// which schedule publications, are waited for first.
///
/// # Errors
///
/// On timeout, or if the queue is poisoned, returns a report of the queue
/// state and the recent publisher attempts, including their stderr.
pub fn wait_for_fs_authority_publication_idle(timeout: std::time::Duration) -> Result<(), String> {
    if !crate::shell::durable_writer::global().flush_timeout(timeout) {
        return Err("capability exports are still queued on the state writer".into());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_missing_publisher_backs_off_to_minutes_not_seconds() {
        let settle = |missing| {
            let mut delay = Duration::from_millis(100);
            for _ in 0..32 {
                delay = next_retry_delay(delay, missing);
            }
            delay
        };
        assert_eq!(settle(false), MAX_FS_AUTHORITY_RETRY_DELAY);
        assert_eq!(settle(true), MAX_MISSING_PUBLISHER_RETRY_DELAY);
        assert!(MAX_MISSING_PUBLISHER_RETRY_DELAY >= Duration::from_secs(60));
        assert_eq!(
            next_retry_delay(Duration::from_millis(100), true),
            Duration::from_millis(200),
            "the first retries stay quick in case the daemon is only starting",
        );
    }
}
