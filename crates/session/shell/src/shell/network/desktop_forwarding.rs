//! Handing a window gesture to the device of this person that owns the
//! window, and applying the gestures handed here.
//!
//! The command travels as an invocation of the desktop command action
//! (`so.tilo.desktop`, `desktop:session`, `command`) over the authenticated
//! device-to-device remote-action route on the room's own topic, so the
//! receiver knows which person, device and room it comes from. Only the same
//! person's devices are asked or answered; another person's windows stay
//! where their writer put them.
use super::{ShellNetwork, room_topic_id};
use crate::shell::remote_action_link::{InboundRemoteAction, RemoteActionLink as _};
use crate::shell::state::{ForwardedCommand, RoomId};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_actions::remote::{
    ActionHostError, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_desktop_model::commands::{ACTION, BoundCommand, CommandSource, DesktopCommand, RESOURCE};

/// How often the namespaces of this person's devices are looked up again.
const REFRESH_EVERY: Duration = Duration::from_secs(2);
/// How long a handed-over gesture waits for its answer before it is handed
/// over again (with the same invocation, so it is applied once).
const RESEND_AFTER: Duration = Duration::from_secs(5);
/// How many applied invocations an owner remembers to answer a repeat.
const REMEMBERED_APPLIED: usize = 1024;

/// What became of a handed-over gesture.
enum Outcome {
    Applied(uuid::Uuid),
    Refused(uuid::Uuid, String),
    Unanswered(uuid::Uuid),
}

#[derive(Default)]
pub(super) struct State {
    refreshed: Mutex<Option<Instant>>,
    /// Answers from owning devices, filled by the reply callbacks.
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    /// Sentences for the person about gestures that did not happen.
    notices: Mutex<Vec<String>>,
    /// Commands other devices of this person handed here, for the frame
    /// thread, which owns the chrome that applies them.
    pub(super) inbound: Mutex<Vec<InboundRemoteAction>>,
    /// Gestures handed over and not answered yet, with when.
    in_flight: Mutex<HashMap<uuid::Uuid, Instant>>,
    /// Invocations this device applied, so a repeat delivery is answered
    /// without applying it twice.
    applied_here: Mutex<VecDeque<uuid::Uuid>>,
}

/// One gesture handed here, ready to apply.
pub struct HandedOver {
    pub room: RoomId,
    pub command: DesktopCommand,
    request: InboundRemoteAction,
    pub(crate) invocation: InvocationId,
}

/// Whether `message` invokes the desktop command action.
pub(super) fn is_desktop_command(message: &RemoteActionMessage) -> bool {
    matches!(message, RemoteActionMessage::Invoke(request)
        if request.target.address.resource == RESOURCE
            && request.target.address.action == ACTION)
}

impl ShellNetwork {
    /// The certificates of this person's devices the transport has
    /// authenticated, other than this one.
    fn personal_device_certs(&self) -> Vec<tilo_user_identity::DeviceCert> {
        let Some(user) = self.personal_continuity_user else {
            return Vec::new();
        };
        let own = self.endpoint_id_bytes();
        self.peer_identities
            .devices_of(&user, tilo_clock::unix_now_ns())
            .into_iter()
            .filter(|cert| Some(cert.device_public_key) != own)
            .collect()
    }

    fn endpoint_id_bytes(&self) -> Option<[u8; 32]> {
        self.personal_continuity_device
            .as_ref()
            .and_then(|device| super::decode_lower_hex_32(device.as_str()))
    }

    /// Tell the tree which topology namespaces belong to this person's other
    /// devices: those enrolled with this one and those authenticated live.
    pub(super) fn refresh_personal_device_salts(&self, force: bool) {
        if let Ok(mut refreshed) = self.desktop_forwarding.refreshed.lock() {
            let now = Instant::now();
            if !force && refreshed.is_some_and(|at| now.duration_since(at) < REFRESH_EVERY) {
                return;
            }
            *refreshed = Some(now);
        }
        let mut salts: BTreeSet<u16> = super::personal_device_peer_addrs(
            &super::personal_device_peer_path(),
            self.endpoint_id_bytes(),
        )
        .iter()
        .map(|addr| crate::network::behaviour::peer_salt_of_endpoint(&addr.id))
        .collect();
        for cert in self.personal_device_certs() {
            if let Ok(endpoint) = iroh::EndpointId::from_bytes(&cert.device_public_key) {
                salts.insert(crate::network::behaviour::peer_salt_of_endpoint(&endpoint));
            }
        }
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let tree = replica.tree.clone();
        drop(replica);
        if tree.read().personal_device_salts != salts {
            tree.write().set_personal_device_salts(salts);
        }
    }

    /// Hand every gesture the tree queued to the device that owns it, and
    /// fold in the answers that arrived.
    pub(super) fn pump_desktop_forwarding(&self) {
        self.refresh_personal_device_salts(false);
        self.publish_hosted_liveness();
        let Some(tree) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return;
        };
        let queued = tree.read().undelivered_forwarded_commands();
        for forwarded in queued {
            let due = self
                .desktop_forwarding
                .in_flight
                .lock()
                .is_ok_and(|in_flight| {
                    in_flight
                        .get(&forwarded.id)
                        .is_none_or(|sent| sent.elapsed() >= RESEND_AFTER)
                });
            if !due {
                continue;
            }
            // An owner that is away gets it when it is back; the gesture
            // already shows here.
            if self.hand_over(forwarded.clone()).is_ok()
                && let Ok(mut in_flight) = self.desktop_forwarding.in_flight.lock()
            {
                in_flight.insert(forwarded.id, Instant::now());
            }
        }
        let outcomes = self
            .desktop_forwarding
            .outcomes
            .lock()
            .map(|mut outcomes| std::mem::take(&mut *outcomes))
            .unwrap_or_default();
        let mut applied = false;
        for outcome in outcomes {
            let id = match &outcome {
                Outcome::Applied(id) | Outcome::Refused(id, _) | Outcome::Unanswered(id) => *id,
            };
            if let Ok(mut in_flight) = self.desktop_forwarding.in_flight.lock() {
                in_flight.remove(&id);
            }
            match outcome {
                // The owner has the change; ask for it now rather than at the
                // next periodic round.
                Outcome::Applied(id) => {
                    tree.write().forwarded_command_applied(id);
                    applied = true;
                }
                Outcome::Refused(id, notice) => {
                    tree.write().forwarded_command_refused(id);
                    self.note_refusal(notice);
                }
                // Handed over again on a later round.
                Outcome::Unanswered(_) => {}
            }
        }
        if applied {
            self.publish_sync_requests();
        }
    }

    /// Tell the hosted-window cards which devices are reachable now: every
    /// device live in each exact room with this one.
    fn publish_hosted_liveness(&self) {
        let peers: Vec<_> = self
            .live_room_peers
            .lock()
            .map_or_else(|_| Vec::new(), |live| live.iter().copied().collect());
        let live = peers
            .into_iter()
            .filter_map(|(topic, endpoint)| {
                self.unique_room_for_topic(topic)
                    .map(|room| (room, hex::encode(endpoint.as_bytes())))
            })
            .collect();
        crate::shell::hosted_liveness::set_live(live);
    }

    fn note_refusal(&self, notice: String) {
        tracing::info!(%notice, "a window gesture was not applied");
        if let Ok(mut notices) = self.desktop_forwarding.notices.lock() {
            notices.push(notice);
        }
    }

    /// Sentences about gestures that did not happen, for the person.
    pub fn take_desktop_command_notices(&self) -> Vec<String> {
        self.desktop_forwarding
            .notices
            .lock()
            .map(|mut notices| std::mem::take(&mut *notices))
            .unwrap_or_default()
    }

    /// Send `forwarded` to the live device of this person whose namespace it
    /// names. `Err` when that device is not reachable now.
    fn hand_over(&self, forwarded: ForwardedCommand) -> Result<(), String> {
        let ForwardedCommand {
            id,
            room,
            salt,
            command,
            ..
        } = forwarded;
        let topic = room_topic_id(room);
        let owner = self.personal_device_certs().into_iter().find(|cert| {
            iroh::EndpointId::from_bytes(&cert.device_public_key).is_ok_and(|endpoint| {
                crate::network::behaviour::peer_salt_of_endpoint(&endpoint) == salt
            })
        });
        let Some(owner) = owner else {
            return Err("the device that opened this window is away".into());
        };
        let label = if owner.device_label.trim().is_empty() {
            "The device that opened this window".to_string()
        } else {
            owner.device_label.clone()
        };
        let endpoint = iroh::EndpointId::from_bytes(&owner.device_public_key)
            .map_err(|_| format!("{label} cannot be reached"))?;
        let live = self
            .live_room_peers
            .lock()
            .is_ok_and(|live| live.contains(&(topic, endpoint)));
        if !live {
            return Err(format!("{label} is away"));
        }
        let bound = serde_json::to_string(&BoundCommand {
            source: CommandSource {
                room: room.to_string(),
                heads: Vec::new(),
            },
            command,
        })
        .map_err(|error| error.to_string())?;
        let mut argument = serde_json::Map::new();
        argument.insert("command".into(), serde_json::Value::String(bound));
        let invocation = InvocationId(id);
        let message = RemoteActionMessage::Invoke(InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: format!("workspace:{room}"),
                address: ActionAddress::new(RESOURCE, ACTION),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
        });
        let reply = self
            .request(topic, endpoint, message)
            .map_err(|error| format!("{label} could not be asked: {error}"))?;
        let outcomes = Arc::clone(&self.desktop_forwarding.outcomes);
        reply.then(move |reply| {
            let outcome = match reply {
                Some(RemoteActionMessage::Receipt(InvocationReceipt {
                    status: ReceiptStatus::Completed { .. },
                    ..
                })) => Outcome::Applied(id),
                Some(RemoteActionMessage::Receipt(InvocationReceipt {
                    status: ReceiptStatus::Rejected { error },
                    ..
                })) => {
                    Outcome::Refused(id, format!("{label} kept the window where it was: {error}"))
                }
                Some(_) | None => Outcome::Unanswered(id),
            };
            if let Ok(mut outcomes) = outcomes.lock() {
                outcomes.push(outcome);
            }
        });
        Ok(())
    }

    /// Keep a desktop command another device sent for the frame thread.
    pub(super) fn queue_handed_over(&self, request: InboundRemoteAction) {
        if let Ok(mut inbound) = self.desktop_forwarding.inbound.lock() {
            inbound.push(request);
        }
    }

    /// The desktop commands other devices of this person handed here, each
    /// checked for who sent it and for which room. Any other request is
    /// refused back to its sender at once.
    pub fn take_handed_over(&self) -> Vec<HandedOver> {
        let requests = self
            .desktop_forwarding
            .inbound
            .lock()
            .map(|mut inbound| std::mem::take(&mut *inbound))
            .unwrap_or_default();
        requests
            .into_iter()
            .filter_map(|request| match self.admit_handed_over(&request) {
                // Delivered again after this device applied it: the answer
                // was lost, not the gesture.
                Ok(handed) if self.applied_here(handed.invocation) => {
                    self.answer(&handed, Ok(()));
                    None
                }
                Ok(handed) => Some(handed),
                Err(reason) => {
                    let invocation = match &request.message {
                        RemoteActionMessage::Invoke(invoke) => invoke.invocation,
                        _ => return None,
                    };
                    self.answer_handed_over(
                        &request,
                        invocation,
                        Err(ActionHostError::Refused { reason }),
                    );
                    None
                }
            })
            .collect()
    }

    fn admit_handed_over(&self, request: &InboundRemoteAction) -> Result<HandedOver, String> {
        let RemoteActionMessage::Invoke(invoke) = &request.message else {
            return Err("not an invocation".into());
        };
        if invoke.on_behalf_of.is_some() {
            return Err("delegated app authority cannot rearrange this device's windows".into());
        }
        let caller = &request.caller;
        if self.personal_continuity_user.is_none()
            || caller.user_public_key() != self.personal_continuity_user.as_ref()
            || caller.is_foreign_device()
        {
            return Err("only this person's own devices may rearrange its windows".into());
        }
        let room = caller
            .room()
            .and_then(RoomId::parse)
            .ok_or("the request reached no workspace")?;
        if invoke.target.provider != format!("workspace:{room}") {
            return Err("the request names another workspace".into());
        }
        self.check_handed_over_origin(request, room)?;
        let InvokeArgument::Json(argument) = &invoke.argument else {
            return Err("the desktop command is missing".into());
        };
        let bound: BoundCommand = argument
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or("the desktop command is missing")
            .and_then(|text| serde_json::from_str(text).map_err(|_| "not a desktop command"))?;
        if bound.source.room != room.to_string() {
            return Err("the command names another workspace".into());
        }
        Ok(HandedOver {
            room,
            command: bound.command,
            request: request.clone(),
            invocation: invoke.invocation,
        })
    }

    // A queued invocation cannot retain authority after its certificate,
    // transport identity or workspace permission has changed.
    fn check_handed_over_origin(
        &self,
        request: &InboundRemoteAction,
        room: RoomId,
    ) -> Result<(), String> {
        let origin = crate::shell::state::PeerOrigin::peer(&request.from);
        let Some(identity) = self.peer_identities.get(&origin) else {
            return Err("the sending device is no longer authenticated".into());
        };
        let current = self.personal_device_certs().iter().any(|cert| {
            cert.device_public_key == *request.from.as_bytes()
                && request.caller.device_public_key() == Some(&cert.device_public_key)
                && cert.user_public_key == identity.user_pubkey
                && !cert.capabilities.foreign
        });
        if !current
            || self.unique_room_for_topic(request.workspace_topic) != Some(room)
            || !self.peer_room_allows(
                &origin,
                request.workspace_topic,
                &(
                    tilo_action_policy::TreePath::root(),
                    tilo_action_policy::ActionKind::Undoable,
                ),
                false,
            )
        {
            return Err("the sending device no longer has authority in this workspace".into());
        }
        Ok(())
    }

    /// Forget when each gesture was last handed over, so the next round
    /// hands every unanswered one over again.
    #[cfg(test)]
    pub(super) fn desktop_forwarding_resend_now(&self) {
        if let Ok(mut in_flight) = self.desktop_forwarding.in_flight.lock() {
            in_flight.clear();
        }
    }

    fn applied_here(&self, invocation: InvocationId) -> bool {
        self.desktop_forwarding
            .applied_here
            .lock()
            .is_ok_and(|applied| applied.contains(&invocation.0))
    }

    /// Tell the device that handed `handed` over what became of it.
    pub fn answer(&self, handed: &HandedOver, result: Result<(), String>) {
        if result.is_ok()
            && let Ok(mut applied) = self.desktop_forwarding.applied_here.lock()
            && !applied.contains(&handed.invocation.0)
        {
            if applied.len() >= REMEMBERED_APPLIED {
                applied.pop_front();
            }
            applied.push_back(handed.invocation.0);
        }
        self.answer_handed_over(
            &handed.request,
            handed.invocation,
            result.map_err(|reason| ActionHostError::Refused { reason }),
        );
    }

    fn answer_handed_over(
        &self,
        request: &InboundRemoteAction,
        invocation: InvocationId,
        result: Result<(), ActionHostError>,
    ) {
        let status = match result {
            Ok(()) => ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
            Err(error) => ReceiptStatus::Rejected { error },
        };
        self.send_remote_action(
            request.workspace_topic,
            request.from,
            RemoteActionMessage::Receipt(InvocationReceipt { invocation, status }),
        );
    }
}
