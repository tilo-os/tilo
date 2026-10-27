//! A bot in the room: a certified participant with its own device key and
//! delegation, holding confined replicas of the records of the apps its
//! program names, reading every change to them after its cursors,
//! committing what its runs do through its session, and showing itself
//! there beside the people.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, DoorError};
use tilo_browser_peer::{
    DocumentRoute, LiveState, MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle,
};
use tilo_relay_protocol::delegation::Delegation;
use tilo_relay_protocol::live::{LiveParticipant, LiveRecord, ParticipantKind};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::apps::{AppCatalogue, ReplicaActor, Seed};
use crate::participant::ParticipantConfig;
use crate::participant::unopened::Unopened;
use crate::worker::ConfinedReplica;

pub struct BotIdentity {
    pub key: SigningKey,
    /// Its root-signed certificate.
    pub cert: DeviceCert,
    /// Its installer's delegation naming the installation.
    pub delegation: Delegation,
}

impl BotIdentity {
    /// The installation the delegation names.
    #[must_use]
    pub fn installation(&self) -> &str {
        self.delegation.delegate.installation().unwrap_or_default()
    }

    /// The id the bot acts and writes under: `bot:<installation>`.
    #[must_use]
    pub fn actor(&self) -> String {
        format!("bot:{}", self.installation())
    }
}

/// One installed bot in one room.
pub struct BotParticipant {
    identity: BotIdentity,
    peer: PeerHandle,
    events: PeerEvents,
    room: String,
    workspace_id: u64,
    catalogue: AppCatalogue,
    /// The app ids whose records the bot holds.
    watched: BTreeSet<String>,
    /// Whom its replicas act as: the installer's standing bounded by the
    /// bot's grants, refreshed before each run.
    actor: ReplicaActor,
    replicas: BTreeMap<DocumentRoute, ConfinedReplica>,
    /// Records of watched apps not held yet, each merged as it changes.
    pending: Unopened,
    live: LiveState,
    /// The host admitted the room; everything it replayed has arrived.
    ready: bool,
}

impl BotParticipant {
    /// Connect as the bot and join `config.room`'s durable and live
    /// planes, holding the records of the `watched` apps as `actor`.
    pub fn start(
        config: ParticipantConfig,
        identity: BotIdentity,
        catalogue: AppCatalogue,
        watched: BTreeSet<String>,
        actor: ReplicaActor,
    ) -> Result<Self, String> {
        let workspace_id = tilo_relay_protocol::room_workspace_id(&config.room)
            .map_err(|_| "the room is not a uuid".to_string())?;
        if identity.delegation.delegate.installation().is_none()
            || !identity.delegation.permits_workspace(workspace_id)
        {
            return Err("the bot is not delegated this room".into());
        }
        let peer = PeerHandle::start(
            PeerConfig {
                host: config.host,
                cert: identity.cert.clone(),
                host_cert: config.host_cert,
                revocations: RevocationList::new(),
                watching: false,
            },
            Arc::new(identity.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        peer.join(workspace_id)?;
        peer.live_join(workspace_id)?;
        Ok(Self {
            pending: Unopened::new(&config.room),
            identity,
            peer,
            events,
            room: config.room,
            workspace_id,
            catalogue,
            watched,
            actor,
            replicas: BTreeMap::new(),
            live: LiveState::starting_now(),
            ready: false,
        })
    }

    /// Call `wake` whenever the session delivers something to take in.
    pub fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.peer.on_change(wake);
    }

    /// Take in what the session delivered and hold the watched records it
    /// named. Whether the room has been replayed, so catch-up may begin.
    pub fn pump(&mut self) -> bool {
        let now = tilo_clock::unix_now_ms();
        for event in self.events.drain() {
            self.receive(event, now);
        }
        self.pending.settle();
        if self.ready {
            self.hold_pending();
        }
        self.ready
    }

    fn receive(&mut self, event: PeerEvent, now: u64) {
        match event {
            PeerEvent::Ready { workspace_id } if workspace_id == self.workspace_id => {
                self.ready = true;
            }
            PeerEvent::Document(document) => {
                let route = document.route.clone();
                if route.cluster_id != self.room || !self.watched.contains(&route.app_id) {
                    return;
                }
                let Some(mutation) = document.mutation() else {
                    return;
                };
                if let Some(app) = self.replicas.get(&route) {
                    if let Err(error) = app.apply(&mutation) {
                        tracing::warn!(%error, "bot: a durable change did not merge");
                    }
                } else {
                    let (collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes)) = mutation;
                    self.pending.receive(route, bytes);
                }
            }
            PeerEvent::Live {
                workspace_id,
                from,
                frame,
            } if workspace_id == self.workspace_id => {
                self.live.receive(from, &frame, now);
            }
            PeerEvent::LiveMember {
                workspace_id,
                device,
                active,
            } if workspace_id == self.workspace_id => {
                self.live.member(device, active);
            }
            _ => {}
        }
    }

