//! What each window on this desk shows, and the continuation channel its app
//! speaks on.
//!
//! A window's app holds the record it shows, and the session's broker knows
//! every record each window holds, by the window instance the shell launched
//! it for ([`tilo_action_broker::collab_broker::AppActionBroker::window_records`]).
//! A window's instance is the id of its tab — the session tells the app so
//! when it starts it — so a note is about a tab, and every place that names a
//! window can read it.
//!
//! That is the one source. The session reads it on every pass, writes the
//! record beside the window in the workspace's layout
//! ([`crate::shared_desktop`]), places the window by it in the person's
//! arrangement ([`crate::arrangement`]) and answers what this device's desk
//! asks of it ([`tilo_desktop_policy::WindowNotes`]). No report from an app
//! writes a note.
//!
//! The continuation channel is served here too: a window's checkpoint and
//! handoff reports reach the session that runs it rather than a refusal, and
//! what they say is about the window, never about the record noted beside it.
//! The answers are kept here until the pass that asked for them takes them up
//! ([`Continuations::take_handoffs`]): they arrive on the broker's own thread,
//! which decides nothing about a move.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page notes its windows' records beside them next"
    )
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use collab_types::handoff::HandoffReport;
use tilo_broker_core::route::CrdtRoute;
use tilo_broker_core::steps::ContinuationServices;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;

/// How many of a window's answers wait to be taken up before the oldest are let go: a step of a
/// move is asked one at a time, and a session that stopped taking them up has no move to finish.
const WAITING: usize = 256;

/// One answer a window's app gave about a handoff step, and whose it is.
#[derive(Clone, Debug)]
pub(crate) struct HandoffNote {
    /// The workspace the window runs in, as its app names it.
    pub(crate) workspace: String,
    /// The package that runs the window's app.
    pub(crate) app: String,
    /// The window's instance: the tab it fills.
    pub(crate) instance: u64,
    /// What it answered.
    pub(crate) report: HandoffReport,
}

/// The record one window shows, and the workspace its window is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Showing {
    pub(crate) workspace: WorkspaceId,
    /// The app the window runs, when its app said so: a window drawn by the
    /// app's own surface names no app in the desk's own tree.
    pub(crate) app: Option<String>,
    pub(crate) record: String,
}

/// What the windows this session runs show, by the tab each runs in.
#[derive(Default)]
pub(crate) struct Continuations {
    shown: Mutex<BTreeMap<u64, Showing>>,
    /// What each window said it shows, by the tab it runs in: its app names
    /// the record it holds once, where the broker's channels cannot (an app
    /// that keeps one document for the whole workspace speaks on a channel
    /// that names no record).
    named: Mutex<BTreeMap<u64, Showing>>,
    /// What the windows' apps answered about a handoff step, oldest first.
    handoffs: Mutex<VecDeque<HandoffNote>>,
}

