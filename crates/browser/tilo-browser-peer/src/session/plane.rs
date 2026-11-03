//! The page's live plane: one `moq-lite` session per joined workspace, in the
//! workspace's own scope.
//!
//! A page takes part in the workspace's live plane the way a device does: it
//! joins the relay that serves its host as its own device — never as a device
//! that bridges anything — seals its tracks under the workspace scope's
//! channel content keys ([`route`]) and opens only what the devices its
//! governance admits published ([`tilo_browser_peer::replica`]'s
//! participants). The one step that differs in a browser is the carrier: a
//! WebTransport session, admitted by the workspace grant `/live` issued, to a
//! listener whose certificate `/live` names. That join is
//! [`crate::live::join`]; this module is the session's bookkeeping around it.
//!
//! The plane lives as long as the page is in the workspace and the session's
//! grant holds: it is joined again after a reconnect (the grant is the
//! session's token), its keys and roster follow the channel and the
//! governance, and it is dropped when the page leaves the workspace or the
//! relay cuts it (the device was removed, the keys moved under it).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_call_lane::Roster;
use tilo_live::plane::Plane;
use tilo_live::scope::Scope;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{Command, Link, PeerEvent, Task, lock};
use crate::ChannelContentKeys;

/// The wait before a plane this page could not reach is asked for again.
pub(crate) const RETRY_MS: u64 = 5_000;
/// The wait before a plane a workspace refused is asked for again: a workspace
/// admits a device on its governance, not on this page's timer.
pub(crate) const REFUSED_MS: u64 = 60_000;
/// How long an answer to `/live` and the join may take before the page asks
/// again.
const JOINING_MS: u64 = 30_000;

/// The workspace scope's record: the channel whose content keys seal the
/// plane's tracks.
pub(crate) fn route(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        record: collab_types::scope::WORKSPACE_RECORD.to_owned(),
    }
}

/// A plane this page is in.
pub(crate) struct Joined {
    /// The carrier session.
    pub(crate) plane: Arc<Plane>,
    /// The devices whose frames its opener admits, as the workspace's
    /// governance names them now.
    pub(crate) members: BTreeSet<[u8; 32]>,
    /// The channel epoch it seals under.
    pub(crate) epoch: u64,
}

/// Why this page is not in a workspace's plane, and since when.
#[derive(Clone, Debug)]
pub(crate) enum Waiting {
    /// `/live` was asked and the join is running: `since_ms` when it started,
    /// with the epoch and roster it joins under.
    Joining {
        since_ms: u64,
        epoch: u64,
        members: BTreeSet<[u8; 32]>,
    },
    /// Asking again at `at_ms`.
    Later { at_ms: u64 },
    /// The workspace does not admit this page: asking again at `at_ms`.
    Refused { at_ms: u64 },
}

/// What asking for a workspace's plane came to when it did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failed {
    /// This browser has no WebTransport, or none its relay negotiates: no
    /// workspace's plane can be joined, and every ask would answer the same.
    Unsupported,
    /// The workspace does not admit this page now.
    Refused,
    /// The host or the relay was not reached, or did not answer as the live
    /// plane does.
    Unavailable,
}

/// The page's planes, one per joined workspace.
#[derive(Default)]
pub(crate) struct Planes {
    joined: BTreeMap<u64, Joined>,
    asking: BTreeMap<u64, Waiting>,
    /// This browser has no WebTransport: no plane is asked for again.
    unsupported: bool,
}

impl Planes {
    pub(crate) fn joined(&self, workspace_id: u64) -> Option<&Joined> {
        self.joined.get(&workspace_id)
    }

    pub(crate) fn joined_mut(&mut self, workspace_id: u64) -> Option<&mut Joined> {
        self.joined.get_mut(&workspace_id)
    }

    /// The plane of `workspace_id`, dropped (which ends its session).
    pub(crate) fn leave(&mut self, workspace_id: u64) -> Option<Joined> {
        self.asking.remove(&workspace_id);
        self.joined.remove(&workspace_id)
    }

    /// Drop the planes of workspaces this page is no longer in, and the asks
    /// for them.
    pub(crate) fn keep_only(&mut self, workspaces: &BTreeSet<u64>) {
        self.joined
            .retain(|workspace_id, _| workspaces.contains(workspace_id));
        self.asking
            .retain(|workspace_id, _| workspaces.contains(workspace_id));
    }

