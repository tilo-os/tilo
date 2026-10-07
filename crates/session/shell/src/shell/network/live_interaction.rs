//! Exact-record live semantic interaction transport state.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame, SemanticFrame};

use crate::shell::collab_broker::CrdtRoute;

const MAX_TRACKED_ROUTES: usize = 1024;
const MAX_TRACKED_EXECUTIONS: usize = 1024;
const SEQUENCE_RESERVATION: u64 = 256;
pub(super) const MAX_QUEUED: usize = 256;

type OutboundKey = (String, String, String);
type ExecutionKey = ([u8; 32], String, String, String, uuid::Uuid);

#[derive(Clone)]
struct ExecutionFloor {
    interaction: tilo_actions::SemanticInteraction,
}

#[derive(Clone, Copy, Default)]
struct OutboundFloor {
    next: u64,
    reserved: u64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFloors {
    #[serde(default)]
    outbound: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    inbound: std::collections::BTreeMap<String, u64>,
}

/// One owner-authenticated interaction waiting for the action broker to bind
/// it to its current local publication.
#[derive(Clone)]
pub(crate) struct Inbound {
    pub(crate) route: CrdtRoute,
    pub(crate) interaction: tilo_actions::SemanticInteraction,
    pub(crate) publisher: tilo_joined_room::AgentLiveInteractionPublisher,
}

/// Bounded sequence state. Outbound reservations and every accepted inbound
/// high-water are durable, so either process may restart without reopening a
/// previously accepted frame. Capacity exhaustion drops new routes rather
/// than evicting replay floors and accepting an old frame again.
pub(super) struct State {
    outbound: HashMap<OutboundKey, OutboundFloor>,
    inbound: HashMap<String, u64>,
    executions: HashMap<ExecutionKey, ExecutionFloor>,
    persisted: PersistedFloors,
    path: Option<PathBuf>,
    _lease: Option<tilo_files::native::atomic::lease::FileLease>,
    healthy: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            outbound: HashMap::new(),
            inbound: HashMap::new(),
            executions: HashMap::new(),
            persisted: PersistedFloors::default(),
            path: None,
            _lease: None,
            healthy: true,
        }
    }
}

