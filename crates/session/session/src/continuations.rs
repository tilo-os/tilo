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
    pub(crate) record: String,
}

thread_local! {
    static DIAGNOSTIC: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// What the windows this session runs show, by the tab each runs in.
#[derive(Default)]
pub(crate) struct Continuations {
    shown: Mutex<BTreeMap<u64, Showing>>,
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
    pub(crate) fn read(&self, held: &[(u64, CrdtRoute)], app_of: &dyn Fn(u64) -> Option<String>) {
        let described = crate::continuations::DIAGNOSTIC.with(|last| {
            let asked = format!(
                "{} {:?}",
                held.len(),
                held.iter()
                    .map(|(instance, route)| format!("{instance}:{}", route.channel_key()))
                    .collect::<Vec<_>>()
            );
            let mut last = last.borrow_mut();
            if *last == asked {
                return None;
            }
            *last = asked.clone();
            Some(asked)
        });
        if let Some(described) = described {
            tracing::error!("DIAGNOSTIC window records read {described}");
        }
        let mut windows: BTreeMap<u64, Vec<&CrdtRoute>> = BTreeMap::new();
        for (instance, route) in held {
            windows.entry(*instance).or_default().push(route);
        }
        let mut shown = self.shown.lock_recover();
        shown.clear();
        for (instance, routes) in windows {
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
            if let Some(second) = records.next() {
                tracing::error!(
                    instance,
                    first = %record.channel_key(),
                    second = %second.channel_key(),
                    held = held.len(),
                    "DIAGNOSTIC a window holds several records"
                );
                continue;
            }
            let Some(workspace) = WorkspaceId::parse(&record.cluster_id) else {
                tracing::error!(
                    instance,
                    cluster = %record.cluster_id,
                    record = %record.record_id,
                    "DIAGNOSTIC a window's record names no workspace"
                );
                continue;
            };
            shown.insert(
                instance,
                Showing {
                    workspace,
                    record: record.record_id.clone(),
                },
            );
        }
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

    /// A window's app holding `records` , in `workspace` .
    /// The app every window in these tests runs.
    fn notes_app(_: u64) -> Option<String> {
        Some("so.tilo.notes".to_owned())
    }

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
        notes.read(&routes, &notes_app);

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
        notes.read(&routes, &notes_app);
        assert_eq!(
            notes.of_workspace(room),
            [(12, "plan".to_owned())],
            "the route its app speaks the session on is no record"
        );

        notes.read(&held(room, 12, &["continuation:12"]), &notes_app);
        assert!(
            notes.of_workspace(room).is_empty(),
            "a window holding only that route shows no record"
        );
    }

    #[test]
    fn a_window_holding_several_records_notes_none() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        notes.read(&held(room, 12, &["plan", "index"]), &notes_app);
        assert!(
            notes.of_workspace(room).is_empty(),
            "which of them it shows is not something the broker says"
        );
        notes.read(&held(room, 12, &["index"]), &notes_app);
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
        notes.read(&routes, &notes_app);
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
        notes.read(&routes, &notes_app);
        assert_eq!(notes.window_record(12), None);
    }

    #[test]
    fn a_window_that_closed_leaves_no_note() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        notes.read(&held(room, 12, &["plan"]), &notes_app);
        assert_eq!(
            notes.window_record(12),
            Some(("plan".to_owned(), String::new()))
        );
        notes.read(&[], &notes_app);
        assert_eq!(notes.window_record(12), None);
        assert!(notes.of_workspace(room).is_empty());
    }

    #[test]
    fn a_members_record_notes_the_kind_of_resource_it_keeps() {
        let notes = Continuations::default();
        let room = WorkspaceId::new();
        let person = "abababababababababababababababababababababababababababababababab";
        let record = tilo_action_policy::member_record_id(person, "tilo.note");
        notes.read(&held(room, 12, &[&record]), &notes_app);
        assert_eq!(
            notes.window_record(12),
            Some((record, "tilo.note".to_owned())),
            "a person's note is shared as the note it is"
        );
    }
}