    /// A new session: every plane of the last one ended with its grant, and
    /// each is joined again with the new one.
    pub(crate) fn end_session(&mut self) {
        self.joined.clear();
        self.asking.clear();
    }

    /// Whether `workspace_id`'s plane is to be asked for at `now_ms`: it is
    /// neither joined nor waiting.
    pub(crate) fn due(&self, workspace_id: u64, now_ms: u64) -> bool {
        if self.unsupported || self.joined.contains_key(&workspace_id) {
            return false;
        }
        match self.asking.get(&workspace_id) {
            None => true,
            Some(Waiting::Joining { since_ms, .. }) => {
                now_ms.saturating_sub(*since_ms) >= JOINING_MS
            }
            Some(Waiting::Later { at_ms } | Waiting::Refused { at_ms }) => now_ms >= *at_ms,
        }
    }

    /// The ask for `workspace_id` went out: it joins under `epoch` and
    /// `members` until the keys or the governance move.
    pub(crate) fn began(
        &mut self,
        workspace_id: u64,
        now_ms: u64,
        epoch: u64,
        members: &BTreeSet<[u8; 32]>,
    ) {
        self.asking.insert(
            workspace_id,
            Waiting::Joining {
                since_ms: now_ms,
                epoch,
                members: members.clone(),
            },
        );
    }

    /// The answer to the ask for `workspace_id`, if one is still owed.
    pub(crate) fn answered(&mut self, workspace_id: u64) -> Option<Waiting> {
        self.asking.remove(&workspace_id)
    }

    /// The plane of `workspace_id` opened.
    pub(crate) fn insert(&mut self, workspace_id: u64, joined: Joined) {
        self.joined.insert(workspace_id, joined);
    }

    /// Ask for `workspace_id` again at `at_ms`.
    pub(crate) fn later(&mut self, workspace_id: u64, at_ms: u64) {
        self.asking.insert(workspace_id, Waiting::Later { at_ms });
    }

    /// The workspace refused this page's plane: ask again at `at_ms`.
    pub(crate) fn refused(&mut self, workspace_id: u64, at_ms: u64) {
        self.asking.insert(workspace_id, Waiting::Refused { at_ms });
    }

    /// This browser has no WebTransport.
    pub(crate) fn no_support(&mut self) {
        self.unsupported = true;
        self.asking.clear();
    }
}

/// Ask `origin`'s host for `workspace_id`'s plane and join it as this page's
/// own device, off the session's task; the outcome comes back as
/// [`Command::PlaneJoined`].
pub(crate) fn join(
    client: Arc<dyn tilo_relay_client::https::HttpClient>,
    session: String,
    commands: tokio::sync::mpsc::WeakUnboundedSender<Command>,
    device: [u8; 32],
    workspace_id: u64,
    workspace: String,
    keys: ChannelContentKeys,
    members: BTreeSet<[u8; 32]>,
) {
    n0_future::task::spawn(async move {
        let request = tilo_relay_protocol::web::LiveRequest {
            workspace_id,
            workspace: workspace.clone(),
        };
        let joined = ask(
            client.as_ref(),
            &session,
            &request,
            workspace,
            device,
            keys,
            members,
        )
        .await;
        #[cfg(not(target_arch = "wasm32"))]
        let joined = Command::PlaneJoined(workspace_id, joined);
        #[cfg(target_arch = "wasm32")]
        let joined = {
            ARRIVED.with(|arrived| arrived.borrow_mut().push((workspace_id, joined)));
            Command::PlaneJoined(workspace_id)
        };
        if let Some(commands) = commands.upgrade() {
            let _ = commands.send(joined);
        }
    });
}

