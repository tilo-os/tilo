//! The host's own word for the runs it executes: no run goes unseen.
//!
//! An agent or a key acting through the host need not say anything of itself
//! (an SDK client never does), so the host says it for each run it executes
//! ([`Caller::agent_run`]): the run is present as a member of its own, in the
//! agent's name and colour, saying the action it takes and the record it works
//! in, and each action is narrated on the record's stream. Every frame names
//! the run it is said for ([`Presence::on_behalf`]), so a reader takes it only
//! from the device the workspace pins as its host, and a client acting through
//! the host says nothing of itself: a person sees one entry per run.
//!
//! The run's presence is a board's, as it is for a device speaking for the
//! agents its person admitted at its own door ([`Board::speak_for`]): one
//! member per run, its face said when it changes and again at the keep-alive,
//! taken down when the run's session ends. The host says it outside the live
//! plane ([`Cmd::SayForRun`](tilo_session_client::Cmd::SayForRun)), signed with
//! its own device key: it is no member of the plane, holds none of its keys,
//! and needs none to speak for its runs. Only the runs' words leave.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use collab_types::presence::{ShellPresence, ShellSelection, agent_rgb};
use tilo_actions::remote::{ActorPlace, Caller, ExecutionLease, RemoteActionTarget};
use tilo_actions::{InteractionPhase, InvocationId};
use tilo_live::presence::{Board, Device};
use tilo_relay_protocol::host_voice::HostWords;
use tilo_relay_protocol::live::LiveRecord;
use tilo_session_client::presence::now_ms;
use tilo_session_client::{Cmd, Link, Outbox, narration};

use super::{CatalogueQuery, InvokeRequest, Outcome, invoke_arguments, record_of as route_of};

/// How soon a run leaves once the session that is the run ended.
const ENDED_WITHIN: Duration = Duration::from_millis(250);

/// What the host says of one run it executes.
struct Voice {
    /// The agent's or key's name, as the host knows it.
    label: String,
    /// What it is doing now: the action it takes.
    activity: Option<String>,
    /// The record it works in, and what its last action left selected there.
    focus: Option<LiveRecord>,
    selection: Option<ShellSelection>,
    /// The sequence of its narration on the live plane.
    narrated: u64,
    /// The execution its session was minted: it closes with the session, which
    /// is when the run leaves.
    lease: ExecutionLease,
}

/// The runs the host speaks for in one workspace, and the board that holds
/// them: one member per run, as a device's agents at its own door are.
pub(super) struct Runs {
    /// The workspace, as the relay's room names it, and the device speaking
    /// for its runs.
    id: u64,
    publisher: Device,
    /// The workspace's presence, while this host has a run to speak for there.
    board: Option<Board>,
    by_run: HashMap<String, Voice>,
}

impl Runs {
    /// The runs of the host whose device is `publisher`, in `workspace`.
    pub(super) fn new(workspace: u64, publisher: Device) -> Self {
        Self {
            id: workspace,
            publisher,
            board: None,
            by_run: HashMap::new(),
        }
    }

    /// `caller`'s run takes the action `target` names (with `args`) in the
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
        let line = narration::lines::doing(app, &target.address.action, args);
        let line = match phase {
            InteractionPhase::Failed { .. } => narration::lines::failed(&line),
            _ => line,
        };
        let selection = left_at
            .filter(|_| *phase == InteractionPhase::Committed)
            .map(|place| ShellSelection {
                anchor: place.anchor.clone(),
                head: place.head.clone(),
            });
        let said = |voice: &mut Voice| {
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
        let (Some(run), Some(record)) = (caller.agent_run(), record) else {
            return;
        };
        let event = narration::Narrated {
            agent: &run.label,
            invocation,
            app: &record.app,
            action: &target.address,
            phase: phase.clone(),
            on_behalf: Some(&run.id),
        };
        if let Some(frame) = narration::frame(&event, narrated, tilo_clock::unix_now_ms()) {
            let _ = peer.send(Cmd::SayForRun(
                self.id,
                HostWords::Narration {
                    record,
                    event: frame,
                },
            ));
        }
    }

    /// `caller`'s run is at `line` in the workspace `cluster`, in the record
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

    /// `caller`'s run connected to the workspace `cluster` and has asked
    /// nothing yet: it is present, waiting, unless it is present already.
    pub(super) fn arrived(&mut self, (peer, cluster): (&Link, &str), caller: &Caller) {
        let _ = self.at((peer, cluster), caller, |voice| {
            if voice.activity.is_none() {
                voice.activity = Some(narration::lines::waiting());
            }
        });
    }