    fn hold_pending(&mut self) {
        for route in self.pending.routes() {
            if !self.watched.contains(&route.app_id) || self.replicas.contains_key(&route) {
                continue;
            }
            let Some(Ok(saved)) = self.pending.seed(&route) else {
                continue;
            };
            let Some(record) = ui::record::RecordId::from_route(&route.record) else {
                continue;
            };
            let opened = ConfinedReplica::open(
                &self.catalogue,
                &route.app_id,
                &self.room,
                &self.identity.actor(),
                &self.actor,
                Seed::Existing {
                    record,
                    changes: vec![saved],
                },
            );
            match opened {
                Ok(app) => {
                    self.pending.forget(&route);
                    self.replicas.insert(route, app);
                }
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "bot: a watched record did not open");
                }
            }
        }
    }

    /// Whether the room has been replayed.
    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    /// Act as `actor` from now on: the standing the host rebuilt from the
    /// room's current policy before a run. Records already held reopen as
    /// it on their next change; a fresh replica is opened per run.
    pub fn set_actor(&mut self, actor: ReplicaActor) {
        if self.actor == actor {
            return;
        }
        self.actor = actor;
        // A replica's door was built with the old standing: hold the
        // records again so no step is decided by authority since revoked.
        let held = std::mem::take(&mut self.replicas);
        for (route, app) in held {
            let saved = app.read_history(automerge::Automerge::save);
            let reopened = Some(saved).and_then(|saved| {
                let record = ui::record::RecordId::from_route(&route.record)?;
                ConfinedReplica::open(
                    &self.catalogue,
                    &route.app_id,
                    &self.room,
                    &self.identity.actor(),
                    &self.actor,
                    Seed::Existing {
                        record,
                        changes: vec![saved],
                    },
                )
                .ok()
            });
            if let Some(app) = reopened {
                self.replicas.insert(route, app);
            } else {
                tracing::warn!(record = %route.record, "bot: a held record did not reopen");
            }
        }
    }

    /// The heads of the held record `record`, hex.
    #[must_use]
    pub fn heads(&self, record: &str) -> Option<Vec<String>> {
        let app = self.replica_of_record(record)?;
        Some(app.read_history(|doc| doc.get_heads().iter().map(ToString::to_string).collect()))
    }

    /// The held records.
    pub fn replicas(&self) -> impl Iterator<Item = (&DocumentRoute, &ConfinedReplica)> {
        self.replicas.iter()
    }

    /// The record held at `route`.
    #[must_use]
    pub fn replica(&self, route: &DocumentRoute) -> Option<&ConfinedReplica> {
        self.replicas.get(route)
    }

    fn replica_of_record(&self, record: &str) -> Option<&ConfinedReplica> {
        self.replicas
            .iter()
            .find(|(route, _)| route.record == record)
            .map(|(_, app)| app)
    }

    /// Invoke one action of the record at `route` as `invoker` within
    /// `context` (`confirmed` when its contract asks for confirmation and
    /// the run gave it), commit the change it made, and show the bot there.
    pub fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        invoker: &tilo_actions::ActionInvoker,
        context: &tilo_actions::ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        let app = self
            .replicas
            .get(route)
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        let output =
            app.invoke_with(Some(invoker), Some(context), address, arguments, confirmed)?;
        self.commit(route)
            .map_err(|reason| DoorError::Unavailable {
                code: "commit".into(),
                reason,
            })?;
        self.publish_presence(Some(route), None);
        Ok(output)
    }

    /// Take the change the record at `route` made and publish it.
    ///
    /// # Errors
    ///
    /// The change was not admitted or the session refused it.
    pub fn commit(&mut self, route: &DocumentRoute) -> Result<(), String> {
        let Some(app) = self.replicas.get(route) else {
            return Ok(());
        };
        match app.take_mutation().map_err(|error| error.to_string())? {
            Some(mutation) => self.peer.publish(route.clone(), &mutation),
            None => Ok(()),
        }
    }

    /// Show the bot in the room, at `focus` when it acts there.
    pub fn publish_presence(&mut self, focus: Option<&DocumentRoute>, activity: Option<String>) {
        let now = tilo_clock::unix_now_ms();
        let participant = LiveParticipant {
            kind: ParticipantKind::Agent,
            label: self.identity.delegation.label.clone(),
            color: self.identity.delegation.color,
            activity: activity.map(|activity| bounded(&activity)),
        };
        let focus = focus.map(|route| LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        match self.live.publish_presence(now, |presence| {
            presence.focus = focus;
            presence.participant = Some(participant);
        }) {
            Ok(frame) => {
                let _ = self.peer.publish_live(self.workspace_id, frame);
            }
            Err(error) => tracing::warn!(%error, "bot: presence out of bounds"),
        }
    }

    /// The bot's device key.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.identity.cert.device_public_key
    }

    /// Leave the room and end the session.
    pub fn close(mut self) {
        self.publish_presence(None, None);
        self.replicas.clear();
        self.peer.close();
    }
}

