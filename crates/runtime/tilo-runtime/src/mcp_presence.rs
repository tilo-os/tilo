//! `tilo mcp`'s agent in its workspace: the presence frame every desktop
//! draws an agent by — who it is, what it is doing now, and the record it
//! acts in — said on the workspace's presence track for as long as the
//! connection lives.
//!
//! A device says its own presence as a [`ShellPresence`] on the workspace's
//! plane, and an agent says one the same way with `agent: true`; the
//! desktops of the workspace read that frame as a member of kind agent, draw
//! its face beside the people's with the line it works on
//! ([`Presence::called`]), and bring whoever follows it to the record the
//! frame names. The agent's word is the client's board, as every member's is
//! ([`tilo_session_client::Presence`]): the server sets it when it enters the
//! workspace and after every tool call, and the board restates it on its own
//! schedule while the agent thinks, so an agent that idles between calls
//! stays present; the board takes the agent down when the connection ends.
//!
//! Invoking an action shows what it did: every record a call makes or
//! changes is opened as a window on the workspace's desk before the frame
//! naming it is said. The host the agent acts through opens it there, as a
//! window of the person's naming the agent's run, so everyone in the
//! workspace sees it as they see a colleague's; a record already open keeps
//! its one window, and following the agent brings that window forward. What
//! a call only reads opens nothing.
//!
//! Each action the agent takes in a record is narrated as it goes
//! ([`Presence::acting`]): the semantic work every desktop draws as the
//! agent's cursor travelling to the control the action is bound to, on the
//! record's live stream. What the action changed is said as the agent's
//! selection for a moment, which every window that shows the record
//! highlights in the agent's colour, and then settles to a caret at its end.

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use collab_types::presence::{MAX_ACTIVITY_BYTES, PresenceKey, ShellSelection, agent_rgb};
use serde_json::Value;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost as _, Caller, InvokeArgument,
    InvokeRequest, ReceiptStatus, application_action_target, application_record_target,
    settle_with,
};
use tilo_relay_protocol::live::{LiveAnchor, LiveFrame, LiveRecord, OpsFrame};
use tilo_relay_protocol::live::{presence_anchors, read_presence_anchors};
use tilo_session_host::session_actions::SessionActions;
use tilo_session_host::{PeerHandle, narration};
use tilo_user_identity::DeviceCert;

use crate::mcp::{ActedRecord, Acting};

/// The longest the board's clock sleeps before it looks again.
const LONGEST_WAIT: Duration = Duration::from_secs(1);

/// How long what an action changed stays the agent's selection, highlighted
/// in its colour, before it settles to the caret at its end: long enough to
/// see what changed, short enough that the next change reads as new. Changes
/// to the same text within it grow the one highlight.
const HIGHLIGHT: Duration = Duration::from_millis(1500);

/// Longest activity line the agent says, the ellipsis included: a line a
/// person reads at a glance, where the roster draws it.
const SHOWN_CHARS: usize = 64;

/// Longest the agent waits for the host to open the record it acts in as a
/// window before it says it is there anyway: a desktop then shows its app
/// until the window arrives.
const OPENING: tilo_clock::Duration = tilo_clock::Duration::from_secs(5);

/// The agent `tilo mcp` serves, present in the workspace its device entered.
pub struct Presence {
    peer: PeerHandle,
    /// The agent's word in the workspace, as every member's board reads it.
    board: Mutex<tilo_session_client::Presence>,
    /// The host the agent acts through, which opens the windows it acts in.
    actions: SessionActions,
    /// Who asks it: the person the agent acts for.
    caller: Caller,
    /// The workspace, as its presence frames name it.
    cluster: String,
    /// The workspace's number on this device's session.
    workspace_id: u64,
    /// The agent's name, as the person's devices list it.
    label: String,
    /// The colour its face wears: the same wherever it is drawn and in every
    /// run of it, derived from its name as History colours its changes.
    colour: (u8, u8, u8),
    /// What the last frame said, and the sequence around it.
    said: Mutex<Said>,
}

