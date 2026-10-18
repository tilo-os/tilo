//! Answers search takes from the network, without the compositor touching
//! it.
//!
//! The `tilo-net-answers` helper fetches (it confines itself to reading the
//! trust bundle and the resolver's files and to connecting to HTTPS) and
//! prints the answer; this module runs it off the compositor thread, keeps
//! the last answer with when it was fetched, in memory and in the cache
//! directory, and answers from that, offline too. A search reads only what
//! is kept here ([`rates`]); a keystroke never waits for the helper. An
//! answer older than [`REFRESH_AFTER`] is refetched in the background, and a
//! failed fetch is not retried for [`RETRY_AFTER`].
//!
//! The same door serves every network answer: a new one is a
//! [`tilo_net_answers::Source`] and a reader here.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_net_answers::{Answer, Rates, Source};

/// Kept answers are refetched after this long; reference rates change once
/// a working day.
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
/// A failed fetch (offline, the server down) is not retried sooner.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// Longest the helper may run; it gives up on the network sooner.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest answer read from the helper.
const OUTPUT_LIMIT: usize = 1024 * 1024;

/// One kept answer and when it was fetched, in seconds since the epoch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Kept<T> {
    pub fetched_at: u64,
    pub value: T,
}

#[derive(Default)]
struct Store {
    rates: Option<Kept<Rates>>,
    loaded_from_disk: bool,
    failed_at: Option<Instant>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
static FETCHING: AtomicBool = AtomicBool::new(false);
/// Moves whenever a kept answer changes, so search knows to ask again.
static REVISION: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    tilo_clock::unix_now()
}

/// Where the kept rates live: `$XDG_CACHE_HOME/tilo/answers/rates.json`.
fn cache_path(source: Source) -> Option<PathBuf> {
    tilo_paths::cache_dir().map(|dir| {
        dir.join("tilo/answers")
            .join(format!("{}.json", source.name()))
    })
}

/// The helper: beside the running compositor, else where the image installs
/// it. Never in a unit test, which must not reach the network.
fn helper() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-net-answers")));
    beside
        .into_iter()
        .chain([
            PathBuf::from("/usr/libexec/tilo-net-answers"),
            PathBuf::from("/usr/bin/tilo-net-answers"),
        ])
        .find(|path| path.exists())
}

/// The kept exchange rates and when they were fetched, if any were. Asks the
/// helper for fresher ones in the background when they are missing or old.
#[must_use]
pub fn rates() -> Option<Kept<Rates>> {
    let (kept, fetch) = {
        let mut store = STORE.lock_recover();
        let store = store.get_or_insert_with(Store::default);
        if !store.loaded_from_disk {
            store.loaded_from_disk = true;
            store.rates = cache_path(Source::Rates).and_then(|path| read_kept(&path));
        }
        let stale = store
            .rates
            .as_ref()
            .is_none_or(|kept| now().saturating_sub(kept.fetched_at) >= REFRESH_AFTER.as_secs());
        let resting = store
            .failed_at
            .is_some_and(|failed| failed.elapsed() < RETRY_AFTER);
        (store.rates.clone(), stale && !resting)
    };
    if fetch {
        fetch_in_background(Source::Rates);
    }
    kept
}

/// Moves whenever a kept answer changes.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

fn read_kept(path: &std::path::Path) -> Option<Kept<Rates>> {
    let text = std::fs::read_to_string(path).ok()?;
    let kept = serde_json::from_str::<Kept<Answer>>(&text).ok()?;
    let Answer::Rates(rates) = kept.value;
    Some(Kept {
        fetched_at: kept.fetched_at,
        value: rates,
    })
}

/// Keep `answer`, fetched now: in memory for search, and on disk for the
/// next session and for offline.
fn keep(source: Source, answer: Answer, fetched_at: u64) {
    if let Some(path) = cache_path(source) {
        let kept = Kept {
            fetched_at,
            value: answer.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&kept)
            && let Err(error) = tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        {
            tracing::warn!(%error, "could not keep the {} answer", source.name());
        }
    }
    let mut store = STORE.lock_recover();
    let store = store.get_or_insert_with(Store::default);
    let Answer::Rates(rates) = answer;
    store.rates = Some(Kept {
        fetched_at,
        value: rates,
    });
    store.failed_at = None;
    drop(store);
    REVISION.fetch_add(1, Ordering::AcqRel);
}

fn fetch_in_background(source: Source) {
    let Some(helper) = helper() else {
        return;
    };
    if FETCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let wake = crate::shell::search::invocation_wake::waker();
    let spawned = std::thread::Builder::new()
        .name("net-answers".into())
        .spawn(move || {
            let mut command = std::process::Command::new(&helper);
            command.arg(source.name());
            let answer = bounded_command::run_prepared(command, None, OUTPUT_LIMIT, HELPER_TIMEOUT)
                .map_err(|error| error.to_string())
                .and_then(|output| {
                    if output.status.success() {
                        serde_json::from_slice::<Answer>(&output.stdout)
                            .map_err(|error| error.to_string())
                    } else {
                        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
                    }
                });
            match answer {
                Ok(answer) => {
                    keep(source, answer, now());
                    wake.wake();
                }
                Err(error) => {
                    tracing::info!(%error, "no fresh {} answer", source.name());
                    STORE
                        .lock_recover()
                        .get_or_insert_with(Store::default)
                        .failed_at = Some(Instant::now());
                }
            }
            FETCHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        FETCHING.store(false, Ordering::Release);
    }
}

/// Keep `rates` as if fetched at `fetched_at`, for tests of what search
/// answers from them.
#[cfg(test)]
pub(crate) fn keep_rates_for_test(rates: Rates, fetched_at: u64) {
    let mut store = STORE.lock_recover();
    let store = store.get_or_insert_with(Store::default);
    store.loaded_from_disk = true;
    store.rates = Some(Kept {
        fetched_at,
        value: rates,
    });
    drop(store);
    REVISION.fetch_add(1, Ordering::AcqRel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_answer_round_trips_through_its_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rates.json");
        let rates = Rates {
            base: "EUR".into(),
            date: "2026-10-16".into(),
            rates: [("USD".to_owned(), 1.08)].into_iter().collect(),
        };
        let kept = Kept {
            fetched_at: 1_800_000_000,
            value: Answer::Rates(rates.clone()),
        };
        std::fs::write(&path, serde_json::to_vec(&kept).unwrap()).unwrap();
        assert_eq!(
            read_kept(&path),
            Some(Kept {
                fetched_at: 1_800_000_000,
                value: rates
            })
        );
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(read_kept(&path), None);
    }
}
