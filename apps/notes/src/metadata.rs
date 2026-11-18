//! Read-only note dates from the canonical, persisted Automerge history.
//!
//! No migration rewrites old notes or invents creation dates. Unstamped legacy
//! changes retain an unknown date; new action timestamps survive saves and merges.
use crate::model::PageId;
use automerge::{AutoCommit, ChangeHash};

// Notes' messages. Every host loads them before the app's view is built;
// a widget built on its own loads them as it is made.
ui::embedded_catalog!(pub static CATALOG, ["en-US", "de"]);

/// What a note is called wherever this app shows one: the title it was given, or the word for a
/// note nobody named. A title is only ever what a person typed, so the word stands in at every
/// surface — the page tree, the palette, a toast, the note's heading — rather than being stored.
#[must_use]
pub fn note_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        CATALOG.t("notes-title-placeholder")
    } else {
        title.to_owned()
    }
}

/// Load the toolkit's widget messages and [`CATALOG`] on this thread, and
/// offer the Unicode emoji a page's reactions pick from.
pub fn register_messages() {
    ui::i18n::I18nManager::register_widget_messages();
    CATALOG.ensure();
    // A page's reactions pick from the whole emoji catalogue, where this
    // build carries it: in the browser desktop Notes shares the start module,
    // which carries none, and there Chat's module offers it.
    tilo_unicode_emoji::offer();
}

impl NoteDates {
    pub fn display_labels(self, compact: bool) -> Vec<String> {
        self.display_labels_in(compact, &jiff::tz::TimeZone::system())
    }