/// What the agent says of itself: its view's sequence, its line and the
/// record it acts in.
#[derive(Default)]
struct Said {
    seq: u32,
    activity: Option<String>,
    focus: Option<LiveRecord>,
    /// What the agent's last change left selected, or its caret.
    selection: Option<ShellSelection>,
    /// The change shown highlighted now, until it settles to its caret.
    highlight: Option<Highlight>,
    /// The sequence of the agent's narration on the live plane.
    narrated: u64,
    /// The records whose live streams this device asked to take part in:
    /// narration on a record is sealed with that record's keys.
    joined: Vec<LiveRecord>,
}

/// What the agent changed, shown as its selection until `until`.
struct Highlight {
    key: PresenceKey,
    start: usize,
    end: usize,
    until: Instant,
}

impl Presence {
    /// The agent's presence in the workspace `cluster` this device (`cert`) entered; `None` when
    /// the certificate names no device or `cluster` no workspace.
    #[must_use]
    pub fn new(
        (peer, cert): (&PeerHandle, &DeviceCert),
        actions: SessionActions,
        person: String,
        (cluster, workspace_id): (String, u64),
        label: String,
    ) -> Option<Arc<Self>> {
        let board = tilo_session_host::saying::enter(peer, cert, &label, &cluster)?;
        Some(Arc::new(Self {
            peer: peer.clone(),
            board: Mutex::new(board),
            actions,
            caller: Caller::local_person(person),
            colour: agent_rgb(&label),
            cluster,
            workspace_id,
            label,
            said: Mutex::new(Said::default()),
        }))
    }

