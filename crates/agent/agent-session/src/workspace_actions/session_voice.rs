//! The host's own word for the invocations it executes: no invocation goes unseen.
//!
//! An agent or a key acting through the host need not say anything of itself
//! (an SDK client never does), so the host says it for each invocation it executes
//! ([`Caller::agent_session`]): the invocation is present as a member of its own, in the
//! agent's name and colour, saying the action it takes and the record it works
//! in, and each action is narrated on the record's stream. Every frame names
//! the invocation it is said for ([`Presence::on_behalf`]), so a reader takes it only
//! from the device the workspace pins as its host, and a client acting through
//! the host says nothing of itself: a person sees one entry per invocation.
//!
//! The invocation's presence is a board's, as it is for a device speaking for the
//! agents its person admitted at its own API ([`Board::speak_for`]): one
//! member per invocation, its face said when it changes and again at the keep-alive,
//! taken down when the invocation's session ends. The host says it outside the live
//! live session ([`Cmd::SayForSession`](tilo_session_client::Cmd::SayForSession)), signed with
//! its own device key: it is no member of the live session, holds none of its keys,
//! and needs none to speak for its invocations. Only the invocations' words leave.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use collab_types::presence::{ShellPresence, ShellSelection, Shown};
use tilo_actions::remote::{ActorPlace, Caller, ExecutionLease, RemoteActionTarget};
use tilo_actions::{InteractionPhase, InvocationId};
use tilo_live::presence::{Board, Device};
use tilo_server_protocol::host_voice::HostWords;
use tilo_server_protocol::live::LiveResource;
use tilo_session_client::presence::now_ms;
use tilo_session_client::{Cmd, Link, Outbox, narration};

use super::{CatalogueQuery, InvokeRequest, Outcome, invoke_arguments, resource_of as route_of};
use tilo_server_protocol::WorkspaceId;

/// How soon an invocation leaves once the session that is the invocation ended.
const ENDED_WITHIN: Duration = Duration::from_millis(250);

/// The colour `caller`'s runs are drawn in: the agent member's own, never its display name's nor
/// its session's, so one agent is one colour in every session and on every surface that draws it
/// ([`collab_types::presence::PresenceHue::of_member`]); a caller that names no agent (a key) is
/// drawn as its invocation.
fn colour_of(caller: &Caller) -> [u8; 3] {
    let member = collab_types::MemberId::agent(caller.acting().agent_id().map_or_else(
        || {
            caller
                .agent_session()
                .map(|session| session.id)
                .unwrap_or_default()
        },
        str::to_owned,
    ));
    let (r, g, b) = collab_types::presence::identity_rgb(
        collab_types::presence::PresenceHue::of_member(&member),
    );
    [r, g, b]
}

/// What the host says of one invocation it executes.
struct Voice {
    /// The agent's or key's name, as the host knows it.
    label: String,
    /// The workspace member it is: the agent the workspace's list of its agents names it by,
    /// which every surface names and colours it by.
    member: String,
    /// Its colour: the agent's own, the one its every session and the workspace's record of
    /// its agents draw it in.
    colour: [u8; 3],
    /// What it is doing now: the action it takes.
    activity: Option<String>,
    /// The record it works in, and what its last action left selected there.
    focus: Option<LiveResource>,
    selection: Option<ShellSelection>,
    /// The sequence of its narration on the live session.
    narrated: u64,
    /// The execution its session was minted: it closes with the session, which
    /// is when the invocation leaves.
    lease: ExecutionLease,
}

/// The invocations the host speaks for in one workspace, and the board that holds
/// them: one member per invocation, as a device's agents at its own API are.
pub(super) struct Sessions {
    /// The workspace and the device speaking for its invocations.
    id: WorkspaceId,
    publisher: Device,
    /// The workspace's presence, while this host has an invocation to speak for there.
    board: Option<Board>,
    by_session: HashMap<String, Voice>,
}