    /// The labels with times in `zone`: the person's local time zone in the
    /// app, a fixed one in tests.
    fn display_labels_in(self, compact: bool, zone: &jiff::tz::TimeZone) -> Vec<String> {
        let date = |value: Option<i64>| {
            value
                .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
                .map(|date| {
                    let date = date.to_zoned(zone.clone()).datetime();
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
                labels.push(crate::metadata::CATALOG.t_args(key, &[("date", date.into())]));
            }
        }
        if labels.is_empty() {
            labels.push(crate::metadata::CATALOG.t("notes-date-unavailable"));
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
    /// zone, read against `now`. A date the history does not know is left
    /// out; a note with no known date shows nothing. `moved` for a note
    /// written elsewhere first: its creation here is when it arrived, and the
    /// line says so rather than naming the day it was written there (which
    /// the header's provenance row shows).
    pub fn labels_at(self, now: &jiff::Zoned, moved: bool) -> Vec<String> {
        let created = self.created.map(|seconds| when(seconds, now));
        let edited = self.edited.map(|seconds| when(seconds, now));
        let line = match (created, edited) {
            (Some(created), Some(edited)) if created != edited => crate::metadata::CATALOG.t_args(
                if moved {
                    "notes-moved-dates"
                } else {
                    "notes-dates"
                },
                &[("created", created.into()), ("edited", edited.into())],
            ),
            (Some(date), _) => crate::metadata::CATALOG.t_args(
                if moved {
                    "notes-moved-date"
                } else {
                    "notes-created-date"
                },
                &[("date", date.into())],
            ),
            (None, Some(date)) => {
                crate::metadata::CATALOG.t_args("notes-edited-date", &[("date", date.into())])
            }
            (None, None) => return Vec::new(),
        };
        vec![line]
    }
}

/// One moment as a person says it: the time today, else the day (with its
/// year when it is not this one), in the local time zone.
fn when(seconds: i64, now: &jiff::Zoned) -> String {
    let Ok(moment) = jiff::Timestamp::from_second(seconds) else {
        return String::new();
    };
    let moment = moment.to_zoned(now.time_zone().clone());
    // A moment the clock has not reached yet (a peer's skewed clock) still
    // reads as today rather than as a date in the future.
    if moment.date() >= now.date() {
        let time = ui::clock::time_of_day(&moment);
        return crate::metadata::CATALOG.t_args("notes-date-today", &[("time", time.into())]);
    }
    if moment.year() == now.year() {
        ui::format::month_day(moment.date())
    } else {
        ui::format::month_day_year(moment.date())
    }
}

/// The open note's created and edited time, read from the document's
/// history.
///
/// Every op names the change that made it, and every change keeps its time.
/// The note was created by the change that made its object, and last edited
/// by the newest change that wrote in that object or any object inside it
/// (its title, its blocks, their text): those are found from the note's own
/// ops ([`automerge::Automerge::latest_changes_within`]), whatever the
/// notebook's history holds besides. After that only the changes added since
/// are read, each by its own ops, and only to tell whether it wrote in the
/// note.
#[derive(Clone, Default)]
pub struct NoteDateIndex {
    read: Option<DatesRead>,
}

/// What the history said of one note up to `heads`.
#[derive(Clone)]
struct DatesRead {
    page: PageId,
    heads: Vec<ChangeHash>,
    dates: NoteDates,
}

impl NoteDateIndex {
    /// `page`'s dates, as the last [`Self::sync`] for it read them; none for
    /// another note.
    pub fn get(&self, page: PageId) -> NoteDates {
        self.read
            .as_ref()
            .filter(|read| read.page == page)
            .map(|read| read.dates)
            .unwrap_or_default()
    }

    /// Read `page`'s dates from `doc`, going on from the last read of the
    /// same note where `doc`'s history holds it.
    pub fn sync(&mut self, doc: &AutoCommit, page: Option<PageId>) {
        let Some(page) = page else {
            self.read = None;
            return;
        };
        let history = doc.committed_document();
        let heads = doc.committed_heads();
        if let Some(read) = self.read.as_mut().filter(|read| read.page == page) {
            if read.heads == heads {
                return;
            }
            // Undo or switching documents can remove the heads read up to:
            // read the note again rather than keep dates from a discarded
            // history.
            if contains_all(history, &read.heads) {
                if let Some(object) = crate::notebook::page_object(doc, page) {
                    for change in history.get_changes_meta(&read.heads) {
                        if history.change_writes_within(&change.hash, &object) {
                            read.dates.edited =
                                latest(read.dates.edited, time_of(change.timestamp));
                        }
                    }
                }
                read.heads = heads;
                return;
            }
        }
        self.read = Some(DatesRead {
            page,
            dates: dates_of(doc, page),
            heads,
        });
    }
}

/// `page`'s dates, from its own ops.
fn dates_of(doc: &AutoCommit, page: PageId) -> NoteDates {
    let Some(object) = crate::notebook::page_object(doc, page) else {
        return NoteDates::default();
    };
    let history = doc.committed_document();
    let time = |hash: &ChangeHash| {
        history
            .get_change_meta_by_hash(hash)
            .and_then(|change| time_of(change.timestamp))
    };
    NoteDates {
        created: history.hash_for_opid(&object).as_ref().and_then(time),
        edited: history
            .latest_changes_within(&object)
            .unwrap_or_default()
            .iter()
            .filter_map(time)
            .max(),
    }
}

/// A change's time; none for one written before changes were stamped.
fn time_of(timestamp: i64) -> Option<i64> {
    (timestamp > 0).then_some(timestamp)
}

fn latest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    a.max(b)
}

fn contains_all(history: &automerge::Automerge, heads: &[ChangeHash]) -> bool {
    heads
        .iter()
        .all(|hash| history.get_change_meta_by_hash(hash).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{self, Action};
    use tilo_actions::Action as _;
    use tilo_automerge::ApplyIncoming;
    /// `page`'s dates, as an index that has not read `doc` before reads them.
    fn dates(doc: &AutoCommit, page: PageId) -> NoteDates {
        let mut index = NoteDateIndex::default();
        index.sync(doc, Some(page));
        index.get(page)
    }

    /// The ops of objects `read` read to tell what last wrote in them.
    fn ops_read<T>(read: impl FnOnce() -> T) -> u64 {
        let before = automerge::work().object_ops;
        read();
        automerge::work().object_ops - before
    }

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
        let cest = jiff::tz::TimeZone::fixed(jiff::tz::offset(2));
        let labels = dates.display_labels_in(false, &cest);
        assert!(labels[1].contains("Oct 4, 2026, 5:54"), "{labels:?}");
        assert!(!labels[1].contains("UTC"), "{labels:?}");
        let compact =
            dates.display_labels_in(true, &jiff::tz::TimeZone::fixed(jiff::tz::offset(-5)));
        assert!(compact[0].contains("Oct 3, 2026"), "{compact:?}");
    }
    /// A note written elsewhere first says when it arrived here, and its
    /// provenance row says when it was written there: the header never shows
    /// two days without saying which is which.
    #[test]
    fn a_moved_notes_dates_say_it_arrived_here() {
        register_messages();
        let now = jiff::Timestamp::from_second(1_791_086_040)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC);
        let arrived = now.timestamp().as_second();
        let dates = NoteDates {
            created: Some(arrived),
            edited: Some(arrived),
        };
        let moved = dates.labels_at(&now, true);
        assert_eq!(moved.len(), 1);
        assert!(
            moved[0].starts_with("Moved here "),
            "a moved note's creation here is when it arrived: {moved:?}"
        );
        let here = dates.labels_at(&now, false);
        assert!(
            here[0].starts_with("Created "),
            "a note written here is created here: {here:?}"
        );
    }

    /// A page's reactions pick from the whole Unicode catalogue: loading the
    /// app's messages hands the toolkit's picker the catalogue this build
    /// carries.
    #[test]
    fn a_page_offers_the_whole_emoji_catalogue() {
        register_messages();
        let catalogue = ui::emoji_catalogue::catalogue()
            .expect("a native build of Notes carries the Unicode catalogue");
        assert!(
            !catalogue.groups().is_empty(),
            "every Unicode group is there"
        );
        assert!(
            catalogue.search("thumbs", 8).contains(&"👍"),
            "the picker finds a Unicode emoji by name"
        );
    }

    tilo_test_env::scenario! {
        #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
        fn the_date_index_reads_note_dates_from_the_documents_history;

        phase metadata_dates_follow_real_edits_and_survive_reload_and_legacy_notes;
        {
            eprintln!("phase metadata_dates_follow_real_edits_and_survive_reload");
            {
                let mut doc = AutoCommit::new();
                let a = PageId::new_v4();
                let b = PageId::new_v4();
                apply(&mut doc, actions::create_page_action(a, None, "A"), 100);
                apply(&mut doc, actions::create_page_action(b, None, "B"), 200);
                let mut index = NoteDateIndex::default();
                index.sync(&doc, Some(a));
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
                index.sync(&doc, Some(a));
                assert_eq!(
                    index.get(a),
                    NoteDates {
                        created: Some(100),
                        edited: Some(300)
                    }
                );
                assert_eq!(dates(&doc, b).edited, Some(200));
                assert_eq!(index.get(b), NoteDates::default(), "the index reads the open note");
                let mut restored = AutoCommit::load(&doc.save()).unwrap();
                let mut restored_index = NoteDateIndex::default();
                restored_index.sync(&restored, Some(a));
                assert_eq!(restored_index.get(a), index.get(a));
                let heads = restored.get_heads();
                restored_index.sync(&restored, Some(a));
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
                index.sync(&doc, Some(page));
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
                index.sync(&origin, Some(page));
                assert_eq!(
                    index.get(page),
                    NoteDates {
                        created: Some(envelope.timestamp),
                        edited: Some(envelope.timestamp),
                    }
                );
                let registry = tilo_actions::ActionRegistry::with(
                    ui::tilo_resource::group::register::<crate::PagePart>,
                );
                tilo_actions::replay(&mut peer, &envelope, &registry).unwrap();
                let mut peer_index = NoteDateIndex::default();
                peer_index.sync(&peer, Some(page));
                assert_eq!(peer_index.get(page), index.get(page));
                assert_eq!(origin.get_heads(), peer.get_heads());
                let heads = origin.get_heads();
                assert!(
                    tilo_actions::apply_local(&mut origin, &action, actions::encode(&action)).is_err(),
                    "a note is created once"
                );
                index.sync(&origin, Some(page));
                assert_eq!(
                    origin.get_heads(),
                    heads,
                    "no-op writes must not alter metadata"
                );
            }
        }

        phase metadata_reads_the_open_note_alone;
        // A note's dates are read from its own ops, whatever else the notebook
        // holds, and an edit elsewhere is told apart from one in the note by
        // that change's own ops.
        {
            let mut doc = AutoCommit::new();
            let page = PageId::new_v4();
            let other = PageId::new_v4();
            apply(&mut doc, actions::create_page_action(page, None, "Read"), 100);
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
            apply(&mut doc, actions::create_page_action(other, None, "Other"), 150);
            let read_alone = ops_read(|| dates(&doc, page));
            // A notebook a hundred long notes larger.
            for at in 0..100 {
                let note = PageId::new_v4();
                apply(&mut doc, actions::create_page_action(note, None, "More"), 300 + at);
                apply(
                    &mut doc,
                    actions::edit_body_action(
                        note,
                        tilo_document::RichTextAction::AppendParagraph {
                            text: "notebook text ".repeat(40),
                        },
                    ),
                    300 + at,
                );
            }
            let mut index = NoteDateIndex::default();
            let read_among_many = ops_read(|| index.sync(&doc, Some(page)));
            assert_eq!(
                read_among_many, read_alone,
                "the notebook's other notes are not read"
            );
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(200)
                }
            );
            // A member types in another note: nothing of this one is read
            // again and its dates stay.
            apply(&mut doc, actions::rename_page_action(other, "Renamed"), 500);
            assert_eq!(ops_read(|| index.sync(&doc, Some(page))), 0);
            assert_eq!(index.get(page).edited, Some(200));
            // and then in this one.
            apply(&mut doc, actions::rename_page_action(page, "Renamed too"), 600);
            assert_eq!(ops_read(|| index.sync(&doc, Some(page))), 0);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(600)
                }
            );
            assert_eq!(index.get(page), dates(&doc, page), "as a fresh read gives");
            // Another note opened is read afresh.
            index.sync(&doc, Some(other));
            assert_eq!(
                index.get(other),
                NoteDates {
                    created: Some(150),
                    edited: Some(500)
                }
            );
            assert_eq!(index.get(page), NoteDates::default());
        }

        phase metadata_converges_across_concurrent_edits_undo_and_replaced_history;
        {
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
                first_index.sync(&first, Some(page));
                second_index.sync(&second, Some(page));
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
                first_index.sync(&first, Some(page));
                second_index.sync(&second, Some(page));
                assert_eq!(first_index.get(page).edited, Some(200));
                assert_eq!(second_index.get(page).edited, Some(300));
                first.merge_incoming(&mut second).unwrap();
                second.merge_incoming(&mut first).unwrap();
                first_index.sync(&first, Some(page));
                second_index.sync(&second, Some(page));
                assert_eq!(
                    first_index.get(page),
                    NoteDates {
                        created: Some(100),
                        edited: Some(300)
                    }
                );
                assert_eq!(second_index.get(page), first_index.get(page));
                let mut rebuilt = NoteDateIndex::default();
                rebuilt.sync(&first, Some(page));
                assert_eq!(rebuilt.get(page), first_index.get(page));
            }

            eprintln!("phase metadata_shared_undo_and_redo_preserve_creation");
            {
                let mut state = ui::State::new();
                crate::read::initialize_part(&mut state.doc);
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
                index.sync(&state.doc, Some(page));
                assert_eq!(index.get(page).edited, Some(200));
                assert!(state.undo(None, None).is_some());
                assert_eq!(crate::read::list_pages(&state.doc)[0].title, "Original");
                index.sync(&state.doc, Some(page));
                let undo_dates = index.get(page);
                assert_eq!(undo_dates.created, Some(100));
                assert!(
                    undo_dates.edited.is_some_and(|time| time > 200),
                    "shared undo commits its own real timestamp"
                );
                assert!(state.redo(None, None).is_some());
                assert_eq!(crate::read::list_pages(&state.doc)[0].title, "Renamed");
                index.sync(&state.doc, Some(page));
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
                index.sync(&doc, Some(page));
                let historical = doc.fork_at(&before).unwrap();
                index.sync(&historical, Some(page));
                assert_eq!(
                    index.get(page),
                    NoteDates {
                        created: Some(100),
                        edited: Some(100)
                    }
                );
                index.sync(&AutoCommit::new(), Some(page));
                assert_eq!(
                    index.get(page),
                    NoteDates::default(),
                    "another notebook must not inherit these dates"
                );
            }
        }
    }
}