    /// Show the agent present from now on: its word, then what the board owes — its repeats and
    /// keepalives — until the last [`Arc<Presence>`] is dropped.
    pub fn arrive(self: &Arc<Self>) {
        self.say();
        let weak: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(wait) = weak.upgrade().map(|presence| presence.owed_in()) else {
                    return;
                };
                tokio::time::sleep(wait).await;
                let Some(presence) = weak.upgrade() else {
                    return;
                };
                presence.board().due_in(&presence.cluster);
                // Held for this pass alone, so the loop keeps the agent
                // present without keeping it alive.
                drop(presence);
            }
        });
    }

    /// The agent's board; a poisoned lock still holds a whole word.
    fn board(&self) -> std::sync::MutexGuard<'_, tilo_session_client::Presence> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How long until the board next owes its workspace, the one in front, a word.
    fn owed_in(&self) -> Duration {
        self.board().next_deadline().map_or(LONGEST_WAIT, |due| {
            due.saturating_duration_since(tilo_clock::Instant::now())
                .min(LONGEST_WAIT)
        })
    }

    /// How long until the agent's board next owes its workspace a word: the
    /// interval the idle loop above keeps, whatever [`LONGEST_WAIT`] caps its
    /// sleeps at. `None` while the board owes none, which is an agent whose
    /// board holds no workspace to say itself in.
    #[must_use]
    pub fn restates_in(&self) -> Option<Duration> {
        self.board()
            .next_deadline()
            .map(|due| due.saturating_duration_since(tilo_clock::Instant::now()))
    }

    /// Say what the call `request` made is doing now, and the record it
    /// worked in (`acted`): the line the person reads under the agent's
    /// face, and where following it brings them. The records the call made
    /// or changed (`changed`) are opened as windows first, so the frame that
    /// names one finds it shown.
    pub async fn called(
        &self,
        request: &Value,
        failed: bool,
        acted: Option<ActedRecord>,
        changed: Vec<ActedRecord>,
    ) {
        let line = now_line(request, failed);
        let focus = acted.map(|worked| LiveRecord {
            app: worked.app,
            record: worked.record,
        });
        for record in changed {
            self.open(&LiveRecord {
                app: record.app,
                record: record.record,
            })
            .await;
        }
        if line.is_none() && focus.is_none() {
            return;
        }
        {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            if line.is_some() {
                said.activity = line;
            }
            if focus.is_some() {
                said.focus = focus;
            }
        }
        self.say();
    }

    /// Open `focus` as a window on the workspace's desk, asked of the host the
    /// agent acts through ([`open_request`]), and wait while the host opens
    /// it. Whether it opened is no matter to the agent's work: a record the
    /// host cannot show is still where the agent acts.
    async fn open(&self, focus: &LiveRecord) {
        let request = open_request(&self.cluster, focus);
        let invocation = request.invocation;
        let Some(Ok(receipt)) = self.actions.invoke(&self.caller, request).await else {
            return;
        };
        if matches!(receipt.status, ReceiptStatus::Accepted) {
            let until = tilo_clock::Instant::now()
                .checked_add(OPENING)
                .unwrap_or_else(tilo_clock::Instant::now);
            let _ = settle_with(&self.actions, &self.caller, invocation, None, until).await;
        }
    }

    /// Narrate what the agent's action `acting` is doing: the semantic work
    /// on its record's live stream, and once it changed something, that
    /// change as the agent's selection for [`HIGHLIGHT`], then the caret at
    /// its end.
    pub fn acting(self: &Arc<Self>, acting: &Acting) {
        let record = LiveRecord {
            app: acting.record.app.clone(),
            record: acting.record.record.clone(),
        };
        let (seq, joining) = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            said.narrated = said.narrated.wrapping_add(1).max(1);
            let joining = !said.joined.contains(&record);
            if joining {
                said.joined.push(record.clone());
            }
            (said.narrated, joining)
        };
        // The device asks for the record's keys once; what it narrates waits
        // for them, and is sealed with them.
        if joining {
            let _ = self.peer.open_route(tilo_session_host::DocumentRoute {
                app_id: record.app.clone(),
                cluster_id: self.cluster.clone(),
                record: record.record.clone(),
            });
        }
        let narrated = narration::Narrated {
            agent: &self.label,
            invocation: acting.invocation,
            app: &acting.record.app,
            action: &acting.action,
            phase: acting.phase.clone(),
            on_behalf: None,
        };
        if let Some(event) = narration::frame(&narrated, seq, tilo_clock::unix_now_ms()) {
            let frame = LiveFrame::Ops(OpsFrame::Semantic { record, event });
            if let Ok(bytes) = frame.encode() {
                let _ = self.peer.publish_live(self.workspace_id, bytes);
            }
        }
        let Some(changed) = acting
            .left_at
            .as_ref()
            .filter(|_| acting.phase == tilo_actions::InteractionPhase::Committed)
            .and_then(|place| {
                let anchor = LiveAnchor {
                    cursor: hex::decode(&place.anchor).ok()?,
                };
                let head = LiveAnchor {
                    cursor: hex::decode(&place.head).ok()?,
                };
                read_presence_anchors(&anchor, &head)
            })
        else {
            return;
        };
        let now = Instant::now();
        {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            let highlight = grown(said.highlight.take(), changed, now);
            said.selection = shell_selection(&highlight.key, highlight.start, highlight.end);
            said.highlight = Some(highlight);
        }
        self.say();
        let weak: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(HIGHLIGHT).await;
            let Some(presence) = weak.upgrade() else {
                return;
            };
            presence.settle();
        });
    }

    /// The highlight whose time is up settles to the caret at its end; a
    /// later change keeps its own.
    fn settle(&self) {
        {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            // Kept once settled: the next caret in the same text says how
            // far that change wrote.
            let Some(caret) = said
                .highlight
                .as_ref()
                .filter(|highlight| highlight.until <= Instant::now())
                .map(|highlight| shell_selection(&highlight.key, highlight.end, highlight.end))
            else {
                return;
            };
            said.selection = caret;
        }
        self.say();
    }

    /// Say the agent has left, so the workspace drops it from the list
    /// instead of holding a member that never speaks again.
    pub fn leave(&self) {
        self.board().taking_part(&[], None);
    }

    /// Set the whole of what the agent is as its word: the view every reader
    /// draws it by, numbered past the last one it said.
    fn say(&self) {
        let (seq, activity, focus, selection) = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            said.seq = said.seq.wrapping_add(1);
            (
                said.seq,
                said.activity.clone(),
                said.focus.clone(),
                said.selection.clone(),
            )
        };
        let mut view = tilo_session_client::presence::view(
            seq,
            tilo_clock::unix_now_ms(),
            &self.label,
            self.colour.into(),
            activity,
            focus.as_ref(),
            None,
        );
        view.selection = selection;
        self.board().set_where(None, Some(Box::new(view)));
    }
}

