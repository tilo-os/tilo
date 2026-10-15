//! What the person chose from the palette before, as a tie-break.
//!
//! Equally good matches are ordered by how often and how recently each was
//! chosen ([`tilo_search::Frecency`]): the app opened every day leads the
//! one opened once last spring. It never lifts a worse match over a better
//! one, because providers order only rows of equal score by it. The record
//! is a private file in the user's state directory, written when a row is
//! chosen.

use std::cmp::Reverse;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tilo_search::Frecency;

use super::SearchResult;

/// Most a row's usage can add: enough to order ties and the empty palette,
/// far less than the gap between two match tiers.
pub const MAX_BOOST: i32 = 8;

struct Usage {
    path: Option<PathBuf>,
    frecency: Frecency,
}

static USAGE: OnceLock<Mutex<Usage>> = OnceLock::new();

/// `$XDG_STATE_HOME/tilo/search-usage.tsv`. Unit tests keep it in memory: a
/// test must not read or write the developer's own history.
#[cfg(not(test))]
fn persistence_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/search-usage.tsv"))
}

#[cfg(test)]
fn persistence_path() -> Option<PathBuf> {
    None
}

fn usage() -> MutexGuard<'static, Usage> {
    USAGE
        .get_or_init(|| {
            let path = persistence_path();
            let frecency = path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|text| Frecency::parse(&text))
                .unwrap_or_default();
            Mutex::new(Usage { path, frecency })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The usage key of a row: its provider and id.
#[must_use]
pub fn key(provider_id: &str, id: &str) -> String {
    format!("{provider_id}:{id}")
}

/// Remember that the row `provider_id`/`id` was chosen now.
pub fn record(provider_id: &str, id: &str) {
    record_at(&key(provider_id, id), now());
}

fn record_at(key: &str, at: u64) {
    let mut usage = usage();
    usage.frecency.record(key, at);
    let Some(path) = usage.path.clone() else {
        return;
    };
    let text = usage.frecency.serialize();
    drop(usage);
    save_in_background(path, text);
}

/// Hand the record to the state writer. A palette activation runs on the
/// compositor's loop, and the write's fsyncs can take seconds under I/O
/// pressure. A newer record that has not started replaces this one.
fn save_in_background(path: PathBuf, text: String) {
    crate::shell::durable_writer::global().submit(path.clone(), move || {
        if let Err(error) = write_private(&path, text.as_bytes()) {
            tracing::warn!(%error, "could not save the palette's usage history");
        }
    });
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600)
}

/// `0..=`[`MAX_BOOST`]: how strongly the row is preferred over an equal one.
#[must_use]
pub fn boost(provider_id: &str, id: &str) -> i32 {
    usage()
        .frecency
        .boost(&key(provider_id, id), now(), MAX_BOOST)
}

/// Order `rows` by score, equal scores by usage. Stable, so rows equal in
/// both keep the provider's order.
pub fn order_ties(rows: &mut [SearchResult]) {
    order_ties_by(rows, |_| 0);
}

/// [`order_ties`], rows equal in both score and usage by `prior`, the
/// provider's own sense of which comes first.
pub fn order_ties_by(rows: &mut [SearchResult], prior: impl Fn(&SearchResult) -> i32) {
    rows.sort_by_cached_key(|row| {
        (
            Reverse(row.score),
            Reverse(boost(&row.provider_id, &row.id)),
            Reverse(prior(row)),
        )
    });
}

/// [`order_ties`] for a provider that returns its rows by value.
#[must_use]
pub fn order_ties_after(mut rows: Vec<SearchResult>) -> Vec<SearchResult> {
    order_ties(&mut rows);
    rows
}

#[cfg(test)]
pub(crate) fn record_for_test(provider_id: &str, id: &str, uses: u32) {
    for _ in 0..uses {
        record_at(&key(provider_id, id), now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_row_outranks_an_unchosen_one_of_the_same_score() {
        record_for_test("usage-test", "often", 6);
        assert!(boost("usage-test", "often") > boost("usage-test", "never"));
        assert_eq!(boost("usage-test", "never"), 0);
        assert!(boost("usage-test", "often") <= MAX_BOOST);
    }

    /// Choosing a palette row runs on the compositor's loop; the record's
    /// write must not wait for the disk there.
    #[cfg(target_os = "linux")]
    #[test]
    fn saving_a_choice_never_waits_for_the_disk() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/tilo/search-usage.tsv");
        let mut frecency = Frecency::default();
        frecency.record("usage-test:chosen", now());
        let text = frecency.serialize();
        let target = path.clone();
        let expected = text.clone();
        crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(move || {
            save_in_background(target, text);
        });
        crate::shell::durable_writer::global().flush();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the usage record is private");
    }
}
