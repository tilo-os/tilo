//! Read-only note dates from the canonical, persisted Automerge history.
//!
//! No migration rewrites old notes or invents creation dates. Unstamped legacy
//! changes retain an unknown date; new action timestamps survive saves and merges.
use crate::model::PageId;
use automerge::{AutoCommit, ChangeHash};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use ui::history_source::{SchemaObjectId, schema_changes_since};

// Notes' messages. Every host loads them before the app's view is built;
// a widget built on its own loads them as it is made.
ui::embedded_catalog!(pub static CATALOG, ["en-US", "de"]);

/// Load the toolkit's widget messages and [`CATALOG`] on this thread.
pub fn register_messages() {
    ui::i18n::I18nManager::register_widget_messages();
    CATALOG.ensure();
}

impl NoteDates {
    pub fn display_labels(self, compact: bool) -> Vec<String> {
        self.display_labels_in(compact, &chrono::Local)
    }

    /// The labels with times in `zone`: the person's local time zone in the
    /// app, a fixed one in tests.
    fn display_labels_in<Tz: chrono::TimeZone>(self, compact: bool, zone: &Tz) -> Vec<String>
    where
        Tz::Offset: std::fmt::Display,
    {
        use ui::i18n::I18nManager;
        let date = |value: Option<i64>| {
            value
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .map(|date| {
                    let date = date.with_timezone(zone).naive_local();
                    let formats = ui::format::Formats::current();
                    if compact {
                        formats.date(date.date())
                    } else {
                        formats.timestamp(date)
                    }
                })
        };
        let mut labels = Vec::new();
        for (key, value) in [
            ("notes-created-date", self.created),
            ("notes-edited-date", self.edited),
        ] {
            if let Some(date) = date(value) {
                labels.push(I18nManager::format(key, &[("date", date.into())]));
            }
        }
        if labels.is_empty() {
            labels.push(I18nManager::lookup("notes-date-unavailable", None));
        }
        labels
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoteDates {
    pub created: Option<i64>,
    pub edited: Option<i64>,
}

impl NoteDates {
    /// The dates as the one line the note header shows, in the local time
    /// zone, read against the clock a capture pins. A date the history does
    /// not know is left out; a note with no known date shows nothing.
    pub fn labels(self) -> Vec<String> {
        self.labels_at(ui::clock::local_now())
    }

    /// [`Self::labels`] against an explicit `now`.
    pub fn labels_at(self, now: chrono::DateTime<chrono::Local>) -> Vec<String> {
        use ui::i18n::I18nManager;
        let created = self.created.map(|seconds| when(seconds, now));
        let edited = self.edited.map(|seconds| when(seconds, now));
        let line = match (created, edited) {
            (Some(created), Some(edited)) if created != edited => I18nManager::format(
                "notes-dates",
                &[("created", created.into()), ("edited", edited.into())],
            ),
            (Some(date), _) => I18nManager::format("notes-created-date", &[("date", date.into())]),
            (None, Some(date)) => {
                I18nManager::format("notes-edited-date", &[("date", date.into())])
            }
            (None, None) => return Vec::new(),
        };
        vec![line]
    }
}

/// One moment as a person says it: the time today, else the day (with its
/// year when it is not this one), in the local time zone.
fn when(seconds: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, TimeZone};
    use ui::i18n::I18nManager;
    let Some(moment) = chrono::Local.timestamp_opt(seconds, 0).single() else {
        return String::new();
    };
    // A moment the clock has not reached yet (a peer's skewed clock) still
    // reads as today rather than as a date in the future.
    if moment.date_naive() >= now.date_naive() {
        let time = ui::clock::time_of_day(&moment);
        return I18nManager::format("notes-date-today", &[("time", time.into())]);
    }
    if moment.year() == now.year() {
        ui::format::month_day(moment.date_naive())
    } else {
        ui::format::month_day_year(moment.date_naive())
    }
}

/// Every note's created and edited time, read from the document's history.
///
/// The history only grows, so the index reads each change once and goes on
/// from the heads it read. Reading a long history whole (a launch) happens
/// away from the UI thread on a copy of the document: until it lands, no
/// note shows a date, and when it lands the caption gains them in place.
#[derive(Clone, Default)]
pub struct NoteDateIndex {
    read: DatesRead,
    /// A whole-history read in flight, shared by the copies a restored view
    /// makes of this index.
    reading: Option<Arc<Reading>>,
}

/// What the history said up to `heads`.
#[derive(Clone, Default)]
struct DatesRead {
    heads: Vec<ChangeHash>,
    objects: HashMap<SchemaObjectId, PageId>,
    page_maps: HashSet<SchemaObjectId>,
    dates: HashMap<PageId, NoteDates>,
}

/// A whole-history read away from the UI thread: of the document at `heads`,
/// landing in `read`.
struct Reading {
    heads: Vec<ChangeHash>,
    read: OnceLock<DatesRead>,
}

/// How many operations a history holds before reading it whole leaves the UI
/// thread. Below it the read costs a few milliseconds and the dates are there
/// in the first frame.
const OFF_THREAD_OPS: u64 = 50_000;

impl NoteDateIndex {
    pub fn get(&self, page: PageId) -> NoteDates {
        self.read.dates.get(&page).copied().unwrap_or_default()
    }