impl super::door::DoorParticipant for BotParticipant {
    fn room(&self) -> &str {
        &self.room
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<tilo_actions::door::ActionDescriptor>)> {
        self.replicas
            .iter()
            .map(|(route, app)| (route.clone(), app.survey()))
            .collect()
    }

    fn holds(&self, route: &DocumentRoute) -> bool {
        self.replicas.contains_key(route)
    }

    fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        invoker: &tilo_actions::ActionInvoker,
        context: &tilo_actions::ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        Self::invoke(self, route, address, arguments, invoker, context, confirmed)
    }

    fn find_in_history(
        &self,
        step: &super::door::RunStep,
        actor: &str,
    ) -> Option<(DocumentRoute, tilo_actions::ActionContext)> {
        self.replicas.iter().find_map(|(route, app)| {
            app.read_history(|doc| {
                doc.get_changes(&[]).into_iter().rev().find_map(|change| {
                    let context =
                        tilo_actions::ActionContext::from_change_message(change.message()?)?;
                    let by = context
                        .invoker
                        .as_ref()
                        .map(|invoker| invoker.actor.as_str());
                    (super::door::RunStep::of(&context).as_ref() == Some(step) && by == Some(actor))
                        .then_some(context)
                })
            })
            .map(|context| (route.clone(), context))
        })
    }

    fn outbox(&self, route: &DocumentRoute, field: &str) -> Vec<super::item_json::OutboxRow> {
        self.replicas.get(route).map_or_else(Vec::new, |app| {
            app.read_history(|doc| super::item_json::outbox_entries(doc, &doc.get_heads(), field))
        })
    }

    fn queued_in_history(
        &self,
        route: &DocumentRoute,
        step: &super::door::RunStep,
        actor: &str,
        field: &str,
    ) -> Vec<String> {
        self.replicas.get(route).map_or_else(Vec::new, |app| {
            app.read_history(|doc| super::door::queued_by_step(doc, step, actor, field))
        })
    }
}

fn bounded(activity: &str) -> String {
    let mut end = activity
        .len()
        .min(tilo_relay_protocol::live::MAX_PARTICIPANT_ACTIVITY_BYTES);
    while !activity.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    activity.get(..end).unwrap_or_default().to_owned()
}

// The runner's door holds the participant behind a mutex shared with its
// pump, which needs it Send.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<BotParticipant>();
};
