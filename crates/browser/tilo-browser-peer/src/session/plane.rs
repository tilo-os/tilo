//! The page's live plane: one `moq-lite` session per joined workspace, in the
//! workspace's own scope.
//!
//! A page takes part in the workspace's live plane the way a device does: it
//! joins the relay that serves its host as its own device — never as a device
//! that bridges anything — seals its tracks under the workspace scope's
//! channel content keys ([`route`]) and opens only what the devices its
//! governance admits published ([`tilo_browser_peer::replica`]'s
//! participants). The carrier is the one every device dials: a direct iroh
//! session to the device that serves the workspace ([`tilo_live::native`]),
//! its grant this device's certificate and its signed statement that the
//! page's endpoint speaks for it. This module is the session's bookkeeping
//! around that join.
//!
//! The plane lives as long as the page is in the workspace and the session's
//! grant holds: it is joined again after a reconnect (the grant is the
//! session's token), its keys and roster follow the channel and the
//! governance, and it is dropped when the page leaves the workspace or the
//! relay cuts it (the device was removed, the keys moved under it).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_live::plane::Join;
use tilo_live::plane::Plane;
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{Command, PeerEvent, Task, lock};
use channel_crypto::ChannelContentKeys;

/// The message a page shows in its members' place when its live plane cannot
/// be joined: the page keeps asking quietly, and nobody is listed from the
/// plane until one opens.
pub(crate) const UNREACHABLE: &str = "this page cannot reach live presence";

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
    /// The last presence object this page said on the plane.
    pub(crate) presence_seq: u32,
    /// The last live-preview object this page said on the plane.
    pub(crate) activity_seq: u32,
    /// The last voice object this page published on the plane: one stream per
    /// page across its calls and its mute, so a call that starts again is
    /// heard at once.
    pub(crate) audio_seq: u32,
    /// The tracks this page shows of each device in the call: what the plane
    /// asks their owners to encode.
    pub(crate) shown: BTreeMap<[u8; 32], BTreeSet<tilo_live::live::Track>>,
    /// The picture demand this page last told its app.
    pub(crate) demand_at: u64,
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
    /// The workspace does not admit this page now.
    Refused,
    /// The host or the relay was not reached, or did not answer as the live
    /// plane does.
    Unavailable,
}

/// How many asks in a row for a workspace's plane may fail to reach one
/// before the page says so in its members' place.
pub(crate) const MISSES_BEFORE_NOTE: u8 = 2;