    pub fn sync(&mut self, doc: &AutoCommit) {
        self.sync_reading_off_thread_from(doc, OFF_THREAD_OPS);
    }

    /// [`Self::sync`], reading a history of at least `off_thread_ops`
    /// operations away from the UI thread.
    pub(crate) fn sync_reading_off_thread_from(&mut self, doc: &AutoCommit, off_thread_ops: u64) {
        let history = doc.committed_document();
        if let Some(reading) = &self.reading {
            match reading.read.get() {
                Some(read) => {
                    self.read = read.clone();
                    self.reading = None;
                }
                // Still reading the history this document goes on from.
                None if contains_all(history, &reading.heads) => return,
                // The document was replaced: that read is of another one.
                None => self.reading = None,
            }
        }
        let heads = doc.committed_heads();
        if heads == self.read.heads {
            return;
        }
        // Undo or switching documents can remove our prior heads. Rebuild the
        // projection instead of leaking dates from a discarded history branch.
        if !contains_all(history, &self.read.heads) {
            self.read = DatesRead::default();
        }
        if self.read.heads.is_empty()
            && automerge::ReadDoc::stats(history).num_ops >= off_thread_ops
            && self.read_off_thread(history, heads.clone())
        {
            return;
        }
        let since = std::mem::take(&mut self.read.heads);
        self.read.apply(history, &since);
        self.read.heads = heads;
    }

