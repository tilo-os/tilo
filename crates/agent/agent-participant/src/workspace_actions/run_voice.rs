//! The host's own word for the runs it executes: no run goes unseen.
//!
//! An agent or a key acting through the host need not say anything of
//! itself (an SDK client never does). The host does it for
//! each run it executes ([`Caller::agent_run`]), whether or not the client
//! narrates too: the run is present in the workspace as a member of its own,
//! named and coloured by the agent or key as the host knows it, saying the
//! action it takes and the record it works in, and each action is narrated
//! on the record's stream as it goes. Every frame names the run it is said
//! for ([`Presence::on_behalf`]), so a reader takes it only from the device
//! the workspace's governance pins as its host, and shows the run as spoken
//! by the host for that agent. The host is the one voice of its runs: a
//! client acting through it says nothing of itself, so a person sees one
//! entry per run. What the run is at reads as the client's own words would
//! ([`narration::lines`]): looking at the apps, reading a record, the action
//! it takes on what, and what that action changed is its selection. The host
//! says it signed with its own device key, outside the live plane
//! ([`PeerHandle::say_for_run`]): a host that is no member of the plane holds
//! none of its keys, and needs none to speak for its runs.
//!
//! A run stays present while its session lasts, however long it goes without
//! acting (an agent thinking, or waiting on its person, is still there): its
//! word is restated at the presence keep-alive, and it leaves once the
//! session that is the run ended.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use collab_types::presence::{Payload, Presence, ShellSelection, agent_rgb};
use tilo_actions::remote::{ActorPlace, Caller, RemoteActionTarget};
use tilo_actions::{InteractionPhase, InvocationId};
use tilo_relay_protocol::host_voice::HostWords;
use tilo_relay_protocol::live::LiveRecord;
use tilo_session_host::{PeerHandle, narration};

use super::{CatalogueQuery, InvokeRequest, Outcome, invoke_arguments, record_of as route_of};

/// How often a present run's word is said again, so a device that came
/// later hears it: the keep-alive every member restates at.
const KEEPALIVE: Duration = Duration::from_millis(3_000);

/// How soon a run leaves once the session that is the run ended.
const ENDED_WITHIN: Duration = Duration::from_millis(250);

/// What the host says of one run it executes.
struct Voice {
    /// The workspace, as its presence frames name it.
    cluster: String,
    /// The agent's or key's name, as the host knows it.
    label: String,
    colour: (u8, u8, u8),
    /// What it is doing now: the action it takes.
    activity: Option<String>,
    /// The record it works in.
    focus: Option<LiveRecord>,
    /// What its last action left selected there: the change it made.
    selection: Option<ShellSelection>,
    /// Its view's sequence: a reader takes a view with a higher one.
    seq: u32,
    /// The sequence of its narration on the live plane.
    narrated: u64,
    said_at: Instant,
    /// The execution the host minted for the run's session: closed when the
    /// session ended, which is when the run leaves.
    lease: tilo_actions::remote::ExecutionLease,
}

/// The runs the host speaks for, by workspace and run.
#[derive(Default)]
pub(super) struct RunVoices {
    runs: HashMap<(u64, String), Voice>,
}

impl RunVoices {
    /// `caller`'s run takes the action `target` names (with `args`) in the
    /// workspace `cluster` (`workspace_id` on `peer`), at `phase`: it is
    /// present, says the action, and the action is narrated on its record's
    /// stream; once done, what it changed is its selection there. Nothing for
    /// a caller in no run.
    pub(super) fn acting(
        &mut self,
        (peer, workspace_id, cluster): (&PeerHandle, u64, &str),
        caller: &Caller,
        (invocation, target, args): (
            InvocationId,
            &RemoteActionTarget,
            Option<&serde_json::Value>,
        ),
        phase: &InteractionPhase,
        left_at: Option<&ActorPlace>,
    ) {
        let record = record_of(target, cluster);
        let app = record
            .as_ref()
            .map_or(target.provider.as_str(), |record| record.app.as_str());
        let line = narration::lines::doing(app, &target.address.action, args);
        let line = match phase {
            InteractionPhase::Failed { .. } => narration::lines::failed(&line),
            _ => line,
        };
        let Some((run, voice)) = self.voice(peer, (workspace_id, cluster), caller) else {
            return;
        };
        voice.activity = Some(line);
        if record.is_some() {
            voice.focus.clone_from(&record);
        }
        if let Some(place) = left_at.filter(|_| *phase == InteractionPhase::Committed) {
            voice.selection = Some(ShellSelection {
                anchor: place.anchor.clone(),
                head: place.head.clone(),
            });
        }
        voice.say(peer, workspace_id, &run, Instant::now());
        let Some(record) = record else {
            return;
        };
        voice.narrated = voice.narrated.wrapping_add(1).max(1);
        let narrated = narration::Narrated {
            agent: &voice.label,
            invocation,
            app: &record.app,
            action: &target.address,
            phase: phase.clone(),
            on_behalf: Some(&run),
        };
        if let Some(event) = narration::frame(&narrated, voice.narrated, tilo_clock::unix_now_ms())
        {
            let _ = peer.say_for_run(workspace_id, HostWords::Narration { record, event });
        }
    }