/// The page's planes, one per joined workspace.
#[derive(Default)]
pub(crate) struct Planes {
    joined: BTreeMap<u64, Joined>,
    asking: BTreeMap<u64, Waiting>,
    /// Per workspace, how many asks in a row did not reach a plane.
    misses: BTreeMap<u64, u8>,
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
        self.misses.remove(&workspace_id);
        self.joined.remove(&workspace_id)
    }

    /// One line per plane for diagnostics: whose objects each joined plane
    /// reads and what it heard and could not open, and for every workspace
    /// whose plane is not joined, what it waits for — so a page that lists
    /// nobody says why.
    pub(crate) fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut lines: Vec<String> = Vec::new();
        for (workspace_id, joined) in &self.joined {
            let mut line = format!(
                "plane {workspace_id}: epoch {}, reads {} device(s), heard {} object(s)",
                joined.epoch,
                joined.members.len(),
                joined.plane.heard_total()
            );
            for (reason, count) in joined.plane.refusals() {
                let _ = write!(line, "\n  refused {count}: {reason}");
            }
            lines.push(line);
        }
        for (workspace_id, waiting) in &self.asking {
            lines.push(match waiting {
                Waiting::Joining { since_ms, .. } => {
                    format!("plane {workspace_id}: asking the host for its live session since {since_ms}")
                }
                Waiting::Later { at_ms } => format!(
                    "plane {workspace_id}: not joined — no live plane was reached; asking \
                     again at {at_ms}"
                ),
                Waiting::Refused { at_ms } => format!(
                    "plane {workspace_id}: not joined — the workspace does not admit this \
                     page's plane; asking again at {at_ms}"
                ),
            });
        }
        lines.join("\n")
    }

    /// Drop the planes of workspaces this page is no longer in, and the asks
    /// for them.
    pub(crate) fn keep_only(&mut self, workspaces: &BTreeSet<u64>) {
        self.joined
            .retain(|workspace_id, _| workspaces.contains(workspace_id));
        self.asking
            .retain(|workspace_id, _| workspaces.contains(workspace_id));
        self.misses
            .retain(|workspace_id, _| workspaces.contains(workspace_id));
    }

    /// A new session: every plane of the last one ended with its grant, and
    /// each is joined again with the new one.
    pub(crate) fn end_session(&mut self) {
        self.joined.clear();
        self.asking.clear();
        self.misses.clear();
    }

    /// Whether `workspace_id`'s plane is to be asked for at `now_ms`: it is
    /// neither joined nor waiting.
    pub(crate) fn due(&self, workspace_id: u64, now_ms: u64) -> bool {
        if self.joined.contains_key(&workspace_id) {
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
        self.misses.remove(&workspace_id);
        self.joined.insert(workspace_id, joined);
    }

    /// Ask for `workspace_id` again at `at_ms`.
    pub(crate) fn later(&mut self, workspace_id: u64, at_ms: u64) {
        self.asking.insert(workspace_id, Waiting::Later { at_ms });
    }

    /// The workspace refused this page's plane: ask again at `at_ms`.
    pub(crate) fn refused(&mut self, workspace_id: u64, at_ms: u64) {
        // The host answered, so the plane was reached: only asks that reached
        // nothing count towards the note in the members' place.
        self.misses.remove(&workspace_id);
        self.asking.insert(workspace_id, Waiting::Refused { at_ms });
    }

    /// An ask for `workspace_id`'s plane reached no plane: how many asks in a
    /// row that is now.
    pub(crate) fn missed(&mut self, workspace_id: u64) -> u8 {
        let misses = self.misses.entry(workspace_id).or_default();
        *misses = misses.saturating_add(1);
        *misses
    }
}

/// Join `workspace_id`'s plane over iroh, as this device, against the device
/// that serves it; the outcome comes back as [`Command::PlaneJoined`].
///
/// The grant is this device's certificate (the roster every member applies),
/// so a page-shaped peer reaches the same plane a browser reaches over
/// WebTransport: it seals and opens as itself, under the workspace scope's
/// keys, and the serving device copies bytes it cannot open.
pub(crate) struct NativeJoin {
    /// The endpoint this device dials from.
    pub endpoint: iroh::Endpoint,
    /// The serving device's address.
    pub host: iroh_base::EndpointAddr,
    /// Where the outcome goes.
    pub commands: tokio::sync::mpsc::WeakUnboundedSender<Command>,
    /// This device's certificate, presented as the grant.
    pub cert: tilo_user_identity::DeviceCert,
    /// This device's statement that the endpoint speaks for it, when the
    /// endpoint is not the device's own key (a page): the grant carries it.
    pub delegation: Vec<u8>,
    /// This device.
    pub device: [u8; 32],
    /// The workspace joined.
    pub workspace_id: u64,
    /// The workspace.
    pub workspace: String,
    /// The workspace's content keys.
    pub keys: ChannelContentKeys,
    /// The workspace's members.
    pub members: BTreeSet<[u8; 32]>,
}