impl Continuations {
    /// Note what each window shows, as the broker lists the records its apps
    /// hold: the instance the shell launched each window for, and a record
    /// the window holds.
    ///
    /// A window notes the record it holds when it holds one: an app launched
    /// for a window speaks to the session on the route its instance names,
    /// which is not a record it shows, and a window holding several records
    /// of its own app shows one of them that nothing here can tell apart — it
    /// notes none rather than naming the wrong one.
    ///
    /// `app_of` names the app each window runs: the records of any other app it
    /// holds (the desk's, a service's) are not what its window shows.
    pub(crate) fn read(
        &self,
        held: &[(u64, CrdtRoute)],
        app_of: &dyn Fn(u64) -> Option<String>,
        live: &std::collections::BTreeSet<u64>,
    ) {
        let mut windows: BTreeMap<u64, Vec<&CrdtRoute>> = BTreeMap::new();
        for (instance, route) in held {
            windows.entry(*instance).or_default().push(route);
        }
        let mut named = self.named.lock_recover();
        // A window the desk no longer shows says nothing any more. A desk drawn with no window at
        // all — the pass that lays it out, a moment between workspaces — says nothing of the kind:
        // only a desk showing other windows can say one is gone.
        if !live.is_empty() {
            named.retain(|tab, _| live.contains(tab));
        }
        let mut shown = self.shown.lock_recover();
        shown.clear();
        for (instance, routes) in windows {
            // What the window's app said it shows is what it shows, however its
            // channels name it.
            if let Some(said) = named.get(&instance) {
                shown.insert(instance, said.clone());
                continue;
            }
            let app = app_of(instance);
            let mut records = routes.into_iter().filter(|route| {
                // The route a window's app speaks to the session on names the window's instance,
                // not a record the window shows.
                !route.record_id.is_empty()
                    && route
                        .record_id
                        .strip_prefix("continuation:")
                        .is_none_or(|named| named != instance.to_string())
                    // A window shows the record of the app it runs; another app's record it holds
                    // is not its own.
                    && app
                        .as_deref()
                        .is_none_or(|app| route.app_id == app)
            });
            let Some(record) = records.next() else {
                continue;
            };
            if records.next().is_some() {
                continue;
            }
            let Some(workspace) = WorkspaceId::parse(&record.cluster_id) else {
                continue;
            };
            shown.insert(
                instance,
                Showing {
                    workspace,
                    app: Some(record.app_id.clone()),
                    record: record.record_id.clone(),
                },
            );
        }
        // A window whose app named what it shows is known by that, even where the broker names no
        // record for it.
        for (tab, said) in named.iter() {
            if !shown.contains_key(tab) {
                shown.insert(*tab, said.clone());
            }
        }
    }

    /// Window `tab` is gone: what it said it shows goes with it.
    pub(crate) fn forget(&self, tab: u64) {
        self.named.lock_recover().remove(&tab);
        self.shown.lock_recover().remove(&tab);
    }

    /// Window `tab` says it shows `record` of `workspace` (a record id as the wire spells it), as
    /// its app holds it: the one source of what a window shows, for its place, the layout beside
    /// it and where anyone here is. Whether that is news.
    pub(crate) fn note(&self, tab: u64, app: &str, workspace: &str, record: &str) -> bool {
        if record.trim().is_empty() {
            return false;
        }
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let showing = Showing {
            workspace,
            app: Some(app.to_owned()),
            record: record.to_owned(),
        };
        {
            let mut named = self.named.lock_recover();
            if named.get(&tab) == Some(&showing) {
                return false;
            }
            named.insert(tab, showing.clone());
        }
        self.shown.lock_recover().insert(tab, showing);
        true
    }

    /// The app each window runs, when its app said which one: what tells a window drawn by its
    /// app's own surface which app it is.
    pub(crate) fn app_of(&self, tab: u64) -> Option<String> {
        self.shown.lock_recover().get(&tab)?.app.clone()
    }

    /// What each window shows, by tab, as its app and its record: the desk's people and its
    /// presence read where a window is from here.
    pub(crate) fn windows(&self) -> Vec<(u64, Option<String>, String)> {
        self.shown
            .lock_recover()
            .iter()
            .map(|(tab, showing)| (*tab, showing.app.clone(), showing.record.clone()))
            .collect()
    }

    /// The record each window shows, by tab: what the desk keeps beside its windows, so a place
    /// comes back showing the record it was made on.
    pub(crate) fn records(&self) -> BTreeMap<u64, String> {
        self.shown
            .lock_recover()
            .iter()
            .map(|(tab, showing)| (*tab, showing.record.clone()))
            .collect()
    }

    /// The record each window in `workspace` shows, by tab: what a write of that workspace's layout
    /// record notes beside its windows.
    pub(crate) fn of_workspace(&self, workspace: WorkspaceId) -> Vec<(u64, String)> {
        self.shown
            .lock_recover()
            .iter()
            .filter(|(_, showing)| showing.workspace == workspace)
            .map(|(tab, showing)| (*tab, showing.record.clone()))
            .collect()
    }

    /// The record `tab`'s window shows and the kind of resource it keeps,
    /// for the windows a person may share.
    pub(crate) fn window_record(&self, tab: u64) -> Option<(String, String)> {
        let record = self.shown.lock_recover().get(&tab)?.record.clone();
        let kind = tilo_action_policy::member_record(&record)
            .map(|(_, kind)| kind.to_owned())
            .unwrap_or_default();
        Some((record, kind))
    }