    /// `caller`'s run is at `line` in the workspace `cluster`, in the record
    /// `provider` names when it names one: what a read, a listing or a
    /// search of the run says beside its face. Nothing for a caller in no
    /// run.
    pub(super) fn saying(
        &mut self,
        (peer, workspace_id, cluster): (&PeerHandle, u64, &str),
        caller: &Caller,
        line: String,
        provider: Option<&str>,
    ) {
        let record = provider.and_then(|provider| {
            crate::participant::actions::record_of(provider, cluster).map(|route| LiveRecord {
                app: route.app_id,
                record: route.record,
            })
        });
        let Some((run, voice)) = self.voice(peer, (workspace_id, cluster), caller) else {
            return;
        };
        voice.activity = Some(line);
        if record.is_some() {
            voice.focus = record;
        }
        voice.say(peer, workspace_id, &run, Instant::now());
    }

    /// `caller`'s run's voice in the workspace, present from now on: it
    /// arrives the first time. `None` for a caller in no run.
    fn voice(
        &mut self,
        peer: &PeerHandle,
        (workspace_id, cluster): (u64, &str),
        caller: &Caller,
    ) -> Option<(String, &mut Voice)> {
        let run = caller.agent_run()?;
        // A run is its session's: every run has the execution its session
        // was minted, which closes with it.
        let lease = caller.execution_lease()?;
        let now = Instant::now();
        let key = (workspace_id, run.id.clone());
        if !self.runs.contains_key(&key) {
            let arrived = Presence::desktop(cluster, Payload::Arrived).spoken_for(&run.id);
            let _ = peer.say_for_run(workspace_id, HostWords::Presence(arrived));
        }
        let voice = self.runs.entry(key).or_insert_with(|| Voice {
            cluster: cluster.to_owned(),
            colour: agent_rgb(&run.label),
            label: run.label.clone(),
            activity: None,
            focus: None,
            selection: None,
            seq: 0,
            narrated: 0,
            said_at: now,
            lease,
        });
        Some((run.id, voice))
    }

    /// When a run's word is next due: said again, or leaving. While any run
    /// is present its session may end at any moment, so it is looked at
    /// within [`ENDED_WITHIN`] and leaves with its session rather than at the
    /// next restatement.
    pub(super) fn next_due(&self) -> Option<Instant> {
        let ending = Instant::now().checked_add(ENDED_WITHIN)?;
        self.runs
            .values()
            .filter_map(|voice| voice.said_at.checked_add(KEEPALIVE))
            .map(|restate| restate.min(ending))
            .min()
    }

    /// Say again what is due by now, on each workspace's `peer`: a present
    /// run's word, and the leaving of a run whose session ended.
    pub(super) fn tick<'a>(&mut self, peer: impl Fn(u64) -> Option<&'a PeerHandle>) {
        let now = Instant::now();
        self.runs.retain(|(workspace_id, run), voice| {
            let Some(peer) = peer(*workspace_id) else {
                return false;
            };
            if !voice.lease.is_live() {
                let left = Presence::desktop(&voice.cluster, Payload::Left).spoken_for(run);
                let _ = peer.say_for_run(*workspace_id, HostWords::Presence(left));
                return false;
            }
            if voice
                .said_at
                .checked_add(KEEPALIVE)
                .is_none_or(|due| due <= now)
            {
                voice.say(peer, *workspace_id, run, now);
            }
            true
        });
    }
}

impl Voice {
    /// Say the run's face: the agent it is, what it does and where.
    fn say(&mut self, peer: &PeerHandle, workspace_id: u64, run: &str, now: Instant) {
        self.seq = self.seq.wrapping_add(1);
        self.said_at = now;
        let (r, g, b) = self.colour;
        let view = tilo_session_client::presence::view(
            self.seq,
            tilo_clock::unix_now_ms(),
            &self.label,
            [r, g, b],
            self.activity.clone(),
            self.focus.as_ref(),
            None,
        );
        let mut view = view;
        view.selection.clone_from(&self.selection);
        let face = Presence::desktop(&self.cluster, Payload::Shell(Box::new(view))).spoken_for(run);
        let _ = peer.say_for_run(workspace_id, HostWords::Presence(face));
    }
}

/// The record `target` acts in, in the workspace `cluster`.
fn record_of(target: &RemoteActionTarget, cluster: &str) -> Option<LiveRecord> {
    crate::participant::actions::record_of(&target.provider, cluster).map(|route| LiveRecord {
        app: route.app_id,
        record: route.record,
    })
}

impl super::Worker {
    /// Say `caller`'s run taking `request` at `phase`, as the host speaks
    /// for the runs it executes ([`run_voice`]): once done, `left_at` is
    /// where the action left it.
    pub(super) fn voice(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
        (phase, left_at): (&InteractionPhase, Option<&ActorPlace>),
    ) {
        // A link changes nothing and moves no one: the people watching are
        // not told the run asked for one.
        if request.target.address.action == tilo_actions::remote::SHARE_LINK_ACTION {
            return;
        }
        let Some(workspace) = self.workspaces.get(&workspace_id) else {
            return;
        };
        let Some(cluster) = workspace.cluster_id(workspace_id) else {
            return;
        };
        let args = serde_json::Value::Object(invoke_arguments(&request.argument));
        self.voices.acting(
            (&workspace.peer, workspace_id, &cluster),
            caller,
            (request.invocation, &request.target, Some(&args)),
            phase,
            left_at,
        );
    }

    /// Say `caller`'s run reading the catalogue `query` asks: looking at the
    /// apps or at one, reading a record's view, or searching
    /// ([`run_voice`]).
    pub(super) fn voice_catalogue(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        query: &CatalogueQuery,
    ) {
        let Some(workspace) = self.workspaces.get(&workspace_id) else {
            return;
        };
        let Some(cluster) = workspace.cluster_id(workspace_id) else {
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
        self.voices.saying(
            (&workspace.peer, workspace_id, &cluster),
            caller,
            line,
            query.provider.as_deref(),
        );
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