// Where a plane waits for the session's command, on the target whose carrier
// cannot travel in one: a browser's `Plane` holds its WebTransport session,
// which is not `Send`, while the command channel crosses the toolkit's `Send`
// transport. Both run on the browser's one thread, so the plane is put down by
// name for `Command::PlaneJoined` to take (`arrived`).
#[cfg(target_arch = "wasm32")]
type Arrived = Vec<(u64, Result<Arc<Plane>, Failed>)>;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static ARRIVED: std::cell::RefCell<Arrived> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What the join for `workspace_id` came to; `Unavailable` when no join put a
/// plane down under that name, which is what an ask the session dropped for
/// another workspace leaves.
#[cfg(target_arch = "wasm32")]
pub(crate) fn arrived(workspace_id: u64) -> Result<Arc<Plane>, Failed> {
    ARRIVED
        .with(|arrived| {
            let mut arrived = arrived.borrow_mut();
            let at = arrived.iter().position(|(id, _)| *id == workspace_id)?;
            Some(arrived.remove(at).1)
        })
        .unwrap_or(Err(Failed::Unavailable))
}

/// `/live`, then the carrier: the page's grant is its session's token, and the
/// listener is the one the host named, pinned to its certificate.
async fn ask(
    client: &dyn tilo_relay_client::https::HttpClient,
    session: &str,
    request: &tilo_relay_protocol::web::LiveRequest,
    workspace: String,
    device: [u8; 32],
    keys: ChannelContentKeys,
    members: BTreeSet<[u8; 32]>,
) -> Result<Arc<Plane>, Failed> {
    use tilo_relay_client::https::LinkError;
    let info = tilo_relay_client::https::live(client, session, request)
        .await
        .map_err(|error| match error {
            LinkError::Refused(_) => Failed::Refused,
            _ => Failed::Unavailable,
        })?;
    let grant = tilo_relay_protocol::web::LiveGrant::from_session(request.workspace_id, session)
        .ok_or(Failed::Unavailable)?;
    let roster = Roster::new(members);
    crate::live::join(
        &info.webtransport_origin,
        &info.certificate_sha256,
        grant,
        Scope::workspace(workspace),
        device,
        keys,
        roster,
    )
    .await
    .map(Arc::new)
    .map_err(|error| match error {
        crate::live::LiveError::Unsupported => Failed::Unsupported,
        crate::live::LiveError::Plane(tilo_live::plane::PlaneError::Denied) => Failed::Refused,
        crate::live::LiveError::WrongProtocol | crate::live::LiveError::Plane(_) => {
            Failed::Unavailable
        }
    })
}

/// Ask the plane's `wake` to run when it hears something, and its `ended` to
/// run when the session ends — as the session's own commands, so a page that
/// has no plane left answers neither.
pub(crate) fn wake_and_ended(
    plane: &Plane,
    workspace_id: u64,
    commands: &tokio::sync::mpsc::WeakUnboundedSender<Command>,
) {
    let heard = commands.clone();
    plane.set_wake(Arc::new(move || {
        if let Some(commands) = heard.upgrade() {
            let _ = commands.send(Command::PlaneHeard(workspace_id));
        }
    }));
    let ended = commands.clone();
    plane.set_ended(Arc::new(move || {
        if let Some(commands) = ended.upgrade() {
            let _ = commands.send(Command::PlaneEnded(workspace_id));
        }
    }));
}

impl Task {
    /// Join, follow or leave the workspaces' live planes: each joined
    /// workspace gets one plane of this page's own, in the workspace scope
    /// ([`route`]'s channel keys, the devices its governance admits),
    /// joined again after a reconnect, and dropped when the page leaves the
    /// workspace.
    pub(super) fn settle_planes(&mut self, client: Option<&Link>) {
        let Some(Link::Https(link)) = client else {
            return;
        };
        if self.config.watching {
            // A watching page is here as a spectator: it joins no plane.
            return;
        }
        self.planes.keep_only(&self.workspaces);
        let now = tilo_clock::unix_now_ms();
        let workspaces: Vec<u64> = self.workspaces.iter().copied().collect();
        for workspace_id in workspaces {
            let Some((workspace, _, _)) = self.replicas.layout(workspace_id) else {
                continue;
            };
            // The workspace scope's keys are the `so.tilo.media/live`
            // channel's: the page asks its participants to admit it, as it
            // does for any record, and joins once they have.
            let route = route(&workspace);
            self.open_channel(&route);
            let Some(keys) = self
                .channels
                .content_keys(crate::channels::Channels::channel(&route))
            else {
                continue;
            };
            let Some(participants) = self.replicas.participants(&route) else {
                continue;
            };
            let members: BTreeSet<[u8; 32]> = participants
                .iter()
                .map(|device| *device.as_bytes())
                .collect();
            if self
                .planes
                .joined(workspace_id)
                .is_some_and(|joined| joined.plane.ended())
            {
                drop(self.planes.leave(workspace_id));
                self.planes
                    .later(workspace_id, now.saturating_add(RETRY_MS));
                continue;
            }
            if let Some(joined) = self.planes.joined_mut(workspace_id) {
                let epoch = keys.epoch();
                if joined.epoch != epoch {
                    joined.epoch = epoch;
                    joined.plane.set_keys(keys);
                }
                if joined.members != members {
                    joined
                        .plane
                        .set_roster(Roster::new(members.iter().copied()));
                    joined.members = members;
                }
                continue;
            }
            if !self.planes.due(workspace_id, now) {
                continue;
            }
            self.planes.began(workspace_id, now, keys.epoch(), &members);
            join(
                link.client().clone(),
                link.session().to_owned(),
                self.commands.clone(),
                self.config.cert.device_public_key,
                workspace_id,
                workspace,
                keys,
                members,
            );
        }
    }

