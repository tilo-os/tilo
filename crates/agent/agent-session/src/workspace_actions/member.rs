//! The host device's one session with the host, in every workspace the
//! service joins.
//!
//! The host keeps one connection per device: a second connection of the same
//! device cuts the first off ("another endpoint connected with the same
//! endpoint id"), and the first hears nothing more until it reconnects. A
//! session per workspace, all of them the host device, cut each other off
//! whenever workspaces were joined close together (a backend making hundreds
//! of workspaces, each agent's first request), so a join went unanswered for
//! seconds and the requests waiting on it outlived their callers' waits. So
//! the service holds one session and joins every workspace on it, as the API
//! server holds one per grant and `tilo mcp` one per computer.
//!
//! The session's events are sorted by workspace here, so each workspace reads
//! only its own; a change wakes only the workspaces whose requests wait.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use collab_types::Member as RosterMember;
use collab_types::presence::Shown;
use iroh_base::EndpointId;
use tilo_live::live::Track;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Event, Events, Link, MemoryPeerStore, Outbox, PeerConfig};
use tilo_user_identity::RevocationList;

use super::{HostDevice, Job, Shared};

/// The events a workspace's membership reads: its join's end, the records it
/// asked for, the writes that landed in its records, and the presence its live
/// session carries — who is here.
fn kept(event: &Event) -> bool {
    matches!(
        event,
        Event::HostReady { .. }
            | Event::DocumentOpened { .. }
            | Event::Landed { .. }
            | Event::LiveSession {
                track: Track::Presence,
                ..
            }
    )
}

/// The workspace `event` is of.
fn workspace_of(event: &Event) -> Option<WorkspaceId> {
    match event {
        Event::HostReady { ws, .. }
        | Event::DocumentOpened { ws, .. }
        | Event::LiveSession { ws, .. } => Some(*ws),
        Event::Landed { route, .. } => WorkspaceId::parse(&route.cluster_id),
        _ => None,
    }
}

/// The host device's session, shared by every workspace the service joined.
pub(super) struct Member {
    peer: Link,
    sorted: Mutex<Sorted>,
    /// The workspaces whose requests wait for their next change.
    awaited: Mutex<HashSet<WorkspaceId>>,
    /// This device, as its presence says it, and who is here in each
    /// workspace: the live session's own presence, which every surface that
    /// draws people reads.
    device: [u8; 32],
    presence_label: String,
    presences: Mutex<HashMap<WorkspaceId, tilo_session_client::Presence>>,
}

/// The session's events, by the workspace they are of.
struct Sorted {
    events: Events,
    by_workspace: HashMap<WorkspaceId, Vec<Event>>,
}