pub(crate) fn join_native(join: NativeJoin) {
    let NativeJoin {
        endpoint,
        host,
        commands,
        cert,
        delegation,
        device,
        workspace_id,
        workspace,
        keys,
        members,
    } = join;
    n0_future::task::spawn(async move {
        let joined = native(
            &endpoint,
            host,
            &cert,
            &delegation,
            device,
            &workspace,
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

/// The join itself: the grant this device presents, then the plane on the
/// serving device's carrier.
async fn native(
    endpoint: &iroh::Endpoint,
    host: iroh_base::EndpointAddr,
    cert: &tilo_user_identity::DeviceCert,
    delegation: &[u8],
    device: [u8; 32],
    workspace: &str,
    keys: ChannelContentKeys,
    members: BTreeSet<[u8; 32]>,
) -> Result<Arc<Plane>, Failed> {
    use tilo_live::plane::PlaneError;
    let Some(uuid) = tilo_desktop_model::WorkspaceId::parse(workspace) else {
        return Err(Failed::Unavailable);
    };
    let scope = Scope::workspace(workspace);
    let certificate = serde_json::to_vec(cert).map_err(|_| Failed::Unavailable)?;
    let grant = tilo_live::grant::Native::new(
        *uuid.as_uuid().as_bytes(),
        &scope.id(),
        keys.epoch(),
        certificate,
        delegation.to_vec(),
    )
    .map_err(|_| Failed::Unavailable)?;
    Plane::join(
        &tilo_live::native::Iroh::new(endpoint.clone(), host),
        &grant.encode(),
        Join {
            scope,
            device,
            keys,
            roster: Roster::new(members),
            publish: true,
        },
    )
    .await
    .map(Arc::new)
    .map_err(|error| match error {
        PlaneError::Denied => Failed::Refused,
        _ => Failed::Unavailable,
    })
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
    pub(super) fn settle_planes(&mut self) {
        let Some((endpoint, delegation)) = self.dial.plane() else {
            return;
        };
        self.planes.keep_only(&self.workspaces);
        let now = tilo_clock::unix_now_ms();
        let due: Vec<u64> = std::mem::take(&mut self.planes_due)
            .into_iter()
            .filter(|workspace_id| self.workspaces.contains(workspace_id))
            .collect();
        for workspace_id in due {
            let Some((workspace, _)) = self.replicas.members(workspace_id) else {
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
            // The workspace's own governance names where its host listens; a
            // host that has not named itself yet is dialled where the session
            // reached it.
            let (addr, relay_url) = match self.replicas.host_endpoint(workspace_id) {
                Some(named) => named,
                None => (
                    self.config.host.addr.clone(),
                    self.config.host.relay_url.clone(),
                ),
            };
            let host = match relay_url {
                Some(url) => addr.with_relay_url(url),
                None => addr,
            };
            join_native(NativeJoin {
                endpoint: endpoint.clone(),
                host,
                commands: self.commands.clone(),
                cert: self.config.cert.clone(),
                delegation: delegation.clone(),
                device: self.config.cert.device_public_key,
                workspace_id,
                workspace,
                keys,
                members,
            });
        }
    }

    /// The answer to this page's ask for `workspace_id`'s plane.
    pub(super) fn plane_joined(&mut self, workspace_id: u64, joined: Result<Arc<Plane>, Failed>) {
        self.planes_due.insert(workspace_id);
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
                        presence_seq: 0,
                        activity_seq: 0,
                        audio_seq: 0,
                        shown: BTreeMap::new(),
                        demand_at: u64::MAX,
                    },
                );
                // The members' place goes back to listing who is on the plane.
                self.set_live_note(None);
            }
            Err(Failed::Refused) => {
                // A workspace the host admitted the page to that refuses its
                // plane does not admit the page to the plane's channel; one
                // that does not admit the page at all is asked again as soon
                // as its admission may have moved.
                let seated = lock(&self.shared).status.workspaces.get(&workspace_id) == Some(&true);
                let wait = if seated { REFUSED_MS } else { RETRY_MS };
                self.planes
                    .refused(workspace_id, tilo_clock::unix_now_ms().saturating_add(wait));
            }
            Err(Failed::Unavailable) => {
                // Asking again is quiet, so the page says in its members'
                // place that it lists nobody from the plane; the reason is in
                // the channel diagnostics either way.
                if self.planes.missed(workspace_id) >= MISSES_BEFORE_NOTE {
                    self.set_live_note(Some(UNREACHABLE));
                }
                self.planes.later(
                    workspace_id,
                    tilo_clock::unix_now_ms().saturating_add(RETRY_MS),
                );
            }
        }
    }

    /// Publish `frames` on `workspace_id`'s plane as this page's voice, in
    /// the order given: one Opus frame per voice object, numbered on from the
    /// last this page published.
    pub(super) fn publish_call_audio(&mut self, workspace_id: u64, frames: Vec<crate::CallFrame>) {
        use tilo_live::live::Track;
        for frame in frames {
            let Some(joined) = self.planes.joined_mut(workspace_id) else {
                return;
            };
            let sequence = joined.audio_seq;
            joined.audio_seq = sequence.wrapping_add(1);
            let object = tilo_live::voice::VoiceObject {
                sequence: u64::from(sequence),
                timestamp: frame.timestamp_48k,
                duration: tilo_live::voice::FRAME_SAMPLES,
                payload: frame.opus,
            };
            let Ok(body) = object.encode() else {
                continue;
            };
            if joined.plane.publish(Track::Audio, sequence, &body).is_err() {
                // The plane ended: its next join carries what follows.
                return;
            }
        }
    }

    /// Publish `pictures` on `workspace_id`'s plane: each one layer of this
    /// page's camera or screen, sealed by the plane as this device and read by
    /// every device showing that layer.
    pub(super) fn publish_call_pictures(
        &self,
        workspace_id: u64,
        pictures: Vec<super::CallPicture>,
    ) {
        for picture in pictures {
            let Some(joined) = self.planes.joined(workspace_id) else {
                return;
            };
            let object = tilo_live::video::VideoObject {
                keyframe: picture.keyframe,
                codec: tilo_live::video::Codec::H264,
                layer: picture.layer,
                sequence: picture.sequence,
                timestamp_us: picture.timestamp_us,
                width: picture.width,
                height: picture.height,
                payload: picture.payload,
            };
            // A layer nobody shows, or a plane that ended, drops this
            // picture: the next layer that gains a reader starts at a
            // keyframe.
            let _ = joined.plane.send_picture(picture.source, &object);
        }
    }

    /// Show the devices' `shown` layers here, and stop reading the devices
    /// left out: the plane asks each owner for exactly these.
    pub(super) fn show_call_pictures(&mut self, workspace_id: u64, shown: &[super::CallShown]) {
        use tilo_live::live::Track;
        let Some(joined) = self.planes.joined_mut(workspace_id) else {
            return;
        };
        let wanted: BTreeMap<[u8; 32], BTreeSet<Track>> = shown
            .iter()
            .map(|entry| {
                let tracks: BTreeSet<Track> = entry
                    .camera
                    .iter()
                    .map(|layer| Track::Camera(*layer))
                    .chain(entry.screen.iter().map(|layer| Track::Screen(*layer)))
                    .collect();
                (entry.device, tracks)
            })
            .collect();
        let gone: Vec<[u8; 32]> = joined
            .shown
            .keys()
            .filter(|device| !wanted.contains_key(*device))
            .copied()
            .collect();
        for device in gone {
            joined.plane.show(device, &[]);
        }
        for (device, tracks) in wanted {
            joined
                .plane
                .show(device, &tracks.iter().copied().collect::<Vec<_>>());
            joined.shown.insert(device, tracks);
        }
    }

    /// Ask `device` for `source` afresh: its next picture starts a group the
    /// page's decoder can begin at.
    pub(super) fn restart_call_picture(
        &mut self,
        workspace_id: u64,
        device: [u8; 32],
        source: tilo_live::video::Source,
    ) {
        use tilo_live::live::Track;
        let Some(joined) = self.planes.joined_mut(workspace_id) else {
            return;
        };
        let Some(tracks) = joined.shown.get(&device).cloned() else {
            return;
        };
        let of_source = |track: &Track| {
            matches!(
                (track, source),
                (Track::Camera(_), tilo_live::video::Source::Camera)
                    | (Track::Screen(_), tilo_live::video::Source::Screen)
            )
        };
        let rest: Vec<Track> = tracks.iter().copied().filter(|t| !of_source(t)).collect();
        let again: Vec<Track> = tracks.iter().copied().filter(|t| of_source(t)).collect();
        // Dropping the readers and asking again is what the plane takes as a
        // new reader: the owner answers with a keyframe.
        joined.plane.show(device, &rest);
        joined.plane.show(device, &again);
    }

    /// Take what `workspace_id`'s plane heard since the last call: each
    /// member's track object, already opened with the scope's keys, as
    /// [`PeerEvent::LivePlane`].
    pub(super) fn plane_heard(&mut self, workspace_id: u64) {
        use tilo_live::live::Track;
        let heard: Vec<_> = match self.planes.joined(workspace_id) {
            Some(joined) => std::iter::from_fn(|| joined.plane.try_recv()).collect(),
            None => return,
        };
        for heard in heard {
            // A call's voice: one Opus frame of the device that published it,
            // as the page's app plays it.
            if heard.track == Track::Audio {
                if let Ok(object) = tilo_live::voice::VoiceObject::decode(&heard.body) {
                    self.emit(&PeerEvent::CallAudio {
                        workspace_id,
                        from: heard.sender,
                        sequence: object.sequence,
                        timestamp_48k: object.timestamp,
                        opus: object.payload,
                    });
                }
                continue;
            }
            // A call's picture: one layer of a device's camera or screen, as
            // the page's decoder draws it.
            if let Track::Camera(_) | Track::Screen(_) = heard.track {
                if let Ok(object) = tilo_live::video::VideoObject::decode(&heard.body) {
                    let source = match heard.track {
                        Track::Screen(_) => tilo_live::video::Source::Screen,
                        _ => tilo_live::video::Source::Camera,
                    };
                    self.emit(&PeerEvent::CallPicture {
                        workspace_id,
                        from: heard.sender,
                        source,
                        keyframe: object.keyframe,
                        object: heard.body,
                    });
                }
                continue;
            }
            // A record's preview frame is still sealed with that record's
            // keys inside the plane's own layer: it opens into the canonical
            // frame the page's live layer reads ([`PeerEvent::Live`]).
            if heard.track == Track::Activity {
                self.live_frame(
                    crate::held_live::Waiting::Receive {
                        workspace_id,
                        from: heard.sender,
                        frame: heard.body,
                    },
                    true,
                );
                continue;
            }
            self.emit(&PeerEvent::LivePlane {
                workspace_id,
                from: heard.sender,
                track: heard.track,
                sequence: heard.sequence,
                body: heard.body,
            });
        }
        // What the call's devices show of this page, and what they need
        // afresh: its encoders read it, and encode only that.
        let demand = {
            let Some(joined) = self.planes.joined_mut(workspace_id) else {
                return;
            };
            let revision = joined.plane.pictures_revision();
            if joined.demand_at == revision {
                None
            } else {
                joined.demand_at = revision;
                let wanted = joined.plane.wanted_pictures();
                let mut keyframes = Vec::new();
                for source in [
                    tilo_live::video::Source::Camera,
                    tilo_live::video::Source::Screen,
                ] {
                    if joined.plane.wants_picture_keyframe(source) {
                        let _ = joined.plane.take_picture_keyframes(source);
                        keyframes.push(source);
                    }
                }
                Some((wanted, keyframes))
            }
        };
        if let Some((wanted, keyframes)) = demand {
            self.emit(&PeerEvent::CallDemand {
                workspace_id,
                wanted,
                keyframes,
            });
        }
    }

    /// `workspace_id`'s plane ended: the relay cut it (the device was removed,
    /// or the keys moved under it) or the carrier went away. It is joined
    /// again, until the workspace refuses this page.
    pub(super) fn plane_ended(&mut self, workspace_id: u64) {
        self.planes_due.insert(workspace_id);
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