    /// Take up what the windows' apps answered about a handoff step since the last pass: what a
    /// move under way waits on.
    pub(crate) fn take_handoffs(&self) -> Vec<HandoffNote> {
        self.handoffs.lock_recover().drain(..).collect()
    }
}

/// The desk's reading of the notes: what the surface asks of the window a person may share.
pub(crate) struct DeskNotes(pub(crate) std::sync::Arc<Continuations>);

impl tilo_desktop_policy::WindowNotes for DeskNotes {
    fn window_record(&self, tab: u64) -> Option<(String, String)> {
        self.0.window_record(tab)
    }
}

impl ContinuationServices for Continuations {
    fn report(
        &self,
        workspace: WorkspaceId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String> {
        tracing::debug!(
            %workspace,
            %app,
            instance,
            schema,
            bytes = metadata.len(),
            "a window reported how it resumes"
        );
        Ok(())
    }

    fn clear(&self, workspace: WorkspaceId, app: &str, instance: u64) -> Result<(), String> {
        tracing::debug!(%workspace, %app, instance, "a window has nothing to resume");
        Ok(())
    }

    fn handoff(
        &self,
        workspace: WorkspaceId,
        app: String,
        instance: u64,
        message: Vec<u8>,
    ) -> Result<(), String> {
        let Some(report) = HandoffReport::decode(&message) else {
            tracing::warn!(
                %workspace,
                %app,
                instance,
                bytes = message.len(),
                "a window's answer to a handoff does not decode"
            );
            return Ok(());
        };
        tracing::debug!(%workspace, %app, instance, ?report, "a window answered a handoff");
        let mut waiting = self.handoffs.lock_recover();
        if waiting.len() >= WAITING {
            waiting.pop_front();
        }
        waiting.push_back(HandoffNote {
            workspace: workspace.to_string(),
            app,
            instance,
            report,
        });
        drop(waiting);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tabs the desk shows in these tests: every one the broker names.
    fn instances(held: &[(u64, CrdtRoute)]) -> std::collections::BTreeSet<u64> {
        held.iter().map(|(tab, _)| *tab).collect()
    }

    /// The app every window in these tests runs.
    fn notes_app() -> impl Fn(u64) -> Option<String> {
        |_| Some("so.tilo.notes".to_owned())
    }

    /// A window's app holding `records` , in `workspace` .
    fn held(workspace: WorkspaceId, instance: u64, records: &[&str]) -> Vec<(u64, CrdtRoute)> {
        records
            .iter()
            .map(|record| {
                (
                    instance,
                    CrdtRoute::record("so.tilo.notes", workspace.to_string(), *record),
                )
            })
            .collect()
    }

    #[test]
    fn a_window_notes_the_record_it_holds_beside_its_tab() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        let other = WorkspaceId::new();
        let mut routes = held(room, 12, &["plan"]);
        routes.extend(held(other, 30, &["budget"]));
        notes.read(&routes, &notes_app(), &instances(&routes));

        assert_eq!(
            notes.window_record(12),
            Some(("plan".to_owned(), String::new()))
        );
        assert_eq!(
            notes.of_workspace(room),
            [(12, "plan".to_owned())],
            "the arrangement places each window by the record it shows, and a layout record notes only the windows of its workspace"
        );
        assert_eq!(notes.of_workspace(other), [(30, "budget".to_owned())]);
    }

    #[test]
    fn a_windows_own_channel_is_not_a_record_it_shows() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        let mut routes = held(room, 12, &["plan"]);
        routes.push((
            12,
            CrdtRoute::record("so.tilo.notes", room.to_string(), "continuation:12"),
        ));
        notes.read(&routes, &notes_app(), &instances(&routes));
        assert_eq!(
            notes.of_workspace(room),
            [(12, "plan".to_owned())],
            "the route its app speaks the session on is no record"
        );

        notes.read(
            &held(room, 12, &["continuation:12"]),
            &notes_app(),
            &instances(&held(room, 12, &["continuation:12"])),
        );
        assert!(
            notes.of_workspace(room).is_empty(),
            "a window holding only that route shows no record"
        );
    }

