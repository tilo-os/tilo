//! The host's own word for the runs it executes: no run goes unseen.
//!
//! An agent or a key acting through the host need not say anything of
//! itself on the live plane (an SDK client never does). The host does it for
//! each run it executes ([`Caller::agent_run`]), whether or not the client
//! narrates too: the run is present in the workspace as a member of its own,
//! named and coloured by the agent or key as the host knows it, saying the
//! action it takes and the record it works in, and each action is narrated
//! on the record's stream as it goes. Every frame names the run it is said
//! for ([`Presence::on_behalf`]), so a reader takes it only from the device
//! the workspace's governance pins as its host, and shows the run as spoken
//! by the host for that agent.
//!
//! A run stays present while it acts: its word is restated at the presence
//! keep-alive, and it leaves once it took no action for [`IDLE`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use collab_types::presence::{Payload, Presence, agent_rgb};
use tilo_actions::remote::{Caller, RemoteActionTarget};
use tilo_actions::{InteractionPhase, InvocationId};
use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame};
use tilo_session_host::{PeerHandle, narration};

/// How often a present run's word is said again, so a device that came
/// later hears it: the keep-alive every member restates at.
const KEEPALIVE: Duration = Duration::from_millis(3_000);

/// How long a run that took no action stays present before it leaves.
const IDLE: Duration = Duration::from_secs(30);

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
    /// Its view's sequence: a reader takes a view with a higher one.
    seq: u32,
    /// The sequence of its narration on the live plane.
    narrated: u64,
    said_at: Instant,
    acted_at: Instant,
}

/// The runs the host speaks for, by workspace and run.
#[derive(Default)]
pub(super) struct RunVoices {
    runs: HashMap<(u64, String), Voice>,
}

impl RunVoices {
    /// `caller`'s run takes the action `target` names in the workspace
    /// `cluster` (`workspace_id` on `peer`), at `phase`: it is present, says
    /// the action, and the action is narrated on its record's stream. Nothing
    /// for a caller in no run.
    pub(super) fn acting(
        &mut self,
        (peer, workspace_id, cluster): (&PeerHandle, u64, &str),
        caller: &Caller,
        invocation: InvocationId,
        target: &RemoteActionTarget,
        phase: &InteractionPhase,
    ) {
        let Some(run) = caller.agent_run() else {
            return;
        };
        let now = Instant::now();
        let record =
            crate::participant::actions::record_of(&target.provider, cluster).map(|route| {
                LiveRecord {
                    app: route.app_id,
                    record: route.record,
                }
            });
        let key = (workspace_id, run.id.clone());
        let arriving = !self.runs.contains_key(&key);
        let voice = self.runs.entry(key).or_insert_with(|| Voice {
            cluster: cluster.to_owned(),
            colour: agent_rgb(&run.label),
            label: run.label.clone(),
            activity: None,
            focus: None,
            seq: 0,
            narrated: 0,
            said_at: now,
            acted_at: now,
        });
        voice.acted_at = now;
        voice.activity = Some(tilo_session_client::presence::activity(
            &narration::action_label(&target.address.action),
        ));
        if record.is_some() {
            voice.focus.clone_from(&record);
        }
        if arriving {
            let arrived = Presence::desktop(cluster, Payload::Arrived).spoken_for(&run.id);
            let _ = peer.publish_presence(workspace_id, vec![arrived]);
        }
        voice.say(peer, workspace_id, &run.id, now);
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
            on_behalf: Some(&run.id),
        };
        if let Some(event) = narration::frame(&narrated, voice.narrated, tilo_clock::unix_now_ms())
            && let Ok(bytes) = LiveFrame::Ops(OpsFrame::Semantic { record, event }).encode()
        {
            let _ = peer.publish_live(workspace_id, bytes);
        }
    }

    /// When a run's word is next due: said again, or leaving.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.runs
            .values()
            .filter_map(|voice| {
                let restate = voice.said_at.checked_add(KEEPALIVE)?;
                let leave = voice.acted_at.checked_add(IDLE)?;
                Some(restate.min(leave))
            })
            .min()
    }

    /// Say again what is due by now, on each workspace's `peer`: a present
    /// run's word, and the leaving of a run that went idle.
    pub(super) fn tick<'a>(&mut self, peer: impl Fn(u64) -> Option<&'a PeerHandle>) {
        let now = Instant::now();
        self.runs.retain(|(workspace_id, run), voice| {
            let Some(peer) = peer(*workspace_id) else {
                return false;
            };
            if voice
                .acted_at
                .checked_add(IDLE)
                .is_none_or(|leave| leave <= now)
            {
                let left = Presence::desktop(&voice.cluster, Payload::Left).spoken_for(run);
                let _ = peer.publish_presence(*workspace_id, vec![left]);
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
        let face = Presence::desktop(&self.cluster, Payload::Shell(Box::new(view))).spoken_for(run);
        let _ = peer.publish_presence(workspace_id, vec![face]);
    }
}
