//! Read-only note dates from the canonical, persisted Automerge history.
//!
//! No migration rewrites old notes or invents creation dates. Unstamped legacy
//! changes retain an unknown date; new action timestamps survive saves and merges.
use crate::model::PageId;
use automerge::{AutoCommit, ChangeHash};
use std::collections::{HashMap, HashSet};
use ui::history_source::{SchemaObjectId, schema_operations_since};

const CATALOG: &str = include_str!("../locales/en-US.ftl");

pub fn register_messages() {
    use ui::i18n::I18nManager;
    I18nManager::register_widget_messages();
    I18nManager::add_resource("en-US", CATALOG.to_owned());
    I18nManager::add_resource("de", include_str!("../locales/de.ftl").to_owned());
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
    use chrono::{Datelike, TimeZone, Timelike};
    use ui::i18n::I18nManager;
    let Some(moment) = chrono::Local.timestamp_opt(seconds, 0).single() else {
        return String::new();
    };
    // A moment the clock has not reached yet (a peer's skewed clock) still
    // reads as today rather than as a date in the future.
    if moment.date_naive() >= now.date_naive() {
        let time = format!("{:02}:{:02}", moment.hour(), moment.minute());
        return I18nManager::format("notes-date-today", &[("time", time.into())]);
    }
    let month = ui::format::month_abbrev(moment.month0() as usize);
    if moment.year() == now.year() {
        I18nManager::format(
            "notes-date-day",
            &[
                ("day", moment.day().to_string().into()),
                ("month", month.into()),
            ],
        )
    } else {
        I18nManager::format(
            "notes-date-day-year",
            &[
                ("day", moment.day().to_string().into()),
                ("month", month.into()),
                ("year", moment.year().to_string().into()),
            ],
        )
    }
}

#[derive(Default)]
pub struct NoteDateIndex {
    heads: Vec<ChangeHash>,
    objects: HashMap<SchemaObjectId, PageId>,
    page_maps: HashSet<SchemaObjectId>,
    assignments: HashSet<SchemaObjectId>,
    dates: HashMap<PageId, NoteDates>,
}

impl NoteDateIndex {
    pub fn get(&self, page: PageId) -> NoteDates {
        self.dates.get(&page).copied().unwrap_or_default()
    }