    /// Read `history` whole on a background thread, to land as of `heads`.
    /// False when this host runs no background work.
    fn read_off_thread(&mut self, history: &automerge::Automerge, heads: Vec<ChangeHash>) -> bool {
        let reading = Arc::new(Reading {
            heads,
            read: OnceLock::new(),
        });
        let copy = ui::tilo_resource::profiling::measure("note_dates.copy", || history.clone());
        let landing = Arc::clone(&reading);
        let wake = ui::WakeHandle::new();
        let spawned = ui::background::Background::current().spawn("note dates", move || {
            let mut read = DatesRead::default();
            read.apply(&copy, &[]);
            read.heads.clone_from(&landing.heads);
            if landing.read.set(read).is_ok() {
                wake.wake();
            }
        });
        if spawned.is_err() {
            return false;
        }
        self.reading = Some(reading);
        true
    }
}

/// Whether `history` holds every change in `heads`.
fn contains_all(history: &automerge::Automerge, heads: &[ChangeHash]) -> bool {
    heads
        .iter()
        .all(|hash| history.get_change_meta_by_hash(hash).is_some())
}

impl DatesRead {
    /// Read the changes of `history` after `since`.
    fn apply(&mut self, history: &automerge::Automerge, since: &[ChangeHash]) {
        for change in schema_changes_since(history, since) {
            let time = change.timestamp;
            for creation in change.created {
                if creation.parent.is_root() && creation.key.as_deref() == Some("pages") {
                    self.page_maps.insert(creation.object);
                    continue;
                }
                let page = if self.page_maps.contains(&creation.parent) {
                    let page = creation.key.as_deref().and_then(|key| key.parse().ok());
                    if let Some(page) = page {
                        let dates = self.dates.entry(page).or_insert(NoteDates {
                            created: time,
                            edited: time,
                        });
                        dates.edited = dates.edited.max(time);
                    }
                    page
                } else {
                    self.objects.get(&creation.parent).copied()
                };
                if let Some(page) = page {
                    self.objects.insert(creation.object, page);
                }
            }
            for object in &change.touched {
                if let Some(page) = self.objects.get(object) {
                    let dates = self.dates.entry(*page).or_default();
                    dates.edited = dates.edited.max(time);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{self, Action};
    use tilo_actions::Action as _;
    use tilo_automerge::ApplyIncoming;
    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper models handing an owned action to the production executor"
    )]
    fn apply(doc: &mut AutoCommit, action: Action, timestamp: i64) {
        action.apply(doc).unwrap();
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(timestamp));
    }

    /// A note edited at 03:54 UTC reads 05:54 to someone on Central
    /// European Summer Time: the label is the person's local time, not UTC.
    #[test]
    fn dates_read_in_the_local_time_zone() {
        register_messages();
        // 2026-10-04 03:54:00 UTC.
        let edited = 1_791_086_040;
        let dates = NoteDates {
            created: Some(edited),
            edited: Some(edited),
        };
        let cest = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let labels = dates.display_labels_in(false, &cest);
        assert!(labels[1].contains("2026-10-04 05:54"), "{labels:?}");
        assert!(!labels[1].contains("UTC"), "{labels:?}");
        let compact =
            dates.display_labels_in(true, &chrono::FixedOffset::east_opt(-5 * 3600).unwrap());
        assert!(compact[0].contains("2026-10-03"), "{compact:?}");
    }
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn metadata_dates_follow_real_edits_and_survive_reload_and_legacy_notes() {
        eprintln!("phase metadata_dates_follow_real_edits_and_survive_reload");
        {
            let mut doc = AutoCommit::new();
            let a = PageId::new_v4();
            let b = PageId::new_v4();
            apply(&mut doc, actions::create_page_action(a, None, "A"), 100);
            apply(&mut doc, actions::create_page_action(b, None, "B"), 200);
            let mut index = NoteDateIndex::default();
            index.sync(&doc);
            assert_eq!(
                index.get(a),
                NoteDates {
                    created: Some(100),
                    edited: Some(100)
                }
            );
            apply(
                &mut doc,
                actions::edit_body_action(
                    a,
                    tilo_document::RichTextAction::AppendParagraph {
                        text: "Hello".into(),
                    },
                ),
                300,
            );
            index.sync(&doc);
            assert_eq!(
                index.get(a),
                NoteDates {
                    created: Some(100),
                    edited: Some(300)
                }
            );
            assert_eq!(index.get(b).edited, Some(200));
            let mut restored = AutoCommit::load(&doc.save()).unwrap();
            let mut restored_index = NoteDateIndex::default();
            restored_index.sync(&restored);
            assert_eq!(restored_index.get(a), index.get(a));
            let heads = restored.get_heads();
            restored_index.sync(&restored);
            assert_eq!(restored.get_heads(), heads, "viewing metadata is read-only");
        }

        eprintln!("phase metadata_legacy_creation_remains_unknown_after_edit");
        {
            let mut doc = AutoCommit::new();
            let page = PageId::new_v4();
            apply(
                &mut doc,
                actions::create_page_action(page, None, "Legacy"),
                0,
            );
            apply(&mut doc, actions::rename_page_action(page, "Renamed"), 500);
            let mut index = NoteDateIndex::default();
            index.sync(&doc);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: None,
                    edited: Some(500)
                }
            );
        }