/// What opens the record `focus` names in workspace `cluster`: its app's
/// `open` on that record, or on the app's own record when it names none.
fn open_request(cluster: &str, focus: &LiveRecord) -> InvokeRequest {
    let resource = format!("{APPLICATION_RESOURCE_PREFIX}{}", focus.app);
    let target = if focus.record.is_empty() {
        application_action_target(cluster, &focus.app, resource, APPLICATION_OPEN_ACTION)
    } else {
        application_record_target(
            cluster,
            &focus.app,
            &focus.record,
            resource,
            APPLICATION_OPEN_ACTION,
        )
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// `changed` (an editor key and range), as the highlight it makes. An
/// action that reports only the caret it left wrote up to it: from where its
/// last change in that text ended, or from the start of a text it had not
/// been in. Grown over the highlight still shown when it changes the same
/// text, so a burst of edits reads as one change rather than a flicker of
/// many.
fn grown(
    shown: Option<Highlight>,
    (key, start, end): (PresenceKey, usize, usize),
    now: Instant,
) -> Highlight {
    let until = now.checked_add(HIGHLIGHT).unwrap_or(now);
    let same = shown.filter(|shown| shown.key == key);
    let start = match &same {
        _ if start != end => start,
        Some(shown) if shown.end <= end => shown.end,
        _ => 0,
    };
    match same {
        Some(shown) if now < shown.until => Highlight {
            key,
            start: shown.start.min(start),
            end: shown.end.max(end),
            until,
        },
        _ => Highlight {
            key,
            start,
            end,
            until,
        },
    }
}

/// The range `start..end` in the editor `key`, as the selection a frame says.
fn shell_selection(key: &PresenceKey, start: usize, end: usize) -> Option<ShellSelection> {
    let (anchor, head) = presence_anchors(key, start, end)?;
    Some(ShellSelection {
        anchor: hex::encode(anchor.cursor),
        head: hex::encode(head.cursor),
    })
}

/// What the call `request` makes is doing now, in the person's words: what
/// the agent is at, in the present, short enough to read beside its face.
/// `None` for a call that watches nothing (`tilo_status`).
fn now_line(request: &Value, failed: bool) -> Option<String> {
    let params = request.get("params")?;
    let tool = params.get("name")?.as_str()?;
    let arguments = params.get("arguments").cloned().unwrap_or_default();
    let named = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let line = match tool {
        "tilo_ls" => match named("path") {
            path if path.is_empty() => "Looking at the apps".to_owned(),
            path => place_line("Looking at", &path),
        },
        "tilo_read" => place_line("Reading", &named("path")),
        "tilo_find" => verb_line("Searching for", &named("query")),
        "tilo_do" => doing(&arguments)?,
        "tilo" => "Running a program".to_owned(),
        _ => return None,
    };
    // A call that failed is said as the action it was, never as one that
    // worked.
    let line = if failed {
        format!("{line} (failed)")
    } else {
        line
    };
    Some(clip(&line).trim().to_owned())
}

/// What a `tilo_do` is doing: the action it names, on the path it names, a
/// shell command as the command line itself.
fn doing(arguments: &Value) -> Option<String> {
    let calls = arguments.get("calls").and_then(Value::as_array);
    let (path, args, more) = match calls {
        Some(calls) => {
            let first = calls.first()?;
            (
                first
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                first.get("args"),
                calls.len().saturating_sub(1),
            )
        }
        None => (
            arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            arguments.get("args"),
            0,
        ),
    };
    let path = path.trim();
    let action = path.split_once('#').map_or("", |(_, action)| action);
    let line = match verb(action) {
        "Running" => {
            let command = ["command", "command_line", "cmd"]
                .iter()
                .find_map(|key| args.and_then(|args| args.get(*key)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|command| !command.is_empty());
            match command {
                Some(command) => verb_line("Running", command),
                None => match path_place(path).0 {
                    app if app.is_empty() => "Running a command".to_owned(),
                    app => format!("Running a command in {app}"),
                },
            }
        }
        verb => place_line(verb, path),
    };
    Some(if more == 0 {
        line
    } else {
        format!("{line} and {more} more")
    })
}

/// The verb the action `action` is taken with, as the person reads it.
fn verb(action: &str) -> &'static str {
    let action = action.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| action.contains(word));
    if any(&["run", "exec", "shell", "command"]) {
        "Running"
    } else if any(&["delete", "remove", "clear"]) {
        "Deleting"
    } else if any(&["new", "add", "create"]) {
        "Adding"
    } else if any(&[
        "edit", "insert", "append", "replace", "set", "update", "write", "type", "format",
        "rename", "move", "toggle",
    ]) {
        "Editing"
    } else if any(&["open", "start", "launch"]) {
        "Opening"
    } else {
        "Working in"
    }
}

/// `<verb> <what>` as one line, the what left out when the call named none.
fn verb_line(verb: &str, what: &str) -> String {
    let what = what.trim();
    if what.is_empty() {
        verb.to_owned()
    } else {
        format!("{verb} {what}")
    }
}

/// The app and the record a tool path names, as a person reads them:
/// `notes/Notebook/Launch notes#append-paragraph` is "Launch notes" in "Notes",
/// and `terminal#run` is the Terminal itself, with no record of its own.
fn path_place(path: &str) -> (String, String) {
    let path = path.split('#').next().unwrap_or_default().trim();
    let mut parts = path
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let app = parts
        .next()
        .map(|app| app.split('@').next().unwrap_or(app))
        .map(|app| {
            let mut letters = app.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let record = record_title(parts.collect());
    (app, record)
}

/// The record a path's segments name: its last one, except for the parts of
/// a record — a write into a note's body is addressed
/// `…/Launch notes/Body of Launch notes`, and "Body" is a field of the note,
/// not something a person has a name for. A field is named for its record,
/// so it keeps the record's name as the title and its own only when it
/// names a different one.
fn record_title(parts: Vec<&str>) -> String {
    let mut title = "";
    for part in parts {
        if !names_part_of(part, title) {
            title = part;
        }
    }
    title.to_owned()
}

/// Whether `part` addresses a field inside `record` rather than a record of
/// its own: a note titled "Best of 2025" keeps its own name, while "Body of
/// Launch notes" is only ever the body of "Launch notes".
fn names_part_of(part: &str, record: &str) -> bool {
    !record.is_empty()
        && part
            .rsplit_once(" of ")
            .is_some_and(|(_, owner)| owner.trim() == record)
}

/// `<verb> <record> in <app>` as one line: an app with no record of its own is
/// the whole of what is named, and a path naming nothing is the verb alone.
fn place_line(verb: &str, path: &str) -> String {
    let (app, record) = path_place(path);
    match (record.is_empty(), app.is_empty()) {
        (false, false) => format!("{verb} {record} in {app}"),
        (false, true) => format!("{verb} {record}"),
        (true, false) => format!("{verb} {app}"),
        (true, true) => verb.to_owned(),
    }
}

/// `line` as far as a person reads it at a glance, and within the bytes the
/// frame carries.
fn clip(line: &str) -> String {
    let mut kept = String::new();
    for character in line.chars() {
        if kept.chars().count().saturating_add(2) > SHOWN_CHARS
            || kept.len().saturating_add(character.len_utf8()) > MAX_ACTIVITY_BYTES
        {
            kept.push('…');
            return kept;
        }
        kept.push(character);
    }
    kept
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]
    #![allow(
        clippy::needless_pass_by_value,
        reason = "a test writes each call as the one JSON value a client sends"
    )]

    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}})
    }

    /// Each tool call is said as what the agent is at now, present tense and
    /// on what: the record it names in the app it is in, never the tool's path.
    #[test]
    fn a_call_is_said_as_what_the_agent_is_doing() {
        let line = |name: &str, arguments: Value| now_line(&call(name, arguments), false);
        assert_eq!(
            line("tilo_ls", json!({"path": "notes"})).as_deref(),
            Some("Looking at Notes")
        );
        assert_eq!(
            line("tilo_ls", json!({"path": "notes/Notebook/Launch notes"})).as_deref(),
            Some("Looking at Launch notes in Notes")
        );
        assert_eq!(
            line("tilo_ls", json!({})).as_deref(),
            Some("Looking at the apps")
        );
        assert_eq!(
            line("tilo_read", json!({"path": "notes/Notebook/Launch notes"})).as_deref(),
            Some("Reading Launch notes in Notes")
        );
        assert_eq!(
            line("tilo_find", json!({"query": "invoices"})).as_deref(),
            Some("Searching for invoices")
        );
        assert_eq!(
            line(
                "tilo_do",
                json!({"path": "sheets/Budget#edit-cell", "args": {"cell": "B2"}})
            )
            .as_deref(),
            Some("Editing Budget in Sheets")
        );
        assert_eq!(
            line(
                "tilo_do",
                json!({"path": "terminal#run", "args": {"command": "cargo test"}})
            )
            .as_deref(),
            Some("Running cargo test")
        );
        assert_eq!(
            line("tilo_do", json!({"path": "terminal#run"})).as_deref(),
            Some("Running a command in Terminal")
        );
        assert_eq!(
            line(
                "tilo_do",
                json!({"path": "sheets#new-sheet", "args": {"name": "Budget"}})
            )
            .as_deref(),
            Some("Adding Sheets")
        );
        assert_eq!(
            line(
                "tilo_do",
                json!({"calls": [{"path": "notes/Welcome#append-paragraph"},
                                 {"path": "notes/Welcome#set-title"}]})
            )
            .as_deref(),
            Some("Editing Welcome in Notes and 1 more")
        );
        // A write lands in a named part of the record, and the part's name is
        // a field of it: what the agent is editing is the note.
        assert_eq!(
            line(
                "tilo_do",
                json!({"path": "notes/Notebook/Launch notes/Body of Launch notes#append-paragraph"})
            )
            .as_deref(),
            Some("Editing Launch notes in Notes")
        );
        assert_eq!(
            line("tilo", json!({"program": "notes.notebook.notes"})).as_deref(),
            Some("Running a program")
        );
        assert_eq!(line("tilo_status", json!({"invocation_id": "x"})), None);
    }

    /// A failed call keeps saying what the agent was doing, and never says
    /// it worked; a long line is cut where a person stops reading.
    #[test]
    fn a_failed_call_says_the_action_and_never_that_it_worked() {
        let failed = now_line(
            &call("tilo_do", json!({"path": "notes/Welcome#edit"})),
            true,
        );
        assert_eq!(failed.as_deref(), Some("Editing Welcome in Notes (failed)"));
        let long = now_line(
            &call(
                "tilo_find",
                json!({"query": "a query so long that it runs past the width a roster line has room for"}),
            ),
            false,
        )
        .unwrap();
        assert!(long.chars().count() <= SHOWN_CHARS, "{long}");
        assert!(long.len() <= MAX_ACTIVITY_BYTES, "{long}");
        assert!(long.ends_with('…'), "{long}");
    }

    /// Field names never reach people: a part addressed inside a record is
    /// the record, while a record whose own title happens to read as one
    /// keeps its name.
    #[test]
    fn a_path_names_the_record_and_never_the_field_written_into() {
        assert_eq!(
            path_place("notes/Notebook/Launch notes/Body of Launch notes#append-paragraph"),
            ("Notes".to_owned(), "Launch notes".to_owned())
        );
        assert_eq!(
            path_place("notes/Notebook/Best of 2025"),
            ("Notes".to_owned(), "Best of 2025".to_owned())
        );
        assert_eq!(
            path_place("notes/Notebook/Launch notes"),
            ("Notes".to_owned(), "Launch notes".to_owned())
        );
        assert_eq!(
            path_place("terminal#run"),
            ("Terminal".to_owned(), String::new())
        );
    }

    /// The frame says the agent is an agent, by its device's name, with its
    /// line and the record it acts in: a receiver draws its face there and
    /// brings the person who follows it to that record.
    #[test]
    fn the_frame_says_an_agent_doing_that_in_that_record() {
        let focus = LiveRecord {
            app: "so.tilo.sheets".into(),
            record: "uuid:7".into(),
        };
        let view = tilo_session_client::presence::view(
            4,
            1_700_000_000_000,
            "Claude Code",
            [0x81, 0xa8, 0xff],
            Some("Editing Budget in Sheets".to_owned()),
            Some(&focus),
            None,
        );
        assert!(view.agent);
        assert_eq!(view.label, "Claude Code");
        assert_eq!(view.colour, "#81a8ff");
        assert_eq!(view.activity.as_deref(), Some("Editing Budget in Sheets"));
        assert_eq!(view.focus(), Some(("so.tilo.sheets", focus.short_digest())));
        assert!(view.selection.is_none(), "the agent names no caret");
        // An agent that acts nowhere yet names no window: a receiver still
        // draws its face, with its line and nothing to follow.
        let nowhere =
            tilo_session_client::presence::view(5, 1, "Claude Code", [1, 2, 3], None, None, None);
        assert_eq!(nowhere.focus(), None);
        assert!(nowhere.windows.is_empty());
        assert!(nowhere.activity.is_none());
    }

    fn paragraph(id: &str) -> PresenceKey {
        PresenceKey::Resource {
            app: "so.tilo.word-processor".into(),
            id: id.into(),
            kind: "word-processor.document".into(),
        }
    }

    /// What an action wrote is highlighted: up to the caret it left in a
    /// text it had not been in, a burst of writes in that text as one
    /// growing highlight, and the next write after the highlight settled
    /// from where the last one ended.
    #[test]
    fn what_the_agent_wrote_is_one_highlight_per_burst() {
        let at = Instant::now();
        let first = grown(None, (paragraph("p1"), 5, 5), at);
        assert_eq!(
            (first.start, first.end),
            (0, 5),
            "the new paragraph it wrote"
        );

        let burst = grown(
            Some(first),
            (paragraph("p1"), 9, 9),
            at + Duration::from_millis(200),
        );
        assert_eq!((burst.start, burst.end), (0, 9), "one highlight, grown");

        let later = grown(Some(burst), (paragraph("p1"), 14, 14), at + HIGHLIGHT * 2);
        assert_eq!(
            (later.start, later.end),
            (9, 14),
            "only what it wrote since"
        );

        let elsewhere = grown(Some(later), (paragraph("p2"), 3, 7), at + HIGHLIGHT * 2);
        assert_eq!(
            (elsewhere.start, elsewhere.end),
            (3, 7),
            "a range is its own"
        );
        assert!(shell_selection(&elsewhere.key, elsewhere.start, elsewhere.end).is_some());
    }
}