impl Member {
    /// Start the host device's session. A change in a workspace whose
    /// requests wait hands `shared`'s thread [`Job::Joined`] for it.
    pub(super) fn start(own_device: &HostDevice, shared: &Arc<Shared>) -> Arc<Self> {
        let _runtime = own_device.runtime.enter();
        let config = PeerConfig {
            host: own_device.endpoint.host.clone(),
            cert: own_device.endpoint.server_cert.clone(),
            server_cert: own_device.endpoint.server_cert.clone(),
            revocations: RevocationList::new(),
            entering: None,
        };
        let (key, store) = (
            Arc::new(own_device.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let peer = match own_device.keys.clone() {
            Some(lent) => tilo_session_client::start_session_lent(config, key, store, lent),
            None => tilo_session_client::start_session(config, key, store),
        };
        let cert = &own_device.endpoint.server_cert;
        let member = Arc::new(Self {
            sorted: Mutex::new(Sorted {
                events: peer.events_where(kept),
                by_workspace: HashMap::new(),
            }),
            peer,
            awaited: Mutex::default(),
            device: cert.device_public_key,
            presence_label: cert.device_label.clone(),
            presences: Mutex::default(),
        });
        let (woken, service): (Weak<Self>, Weak<Shared>) =
            (Arc::downgrade(&member), Arc::downgrade(shared));
        member.peer.on_change(Arc::new(move || {
            // Only a change that arrives while a request waits is worth the
            // thread's turn: the workspace's other changes still reach it as
            // documents.
            let Some(member) = woken.upgrade() else {
                return;
            };
            let waiting = std::mem::take(&mut *member.awaited.lock_recover());
            if waiting.is_empty() {
                return;
            }
            if let Some(shared) = service.upgrade()
                && let Some(jobs) = shared.jobs.lock_recover().as_ref()
            {
                for workspace_id in waiting {
                    let _ = jobs.send((Instant::now(), Job::Joined(workspace_id)));
                }
            }
        }));
        member
    }

    /// The session, as every workspace of the service writes and reads through it.
    pub(super) fn peer(&self) -> &Link {
        &self.peer
    }

    /// Join `workspace_id`: its events are kept for it from now on.
    pub(super) fn join(&self, workspace_id: WorkspaceId) -> Result<(), String> {
        self.sorted
            .lock_recover()
            .by_workspace
            .entry(workspace_id)
            .or_default();
        // This device says on the workspace's live session what it holds of
        // people here: the host's own eye, so a page shows who is here without
        // a session of its own.
        let cert = &self.peer.engine().identity().device_cert;
        if let Some(presence) = tilo_session_client::saying::enter(
            &self.peer,
            cert,
            &self.presence_label,
            &workspace_id.to_string(),
        ) {
            self.presences.lock_recover().insert(workspace_id, presence);
        }
        self.peer.send(Cmd::Join(workspace_id)).map_err(|error| {
            self.sorted
                .lock_recover()
                .by_workspace
                .remove(&workspace_id);
            error.to_string()
        })
    }

    /// Everyone who is here in `workspace_id`, as the live session says it:
    /// each by the name everyone calls them, what they are doing, and whether
    /// their device is one person's or an agent's.
    pub(super) fn present(&self, workspace_id: WorkspaceId) -> Vec<Shown> {
        self.presences
            .lock_recover()
            .get(&workspace_id)
            .map(|presence| presence.members(&workspace_id.to_string()))
            .unwrap_or_default()
    }

    /// The workspace's members, as its own governance lists them: everyone in
    /// it, those not here now included, each with the role it gave them.
    pub(super) fn roster(&self, workspace_id: WorkspaceId) -> Vec<RosterMember> {
        self.peer
            .engine()
            .members(&workspace_id.to_string())
            .map(|list| list.members)
            .unwrap_or_default()
    }

    /// Leave `workspace_id`, dropping what it had not read.
    pub(super) fn leave(&self, workspace_id: WorkspaceId) {
        self.sorted
            .lock_recover()
            .by_workspace
            .remove(&workspace_id);
        self.awaited.lock_recover().remove(&workspace_id);
        self.presences.lock_recover().remove(&workspace_id);
        let _ = self.peer.send(Cmd::Leave(workspace_id));
    }

    /// Take the events of `workspace_id` since it last took them.
    pub(super) fn take(&self, workspace_id: WorkspaceId) -> Vec<Event> {
        let mut sorted = self.sorted.lock_recover();
        let Sorted {
            events,
            by_workspace,
        } = &mut *sorted;
        for event in events.drain() {
            // What someone here says of themselves is taken in by the
            // presence this device keeps for their workspace, even when no
            // request is waiting on it: who is here is not a document.
            if let Event::LiveSession {
                ws,
                from,
                track: Track::Presence,
                body,
                ..
            } = &event
                && *from != self.device
                && let Ok(publisher) = EndpointId::from_bytes(from)
                && let Some(presence) = self.presences.lock_recover().get(ws)
            {
                presence.heard_body(&ws.to_string(), publisher, body);
            }
            // A workspace the service left (or never joined) reads nothing.
            if let Some(kept) = workspace_of(&event).and_then(|ws| by_workspace.get_mut(&ws)) {
                kept.push(event);
            }
        }
        let taken = by_workspace
            .get_mut(&workspace_id)
            .map(std::mem::take)
            .unwrap_or_default();
        drop(sorted);
        taken
    }

    /// Ask for `workspace_id`'s next change to wake the service's thread, or
    /// stop asking.
    pub(super) fn await_change(&self, workspace_id: WorkspaceId, waiting: bool) {
        if waiting {
            self.awaited.lock_recover().insert(workspace_id);
        } else {
            self.awaited.lock_recover().remove(&workspace_id);
        }
    }
}