    /// `caller`'s run is present in `cluster` with what `update` makes of its
    /// word: a member of its own on the board from this first call, and what
    /// the board owes for it said at once, as the run acts rather than once
    /// the action that says so is over. What `update` answered.
    fn at<R>(
        &mut self,
        (peer, cluster): (&Link, &str),
        caller: &Caller,
        update: impl FnOnce(&mut Voice) -> R,
    ) -> Option<R> {
        let run = caller.agent_run()?;
        // A run is its session's: every run has the execution its session was
        // minted, which closes with it.
        let lease = caller.execution_lease()?;
        let voice = self.by_run.entry(run.id).or_insert_with(|| Voice {
            label: run.label,
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

    /// Say what the board owes by `now_ms`, on `peer`: a run's word as it acts,
    /// said again at the keep-alive, and the leaving of a run whose session
    /// ended.
    pub(super) fn tick(&mut self, peer: &Link, now_ms: f64) {
        // A run whose session ended leaves with it.
        self.by_run.retain(|_, voice| voice.lease.is_live());
        let faces = self.faces();
        let Some(mut board) = self.board.take() else {
            return;
        };
        board.speak_for(&faces);
        // The leaving of the last run here is said at once, and the board goes
        // with it: nothing else is left to say.
        let left = faces.is_empty();
        let burst = if left {
            board.flush(now_ms)
        } else {
            board.due(now_ms).unwrap_or_default()
        };
        for (_, frame) in burst {
            // A board says its device's own word too, and this host is no
            // member here: only the runs' words leave.
            if frame.on_behalf.is_some() {
                let _ = peer.send(Cmd::SayForRun(self.id, HostWords::Presence(frame)));
            }
        }
        if !left {
            self.board = Some(board);
        }
    }

    /// The runs here, each with its face: the agent it is, what it does and
    /// where, in its own colour. When a face is said, and its sequence, are the
    /// board's.
    fn faces(&self) -> Vec<(String, ShellPresence)> {
        self.by_run
            .iter()
            .map(|(run, voice)| {
                let (r, g, b) = agent_rgb(&voice.label);
                let mut face = tilo_session_client::presence::view(
                    0,
                    tilo_clock::unix_now_ms(),
                    &voice.label,
                    [r, g, b],
                    voice.activity.clone(),
                    voice.focus.as_ref(),
                    None,
                );
                face.selection.clone_from(&voice.selection);
                (run.clone(), face)
            })
            .collect()
    }
}

/// The record `provider` names in `cluster`, as a run's presence names it.
fn live(provider: &str, cluster: &str) -> Option<LiveRecord> {
    crate::participant::actions::record_of(provider, cluster).map(|route| LiveRecord {
        app: route.app_id,
        record: route.record,
    })
}

/// When a run this host speaks for is next due: said again, or leaving. A run
/// present is looked at within [`ENDED_WITHIN`], so it leaves with its session
/// rather than at the next restatement.
pub(super) fn next_due(workspaces: &HashMap<u64, super::Workspace>) -> Option<Instant> {
    let now = now_ms();
    let said = workspaces
        .values()
        .filter_map(|workspace| workspace.runs.board.as_ref()?.next_due(now))
        .reduce(f64::min)
        .and_then(|due| {
            let wait = Duration::from_secs_f64((due - now).max(0.0) / 1_000.0);
            Instant::now().checked_add(wait)
        });
    let ending = workspaces
        .values()
        .any(|workspace| !workspace.runs.by_run.is_empty())
        .then(|| Instant::now().checked_add(ENDED_WITHIN))
        .flatten();
    said.into_iter().chain(ending).min()
}

impl super::Worker {
    /// Say `caller`'s run taking `request` at `phase`, `left_at` where a done
    /// action left it ([`run_voice`]).
    pub(super) fn voice(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
        (phase, left_at): (&InteractionPhase, Option<&ActorPlace>),
    ) {
        // A link changes nothing and moves no one: the people watching are not
        // told the run asked for one.
        if request.target.address.action == tilo_actions::remote::SHARE_LINK_ACTION {
            return;
        }
        let args = serde_json::Value::Object(invoke_arguments(&request.argument));
        let Some((workspace, cluster)) = self.said_in(workspace_id) else {
            return;
        };
        workspace.runs.acting(
            (&workspace.peer, &cluster),
            caller,
            (request.invocation, &request.target, Some(&args)),
            phase,
            left_at,
        );
    }

    /// Say `caller`'s run present in `workspace_id`, waiting ([`run_voice`]).
    pub(super) fn voice_arrived(&mut self, workspace_id: u64, caller: &Caller) {
        let Some((workspace, cluster)) = self.said_in(workspace_id) else {
            return;
        };
        workspace.runs.arrived((&workspace.peer, &cluster), caller);
    }

    /// Say `caller`'s run reading the catalogue `query` asks ([`run_voice`]).
    pub(super) fn voice_catalogue(
        &mut self,
        workspace_id: u64,
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
            narration::lines::searching(&query.needle)
        } else if query.view_resource.is_some() {
            narration::lines::reading(app.unwrap_or_default())
        } else {
            narration::lines::looking(app)
        };
        workspace.runs.saying(
            (&workspace.peer, &cluster),
            caller,
            line,
            query.provider.as_deref(),
        );
    }

    /// The workspace `workspace_id` names, as its presence frames name it:
    /// where a run of this host's is said.
    fn said_in(&mut self, workspace_id: u64) -> Option<(&mut super::Workspace, String)> {
        let workspace = self.workspaces.get_mut(&workspace_id)?;
        let cluster = workspace.cluster_id(workspace_id)?;
        Some((workspace, cluster))
    }
}

/// How a run's action ended, as it is narrated.
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

/// How a run's action the host turned away is narrated.
pub(super) fn refused_phase() -> InteractionPhase {
    InteractionPhase::Failed {
        code: None,
        message: "refused".into(),
    }
}