impl State {
    pub fn open(path: PathBuf) -> Self {
        const MAX_FLOOR_BYTES: usize = 256 * 1024;
        let mut sentinel = path.as_os_str().to_os_string();
        sentinel.push(".lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let lease = options.open(PathBuf::from(sentinel)).ok().and_then(|file| {
            tilo_files::native::atomic::lease::FileLease::try_acquire(
                file,
                tilo_files::native::atomic::lease::LeaseMode::Exclusive,
            )
            .ok()
        });
        let loaded = match (lease.is_some(), std::fs::File::open(&path)) {
            (true, Ok(file)) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                let read = file
                    .take((MAX_FLOOR_BYTES as u64).saturating_add(1))
                    .read_to_end(&mut bytes);
                (read.is_ok() && bytes.len() <= MAX_FLOOR_BYTES)
                    .then(|| serde_json::from_slice::<PersistedFloors>(&bytes).ok())
                    .flatten()
            }
            (true, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(PersistedFloors::default())
            }
            _ => None,
        };
        let healthy = loaded.as_ref().is_some_and(|value| {
            value.outbound.len() <= MAX_TRACKED_ROUTES
                && value.inbound.len() <= MAX_TRACKED_ROUTES
                && value
                    .outbound
                    .keys()
                    .chain(value.inbound.keys())
                    .all(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        });
        Self {
            path: Some(path),
            persisted: loaded.filter(|_| healthy).unwrap_or_default(),
            _lease: lease,
            healthy,
            ..Self::default()
        }
    }

    fn persist(&self) -> bool {
        if !self.healthy {
            return false;
        }
        let Some(path) = &self.path else {
            return true;
        };
        tilo_files::native::atomic::json::write_private(path, &self.persisted).is_ok()
    }

    fn route_key(parts: &[&[u8]]) -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo live Agent interaction floor v1\0");
        for part in parts {
            hash.update(&(part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        hash.finalize().to_hex().to_string()
    }

    pub fn encode(
        &mut self,
        route: &CrdtRoute,
        wire_record: String,
        source_peer: u64,
        interaction: &tilo_actions::SemanticInteraction,
        sent_at_ms: u64,
    ) -> Option<Vec<u8>> {
        if !self.healthy {
            return None;
        }
        let key = (
            route.cluster_id.clone(),
            route.app_id.clone(),
            wire_record.clone(),
        );
        if !self.outbound.contains_key(&key) && self.outbound.len() >= MAX_TRACKED_ROUTES {
            return None;
        }
        let durable_key = Self::route_key(&[
            route.cluster_id.as_bytes(),
            route.app_id.as_bytes(),
            wire_record.as_bytes(),
        ]);
        let initial = self
            .persisted
            .outbound
            .get(&durable_key)
            .copied()
            .unwrap_or(0);
        let mut floor = self.outbound.get(&key).copied().unwrap_or(OutboundFloor {
            next: initial,
            reserved: initial,
        });
        if floor.next >= floor.reserved {
            let reserved = floor.reserved.checked_add(SEQUENCE_RESERVATION)?;
            if !self.persisted.outbound.contains_key(&durable_key)
                && self.persisted.outbound.len() >= MAX_TRACKED_ROUTES
            {
                return None;
            }
            let prior = self
                .persisted
                .outbound
                .insert(durable_key.clone(), reserved);
            if !self.persist() {
                if let Some(prior) = prior {
                    self.persisted.outbound.insert(durable_key.clone(), prior);
                } else {
                    self.persisted.outbound.remove(&durable_key);
                }
                return None;
            }
            floor.reserved = reserved;
        }
        let sequence = floor.next.checked_add(1)?;
        let interaction_json = serde_json::to_string(interaction).ok()?;
        let frame = LiveFrame::Ops(OpsFrame::Semantic {
            record: LiveRecord {
                app: route.app_id.clone(),
                record: wire_record,
            },
            event: SemanticFrame {
                version: 1,
                seq: sequence,
                sent_at_ms,
                execution: interaction.invocation.0,
                peer: source_peer,
                interaction_json,
            },
        });
        let encoded = frame.encode().ok()?;
        floor.next = sequence;
        self.outbound.insert(key, floor);
        Some(encoded)
    }

    pub fn decode(
        &mut self,
        publisher: [u8; 32],
        room: &str,
        local_record: &str,
        frame: &SemanticFrame,
        record: &LiveRecord,
    ) -> Option<tilo_actions::SemanticInteraction> {
        if !self.healthy {
            return None;
        }
        let interaction: tilo_actions::SemanticInteraction =
            serde_json::from_str(&frame.interaction_json).ok()?;
        if interaction.validate().is_err()
            || interaction.sequence == 0
            || frame.execution != interaction.invocation.0
            || interaction.action.app.as_ref().map(|app| app.id.as_str())
                != Some(record.app.as_str())
        {
            return None;
        }
        let route_floor_key = Self::route_key(&[
            &publisher,
            room.as_bytes(),
            record.app.as_bytes(),
            local_record.as_bytes(),
        ]);
        let route_floor = self
            .inbound
            .get(&route_floor_key)
            .copied()
            .or_else(|| self.persisted.inbound.get(&route_floor_key).copied())
            .unwrap_or(0);
        if frame.seq <= route_floor {
            return None;
        }
        if !self.inbound.contains_key(&route_floor_key) && self.inbound.len() >= MAX_TRACKED_ROUTES
        {
            return None;
        }
        let execution_key = (
            publisher,
            room.to_owned(),
            record.app.clone(),
            local_record.to_owned(),
            interaction.invocation.0,
        );
        if !self.executions.contains_key(&execution_key)
            && self.executions.len() >= MAX_TRACKED_EXECUTIONS
        {
            let retired = self.executions.iter().find_map(|(key, floor)| {
                floor.interaction.phase.is_terminal().then(|| key.clone())
            });
            let retired = retired?;
            self.executions.remove(&retired);
        }
        if let Some(previous) = self.executions.get(&execution_key) {
            let same_identity = previous.interaction.actor == interaction.actor
                && previous.interaction.action == interaction.action
                && previous.interaction.resource == interaction.resource
                && previous.interaction.group == interaction.group;
            if !same_identity
                || interaction.sequence <= previous.interaction.sequence
                || previous.interaction.phase.is_terminal()
            {
                return None;
            }
        }
        if !self.persisted.inbound.contains_key(&route_floor_key)
            && self.persisted.inbound.len() >= MAX_TRACKED_ROUTES
        {
            return None;
        }
        let prior = self
            .persisted
            .inbound
            .insert(route_floor_key.clone(), frame.seq);
        if !self.persist() {
            if let Some(prior) = prior {
                self.persisted.inbound.insert(route_floor_key, prior);
            } else {
                self.persisted.inbound.remove(&route_floor_key);
            }
            return None;
        }
        self.inbound.insert(route_floor_key, frame.seq);
        self.executions.insert(
            execution_key,
            ExecutionFloor {
                interaction: interaction.clone(),
            },
        );
        Some(interaction)
    }
}

pub(super) fn state_path(keypair_path: &Path) -> PathBuf {
    keypair_path.with_extension("live-agent-floors.json")
}

#[cfg(test)]
pub(super) fn semantic(frame: &[u8]) -> Option<(LiveRecord, SemanticFrame)> {
    let LiveFrame::Ops(OpsFrame::Semantic { record, event }) = LiveFrame::decode(frame).ok()?
    else {
        return None;
    };
    Some((record, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interaction() -> tilo_actions::SemanticInteraction {
        tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(9)),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: "agent:scribe".into(),
                kind: tilo_actions::InteractionActorKind::Agent,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "insert".into(),
                label: "Insert".into(),
                app: Some(tilo_actions::ActionApp {
                    id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                }),
            },
            resource: tilo_actions::ActionResource {
                id: "note:7".into(),
                kind: "note".into(),
                label: "Note".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }
    }

    #[test]
    fn exact_record_sequence_is_monotonic_and_replay_fails_closed() {
        let route = CrdtRoute::record(
            "so.tilo.notes",
            "11111111-1111-4111-8111-111111111111",
            "note-7",
        );
        let mut outbound = State::default();
        let bytes = outbound
            .encode(&route, "note-7".into(), 17, &interaction(), 1)
            .unwrap();
        let (record, frame) = semantic(&bytes).unwrap();
        let mut inbound = State::default();
        assert!(
            inbound
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record,
                )
                .is_some()
        );
        assert!(
            inbound
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record
                )
                .is_none(),
            "an equal sequence cannot replay"
        );
    }

    #[test]
    fn terminal_execution_never_reopens_at_a_higher_transport_sequence() {
        let route = CrdtRoute::record(
            "so.tilo.notes",
            "11111111-1111-4111-8111-111111111111",
            "note-7",
        );
        let record = LiveRecord {
            app: route.app_id.clone(),
            record: route.record_id.clone(),
        };
        let mut terminal = interaction();
        terminal.sequence = 2;
        terminal.phase = tilo_actions::InteractionPhase::Committed;
        let terminal_frame = SemanticFrame {
            version: 1,
            seq: 2,
            sent_at_ms: 2,
            execution: terminal.invocation.0,
            peer: 17,
            interaction_json: serde_json::to_string(&terminal).unwrap(),
        };
        let mut state = State::default();
        assert!(
            state
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &terminal_frame,
                    &record,
                )
                .is_some()
        );

        let mut reopened = terminal;
        reopened.sequence = 3;
        reopened.phase = tilo_actions::InteractionPhase::Applying;
        let reopened_frame = SemanticFrame {
            version: 1,
            seq: 3,
            sent_at_ms: 3,
            execution: reopened.invocation.0,
            peer: 17,
            interaction_json: serde_json::to_string(&reopened).unwrap(),
        };
        assert!(
            state
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &reopened_frame,
                    &record,
                )
                .is_none(),
            "a terminal invocation cannot be reopened by a newer transport frame"
        );
    }

    #[test]
    fn persisted_route_floor_survives_both_process_restarts_without_clock_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let sender_path = dir.path().join("sender.json");
        let receiver_path = dir.path().join("receiver.json");
        let route = CrdtRoute::record(
            "so.tilo.notes",
            "11111111-1111-4111-8111-111111111111",
            "note-7",
        );
        let mut sender = State::open(sender_path.clone());
        let applying_bytes = sender
            .encode(&route, route.record_id.clone(), 17, &interaction(), 500)
            .unwrap();
        let mut terminal = interaction();
        terminal.sequence = 2;
        terminal.phase = tilo_actions::InteractionPhase::Committed;
        let terminal_bytes = sender
            .encode(&route, route.record_id.clone(), 17, &terminal, 400)
            .unwrap();

        let mut receiver = State::open(receiver_path.clone());
        let (record, frame) = semantic(&applying_bytes).unwrap();
        assert!(
            receiver
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record,
                )
                .is_some()
        );
        drop(receiver);
        let mut restarted_receiver = State::open(receiver_path.clone());
        let (record, frame) = semantic(&applying_bytes).unwrap();
        assert!(
            restarted_receiver
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record,
                )
                .is_none(),
            "every accepted route high-water survives restart"
        );
        let (record, frame) = semantic(&terminal_bytes).unwrap();
        assert!(
            restarted_receiver
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record,
                )
                .is_some()
        );
        drop(restarted_receiver);
        let mut restarted_receiver = State::open(receiver_path);
        let (record, frame) = semantic(&applying_bytes).unwrap();
        assert!(
            restarted_receiver
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record,
                )
                .is_none(),
            "the later terminal high-water also rejects delayed pre-terminal bytes"
        );

        let mut next = interaction();
        next.invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(10));
        drop(sender);
        let mut restarted_sender = State::open(sender_path);
        let next_bytes = restarted_sender
            .encode(&route, route.record_id.clone(), 17, &next, 1)
            .unwrap();
        let (record, frame) = semantic(&next_bytes).unwrap();
        assert!(
            restarted_receiver
                .decode(
                    [3; 32],
                    &route.cluster_id,
                    &route.record_id,
                    &frame,
                    &record
                )
                .is_some(),
            "a fresh invocation uses the durably reserved transport sequence even when its descriptive clock moved backward"
        );
    }

    #[test]
    fn corrupt_persisted_floor_fails_closed_instead_of_resetting_replay_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("floors.json");
        std::fs::write(&path, b"not a floor").unwrap();
        let route = CrdtRoute::record(
            "so.tilo.notes",
            "11111111-1111-4111-8111-111111111111",
            "note-7",
        );
        assert!(
            State::open(path)
                .encode(&route, route.record_id.clone(), 17, &interaction(), 1)
                .is_none(),
            "an unreadable durable floor cannot reset the sender sequence"
        );

        let oversized = dir.path().join("oversized.json");
        std::fs::write(&oversized, vec![b' '; 256 * 1024 + 1]).unwrap();
        assert!(
            State::open(oversized)
                .encode(&route, route.record_id.clone(), 17, &interaction(), 1)
                .is_none(),
            "the persisted floor is read through its fixed byte bound"
        );
    }

    #[test]
    fn a_second_process_cannot_reserve_the_same_sequence_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("floors.json");
        let route = CrdtRoute::record(
            "so.tilo.notes",
            "11111111-1111-4111-8111-111111111111",
            "note-7",
        );
        let mut first = State::open(path.clone());
        assert!(
            first
                .encode(&route, route.record_id.clone(), 17, &interaction(), 1)
                .is_some()
        );
        assert!(
            State::open(path.clone())
                .encode(&route, route.record_id.clone(), 17, &interaction(), 1)
                .is_none(),
            "a concurrent network cannot share the device's reserved floor"
        );
        drop(first);
        assert!(
            State::open(path)
                .encode(&route, route.record_id.clone(), 17, &interaction(), 1)
                .is_some(),
            "the durable floor is reusable only after the owning session releases its lease"
        );
    }
}