impl Sessions {
    /// The invocations of the host whose device is `publisher`, in `workspace`.
    pub(super) fn new(workspace: WorkspaceId, publisher: Device) -> Self {
        Self {
            id: workspace,
            publisher,
            board: None,
            by_session: HashMap::new(),
        }
    }

    /// The agents this host speaks for, as the workspace's people see them.
    /// The relay sends this board's signed frames to the other devices only.
    pub(super) fn present(&self) -> Vec<Shown> {
        let now = now_ms();
        self.board
            .iter()
            .flat_map(Board::here)
            .map(|member| tilo_live::presence::shown(member, now))
            .collect()
    }

    /// `caller`'s session takes the action `target` names (with `args`) in the
    /// workspace `cluster` (`peer` is its session), at `phase`: it is present,
    /// says the action, and the action is narrated on its record's stream. What
    /// it changed is its selection there once done.
    pub(super) fn acting(
        &mut self,
        (peer, cluster): (&Link, &str),
        caller: &Caller,
        (invocation, target, args): (
            InvocationId,
            &RemoteActionTarget,
            Option<&serde_json::Value>,
        ),
        phase: &InteractionPhase,
        left_at: Option<&ActorPlace>,
    ) {
        let record = live(&target.provider, cluster);
        let app = record
            .as_ref()
            .map_or(target.provider.as_str(), |record| record.app.as_str());
        let line = ui::step::lines::doing(app, &target.address.action, args);
        let line = match phase {
            InteractionPhase::Failed { .. } => ui::step::lines::failed(&line),
            _ => line,
        };
        let selection = left_at
            .filter(|_| *phase == InteractionPhase::Committed)
            .map(|place| ShellSelection {
                anchor: place.anchor.clone(),
                head: place.head.clone(),
                composing: 0,
            });
        let said = |voice: &mut Voice| {
            // What a session does says the name it goes by: one named after it was first said
            // present goes by that name from then on.
            if let Some(name) = caller.agent_name() {
                name.clone_into(&mut voice.label);
            }
            voice.activity = Some(line);
            if record.is_some() {
                voice.focus.clone_from(&record);
            }
            if selection.is_some() {
                voice.selection = selection;
            }
            voice.narrated = voice.narrated.wrapping_add(1).max(1);
            voice.narrated
        };
        let Some(narrated) = self.at((peer, cluster), caller, said) else {
            return;
        };
        let (Some(session), Some(record)) = (caller.agent_session(), record) else {
            return;
        };
        let event = narration::Narrated {
            agent: caller.agent_called().unwrap_or_default(),
            invocation,
            app: &record.app,
            action: &target.address,
            phase: phase.clone(),
            on_behalf: Some(&session.id),
        };
        if let Some(frame) = narration::frame(&event, narrated, tilo_clock::unix_now_ms()) {
            let _ = peer.send(Cmd::SayForSession(
                self.id,
                HostWords::Narration {
                    record,
                    event: frame,
                },
            ));
        }
    }

    /// `caller`'s session is at `line` in the workspace `cluster`, in the record
    /// `provider` names when it names one: what a read, a listing or a search
    /// says beside its face.
    pub(super) fn saying(
        &mut self,
        (peer, cluster): (&Link, &str),
        caller: &Caller,
        line: String,
        provider: Option<&str>,
    ) {
        let record = provider.and_then(|provider| live(provider, cluster));
        let _ = self.at((peer, cluster), caller, |voice| {
            voice.activity = Some(line);
            if record.is_some() {
                voice.focus = record;
            }
        });
    }

    /// `caller`'s session connected to the workspace `cluster` and has asked
    /// nothing yet: it is present, waiting, unless it is present already.
    pub(super) fn arrived(&mut self, (peer, cluster): (&Link, &str), caller: &Caller) {
        let _ = self.at((peer, cluster), caller, |voice| {
            if voice.activity.is_none() {
                voice.activity = Some(ui::step::lines::waiting());
            }
        });
    }