    /// The answer to this page's ask for `workspace_id`'s plane.
    pub(super) fn plane_joined(&mut self, workspace_id: u64, joined: Result<Arc<Plane>, Failed>) {
        let Some(Waiting::Joining { epoch, members, .. }) = self.planes.answered(workspace_id)
        else {
            return;
        };
        // The page left the workspace while the plane was being joined: the
        // plane is dropped, which ends its session.
        if !self.workspaces.contains(&workspace_id) {
            return;
        }
        match joined {
            Ok(plane) => {
                wake_and_ended(&plane, workspace_id, &self.commands);
                self.planes.insert(
                    workspace_id,
                    Joined {
                        plane,
                        members,
                        epoch,
                    },
                );
            }
            Err(Failed::Unsupported) => {
                // There is no fallback: no workspace's plane can be joined,
                // and none is asked for again.
                self.planes.no_support();
                self.set_live_note(Some(crate::live::UNSUPPORTED));
            }
            Err(Failed::Refused) => {
                // A workspace the host admitted the page to that refuses its
                // plane does not admit the page to the plane's channel; one
                // that does not admit the page at all is asked again as soon
                // as its admission may have moved.
                let seated = lock(&self.shared).status.rooms.get(&workspace_id) == Some(&true);
                let wait = if seated { REFUSED_MS } else { RETRY_MS };
                self.planes
                    .refused(workspace_id, tilo_clock::unix_now_ms().saturating_add(wait));
            }
            Err(Failed::Unavailable) => {
                self.planes.later(
                    workspace_id,
                    tilo_clock::unix_now_ms().saturating_add(RETRY_MS),
                );
            }
        }
    }

    /// Take what `workspace_id`'s plane heard since the last call: each
    /// member's track object, already opened with the scope's keys, as
    /// [`PeerEvent::LivePlane`].
    pub(super) fn plane_heard(&self, workspace_id: u64) {
        let heard: Vec<_> = match self.planes.joined(workspace_id) {
            Some(joined) => std::iter::from_fn(|| joined.plane.try_recv()).collect(),
            None => return,
        };
        for heard in heard {
            self.emit(&PeerEvent::LivePlane {
                workspace_id,
                from: heard.sender,
                track: tilo_live::live::track(heard.track),
                sequence: heard.sequence,
                body: heard.body,
            });
        }
    }

    /// `workspace_id`'s plane ended: the relay cut it (the device was removed,
    /// or the keys moved under it) or the carrier went away. It is joined
    /// again, until the workspace refuses this page.
    pub(super) fn plane_ended(&mut self, workspace_id: u64) {
        if self.planes.leave(workspace_id).is_some() {
            self.planes.later(
                workspace_id,
                tilo_clock::unix_now_ms().saturating_add(RETRY_MS),
            );
        }
    }

    /// Put the note the page shows where its live plane would be (in place of
    /// the members) in place, waking every watcher when it changed.
    pub(super) fn set_live_note(&self, note: Option<&str>) {
        let wakes = {
            let mut shared = lock(&self.shared);
            let note = note.map(str::to_owned);
            if shared.live_note == note {
                return;
            }
            shared.live_note = note;
            shared.wakes.clone()
        };
        for wake in &wakes {
            wake();
        }
    }
}