        eprintln!("phase metadata_production_executor_stamps_and_replays_creation");
        {
            let mut origin = AutoCommit::new();
            let mut peer = AutoCommit::new();
            let page = PageId::new_v4();
            let action = actions::create_page_action(page, None, "New note");
            let envelope =
                tilo_actions::apply_local(&mut origin, &action, actions::encode(&action)).unwrap();
            assert!(
                envelope.timestamp > 0,
                "new notes must have a real persisted time"
            );
            let mut index = NoteDateIndex::default();
            index.sync(&origin);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(envelope.timestamp),
                    edited: Some(envelope.timestamp),
                }
            );
            let mut registry = tilo_actions::ActionRegistry::new();
            <crate::Notebook as ui::tilo_resource::Resource>::register(&mut registry);
            tilo_actions::replay(&mut peer, &envelope, &registry).unwrap();
            let mut peer_index = NoteDateIndex::default();
            peer_index.sync(&peer);
            assert_eq!(peer_index.get(page), index.get(page));
            assert_eq!(origin.get_heads(), peer.get_heads());
            let heads = origin.get_heads();
            assert!(
                tilo_actions::apply_local(&mut origin, &action, actions::encode(&action)).is_err(),
                "a note is created once"
            );
            index.sync(&origin);
            assert_eq!(
                origin.get_heads(),
                heads,
                "no-op writes must not alter metadata"
            );
        }
    }

    /// A history long enough to read away from the UI thread is read there:
    /// no note shows a date until the read lands, the dates that land are the
    /// ones reading the history in place gives, an edit made while it was
    /// read still moves the edited time, and a read of a document since
    /// replaced is dropped rather than shown.
    #[test]
    fn metadata_long_history_is_read_off_the_ui_thread() {
        let pumped = ui::rt::background::Pumped::new();
        let _scope = pumped.background().enter();
        let mut doc = AutoCommit::new();
        let page = PageId::new_v4();
        let other = PageId::new_v4();
        apply(
            &mut doc,
            actions::create_page_action(page, None, "Long"),
            100,
        );
        apply(
            &mut doc,
            actions::create_page_action(other, None, "Other"),
            150,
        );
        apply(
            &mut doc,
            actions::edit_body_action(
                page,
                tilo_document::RichTextAction::AppendParagraph {
                    text: "typed ".repeat(50),
                },
            ),
            200,
        );
        let mut in_place = NoteDateIndex::default();
        in_place.sync_reading_off_thread_from(&doc, u64::MAX);
        assert!(in_place.reading.is_none(), "below the bound reads in place");
        assert_eq!(
            in_place.get(page),
            NoteDates {
                created: Some(100),
                edited: Some(200)
            }
        );

        let mut index = NoteDateIndex::default();
        index.sync_reading_off_thread_from(&doc, 1);
        assert!(
            index.reading.is_some(),
            "a long history is read off the UI thread"
        );
        assert_eq!(index.get(page), NoteDates::default(), "nothing lands yet");
        // A collaborator's edit arrives while the history is read.
        apply(&mut doc, actions::rename_page_action(other, "Renamed"), 300);
        index.sync_reading_off_thread_from(&doc, 1);
        assert_eq!(index.get(other), NoteDates::default(), "still reading");
        assert_eq!(pumped.run_pending(), 1, "one read of the whole history");
        index.sync_reading_off_thread_from(&doc, 1);
        assert!(index.reading.is_none(), "the read landed");
        assert_eq!(index.get(page), in_place.get(page));
        assert_eq!(
            index.get(other),
            NoteDates {
                created: Some(150),
                edited: Some(300)
            },
            "the edit made while reading moved the edited time"
        );
        // Copies of the index taken while reading share that read.
        let mut copy = NoteDateIndex::default();
        copy.sync_reading_off_thread_from(&doc, 1);
        let mut restored = copy.clone();
        assert_eq!(pumped.run_pending(), 1);
        restored.sync_reading_off_thread_from(&doc, 1);
        assert_eq!(restored.get(page), in_place.get(page));

        // The document is replaced while its history is read: that read is
        // of another notebook and must not show.
        let mut switched = NoteDateIndex::default();
        switched.sync_reading_off_thread_from(&doc, 1);
        let mut replacement = AutoCommit::new();
        let fresh = PageId::new_v4();
        apply(
            &mut replacement,
            actions::create_page_action(fresh, None, "Fresh"),
            400,
        );
        switched.sync_reading_off_thread_from(&replacement, u64::MAX);
        assert_eq!(pumped.run_pending(), 1);
        switched.sync_reading_off_thread_from(&replacement, u64::MAX);
        assert_eq!(switched.get(page), NoteDates::default());
        assert_eq!(switched.get(fresh).created, Some(400));
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn metadata_converges_across_concurrent_edits_undo_and_replaced_history() {
        eprintln!("phase metadata_concurrent_edits_converge_incrementally");
        {
            let mut base = AutoCommit::new();
            let page = PageId::new_v4();
            apply(
                &mut base,
                actions::create_page_action(page, None, "Original"),
                100,
            );
            let mut first = base.fork();
            let mut second = base.fork();
            let mut first_index = NoteDateIndex::default();
            let mut second_index = NoteDateIndex::default();
            first_index.sync(&first);
            second_index.sync(&second);
            apply(
                &mut first,
                actions::rename_page_action(page, "Renamed"),
                200,
            );
            apply(
                &mut second,
                actions::edit_body_action(
                    page,
                    tilo_document::RichTextAction::AppendParagraph {
                        text: "Remote text".into(),
                    },
                ),
                300,
            );
            first_index.sync(&first);
            second_index.sync(&second);
            assert_eq!(first_index.get(page).edited, Some(200));
            assert_eq!(second_index.get(page).edited, Some(300));
            first.merge_incoming(&mut second).unwrap();
            second.merge_incoming(&mut first).unwrap();
            first_index.sync(&first);
            second_index.sync(&second);
            assert_eq!(
                first_index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(300)
                }
            );
            assert_eq!(second_index.get(page), first_index.get(page));
            let mut rebuilt = NoteDateIndex::default();
            rebuilt.sync(&first);
            assert_eq!(rebuilt.get(page), first_index.get(page));
        }

        eprintln!("phase metadata_shared_undo_and_redo_preserve_creation");
        {
            let mut state = ui::State::new();
            crate::read::initialize(&mut state.doc);
            let page = PageId::new_v4();
            apply(
                &mut state.doc,
                actions::create_page_action(page, None, "Original"),
                100,
            );
            let before = state.doc.get_heads();
            apply(
                &mut state.doc,
                actions::rename_page_action(page, "Renamed"),
                200,
            );
            state.record_committed_action(before, None, None);
            let mut index = NoteDateIndex::default();
            index.sync(&state.doc);
            assert_eq!(index.get(page).edited, Some(200));
            assert!(state.undo(None, None).is_some());
            assert_eq!(crate::read::list_pages(&state.doc)[0].title, "Original");
            index.sync(&state.doc);
            let undo_dates = index.get(page);
            assert_eq!(undo_dates.created, Some(100));
            assert!(
                undo_dates.edited.is_some_and(|time| time > 200),
                "shared undo commits its own real timestamp"
            );
            assert!(state.redo(None, None).is_some());
            assert_eq!(crate::read::list_pages(&state.doc)[0].title, "Renamed");
            index.sync(&state.doc);
            assert_eq!(index.get(page).created, Some(100));
            assert!(index.get(page).edited >= undo_dates.edited);
        }

        eprintln!("phase metadata_rebuilds_when_document_history_is_replaced");
        {
            let mut doc = AutoCommit::new();
            let page = PageId::new_v4();
            apply(
                &mut doc,
                actions::create_page_action(page, None, "Original"),
                100,
            );
            let before = doc.get_heads();
            apply(&mut doc, actions::rename_page_action(page, "Renamed"), 200);
            let mut index = NoteDateIndex::default();
            index.sync(&doc);
            let historical = doc.fork_at(&before).unwrap();
            index.sync(&historical);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(100)
                }
            );
            index.sync(&AutoCommit::new());
            assert_eq!(
                index.get(page),
                NoteDates::default(),
                "another notebook must not inherit these dates"
            );
        }
    }
}