    #[test]
    fn a_window_holding_several_records_notes_none() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        notes.read(
            &held(room, 12, &["plan", "index"]),
            &notes_app(),
            &instances(&held(room, 12, &["plan", "index"])),
        );
        assert!(
            notes.of_workspace(room).is_empty(),
            "which of them it shows is not something the broker says"
        );
        notes.read(
            &held(room, 12, &["index"]),
            &notes_app(),
            &instances(&held(room, 12, &["index"])),
        );
        assert_eq!(
            notes.window_record(12),
            Some(("index".to_owned(), String::new())),
            "and one record left is the one it shows"
        );
    }

    /// A window holds the desk's records too (the workspace's layout, a service's): only the
    /// record of the app it runs is what it shows, so its place and the person's are still named.
    #[test]
    fn a_window_notes_its_own_apps_record_and_no_other_apps() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        let mut routes = held(room, 12, &["plan"]);
        routes.push((
            12,
            CrdtRoute::record("so.tilo.desktop", room.to_string(), "desk"),
        ));
        notes.read(&routes, &notes_app(), &instances(&routes));
        assert_eq!(
            notes.window_record(12),
            Some(("plan".to_owned(), String::new())),
            "the record of the app its window runs"
        );
        // Two records of its own app still name none: nothing tells which it shows.
        routes.push((
            12,
            CrdtRoute::record("so.tilo.notes", room.to_string(), "index"),
        ));
        notes.read(&routes, &notes_app(), &instances(&routes));
        assert_eq!(notes.window_record(12), None);
    }

    /// A window whose app names the record it shows is what it shows, however its channels name
    /// it: an app that keeps one document for the whole workspace speaks on a channel that names
    /// no record. The window that closes is forgotten with it.
    #[test]
    fn a_window_that_names_its_record_is_known_by_it() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        assert!(
            notes.note(12, "so.tilo.notes", &room.to_string(), "notes.notebook"),
            "it is news"
        );
        assert!(
            !notes.note(12, "so.tilo.notes", &room.to_string(), "notes.notebook"),
            "the same window, once"
        );
        notes.read(
            &[(12, CrdtRoute::app("so.tilo.notes", room.to_string()))],
            &notes_app(),
            &[12].into_iter().collect(),
        );
        assert_eq!(
            notes.window_record(12),
            Some(("notes.notebook".to_owned(), String::new())),
            "the record it said it shows, not the app-scoped channel it speaks on"
        );
        assert_eq!(
            notes.of_workspace(room),
            [(12, "notes.notebook".to_owned())]
        );
        // A desk drawn with no window at all says nothing of the kind: only a desk showing other
        // windows can say this one is gone.
        notes.read(&[], &notes_app(), &std::collections::BTreeSet::new());
        assert!(notes.window_record(12).is_some());
        notes.read(&[], &notes_app(), &[13].into_iter().collect());
        assert_eq!(notes.window_record(12), None, "gone with its window");
        // Said again by a window the session has not heard of yet: it is news.
        assert!(notes.note(12, "so.tilo.notes", &room.to_string(), "notes.notebook"));
    }

    #[test]
    fn a_window_that_closed_leaves_no_note() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        notes.read(
            &held(room, 12, &["plan"]),
            &notes_app(),
            &instances(&held(room, 12, &["plan"])),
        );
        assert_eq!(
            notes.window_record(12),
            Some(("plan".to_owned(), String::new()))
        );
        notes.read(&[], &notes_app(), &std::collections::BTreeSet::new());
        assert_eq!(notes.window_record(12), None);
        assert!(notes.of_workspace(room).is_empty());
    }

    #[test]
    fn a_members_record_notes_the_kind_of_resource_it_keeps() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        let person = "abababababababababababababababababababababababababababababababab";
        let record = tilo_action_policy::member_record_id(person, "tilo.note");
        notes.read(
            &held(room, 12, &[&record]),
            &notes_app(),
            &instances(&held(room, 12, &[&record])),
        );
        assert_eq!(
            notes.window_record(12),
            Some((record, "tilo.note".to_owned())),
            "a person's note is shared as the note it is"
        );
    }
}