    pub fn sync(&mut self, doc: &mut AutoCommit) {
        let heads = doc.get_heads();
        if heads == self.heads {
            return;
        }
        // Undo or switching documents can remove our prior heads. Rebuild the
        // projection instead of leaking dates from a discarded history branch.
        if self
            .heads
            .iter()
            .any(|hash| doc.get_change_by_hash(hash).is_none())
        {
            *self = Self::default();
        }
        for op in schema_operations_since(doc, &self.heads) {
            let key = op.key.as_deref();
            if op.object.is_root() && op.new_object.is_some() {
                match key {
                    Some("pages") => {
                        if let Some(object) = op.new_object.clone() {
                            self.page_maps.insert(object);
                        }
                    }
                    Some("tag_assignments") => {
                        if let Some(object) = op.new_object.clone() {
                            self.assignments.insert(object);
                        }
                    }
                    _ => {}
                }
            }
            let mut page = self.objects.get(&op.object).copied();
            if self.page_maps.contains(&op.object) && op.new_object.is_some() {
                page = key.and_then(|key| key.parse().ok());
                if let Some(page) = page {
                    self.dates.entry(page).or_insert(NoteDates {
                        created: op.timestamp,
                        edited: op.timestamp,
                    });
                }
            } else if self.assignments.contains(&op.object) {
                page = key
                    .and_then(|key| key.split_once(':'))
                    .and_then(|(page, _)| page.parse().ok());
            }
            if let Some(page) = page {
                if let Some(new_object) = op.new_object {
                    self.objects.insert(new_object, page);
                }
                let dates = self.dates.entry(page).or_default();
                dates.edited = dates.edited.max(op.timestamp);
            }
        }
        self.heads = heads;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{self, NotesAction};
    use tilo_actions::Action;
    use tilo_automerge::ApplyIncoming;
    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper models handing an owned action to the production executor"
    )]
    fn apply(doc: &mut AutoCommit, action: NotesAction, timestamp: i64) {
        action.apply(doc).unwrap();
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(timestamp));
    }
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn metadata_dates_follow_real_edits_and_survive_reload_and_legacy_notes() {
        eprintln!("phase metadata_dates_follow_real_edits_and_survive_reload");
        {
            let mut doc = AutoCommit::new();
            let a = PageId::new_v4();
            let b = PageId::new_v4();
            apply(&mut doc, actions::new_page_action(a, None, 0, "A"), 100);
            apply(&mut doc, actions::new_page_action(b, None, 1, "B"), 200);
            let mut index = NoteDateIndex::default();
            index.sync(&mut doc);
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
            index.sync(&mut doc);
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
            restored_index.sync(&mut restored);
            assert_eq!(restored_index.get(a), index.get(a));
            let heads = restored.get_heads();
            restored_index.sync(&mut restored);
            assert_eq!(restored.get_heads(), heads, "viewing metadata is read-only");
        }

        eprintln!("phase metadata_legacy_creation_remains_unknown_after_edit");
        {
            let mut doc = AutoCommit::new();
            let page = PageId::new_v4();
            apply(
                &mut doc,
                actions::new_page_action(page, None, 0, "Legacy"),
                0,
            );
            let meta = crate::doc::NotesDoc::list_pages(&doc).remove(0);
            apply(&mut doc, actions::rename_page_action(&meta, "Renamed"), 500);
            let mut index = NoteDateIndex::default();
            index.sync(&mut doc);
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
            let action = actions::new_page_action(page, None, 0, "New note");
            let envelope =
                tilo_actions::apply_local(&mut origin, &action, actions::encode(&action)).unwrap();
            assert!(
                envelope.timestamp > 0,
                "new notes must have a real persisted time"
            );
            let mut index = NoteDateIndex::default();
            index.sync(&mut origin);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(envelope.timestamp),
                    edited: Some(envelope.timestamp),
                }
            );
            let mut registry = tilo_actions::ActionRegistry::new();
            actions::register(&mut registry);
            tilo_actions::replay(&mut peer, &envelope, &registry).unwrap();
            let mut peer_index = NoteDateIndex::default();
            peer_index.sync(&mut peer);
            assert_eq!(peer_index.get(page), index.get(page));
            assert_eq!(origin.get_heads(), peer.get_heads());
            let heads = origin.get_heads();
            tilo_actions::apply_local(&mut origin, &action, actions::encode(&action)).unwrap();
            index.sync(&mut origin);
            assert_eq!(
                origin.get_heads(),
                heads,
                "no-op writes must not alter metadata"
            );
        }
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
                actions::new_page_action(page, None, 0, "Original"),
                100,
            );
            let meta = crate::doc::NotesDoc::list_pages(&base).remove(0);
            let mut first = base.fork();
            let mut second = base.fork();
            let mut first_index = NoteDateIndex::default();
            let mut second_index = NoteDateIndex::default();
            first_index.sync(&mut first);
            second_index.sync(&mut second);
            apply(
                &mut first,
                actions::rename_page_action(&meta, "Renamed"),
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
            first_index.sync(&mut first);
            second_index.sync(&mut second);
            assert_eq!(first_index.get(page).edited, Some(200));
            assert_eq!(second_index.get(page).edited, Some(300));
            first.merge_incoming(&mut second).unwrap();
            second.merge_incoming(&mut first).unwrap();
            first_index.sync(&mut first);
            second_index.sync(&mut second);
            assert_eq!(
                first_index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(300)
                }
            );
            assert_eq!(second_index.get(page), first_index.get(page));
            let mut rebuilt = NoteDateIndex::default();
            rebuilt.sync(&mut first);
            assert_eq!(rebuilt.get(page), first_index.get(page));
        }

        eprintln!("phase metadata_shared_undo_and_redo_preserve_creation");
        {
            let mut state = ui::State::new();
            let page = PageId::new_v4();
            apply(
                &mut state.doc,
                actions::new_page_action(page, None, 0, "Original"),
                100,
            );
            let before = state.doc.get_heads();
            let meta = crate::doc::NotesDoc::list_pages(&state.doc).remove(0);
            apply(
                &mut state.doc,
                actions::rename_page_action(&meta, "Renamed"),
                200,
            );
            state.record_committed_action(before, None, None);
            let mut index = NoteDateIndex::default();
            index.sync(&mut state.doc);
            assert_eq!(index.get(page).edited, Some(200));
            assert!(state.undo(None, None).is_some());
            assert_eq!(
                crate::doc::NotesDoc::list_pages(&state.doc)[0].title,
                "Original"
            );
            index.sync(&mut state.doc);
            let undo_dates = index.get(page);
            assert_eq!(undo_dates.created, Some(100));
            assert!(
                undo_dates.edited.is_some_and(|time| time > 200),
                "shared undo commits its own real timestamp"
            );
            assert!(state.redo(None, None).is_some());
            assert_eq!(
                crate::doc::NotesDoc::list_pages(&state.doc)[0].title,
                "Renamed"
            );
            index.sync(&mut state.doc);
            assert_eq!(index.get(page).created, Some(100));
            assert!(index.get(page).edited >= undo_dates.edited);
        }

        eprintln!("phase metadata_rebuilds_when_document_history_is_replaced");
        {
            let mut doc = AutoCommit::new();
            let page = PageId::new_v4();
            apply(
                &mut doc,
                actions::new_page_action(page, None, 0, "Original"),
                100,
            );
            let before = doc.get_heads();
            let meta = crate::doc::NotesDoc::list_pages(&doc).remove(0);
            apply(&mut doc, actions::rename_page_action(&meta, "Renamed"), 200);
            let mut index = NoteDateIndex::default();
            index.sync(&mut doc);
            let mut historical = doc.fork_at(&before).unwrap();
            index.sync(&mut historical);
            assert_eq!(
                index.get(page),
                NoteDates {
                    created: Some(100),
                    edited: Some(100)
                }
            );
            index.sync(&mut AutoCommit::new());
            assert_eq!(
                index.get(page),
                NoteDates::default(),
                "another notebook must not inherit these dates"
            );
        }
    }
}