    /// `caller`'s invocation is present in `cluster` with what `update` makes of its
    /// word: a member of its own on the board from this first call, and what
    /// the board owes for it said at once, as the invocation acts rather than once
    /// the action that says so is over. What `update` answered.
    fn at<R>(
        &mut self,
        (peer, cluster): (&Link, &str),
        caller: &Caller,
        update: impl FnOnce(&mut Voice) -> R,
    ) -> Option<R> {
        let session = caller.agent_session()?;
        // An invocation is its session's: every invocation has the execution its session was
        // minted, which closes with it.
        let lease = caller.execution_lease()?;
        let voice = self.by_session.entry(session.id).or_insert_with(|| Voice {
            label: caller.agent_called().unwrap_or_default().to_owned(),
            member: caller.acting().agent_id().unwrap_or_default().to_owned(),
            colour: colour_of(caller),
            activity: None,
            focus: None,
            selection: None,
            narrated: 0,
            lease,
        });
        let said = update(voice);
        let publisher = self.publisher;
        let _ = self
            .board
            .get_or_insert_with(|| Board::new(cluster, publisher));
        self.tick(peer, now_ms());
        Some(said)
    }

    /// Say what the board owes by `now_ms`, on `peer`: an invocation's word as it acts,
    /// said again at the keep-alive, and the leaving of an invocation whose session
    /// ended.
    pub(super) fn tick(&mut self, peer: &Link, now_ms: f64) {
        // An invocation whose session ended leaves with it.
        self.by_session.retain(|_, voice| voice.lease.is_live());
        let faces = self.faces();
        let Some(mut board) = self.board.take() else {
            return;
        };
        board.speak_for(&faces);
        // The leaving of the last invocation here is said at once, and the board goes
        // with it: nothing else is left to say.
        let left = faces.is_empty();
        let burst = if left {
            board.flush(now_ms)
        } else {
            board.due(now_ms).unwrap_or_default()
        };
        for (_, frame) in burst {
            // A board says its device's own word too, and this host is no
            // member here: only the invocations' words leave.
            if frame.on_behalf.is_some() {
                let _ = peer.send(Cmd::SayForSession(self.id, HostWords::Presence(frame)));
            }
        }
        if !left {
            self.board = Some(board);
        }
    }

    /// The invocations here, each with its face: the agent it is, what it does and
    /// where, in its own colour. When a face is said, and its sequence, are the
    /// board's.
    fn faces(&self) -> Vec<(String, String, ShellPresence)> {
        self.by_session
            .iter()
            .map(|(session, voice)| {
                let mut face = tilo_session_client::presence::view(
                    0,
                    tilo_clock::unix_now_ms(),
                    &voice.label,
                    voice.colour,
                    voice.activity.clone(),
                    voice.focus.as_ref(),
                    None,
                );
                face.selection.clone_from(&voice.selection);
                (session.clone(), voice.member.clone(), face)
            })
            .collect()
    }
}

/// The record `provider` names in `cluster`, as an invocation's presence names it.
fn live(provider: &str, cluster: &str) -> Option<LiveResource> {
    crate::record_actions::resource_of(provider, cluster).map(|route| LiveResource {
        app: route.app_id,
        record: route.document,
    })
}

/// When an invocation this host speaks for is next due: said again, or leaving. An invocation
/// present is looked at within [`ENDED_WITHIN`], so it leaves with its session
/// rather than at the next restatement.
pub(super) fn next_due(workspaces: &HashMap<WorkspaceId, super::Workspace>) -> Option<Instant> {
    let now = now_ms();
    let said = workspaces
        .values()
        .filter_map(|workspace| workspace.sessions.board.as_ref()?.next_due(now))
        .reduce(f64::min)
        .and_then(|due| {
            let wait = Duration::from_secs_f64((due - now).max(0.0) / 1_000.0);
            Instant::now().checked_add(wait)
        });
    let ending = workspaces
        .values()
        .any(|workspace| !workspace.sessions.by_session.is_empty())
        .then(|| Instant::now().checked_add(ENDED_WITHIN))
        .flatten();
    said.into_iter().chain(ending).min()
}

