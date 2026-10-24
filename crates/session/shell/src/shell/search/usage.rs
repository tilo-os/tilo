//! What the person chose from the palette before.
//!
//! Two records, both private files in the user's state directory, written
//! when a row is chosen:
//!
//! - which rows were chosen, how often and how recently
//!   ([`tilo_search::Frecency`]). Within a provider it orders equally good
//!   matches; at the merge it leads short queries, where launchers live
//!   ("t" is the Terminal opened every day), and leads the launcher's
//!   Apps with the apps used most;
//! - which row was chosen for which typed text: what "te" was for last time
//!   is what it is for next time.

use std::cmp::Reverse;
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tilo_search::Frecency;

use super::SearchResult;
use tilo_lock::MutexRecover as _;

/// Most a row's usage can add: enough to order ties and the empty palette,
/// far less than the gap between two match tiers.
pub const MAX_BOOST: i32 = 8;

struct Usage {
    path: Option<PathBuf>,
    frecency: Frecency,
}

#[cfg(not(test))]
static USAGE: OnceLock<Mutex<Usage>> = OnceLock::new();
#[cfg(not(test))]
static CHOICES: OnceLock<Mutex<Usage>> = OnceLock::new();

/// `$XDG_STATE_HOME/tilo/<file>`. Unit tests keep it in memory: a test must
/// not read or write the developer's own history.
#[cfg(not(test))]
fn persistence_path(file: &str) -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo").join(file))
}

#[cfg(test)]
fn persistence_path(_file: &str) -> Option<PathBuf> {
    None
}

fn load(file: &str) -> Mutex<Usage> {
    let path = persistence_path(file);
    let frecency = path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| Frecency::parse(&text))
        .unwrap_or_default();
    Mutex::new(Usage { path, frecency })
}

#[cfg(not(test))]
fn usage() -> MutexGuard<'static, Usage> {
    USAGE
        .get_or_init(|| load("search-usage.tsv"))
        .lock_recover()
}

/// Typed text → chosen row, keyed `<text>\u{1f}<provider>:<id>`.
#[cfg(not(test))]
fn choices() -> MutexGuard<'static, Usage> {
    CHOICES
        .get_or_init(|| load("search-choices.tsv"))
        .lock_recover()
}

// Each test runs on a thread of its own with a history of its own: what one
// test chooses must not reorder another test's palette.
#[cfg(test)]
thread_local! {
    static TEST_USAGE: &'static Mutex<Usage> = Box::leak(Box::new(load("search-usage.tsv")));
    static TEST_CHOICES: &'static Mutex<Usage> = Box::leak(Box::new(load("search-choices.tsv")));
}

#[cfg(test)]
fn usage() -> MutexGuard<'static, Usage> {
    TEST_USAGE.with(|usage| *usage).lock_recover()
}

#[cfg(test)]
fn choices() -> MutexGuard<'static, Usage> {
    TEST_CHOICES.with(|choices| *choices).lock_recover()
}

/// Most characters of a typed query a choice is remembered for: past that
/// the text names the thing on its own.
const CHOICE_PREFIX_SCALARS: usize = 6;

/// The texts a choice made after typing `query` answers: each start of it up
/// to [`CHOICE_PREFIX_SCALARS`], folded. Choosing Terminal after "term" makes
/// it the answer to "t", "te", "ter" and "term".
fn choice_texts(query: &str) -> Vec<String> {
    let folded = tilo_search::fold(query.trim());
    let scalars: Vec<char> = folded.chars().collect();
    (1..=scalars.len().min(CHOICE_PREFIX_SCALARS))
        .map(|end| scalars.iter().take(end).collect())
        .collect()
}

fn choice_key(text: &str, provider_id: &str, id: &str) -> String {
    format!("{text}\u{1f}{}", key(provider_id, id))
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
    record_at(usage(), &key(provider_id, id), now());
}

/// Remember that the row `provider_id`/`id` was chosen now, after typing
/// `query`: as a choice, and as the answer to that text.
pub fn record_choice(query: &str, provider_id: &str, id: &str) {
    let at = now();
    record_at(usage(), &key(provider_id, id), at);
    let texts = choice_texts(query);
    if texts.is_empty() {
        return;
    }
    let mut held = choices();
    for text in &texts {
        held.frecency.record(&choice_key(text, provider_id, id), at);
    }
    let Some(path) = held.path.clone() else {
        return;
    };
    let text = held.frecency.serialize();
    drop(held);
    save_in_background(path, text);
}

fn record_at(mut usage: MutexGuard<'static, Usage>, key: &str, at: u64) {
    usage.frecency.record(key, at);
    let Some(path) = usage.path.clone() else {
        return;
    };
    let text = usage.frecency.serialize();
    drop(usage);
    save_in_background(path, text);
}

/// `0.0..=1.0`: how often and how recently the row was chosen.
#[must_use]
pub fn share(provider_id: &str, id: &str) -> f32 {
    usage().frecency.share(&key(provider_id, id), now())
}

/// `0.0..=1.0`: how often and how recently the row was chosen after typing
/// `query` (or a text it starts the same way as, up to
/// `CHOICE_PREFIX_SCALARS`).
#[must_use]
pub fn choice_share(query: &str, provider_id: &str, id: &str) -> f32 {
    let Some(text) = choice_texts(query).pop() else {
        return 0.0;
    };
    choices()
        .frecency
        .share(&choice_key(&text, provider_id, id), now())
}

/// The rows chosen most recently, newest first, as provider and id: at most
/// `limit` of them.
#[must_use]
pub fn recent_choices(limit: usize) -> Vec<(String, String)> {
    usage()
        .frecency
        .most_recent(limit)
        .into_iter()
        .filter_map(|key| {
            let (provider, id) = key.split_once(':')?;
            Some((provider.to_owned(), id.to_owned()))
        })
        .collect()
}

/// When the row `provider_id`/`id` was last chosen, if ever.
#[must_use]
pub fn last_used(provider_id: &str, id: &str) -> Option<std::time::SystemTime> {
    usage()
        .frecency
        .last_used(&key(provider_id, id))
        .and_then(|secs| UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs)))
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
        record_at(usage(), &key(provider_id, id), now());
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

    #[test]
    fn a_choice_answers_the_text_typed_for_it_and_its_starts() {
        record_choice("Term", "usage-choice-test", "terminal");
        assert!(choice_share("t", "usage-choice-test", "terminal") > 0.0);
        assert!(choice_share("ter", "usage-choice-test", "terminal") > 0.0);
        assert!(choice_share("term", "usage-choice-test", "terminal") > 0.0);
        assert!(choice_share("x", "usage-choice-test", "terminal") <= 0.0);
        assert!(choice_share("t", "usage-choice-test", "tube") <= 0.0);
        assert!(share("usage-choice-test", "terminal") > 0.0);
        assert!(
            recent_choices(64)
                .iter()
                .any(|(provider, id)| provider == "usage-choice-test" && id == "terminal")
        );
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