impl super::Worker {
    /// Say `caller`'s session taking `request` at `phase`, `left_at` where a done
    /// action left it ([`session_voice`]).
    pub(super) fn voice(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        (phase, left_at): (&InteractionPhase, Option<&ActorPlace>),
    ) {
        // A link changes nothing and moves no one, and a window shown on the
        // layout shows what the invocation did already: the people watching are not
        // told the invocation asked for either.
        if request.target.address.action == tilo_actions::remote::SHARE_LINK_ACTION
            || tilo_actions::remote::application_provider(&request.target.provider).is_some()
        {
            return;
        }
        let args = serde_json::Value::Object(invoke_arguments(&request.argument));
        let Some((workspace, cluster)) = self.said_in(workspace_id) else {
            return;
        };
        // The session goes by the name the request says it has, as its windows do.
        let caller = caller.clone().in_session_of(request);
        workspace.sessions.acting(
            (&workspace.peer, &cluster),
            &caller,
            (request.invocation, &request.target, Some(&args)),
            phase,
            left_at,
        );
    }

    /// `caller`'s session arrived in `workspace_id`: an agent a person's device handed its session
    /// to is added to the workspace's list of its agents before its first request, and the
    /// session is said present there.
    pub(super) fn arrived(&mut self, workspace_id: WorkspaceId, caller: &Caller) {
        if caller.is_session_delegated()
            && let Some(link) = self.shared.link.get()
        {
            link.workspace_agent(workspace_id, caller);
        }
        self.voice_arrived(workspace_id, caller);
    }

    /// Say `caller`'s session present in `workspace_id`, waiting ([`session_voice`]).
    pub(super) fn voice_arrived(&mut self, workspace_id: WorkspaceId, caller: &Caller) {
        let Some((workspace, cluster)) = self.said_in(workspace_id) else {
            return;
        };
        workspace
            .sessions
            .arrived((&workspace.peer, &cluster), caller);
    }

    /// Say `caller`'s session reading the catalogue `query` asks ([`session_voice`]).
    pub(super) fn voice_catalogue(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) {
        let Some((workspace, cluster)) = self.said_in(workspace_id) else {
            return;
        };
        let routed = query
            .provider
            .as_deref()
            .and_then(|provider| route_of(provider, ""))
            .map(|route| route.app_id);
        let app = query.app_id.as_deref().or(routed.as_deref());
        let line = if !query.needle.trim().is_empty() {
            ui::step::lines::searching(&query.needle)
        } else if query.view_resource.is_some() {
            ui::step::lines::reading(app.unwrap_or_default())
        } else {
            ui::step::lines::looking(app)
        };
        workspace.sessions.saying(
            (&workspace.peer, &cluster),
            caller,
            line,
            query.provider.as_deref(),
        );
    }

    /// The workspace `workspace_id` names, as its presence frames name it:
    /// where an invocation of this host's is said.
    fn said_in(&mut self, workspace_id: WorkspaceId) -> Option<(&mut super::Workspace, String)> {
        let workspace = self.workspaces.get_mut(&workspace_id)?;
        let cluster = workspace.cluster_id(workspace_id)?;
        Some((workspace, cluster))
    }
}

/// How an invocation's action ended, as it is narrated.
pub(super) fn ended_phase(
    outcome: &Result<(tilo_actions::ActionOutput, Option<ActorPlace>), Outcome>,
) -> InteractionPhase {
    match outcome {
        Ok(_) => InteractionPhase::Committed,
        Err(Outcome::Failed(error)) => InteractionPhase::Failed {
            code: Some(error.code.clone()),
            message: error.message.clone(),
        },
        Err(Outcome::Rejected(error)) => InteractionPhase::Failed {
            code: Some(error.code().to_owned()),
            message: error.to_string(),
        },
    }
}

/// How an invocation's action the host turned away is narrated.
pub(super) fn refused_phase() -> InteractionPhase {
    InteractionPhase::Failed {
        code: None,
        message: "refused".into(),
    }
}
