//! This device's windows shared with a workspace, and the windows its members
//! share: the session's side of a window's stream.
//! A person shares a window with the workspace they are in (`share`): the session
//! captures it through the compositor's own read-back, where capture protection
//! blanks a secret window, encodes it on a thread of its own, and publishes its
//! pictures on the workspace's live plane, one sealed track per window
//! (`window/<tab>/high`, [`Track::Window`]). The plane is the demand: a window is
//! captured and encoded only while a member's desk shows it, and a member that
//! starts showing it is sent a keyframe. The share ends when the person stops it
//! or the window closes, and the workspace's governance stops naming it.
//! A member views a window another device shares by asking the plane for its
//! track; the keyframe's header says the codec, size and colour, and what comes
//! is decoded into that tab's tile here ([`PeerRead`]). Its viewer also says it
//! views the window ([`SurfaceViewRequest`]) and sends its input back, as
//! control messages to the hosting device alone: directly, or through the
//! workspace's host where that device takes no connection. Another member's
//! input reaches the window through a seat of its own ([`Compositor::open_seat`]),
//! only while the person allows it, and the compositor's firewall refuses every
//! member's input after the cut chord or while the session is locked. A session
//! that exists to be seen (a hosted environment nobody sits at) lets every member
//! the workspace lets edit control what it shares.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use media::StreamCodec;
use media::realtime::CodecCapabilities;
use neighbour_wire::{
    MAX_SURFACE_VIEW_DECODERS, MAX_SURFACE_VIEW_SIDE, MAX_SURFACE_VIEWS, NeighbourMsg, PeerId,
    SurfaceDecodeLimit, SurfaceId, SurfaceView,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_action_policy::share::{ShareAccess, is_of_share, share_grants};
use tilo_actions::{
    ActionAddress, ActionContract, ActionParam, ActionSpec, DoorError, EncodedAction,
    ExecutionMode, OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_compositor::capture::Target;
use tilo_compositor::{Compositor, InputEvent, Selection};

use tilo_desktop::chrome::ActiveExposure;
use tilo_desktop_model::window_share::{window_node, window_share_id, window_shares};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, peer_salt_of,
};
use tilo_desktop_surface::i18n::t;
use tilo_desktop_surface::{DesktopHandle, PeerStreamStart};
use tilo_live::live::Track;
use tilo_live::plane::Plane;
use tilo_live::video::{Layer, VideoObject};
use tilo_network_actor::WorkspaceId;
use tilo_service::{Service, Started, Wake};
use tilo_window_share::config::FlatpakShareConfig;
use tilo_window_share::flatpak_share::{EncodedFrame, SharedFlatpakSession};
use tilo_window_share::streaming::InputEvent as SharedInput;

use crate::sync::{Client, ReceivedSurface};

/// The resource the session publishes its window shares on: what it shares,
/// what it waits to share, what it watches and what it received.
pub(crate) const SHARES_RESOURCE: &str = "session:window-share";

/// The operation type this resource's actions carry.
const OPERATION: &str = "tilo.session.window-share";

/// How often a viewer says again which windows of a host it shows: the host ends the streams of a
/// viewer it stops hearing from.
const VIEW_AGAIN: Duration = Duration::from_secs(2);
/// How long a viewer's ask keeps its streams up after the last one arrived.
const VIEW_EXPIRY: Duration = Duration::from_secs(6);

/// Why a shared window's tile went: its tab is no longer on the desk.
const TAB_GONE: &str = "the window is no longer shown here";
/// Why a shared window's tile went: this session left its workspaces.
const LEFT_WORKSPACE: &str = "this device left the workspace";
/// Why a shared window's tile went: its sender ended the stream.
const SENDER_STOPPED: &str = "the sender stopped sharing it";
/// Why a shared window's tile was made anew: its sender started its stream over at another size or
/// in another codec.
const RESTARTED: &str = "its picture changed size";

/// What the person asks of the shares, as the resource's actions spell it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
enum ShareCommand {
    /// Share the window `window` (a compositor identifier,
    /// [`Compositor::shareable_windows`]) with `workspace` as the window of `tab`
    /// belonging to `app_id`.
    Share {
        window: String,
        tab: TabId,
        app_id: String,
        workspace: String,
    },
    /// Stop sharing the window of `tab` .
    Stop { tab: TabId },
    /// Let the members viewing the window of `tab` control it (`Some(true)`),
    /// stop them (`Some(false)`), or the other way from how it is.
    AllowInput { tab: TabId, on: Option<bool> },
    /// Show the window another member shares as `tab` of `workspace` .
    Watch { tab: TabId, workspace: String },
    /// Stop showing it.
    Unwatch { tab: TabId, workspace: String },
}

/// One window this session shares.
struct Hosted {
    /// The compositor's identifier of the window, as the share named it.
    window: String,
    app_id: String,
    workspace: String,
    share: Arc<SharedFlatpakSession>,
    /// Whether a member's desk shows the window: its track is asked for on the plane, so it is
    /// captured and encoded.
    shown: bool,
    /// Whether every member the workspace lets edit controls the window, without the person
    /// allowing each: a session that exists to be seen shares its windows so.
    open: bool,
    /// When its frame was last captured.
    captured_at: Option<Instant>,
    /// What the members it streams to decode: a stream is only encoded in a codec every one of them
    /// takes.
    receivers: Vec<CodecCapabilities>,
    /// The members the person let control the window: their input reaches it for as long as the
    /// share runs, across a restart of its stream.
    controlled: BTreeSet<PeerId>,
}

/// A share the session was asked for and has not started: the window it shows was not on screen
/// when it was asked, and the session tries again every pass until it is.
struct Starting {
    window: String,
    tab: TabId,
    app_id: String,
    workspace: String,
    /// Why it is still waiting.
    waiting_for: String,
    /// The members the person let control it, carried across a restart.
    controlled: BTreeSet<PeerId>,
}

/// A window of this device whose page this session publishes for the members that show it: the
/// app draws it, so no picture of it is captured or encoded here.
struct Paged {
    /// The tab the window stands in on every device of the workspace, and the surface its
    /// announcement names.
    tab: TabId,
    /// The compositor's identifier of the window, as the share named it.
    window: String,
    app_id: String,
    workspace: String,
    /// The workspace's topic the announcement goes out on.
    ws: WorkspaceId,
    /// The window's size, which the announcement carries so a viewer reserves its place before the
    /// page's first frame.
    width: u32,
    height: u32,
    /// The viewers the announcement went to, and their devices: the ones a page's track is
    /// published for.
    announced: BTreeMap<PeerId, EndpointId>,
}

impl Paged {
    /// The announcement of this window's page: a stream of its own, its codec the display list.
    fn stream_start_msg(&self, peer: PeerId) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamStart {
            surface: self.tab,
            source_peer: peer,
            app_id: self.app_id.clone(),
            width: self.width,
            height: self.height,
            codec: ui::webview::paint::stream::wire::CODEC.to_owned(),
            ts: 0,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 8,
            tab: self.tab,
        }
    }

    /// The end of it, for a viewer that stops showing the window or a share that ends.
    fn stream_end_msg(&self, peer: PeerId) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamEnd {
            surface: self.tab,
            source_peer: peer,
            ts: 0,
        }
    }
}

/// A member viewing a window of this device: its device, when it last asked, and what it can
/// decode.
struct Asked {
    device: EndpointId,
    at: Instant,
    /// The largest picture its hardware decoders take, by codec: what its stream is encoded for.
    decode: Vec<SurfaceDecodeLimit>,
}

/// A window another member shares, as this session shows it.
struct Received {
    /// The tab the window stands in: its tile decodes the stream there.
    tab: TabId,
    /// The device sharing it, which its viewer's input and keyframe requests go back to.
    host: EndpointId,
    /// The workspace's topic its stream came in on, which those go back over.
    ws: WorkspaceId,
    /// Whether the desk holds the tab its tile stands in yet.
    shown: bool,
    /// The stream as its keyframe or its page's announcement says it.
    start: PeerStreamStart,
    /// Whether the stream is the window's page (a `tilo-paint` display list, announced by its
    /// host, [`crate::page_track`]) rather than its pictures: this device asks the plane for the
    /// page's track instead of the window's.
    page: bool,
}

/// The state behind the resource: what this session shares, what it watches and what reached it.
/// The service reads it and queues commands; the session's pass carries them out with the
/// compositor and the plane.
#[derive(Default)]
pub(crate) struct WindowShares {
    hosted: BTreeMap<TabId, Hosted>,
    /// The windows whose page this session publishes for the members showing them: their apps draw
    /// them, and nothing of them is captured here.
    pages: BTreeMap<TabId, Paged>,
    /// The tracks this device shows of other members' windows (their pictures or pages), by
    /// workspace and the device hosting them: what each plane is asked for, so the ones it stops
    /// showing are published no longer.
    shown_tracks: BTreeMap<String, BTreeMap<EndpointId, BTreeSet<Track>>>,
    starting: Vec<Starting>,
    asked: BTreeMap<(TabId, PeerId), Asked>,
    watching: BTreeMap<(String, TabId), Watched>,
    received: BTreeMap<(PeerId, SurfaceId), Received>,
    commands: Vec<ShareCommand>,
    /// Shares whose viewers' decoders changed, so their stream is encoded again for the codecs all
    /// of them take.
    renegotiate: BTreeSet<TabId>,
    /// Moves whenever what the resource says changes.
    revision: u64,
    /// The wake a share's encoder thread rings when a frame is ready.
    wake: Option<Wake>,
    /// The person at this device, as the workspaces' governance names them: the sharer of the
    /// windows this session shares.
    person: String,
    /// The salt this device mints its tabs under: a tab carrying it is this device's window, and
    /// every other tab in a share is another member's.
    salt: u16,
    /// The tabs this session put on the desk because the workspace's governance says another member
    /// shares the window there, by the workspace id it says it in: what it takes away again when
    /// the share ends.
    governed: BTreeMap<TabId, Governed>,
    /// The seat each member controlling a shared window acts at: its own pointer and keyboard in
    /// that window, beside the person's.
    seats: BTreeMap<(PeerId, SurfaceId), tilo_compositor::Seat>,
    /// Whether this session exists to be seen: a hosted browser environment shares its windows with
    /// the workspace's members the moment they open, since nobody runs it for itself.
    share_by_default: bool,
}

/// A window another member shares, as this session shows it on the desk.
struct Governed {
    /// The workspace whose governance says so.
    workspace: String,
    /// Whether the strip it is in was made for it, and goes with it.
    strip: bool,
}

/// A window another device shares in a workspace of this desk, as the workspace's governance names
/// it.
struct Shared {
    /// The tab the window is shown in, the one its sharer's desk names.
    tab: TabId,
    /// The app the window belongs to.
    app_id: String,
    /// The key of the device the window runs on.
    host: String,
    /// The workspace the governance says it in.
    workspace: String,
    /// What the workspace calls the device that hosts it.
    host_label: String,
}

/// A window of another member's this device shows.
struct Watched {
    /// The device that hosts the window.
    host: EndpointId,
    /// When this device last asked to view it.
    asked_at: Instant,
}

impl WindowShares {
    /// The shares, with `wake` being what a share's encoder rings when it has a frame for the next
    /// pass, `person` the person whose shares the workspaces' governance carries, and `salt` what
    /// this device mints its tabs under.
    pub(crate) fn new(
        wake: Wake,
        person: String,
        salt: u16,
        share_by_default: bool,
    ) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            wake: Some(wake),
            person,
            salt,
            share_by_default,
            ..Self::default()
        }))
    }

    /// What the bar says is being shared: each member viewing a window of this device, and whether
    /// it may control it.
    pub(crate) fn exposures(&self) -> Vec<ActiveExposure> {
        let mut exposures = Vec::new();
        for (tab, hosted) in &self.hosted {
            for peer in self.viewers(*tab) {
                exposures.push(ActiveExposure {
                    peer,
                    surface: *tab,
                    can_control: hosted.open || hosted.controlled.contains(&peer),
                });
            }
        }
        for tab in self.pages.keys() {
            for peer in self.viewers(*tab) {
                exposures.push(ActiveExposure {
                    peer,
                    surface: *tab,
                    can_control: false,
                });
            }
        }
        exposures
    }

    /// The members viewing `tab` of this device: the ones that asked to, in order.
    fn viewers(&self, tab: TabId) -> Vec<PeerId> {
        self.asked
            .keys()
            .filter(|(asking, _)| *asking == tab)
            .map(|(_, peer)| *peer)
            .collect()
    }

    /// The resource's reading now.
    fn reading(&self) -> Value {
        let sharing: Vec<Value> = self
            .hosted
            .iter()
            .map(|(tab, hosted)| {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": hosted.open || hosted.controlled.contains(&peer),
                        })
                    })
                    .collect();
                json!({
                    "tab": tab.to_string(),
                    "window": hosted.window,
                    "app_id": hosted.app_id,
                    // Kept as it is: the views that read this know the key.
                    "room": hosted.workspace,
                    "input_allowed": viewers.iter().any(|viewer| {
                        viewer["can_control"].as_bool().unwrap_or(false)
                    }),
                    "viewers": viewers,
                })
            })
            .collect();
        let sharing = {
            let mut sharing = sharing;
            for (tab, page) in &self.pages {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": false,
                        })
                    })
                    .collect();
                sharing.push(json!({
                    "tab": tab.to_string(),
                    "window": page.window,
                    "app_id": page.app_id,
                    "room": page.workspace,
                    "input_allowed": false,
                    "viewers": viewers,
                }));
            }
            sharing
        };
        let starting: Vec<Value> = self
            .starting
            .iter()
            .map(|start| {
                json!({
                    "tab": start.tab.to_string(),
                    "window": start.window,
                    "app_id": start.app_id,
                    "room": start.workspace,
                    "waiting_for": start.waiting_for,
                })
            })
            .collect();
        let watching: Vec<Value> = self
            .watching
            .iter()
            .map(|((workspace, tab), watched)| {
                json!({ "tab": tab.to_string(), "room": workspace, "host": watched.host.to_string() })
            })
            .collect();
        let received: Vec<Value> = self
            .received
            .iter()
            .map(|((from, surface), stream)| {
                json!({
                    "tab": stream.tab.to_string(),
                    "surface": surface,
                    "from": from,
                    "app_id": stream.start.app_id,
                    "width": stream.start.width,
                    "height": stream.start.height,
                    "codec": stream.start.codec,
                    "pictured": stream.shown,
                })
            })
            .collect();
        json!({
            "sharing": sharing,
            "starting": starting,
            "watching": watching,
            "received": received,
        })
    }

    /// Take a command from the resource's actions.
    fn queue(&mut self, command: ShareCommand) {
        self.commands.push(command);
        self.changed();
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// One pass of the shares: take what the plane brought and carry out what the person asked,
    /// then show what the workspaces' governance says the members share, encode and publish the
    /// shared windows, and end what is gone.
    pub(crate) fn step(
        &mut self,
        compositor: &mut Compositor,
        desk: &DesktopHandle,
        displays: &crate::displays::Displays,
        sync: Option<&Client>,
        pages: &Rc<RefCell<crate::page_track::PageTracks>>,
        now: Instant,
    ) {
        let Some(sync) = sync else {
            // Without the plane a window cannot be shared: nothing reaches a member, and nothing of
            // theirs arrives.
            if !self.commands.is_empty()
                || !self.hosted.is_empty()
                || !self.pages.is_empty()
                || !self.governed.is_empty()
                || !self.received.is_empty()
            {
                self.commands.clear();
                self.end_all(compositor, desk, None);
                self.changed();
            }
            return;
        };
        // What arrived is read first, so a share that starts this pass already knows who views it
        // and in which codec to encode.
        self.take_received(compositor, desk, sync);
        let tree = desk.read(Clone::clone);
        self.share_environment_windows(compositor, &tree);
        self.carry_out(compositor, desk, &tree, sync, pages, now);
        self.follow_governance(desk, sync, now);
        // A stream that reached this device before the desk held the tab of its window is decoded
        // there from this pass on.
        self.show_streams(desk);
        land_displays(desk, displays);
        self.renegotiate();
        self.ask_again(compositor, sync, now);
        self.capture(compositor, sync, now);
        // What the members' windows this device shows bring: the pictures and pages the planes
        // sent, and the tracks of a window that left the desk no longer asked for.
        self.follow_windows(desk, sync);
        self.end_gone(compositor, desk, sync);
        // Last, so what the tiles asked for while this pass decoded them reaches the devices
        // hosting the windows.
        self.send_outgoing(desk, sync);
    }

    /// When the shares next need a pass of their own: a viewer's ask for a window said again before
    /// its host stops hearing it, an ask that stops counting as this device still viewing what it
    /// asked for, or the next frame of a shared window that has someone to send it to.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let asked_again = self
            .watching
            .values()
            .filter_map(|watched| watched.asked_at.checked_add(VIEW_AGAIN));
        let expired = self
            .asked
            .values()
            .filter_map(|asked| asked.at.checked_add(VIEW_EXPIRY));
        let frames = self.hosted.values().filter_map(|hosted| {
            if !hosted.shown {
                return None;
            }
            let rate = hosted.share.rate().max(1);
            let frame = Duration::from_secs_f64(1.0 / f64::from(rate));
            hosted.captured_at.and_then(|at| at.checked_add(frame))
        });
        asked_again.chain(expired).chain(frames).min()
    }

    /// Carry out what the person asked: start and stop shares, let a member control one, and ask to
    /// view another member's.
    fn carry_out(
        &mut self,
        compositor: &mut Compositor,
        desk: &DesktopHandle,
        tree: &DesktopTree,
        sync: &Client,
        pages: &Rc<RefCell<crate::page_track::PageTracks>>,
        now: Instant,
    ) {
        let commands = std::mem::take(&mut self.commands);
        for command in commands {
            match command {
                ShareCommand::Share {
                    window,
                    tab,
                    app_id,
                    workspace,
                } => {
                    if self.hosted.contains_key(&tab) {
                        continue;
                    }
                    // The workspace's governance names the window and the device it runs on, which
                    // is how the other members learn of the share. Only this device's own window is
                    // hosted here.
                    if peer_salt_of(tab) == self.salt {
                        self.announce(desk, tab, &app_id, &workspace);
                    } else {
                        tracing::debug!(
                            tab,
                            "a window another device hosts is not shared from here"
                        );
                    }
                    self.starting.retain(|start| start.tab != tab);
                    self.starting.push(Starting {
                        window,
                        tab,
                        app_id,
                        workspace,
                        waiting_for: String::new(),
                        controlled: BTreeSet::new(),
                    });
                }
                ShareCommand::Stop { tab } => {
                    self.starting.retain(|start| start.tab != tab);
                    if let Some(hosted) = self.hosted.remove(&tab) {
                        self.end(&hosted, tab, sync);
                    }
                    if let Some(page) = self.pages.remove(&tab) {
                        Self::end_page(&page, sync);
                    }
                    self.retract(desk, tab);
                    self.asked.retain(|(asking, _), _| *asking != tab);
                    self.renegotiate.remove(&tab);
                    self.close_seats(compositor, tab, None);
                    self.changed();
                }
                ShareCommand::AllowInput { tab, on } => {
                    if !self.hosted.contains_key(&tab) {
                        continue;
                    }
                    let peers = self.viewers(tab);
                    let Some(hosted) = self.hosted.get_mut(&tab) else {
                        continue;
                    };
                    let allow = on.unwrap_or_else(|| {
                        !peers.iter().any(|peer| hosted.controlled.contains(peer))
                    });
                    for peer in &peers {
                        hosted.share.set_read_only(*peer, !allow);
                    }
                    if allow {
                        hosted.controlled.extend(peers);
                    } else {
                        hosted.controlled.clear();
                    }
                    if !allow {
                        self.close_seats(compositor, tab, None);
                    }
                    self.changed();
                }
                ShareCommand::Watch { tab, workspace } => {
                    let Some((host, _)) = hosted_window(tree, tab) else {
                        tracing::debug!(tab, "no window of another device is shown in this tab");
                        continue;
                    };
                    let Some(host) = device_key(&host) else {
                        tracing::debug!(tab, %host, "a tab is hosted by no device key");
                        continue;
                    };
                    self.watching.insert(
                        (workspace, tab),
                        Watched {
                            host,
                            asked_at: now,
                        },
                    );
                    self.changed();
                }
                ShareCommand::Unwatch { tab, workspace } => {
                    if self.watching.remove(&(workspace, tab)).is_some() {
                        self.changed();
                    }
                }
            }
        }
        self.start_waiting(compositor, desk, sync, pages, now);
    }

    /// Write into the governance of the workspace `workspace` that this person shares the window
    /// `tab` of `app_id` : the share grants of the window's own node, which every device of the
    /// workspace reads to show it. The workspace's own governance checks them as it admits them, as
    /// it does any grant.
    fn announce(&self, desk: &DesktopHandle, tab: TabId, app_id: &str, workspace: &str) {
        let share = window_share_id(tab);
        let node = window_node(workspace, app_id, tab);
        let grants = match share_grants(workspace, &self.person, &share, &node, ShareAccess::View) {
            Ok(grants) => grants,
            Err(error) => {
                tracing::warn!(%error, tab, "a window cannot be shared with the workspace");
                return;
            }
        };
        let mut tree = desk.read(Clone::clone);
        let mut changed = false;
        for held in tree
            .workspaces
            .iter_mut()
            .filter(|held| held.policy.workspace == workspace)
        {
            for grant in &grants {
                let present = held.policy.grants.iter().any(|known| known.id == grant.id);
                if !present {
                    held.policy.grants.push(grant.clone());
                    changed = true;
                }
            }
        }
        if changed {
            desk.replace_tree(tree);
        }
    }

    /// Take the share of the window `tab` out of the governance of the workspaces addressing the
    /// workspace it was shared with: it is not shared any more, so no device shows it.
    fn retract(&mut self, desk: &DesktopHandle, tab: TabId) {
        let share = window_share_id(tab);
        let held = |workspace: &tilo_desktop_model::Workspace| {
            workspace
                .policy
                .grants
                .iter()
                .any(|grant| is_of_share(grant, &share))
        };
        if !desk.read(|tree| tree.workspaces.iter().any(held)) {
            return;
        }
        let mut tree = desk.read(Clone::clone);
        for workspace in tree
            .workspaces
            .iter_mut()
            .filter(|workspace| held(workspace))
        {
            workspace.end_window_shares(tab);
        }
        desk.replace_tree(tree);
        self.changed();
    }

    /// Show what the workspaces' governance says the members share, and take away what ended: each
    /// window another device hosts as a tab of its own at the end of the workspace's last strip,
    /// watched so its stream comes here. A share is the workspace's, so every device shows it
    /// without anyone naming the window there.
    fn follow_governance(&mut self, desk: &DesktopHandle, sync: &Client, now: Instant) {
        let mut tree = desk.read(Clone::clone);
        let shared = self.shared_in(&tree, sync);
        let live: BTreeSet<TabId> = shared.iter().map(|shared| shared.tab).collect();
        let mut changed = self.take_ended(desk, &mut tree, &live);
        for shared in &shared {
            changed |= self.show_shared(&mut tree, shared, now);
        }
        if changed {
            desk.replace_tree(tree);
        }
    }

    /// The windows another device shares in `tree` 's workspaces, as their governance names them.
    fn shared_in(&self, tree: &DesktopTree, sync: &Client) -> Vec<Shared> {
        let now_ms = tilo_clock::unix_now_ms();
        let mut shared = Vec::new();
        for held in &tree.workspaces {
            let workspace = held.policy.workspace.clone();
            let devices = sync.workspace_devices(&workspace);
            let owners = owners_of(self.salt, sync.publisher(), &devices);
            for (tab, share) in window_shares(&held.policy, &owners, now_ms) {
                if peer_salt_of(tab) == self.salt {
                    // This device's own window: it streams from here, and shows no tab of itself.
                    continue;
                }
                let host_label = devices
                    .iter()
                    .find(|(device, _)| hex::encode(device.as_bytes()) == share.host)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_default();
                shared.push(Shared {
                    tab,
                    app_id: share.app_id,
                    host: share.host,
                    workspace: workspace.clone(),
                    host_label,
                });
            }
        }
        shared
    }

    /// Take the tabs of shares the governance no longer names out of `tree`,
    /// with the watches they were asked for: what ended is shown nowhere.
    fn take_ended(
        &mut self,
        desk: &DesktopHandle,
        tree: &mut DesktopTree,
        live: &BTreeSet<TabId>,
    ) -> bool {
        let ended: Vec<TabId> = self
            .governed
            .keys()
            .copied()
            .filter(|tab| !live.contains(tab))
            .collect();
        let mut changed = false;
        for tab in ended {
            let Some(governed) = self.governed.remove(&tab) else {
                continue;
            };
            self.watching.remove(&(governed.workspace, tab));
            self.end_tab(desk, tab, TAB_GONE);
            remove_governed_tile(tree, tab, governed.strip.then_some(StripId(tab)));
            changed = true;
        }
        changed
    }

    /// Add the tab of `shared` 's window to `tree` at the end of its workspace's last strip, and
    /// watch it from the device the governance says hosts it. Answers whether `tree` changed.
    fn show_shared(&mut self, tree: &mut DesktopTree, shared: &Shared, now: Instant) -> bool {
        let Shared {
            tab,
            app_id,
            host,
            workspace,
            host_label,
        } = shared;
        let (tab, workspace) = (*tab, workspace.clone());
        let mut changed = false;
        if shown(tree, tab) {
            self.governed.entry(tab).or_insert_with(|| Governed {
                workspace: workspace.clone(),
                strip: false,
            });
        } else if let Some(held) = tree
            .workspaces
            .iter_mut()
            .find(|held| held.policy.workspace == workspace)
        {
            let label = app_id.clone();
            let group = WindowGroup {
                id: tab,
                tabs: vec![Tab {
                    id: tab,
                    title: label.clone(),
                    tile: TileContent::HostedWindow {
                        app_id: app_id.clone(),
                        label,
                        host: host.clone(),
                        host_label: host_label.clone(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            };
            let strip = held.strips.is_empty();
            if strip {
                held.strips.push(Strip {
                    id: StripId(tab),
                    name: String::new(),
                    window_groups: Vec::new(),
                });
            }
            if let Some(last) = held.strips.last_mut() {
                last.window_groups.push(group);
            }
            self.governed.insert(
                tab,
                Governed {
                    workspace: workspace.clone(),
                    strip,
                },
            );
            self.changed();
            changed = true;
        }
        // Its stream is asked for as any watched window's is, from the device the governance says
        // hosts it.
        if let Some(host) = device_key(host)
            && let std::collections::btree_map::Entry::Vacant(seat) =
                self.watching.entry((workspace, tab))
        {
            seat.insert(Watched {
                host,
                asked_at: now.checked_sub(VIEW_AGAIN).unwrap_or(now),
            });
            self.changed();
        }
        changed
    }

    /// Start the shares that can start now, and keep those that cannot waiting with why.
    fn start_waiting(
        &mut self,
        compositor: &mut Compositor,
        desk: &DesktopHandle,
        sync: &Client,
        pages: &Rc<RefCell<crate::page_track::PageTracks>>,
        now: Instant,
    ) {
        if self.starting.is_empty() {
            return;
        }
        let shareable: Vec<String> = compositor
            .shareable_windows()
            .into_iter()
            .map(|(identifier, _, _)| identifier)
            .collect();
        for mut start in std::mem::take(&mut self.starting) {
            if !shareable.contains(&start.window) {
                // The window closed before it could be shared: the workspace stops naming it as
                // shared.
                tracing::debug!(
                    window = %start.window,
                    "a window to share is gone before its share started"
                );
                self.retract(desk, start.tab);
                continue;
            }
            let Some(ws) = sync.workspace_topic(&start.workspace) else {
                t("session-window-share-workspace-left").clone_into(&mut start.waiting_for);
                self.starting.push(start);
                continue;
            };
            // A window whose app draws its page hands that page over rather than a picture of it.
            if pages.borrow().streamed(start.tab, &start.workspace) {
                self.pages
                    .insert(start.tab, page_of(compositor, &start, ws));
                self.changed();
                continue;
            }
            match self.begin_share(compositor, sync, &start, now) {
                Ok(hosted) => {
                    self.hosted.insert(start.tab, hosted);
                    self.changed();
                }
                Err(waiting_for) => {
                    start.waiting_for = waiting_for;
                    self.starting.push(start);
                }
            }
        }
    }

    /// Capture and start the stream of one waiting window, or say why it must keep waiting.
    fn begin_share(
        &self,
        compositor: &mut Compositor,
        sync: &Client,
        start: &Starting,
        now: Instant,
    ) -> Result<Hosted, String> {
        let picture = compositor
            .read_back(&Target::window(start.window.clone()))
            .map_err(|error| error.to_string())?;
        let receivers = self.receivers_for(start.tab);
        let share = SharedFlatpakSession::start_negotiated(
            sync.peer(),
            start.app_id.clone(),
            start.tab,
            picture.width,
            picture.height,
            &FlatpakShareConfig::default(),
            &receivers,
        )
        .map(Arc::new)
        .inspect_err(|error| {
            tracing::warn!(
                window = %start.window,
                %error,
                "a window cannot be shared"
            );
        })
        .map_err(|error| error.to_string())?;
        if let Some(wake) = &self.wake
            && let Err(error) = share.encode_in_background(Arc::clone(wake))
        {
            tracing::warn!(%error, "a share encodes on the session's loop");
        }
        share.deposit_captured_rgba(picture.rgba);
        Ok(Hosted {
            window: start.window.clone(),
            app_id: start.app_id.clone(),
            workspace: start.workspace.clone(),
            share,
            shown: false,
            open: self.share_by_default,
            captured_at: Some(now),
            receivers,
            controlled: start.controlled.clone(),
        })
    }

    /// What the members viewing `tab` decode: a stream is encoded in a codec every one of them
    /// takes, no larger than the smallest hardware decoder allows.
    fn receivers_for(&self, tab: TabId) -> Vec<CodecCapabilities> {
        self.asked
            .iter()
            .filter(|((asking, _), _)| *asking == tab)
            .map(|((_, peer), asked)| {
                let mut caps = tilo_network_actor::media_caps::capabilities(*peer);
                let limits = decode_limits(&asked.decode);
                caps.hardware_decode.extend(limits.keys().copied());
                caps.hardware_decode_max = limits;
                caps
            })
            .collect()
    }

    /// Encode the shares whose viewers' decoders changed again, so their stream stays one every
    /// viewer takes.
    fn renegotiate(&mut self) {
        if self.renegotiate.is_empty() {
            return;
        }
        let due: Vec<TabId> = std::mem::take(&mut self.renegotiate)
            .into_iter()
            .filter(|tab| self.hosted.contains_key(tab))
            .collect();
        for tab in due {
            let Some(hosted) = self.hosted.get(&tab) else {
                continue;
            };
            if hosted.receivers == self.receivers_for(tab) {
                continue;
            }
            self.restart(tab);
        }
    }

    /// End the shares whose window closed, and the ones whose workspace this session no longer
    /// takes part in.
    fn end_gone(&mut self, compositor: &mut Compositor, desk: &DesktopHandle, sync: &Client) {
        if self.hosted.is_empty() && self.pages.is_empty() {
            return;
        }
        let shareable: Vec<String> = compositor
            .shareable_windows()
            .into_iter()
            .map(|(identifier, _, _)| identifier)
            .collect();
        let gone: Vec<TabId> = self
            .hosted
            .iter()
            .filter(|(_, hosted)| {
                !shareable.contains(&hosted.window)
                    || sync.workspace_topic(&hosted.workspace).is_none()
            })
            .map(|(tab, _)| *tab)
            .collect();
        // A page's share ends with its window the same way, and its viewers are told so.
        let poured: Vec<TabId> = self
            .pages
            .iter()
            .filter(|(_, page)| {
                !shareable.contains(&page.window) || sync.workspace_topic(&page.workspace).is_none()
            })
            .map(|(tab, _)| *tab)
            .collect();
        for tab in gone.into_iter().chain(poured) {
            if let Some(hosted) = self.hosted.remove(&tab) {
                self.end(&hosted, tab, sync);
            }
            if let Some(page) = self.pages.remove(&tab) {
                Self::end_page(&page, sync);
            }
            // The window is gone, or the workspace it was shared with is: it stops naming it.
            self.retract(desk, tab);
            self.asked.retain(|(asking, _), _| *asking != tab);
            self.close_seats(compositor, tab, None);
            self.changed();
        }
    }

    /// End every share: the session takes part in no plane.
    fn end_all(
        &mut self,
        _compositor: &mut Compositor,
        desk: &DesktopHandle,
        sync: Option<&Client>,
    ) {
        let hosted = std::mem::take(&mut self.hosted);
        for (tab, share) in hosted {
            if let Some(sync) = sync {
                self.end(&share, tab, sync);
            }
            self.retract(desk, tab);
        }
        let pages = std::mem::take(&mut self.pages);
        for (tab, page) in pages {
            if let Some(sync) = sync {
                Self::end_page(&page, sync);
            }
            self.retract(desk, tab);
        }
        self.shown_tracks.clear();
        self.starting.clear();
        self.asked.clear();
        self.watching.clear();
        for ((peer, surface), stream) in std::mem::take(&mut self.received) {
            if stream.shown {
                desk.end_peer_surface(peer, surface, LEFT_WORKSPACE);
            }
        }
        self.renegotiate.clear();
        self.end_governed(desk);
    }

    /// Take away every window this session showed because a workspace's governance said a member
    /// shared it: without the plane no stream of it comes, and the governance that named it is gone
    /// with it.
    fn end_governed(&mut self, desk: &DesktopHandle) {
        let governed = std::mem::take(&mut self.governed);
        if governed.is_empty() {
            return;
        }
        let mut tree = desk.read(Clone::clone);
        for (tab, entry) in governed {
            self.watching
                .retain(|(workspace, watched), _| *watched != tab || *workspace != entry.workspace);
            self.end_tab(desk, tab, TAB_GONE);
            remove_governed_tile(&mut tree, tab, entry.strip.then_some(StripId(tab)));
        }
        desk.replace_tree(tree);
        self.changed();
    }

    /// Send the end of `page` 's announcement to the viewers it went to, so each drops the window
    /// rather than reading a track nothing publishes any more.
    fn end_page(page: &Paged, sync: &Client) {
        for device in page.announced.values() {
            sync.publish_control(page.ws, page.stream_end_msg(sync.peer()), *device);
        }
    }

    /// Tell the members that said they view `hosted`, the window of `tab`, that its share ended,
    /// so each lets its tile go rather than waiting on pictures that stopped. A member that shows
    /// it because the workspace's governance names it learns it from the governance too.
    fn end(&self, hosted: &Hosted, tab: TabId, sync: &Client) {
        let Some(ws) = sync.workspace_topic(&hosted.workspace) else {
            return;
        };
        let end = NeighbourMsg::SurfaceStreamEnd {
            surface: tab,
            source_peer: sync.peer(),
            ts: 0,
        };
        for ((_, _), asked) in self.asked.iter().filter(|((asking, _), _)| *asking == tab) {
            sync.publish_control(ws, end.clone(), asked.device);
        }
    }

    /// Take what the plane carried in: a member's ask to view this device's windows, their input,
    /// their stream of a window this device shows.
    fn take_received(&mut self, compositor: &mut Compositor, desk: &DesktopHandle, sync: &Client) {
        let arrivals = sync.take_surfaces();
        if arrivals.is_empty() {
            return;
        }
        let mut changed = false;
        for ReceivedSurface {
            ws,
            from: publisher,
            msg,
        } in arrivals
        {
            // Someone else's speaker is one of the workspace's, by a handle: what a stream came
            // from and what its input names are the same member.
            let from = neighbour_wire::peer_handle(publisher.as_bytes());
            match msg {
                // Who asks and whose input it is is the device that sent it, as its route
                // authenticated it, whatever the message says of itself.
                NeighbourMsg::SurfaceViewRequest {
                    room: workspace,
                    views,
                    decode,
                    ..
                } => {
                    changed |=
                        self.note_views(sync, ws, (from, publisher), &workspace, &views, &decode);
                }
                NeighbourMsg::RemoteInput { surface, event, .. } => {
                    self.apply_input(compositor, from, surface, &event);
                }
                NeighbourMsg::SurfaceKeyframeRequest { surface, seq, .. } => {
                    if let Some(hosted) = self.hosted.get(&surface) {
                        hosted.share.request_keyframe(seq, Instant::now());
                    }
                }
                NeighbourMsg::SurfaceStreamStart {
                    surface,
                    tab: named,
                    ..
                } => {
                    // A page's announcement, with what only this desktop knows of it: who it is to
                    // the sender, what the desk calls the window, and whose device it comes from.
                    let (title, sharer) = desk.read(|tree| tab_title(tree, named));
                    let Some(start) =
                        PeerStreamStart::announced(&msg, sync.peer(), sharer, title, true)
                    else {
                        continue;
                    };
                    // A stream that is already decoded keeps its tile until its sender ends it,
                    // which is how a restarted stream reaches this device.
                    let shown = self
                        .received
                        .get(&(from, surface))
                        .is_some_and(|stream| stream.shown);
                    let page = start.codec == ui::webview::paint::stream::wire::CODEC;
                    self.received.insert(
                        (from, surface),
                        Received {
                            tab: start.tab,
                            host: publisher,
                            ws,
                            shown,
                            start,
                            page,
                        },
                    );
                    changed = true;
                }
                NeighbourMsg::SurfaceStreamEnd { surface, .. }
                    if self.received.remove(&(from, surface)).is_some() =>
                {
                    desk.end_peer_surface(from, surface, SENDER_STOPPED);
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.changed();
        }
    }

    /// Show the windows of other members this device shows, and let go of the ones that left the
    /// desk.
    ///
    /// A shared window travels the workspace's live plane, one track per window: its pictures
    /// (`tilo-video/1`) or, for a window whose app draws its page, its page (`tilo-page/1`,
    /// announced by its host). This device asks for exactly the tracks of the windows the desk
    /// holds and watches, and the plane tells the hosting device to publish those, and only those,
    /// while it does. A picture's keyframe makes or remakes the tile it is decoded in; a page is
    /// drawn into the tile its announcement made ([`crate::page_track`]).
    fn follow_windows(&mut self, desk: &DesktopHandle, sync: &Client) {
        let tree = desk.read(Clone::clone);
        let wanted = tracks_to_show(&self.received, &self.watching, |tab| shown(&tree, tab));
        if wanted != self.shown_tracks || !wanted.is_empty() {
            let planes = crate::calls::planes_of(sync);
            for (workspace, hosts) in &wanted {
                let Some(plane) = planes.get(workspace) else {
                    continue;
                };
                for (host, tracks) in hosts {
                    let tracks: Vec<Track> = tracks.iter().copied().collect();
                    sync.show_tracks(plane, *host.as_bytes(), &tracks);
                }
            }
            // A device whose windows this session showed and no longer does stops publishing them.
            for (workspace, hosts) in &self.shown_tracks {
                let Some(plane) = planes.get(workspace) else {
                    continue;
                };
                for host in hosts.keys() {
                    if wanted
                        .get(workspace)
                        .is_none_or(|showing| !showing.contains_key(host))
                    {
                        sync.show_tracks(plane, *host.as_bytes(), &[]);
                    }
                }
            }
            self.shown_tracks = wanted;
        }
        for (workspace, heard) in sync.take_window_objects() {
            let Ok(from) = EndpointId::from_bytes(&heard.sender) else {
                continue;
            };
            match heard.track {
                Track::Page(tab) => {
                    let Some(stream) = self
                        .received
                        .values()
                        .find(|stream| stream.page && stream.tab == tab && stream.host == from)
                    else {
                        continue;
                    };
                    desk.peer_surface_page(
                        neighbour_wire::peer_handle(from.as_bytes()),
                        stream.start.surface,
                        &heard.body,
                    );
                }
                Track::Window(tab, _) => {
                    self.picture_heard(desk, sync, &workspace, from, tab, &heard.body);
                }
                _ => {}
            }
        }
    }

    /// One picture of the window `tab` the device `from` shares in `workspace` : decoded in its
    /// tile, which its keyframe makes the first time and again when the stream starts over at
    /// another size, codec or colour.
    fn picture_heard(
        &mut self,
        desk: &DesktopHandle,
        sync: &Client,
        workspace: &str,
        from: EndpointId,
        tab: TabId,
        body: &[u8],
    ) {
        if self
            .watching
            .get(&(workspace.to_owned(), tab))
            .is_none_or(|watched| watched.host != from)
        {
            return;
        }
        // ignored-failure: the plane opened only what its relay validated as a picture
        let Ok(object) = VideoObject::decode(body) else {
            return;
        };
        let peer = neighbour_wire::peer_handle(from.as_bytes());
        let key = (peer, tab);
        if object.keyframe {
            let start = desk.read(|tree| picture_start(tree, sync.peer(), peer, tab, &object));
            let Some(start) = start else {
                tracing::debug!(tab, codec = ?object.codec, "a shared window's codec is not decoded here");
                return;
            };
            let same = self.received.get(&key).is_some_and(|stream| {
                let held = &stream.start;
                (held.codec.as_str(), held.width, held.height, held.bit_depth)
                    == (
                        start.codec.as_str(),
                        start.width,
                        start.height,
                        start.bit_depth,
                    )
            });
            if !same {
                let Some(ws) = sync.workspace_topic(workspace) else {
                    return;
                };
                if self
                    .received
                    .remove(&key)
                    .is_some_and(|stream| stream.shown)
                {
                    desk.end_peer_surface(peer, tab, RESTARTED);
                }
                let shown = desk.show_peer_surface(&start).is_some();
                self.received.insert(
                    key,
                    Received {
                        tab,
                        host: from,
                        ws,
                        shown,
                        start,
                        page: false,
                    },
                );
                self.changed();
            }
        }
        if self.received.contains_key(&key) {
            desk.peer_surface_picture(peer, tab, object.sequence, &object.payload);
        }
    }

    /// Share the windows a session that exists only to be seen runs: a hosted browser environment
    /// nobody works at shares its windows with the workspace's members the moment they open, since
    /// the whole point of it is that they watch and drive it.
    fn share_environment_windows(&mut self, compositor: &Compositor, tree: &DesktopTree) {
        if !self.share_by_default {
            return;
        }
        let Some(workspace) = tree
            .workspaces
            .iter()
            .map(|held| held.policy.workspace.clone())
            .find(|workspace| !workspace.is_empty())
        else {
            return;
        };
        let mut shares = Vec::new();
        for (identifier, app_id, _) in compositor.shareable_windows() {
            let Some(app_id) = app_id else {
                continue;
            };
            let Some(tab) = tree
                .workspaces
                .iter()
                .flat_map(|held| held.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .find(|tab| {
                    matches!(
                        &tab.tile,
                        TileContent::Foreign { app_id: tile, .. } if *tile == app_id
                    )
                })
                .map(|tab| tab.id)
            else {
                continue;
            };
            if self.hosted.contains_key(&tab)
                || self.pages.contains_key(&tab)
                || self.starting.iter().any(|start| start.tab == tab)
            {
                continue;
            }
            shares.push(ShareCommand::Share {
                window: identifier,
                tab,
                app_id,
                workspace: workspace.clone(),
            });
        }
        for share in shares {
            self.queue(share);
        }
    }

    /// Show every stream whose window the desk holds a tab for now, so its tile decodes it there: a
    /// stream that reached this device before the tab was made has waited for one.
    fn show_streams(&mut self, desk: &DesktopHandle) {
        for ((peer, surface), stream) in &mut self.received {
            if stream.shown {
                continue;
            }
            if desk.show_peer_surface(&stream.start).is_some() {
                stream.shown = true;
            } else {
                tracing::debug!(
                    peer,
                    surface,
                    tab = stream.tab,
                    "a shared window waits for the tab it is shown in"
                );
            }
        }
    }

    /// Send back what the tiles ask of the devices that share their windows: a viewer's pointer and
    /// keys, and the keyframe request of a stream that lost a unit.
    fn send_outgoing(&self, desk: &DesktopHandle, sync: &Client) {
        for msg in desk.take_peer_outgoing() {
            let surface = match &msg {
                NeighbourMsg::RemoteInput { surface, .. }
                | NeighbourMsg::SurfaceKeyframeRequest { surface, .. } => *surface,
                _ => continue,
            };
            let Some(stream) = self
                .received
                .values()
                .find(|stream| stream.start.surface == surface)
            else {
                continue;
            };
            // Where the stream came from is where its input goes: the device hosting the window,
            // over the topic of its workspace.
            sync.publish_control(stream.ws, msg, stream.host);
        }
    }

    /// The tile of the stream shown in `tab` goes, and its window's tab shows what this desktop
    /// holds for it again.
    fn end_tab(&mut self, desk: &DesktopHandle, tab: TabId, why: &str) {
        let Some(key) = self
            .received
            .iter()
            .find(|(_, stream)| stream.tab == tab)
            .map(|(key, _)| *key)
        else {
            return;
        };
        self.received.remove(&key);
        desk.end_peer_surface(key.0, key.1, why);
    }

    /// A member `peer` of `workspace` said which of this device's windows it shows: it views those,
    /// read only until the person lets it control them, and stops viewing the ones it no longer
    /// names.
    fn note_views(
        &mut self,
        sync: &Client,
        ws: WorkspaceId,
        (peer, device): (PeerId, EndpointId),
        workspace: &str,
        views: &[SurfaceView],
        decode: &[SurfaceDecodeLimit],
    ) -> bool {
        // The ask is made in a workspace whose topic it arrives on, and names windows and decoders
        // the wire allows.
        if !tilo_relay_protocol::room_workspace_id_matches(workspace, ws)
            || views.len() > MAX_SURFACE_VIEWS
            || decode.len() > MAX_SURFACE_VIEW_DECODERS
            || views.iter().any(|view| {
                !(1..=MAX_SURFACE_VIEW_SIDE).contains(&view.width)
                    || !(1..=MAX_SURFACE_VIEW_SIDE).contains(&view.height)
            })
            || sync.workspace_topic(workspace) != Some(ws)
        {
            return false;
        }
        let now = Instant::now();
        let mut changed = false;
        let named: HashSet<TabId> = views
            .iter()
            .map(|view| view.tab)
            .filter(|tab| self.hosted.contains_key(tab) || self.pages.contains_key(tab))
            .collect();
        for tab in &named {
            if let Some(asked) = self.asked.get_mut(&(*tab, peer)) {
                asked.at = now;
                asked.device = device;
                if asked.decode != decode {
                    asked.decode = decode.to_vec();
                    changed = true;
                }
            } else {
                self.asked.insert(
                    (*tab, peer),
                    Asked {
                        device,
                        at: now,
                        decode: decode.to_vec(),
                    },
                );
                changed = true;
            }
        }
        for tab in self.hosted.keys().copied() {
            let asked = self.asked.contains_key(&(tab, peer));
            let wanted = named.contains(&tab);
            if asked && !wanted {
                self.asked.remove(&(tab, peer));
                changed = true;
            }
        }
        // A viewer that came or whose decoders changed has its stream encoded for what it takes.
        for tab in &named {
            if self
                .hosted
                .get(tab)
                .is_some_and(|hosted| hosted.receivers != self.receivers_for(*tab))
            {
                self.renegotiate.insert(*tab);
            }
        }
        // Each share streams to exactly the members that named it, and to no one else.
        for (tab, hosted) in &self.hosted {
            let desired: HashSet<PeerId> = self
                .asked
                .keys()
                .filter(|(asking, _)| asking == tab)
                .map(|(_, viewing)| *viewing)
                .collect();
            let before = hosted.share.viewer_ids();
            let read_only: HashSet<PeerId> = desired
                .iter()
                .copied()
                .filter(|peer| !hosted.controlled.contains(peer))
                .collect();
            hosted.share.sync_viewers_read_only(&desired, &read_only);
            changed |= before != hosted.share.viewer_ids();
        }
        changed
    }

    /// A member's input for the window of `surface` : it reaches the window only while the share
    /// lets that member control it and the compositor's firewall lets remote input through at all.
    fn apply_input(
        &mut self,
        compositor: &mut Compositor,
        peer: PeerId,
        surface: SurfaceId,
        event: &[u8],
    ) {
        // A viewer's pointer is in the pixels of the pictures it is sent, which a viewer's decoder
        // may have made smaller than the window.
        let Some((window, size)) = self.hosted.get(&surface).and_then(|hosted| {
            (hosted.open || hosted.controlled.contains(&peer)).then(|| {
                let picture = hosted.share.picture();
                (hosted.window.clone(), (picture.width, picture.height))
            })
        }) else {
            return;
        };
        let Ok(event) = postcard::from_bytes::<SharedInput>(event) else {
            tracing::debug!(peer, surface, "dropping malformed remote input");
            return;
        };
        // The person's cut, their own hand on this machine, and a locked session hold a viewer back
        // like any input from another device.
        if let Some(refusal) = compositor.remote_input_refusal() {
            tracing::debug!(refusal, peer, "remote input refused");
            return;
        }
        let Some(seat) = self.seat_for(compositor, peer, surface, &window) else {
            return;
        };
        let delivered = match &event {
            SharedInput::PointerMotion { x, y } => {
                match remote_point(compositor, &window, size, *x, *y) {
                    Some((x, y)) => compositor
                        .seat_input(seat, &InputEvent::PointerMotion { x, y })
                        .map(drop),
                    None => Ok(()),
                }
            }
            SharedInput::PointerButton { button, pressed } => compositor
                .seat_input(
                    seat,
                    &InputEvent::PointerButton {
                        button: *button,
                        pressed: *pressed,
                    },
                )
                .map(drop),
            SharedInput::PointerAxis { dx, dy } => compositor
                .seat_input(
                    seat,
                    &InputEvent::PointerAxis {
                        horizontal: *dx,
                        vertical: *dy,
                    },
                )
                .map(drop),
            SharedInput::Key {
                keysym, pressed, ..
            } => compositor
                .seat_input(
                    seat,
                    &InputEvent::Key {
                        keycode: *keysym,
                        pressed: *pressed,
                    },
                )
                .map(drop),
            // What the viewer composed arrives as text, into the field the window has focused, as
            // this seat's own input method writes it.
            SharedInput::Text { text } => compositor.seat_text(seat, text).map(drop),
            // What the viewer pasted becomes this desktop's clipboard, so the window's own paste
            // reads it.
            SharedInput::ClipboardText { text } => {
                let offer = [
                    (
                        "text/plain;charset=utf-8".to_owned(),
                        text.clone().into_bytes(),
                    ),
                    ("text/plain".to_owned(), text.clone().into_bytes()),
                ]
                .into_iter()
                .collect();
                compositor.set_selection(Selection::Clipboard, offer)
            }
            // The viewer closed its view: the window is asked to close, as a person's close of its
            // tab does.
            SharedInput::Close => {
                compositor.seat_close_window(seat);
                Ok(())
            }
            // Where a viewer's pointer is and which surface it is on is the seat's own: it acts in
            // the window it watches, and no other.
            SharedInput::PointerEnter { .. }
            | SharedInput::PointerLeave { .. }
            | SharedInput::KeyboardFocus { .. } => Ok(()),
        };
        if let Err(error) = delivered {
            tracing::warn!(%error, "remote input did not reach the window");
        }
    }

    /// The seat `peer` acts at in the window of `surface` , opened the first time it controls it.
    fn seat_for(
        &mut self,
        compositor: &mut Compositor,
        peer: PeerId,
        surface: SurfaceId,
        window: &str,
    ) -> Option<tilo_compositor::Seat> {
        if let Some(seat) = self.seats.get(&(peer, surface))
            && compositor.seat_view(*seat).is_some()
        {
            return Some(*seat);
        }
        let seat = compositor.open_seat();
        if let Err(error) = compositor.seat_aim_window(seat, window) {
            tracing::warn!(%error, peer, "a viewer's seat found no window to act in");
            compositor.close_seat(seat);
            return None;
        }
        self.seats.insert((peer, surface), seat);
        Some(seat)
    }

    /// Close the seats acting in the window of `tab` : all of them, or the one `peer` acts at.
    fn close_seats(&mut self, compositor: &mut Compositor, tab: TabId, peer: Option<PeerId>) {
        let ended: Vec<tilo_compositor::Seat> = self
            .seats
            .iter()
            .filter(|((viewer, surface), _)| {
                *surface == tab && peer.is_none_or(|peer| *viewer == peer)
            })
            .map(|(_, seat)| *seat)
            .collect();
        if ended.is_empty() {
            return;
        }
        self.seats.retain(|(viewer, surface), _| {
            *surface != tab || peer.is_some_and(|peer| *viewer != peer)
        });
        for seat in ended {
            compositor.close_seat(seat);
        }
    }

    /// Say again which windows of which hosts this device shows, so their hosts keep streaming to
    /// it.
    fn ask_again(&mut self, compositor: &Compositor, sync: &Client, now: Instant) {
        /// What one host is asked for: the views of one workspace.
        struct Ask {
            host: EndpointId,
            workspace: String,
            views: Vec<TabId>,
            due: bool,
        }
        if self.watching.is_empty() {
            return;
        }
        // Who watches which window of this device: a viewer that stopped asking stops viewing.
        let expired: Vec<(TabId, PeerId)> = self
            .asked
            .iter()
            .filter(|(_, asked)| now.duration_since(asked.at) > VIEW_EXPIRY)
            .map(|(key, _)| *key)
            .collect();
        if !expired.is_empty() {
            for key in &expired {
                self.asked.remove(key);
            }
            for (tab, hosted) in &self.hosted {
                let desired: HashSet<PeerId> = self
                    .asked
                    .keys()
                    .filter(|(asking, _)| asking == tab)
                    .map(|(_, viewing)| *viewing)
                    .collect();
                hosted.share.sync_viewers(&desired);
            }
            self.changed();
        }
        let (width, height) = view_size(compositor);
        let decode = local_decode();
        // Every window of one host in one workspace goes in a single ask: an ask says what this
        // device wants of that host, so one naming a single window would take back the others.
        let mut asks: Vec<Ask> = Vec::new();
        for ((workspace, tab), watched) in &self.watching {
            let due = now.duration_since(watched.asked_at) >= VIEW_AGAIN;
            match asks
                .iter_mut()
                .find(|ask| ask.host == watched.host && &ask.workspace == workspace)
            {
                Some(ask) => {
                    ask.views.push(*tab);
                    ask.due |= due;
                }
                None => asks.push(Ask {
                    host: watched.host,
                    workspace: workspace.clone(),
                    views: vec![*tab],
                    due,
                }),
            }
        }
        for Ask {
            host,
            workspace,
            views,
            due,
        } in asks
        {
            let Some(ws) = sync.workspace_topic(&workspace).filter(|_| due) else {
                continue;
            };
            let ask = NeighbourMsg::SurfaceViewRequest {
                peer: sync.peer(),
                room: workspace.clone(),
                views: views
                    .iter()
                    .map(|tab| SurfaceView {
                        tab: *tab,
                        width,
                        height,
                    })
                    .collect(),
                decode: decode.clone(),
            };
            sync.publish_control(ws, ask, host);
            for tab in views {
                if let Some(watched) = self.watching.get_mut(&(workspace.clone(), tab)) {
                    watched.asked_at = now;
                }
            }
        }
    }

    /// Announce the windows whose page this session publishes to the members showing them, and tell
    /// the ones that stopped showing it. Nothing of a page is captured or encoded here: its app
    /// draws it, and the plane carries it.
    fn announce_pages(&mut self, sync: &Client) {
        let tabs: Vec<TabId> = self.pages.keys().copied().collect();
        for tab in tabs {
            let viewers: BTreeMap<PeerId, EndpointId> = self
                .asked
                .iter()
                .filter(|((asking, _), _)| *asking == tab)
                .map(|((_, peer), asked)| (*peer, asked.device))
                .collect();
            let Some(page) = self.pages.get(&tab) else {
                continue;
            };
            let ws = page.ws;
            let start = page.stream_start_msg(sync.peer());
            let end = page.stream_end_msg(sync.peer());
            let mut announced = page.announced.clone();
            let dropped: Vec<(PeerId, EndpointId)> = announced
                .iter()
                .map(|(peer, device)| (*peer, *device))
                .filter(|(peer, _)| !viewers.contains_key(peer))
                .collect();
            for (peer, device) in dropped {
                announced.remove(&peer);
                sync.publish_control(ws, end.clone(), device);
            }
            for (peer, device) in &viewers {
                if announced.insert(*peer, *device).is_none() {
                    sync.publish_control(ws, start.clone(), *device);
                }
            }
            if let Some(page) = self.pages.get_mut(&tab) {
                page.announced = announced;
            }
        }
    }

    /// Capture the shared windows a member's desk shows that are due, and publish what they
    /// encoded on their workspace's plane. A window nobody shows is neither captured nor encoded,
    /// and a member that starts showing one starts at a keyframe.
    fn capture(&mut self, compositor: &mut Compositor, sync: &Client, now: Instant) {
        if !self.pages.is_empty() {
            self.announce_pages(sync);
        }
        if self.hosted.is_empty() {
            return;
        }
        let planes = crate::calls::planes_of(sync);
        // The windows each plane's members show, and those a viewer needs a keyframe of: one that
        // started showing it, or one whose group could not take another picture.
        let mut wanted = BTreeSet::new();
        let mut keyframes = BTreeSet::new();
        for (workspace, plane) in &planes {
            for tab in plane.wanted_windows() {
                wanted.insert((workspace.clone(), tab));
            }
            for tab in plane.take_window_keyframes() {
                keyframes.insert((workspace.clone(), tab));
            }
        }
        let tabs: Vec<TabId> = self.hosted.keys().copied().collect();
        for tab in tabs {
            let Some(hosted) = self.hosted.get_mut(&tab) else {
                continue;
            };
            let key = (hosted.workspace.clone(), tab);
            let Some(plane) = planes
                .get(&hosted.workspace)
                .filter(|_| wanted.contains(&key))
            else {
                hosted.shown = false;
                continue;
            };
            if !hosted.shown || keyframes.contains(&key) {
                // A still window sends its new viewer the keyframe it stands on; otherwise the next
                // frame is one, captured now.
                if let Some(standing) = hosted.share.start_reader()
                    && let Err(error) = send_picture(plane, tab, &hosted.share, standing)
                {
                    tracing::warn!(%error, tab, "a shared window's standing keyframe did not go out");
                }
                hosted.captured_at = None;
            }
            hosted.shown = true;
            let rate = hosted.share.rate().max(1);
            let due = hosted.captured_at.is_none_or(|at| {
                now.duration_since(at) >= Duration::from_secs_f64(1.0 / f64::from(rate))
            });
            if due {
                let target = Target::window(hosted.window.clone());
                match compositor.read_back(&target) {
                    Ok(picture) => {
                        if (picture.width, picture.height) != hosted.share.dimensions() {
                            // The window was resized: its stream restarts at the size it shows now.
                            self.restart(tab);
                            continue;
                        }
                        hosted.share.deposit_captured_rgba(picture.rgba);
                        hosted.captured_at = Some(now);
                    }
                    // A locked or asleep screen sends nothing, and the share goes on when it wakes.
                    Err(error) => {
                        // ignored-failure: each pass captures the windows it can
                        tracing::debug!(%error, window = %hosted.window, "a shared window is not captured this pass");
                    }
                }
            }
            for frame in hosted.share.pump(now) {
                let seq = frame.seq;
                if let Err(error) = send_picture(plane, tab, &hosted.share, frame) {
                    // The window's group is past what one carries, or the picture past what an
                    // object does: the next frame starts it over at a keyframe.
                    // ignored-failure: the keyframe asked for here starts the viewers over
                    tracing::debug!(%error, tab, "a shared window's picture did not go out");
                    hosted.share.request_keyframe(seq, now);
                }
            }
        }
    }

    /// Start `tab` 's share again, at the size its window shows now: its first picture is a keyframe
    /// of that size, which its viewers' tiles start over at. The restarted share starts on the next
    /// pass, as any share does.
    fn restart(&mut self, tab: TabId) {
        let Some(hosted) = self.hosted.remove(&tab) else {
            return;
        };
        self.starting.push(Starting {
            window: hosted.window,
            tab,
            app_id: hosted.app_id,
            workspace: hosted.workspace,
            waiting_for: String::new(),
            controlled: hosted.controlled,
        });
        self.changed();
    }
}

/// The page of a window whose app draws it: the members showing it read the workspace's live
/// plane, so no picture of it is captured or encoded here.
fn page_of(compositor: &Compositor, start: &Starting, ws: WorkspaceId) -> Paged {
    let (_, _, width, height) = compositor
        .shareable_window_rect(&start.window)
        .unwrap_or_default();
    Paged {
        tab: start.tab,
        window: start.window.clone(),
        app_id: start.app_id.clone(),
        workspace: start.workspace.clone(),
        ws,
        width: width.unsigned_abs(),
        height: height.unsigned_abs(),
        announced: BTreeMap::new(),
    }
}

/// The tracks this device shows of other members' windows, by workspace and the device hosting
/// them: of each window the desk holds (`on_desk`) and watches, its page where its host announced
/// one, and its pictures otherwise. A tab the desk stopped holding, or a watch that ended, takes
/// its track with it.
fn tracks_to_show(
    received: &BTreeMap<(PeerId, SurfaceId), Received>,
    watching: &BTreeMap<(String, TabId), Watched>,
    on_desk: impl Fn(TabId) -> bool,
) -> BTreeMap<String, BTreeMap<EndpointId, BTreeSet<Track>>> {
    let mut wanted: BTreeMap<String, BTreeMap<EndpointId, BTreeSet<Track>>> = BTreeMap::new();
    for ((workspace, tab), watched) in watching {
        if !on_desk(*tab) {
            continue;
        }
        let page = received
            .values()
            .any(|stream| stream.page && stream.tab == *tab && stream.host == watched.host);
        wanted
            .entry(workspace.clone())
            .or_default()
            .entry(watched.host)
            .or_default()
            .insert(if page {
                Track::Page(*tab)
            } else {
                Track::Window(*tab, Layer::High)
            });
    }
    wanted
}

/// Send one encoded picture of the window `tab` on `plane`, as its share describes it.
fn send_picture(
    plane: &Plane,
    tab: TabId,
    share: &SharedFlatpakSession,
    frame: EncodedFrame,
) -> Result<(), tilo_live::plane::PlaneError> {
    let picture = share.picture();
    let side = |side: u32| u16::try_from(side).unwrap_or(tilo_live::video::MAX_SIDE);
    let object = VideoObject {
        keyframe: frame.keyframe,
        hdr: picture.colorimetry.is_hdr(),
        codec: video_codec(picture.codec),
        layer: Layer::High,
        sequence: frame.seq,
        timestamp_us: share.captured_at_us(&frame),
        width: side(picture.width),
        height: side(picture.height),
        payload: frame.nalus,
    };
    plane.send_window(tab, &object)
}

/// The `tilo-video/1` codec of a share's stream.
const fn video_codec(codec: StreamCodec) -> tilo_live::video::Codec {
    match codec {
        StreamCodec::H264 => tilo_live::video::Codec::H264,
        StreamCodec::HevcMain10 => tilo_live::video::Codec::HevcMain10,
        StreamCodec::Vp9 => tilo_live::video::Codec::Vp9,
        StreamCodec::Av1 => tilo_live::video::Codec::Av1,
    }
}

/// The stream a shared window's keyframe `object` starts, as the tile in `tab` decodes it: `None`
/// for a codec no decoder here takes.
fn picture_start(
    tree: &DesktopTree,
    local_peer: PeerId,
    peer: PeerId,
    tab: TabId,
    object: &VideoObject,
) -> Option<PeerStreamStart> {
    let codec = match object.codec {
        tilo_live::video::Codec::H264 => StreamCodec::H264,
        tilo_live::video::Codec::HevcMain10 => StreamCodec::HevcMain10,
        tilo_live::video::Codec::Vp9 => StreamCodec::Vp9,
        tilo_live::video::Codec::Av1 => StreamCodec::Av1,
        tilo_live::video::Codec::Vp8 => return None,
    };
    let (title, sharer) = tab_title(tree, tab);
    let app_id = hosted_window(tree, tab)
        .map(|(_, app_id)| app_id)
        .unwrap_or_default();
    let colorimetry = if object.hdr {
        neighbour_wire::colorimetry::Colorimetry::hdr10()
    } else {
        neighbour_wire::colorimetry::Colorimetry::sdr_bt709()
    };
    Some(PeerStreamStart {
        surface: tab,
        peer,
        local_peer,
        width: u32::from(object.width),
        height: u32::from(object.height),
        codec: codec.wire_label().to_owned(),
        bit_depth: if object.hdr { 10 } else { 8 },
        colorimetry: Some(colorimetry),
        hdr: None,
        title: if title.trim().is_empty() {
            app_id.clone()
        } else {
            title
        },
        app_id,
        tab,
        sharer,
        // Whether the sender lets its viewer type is the sender's own rule, so this device offers
        // its pointer and keys and the sender decides whether they land.
        writable: true,
    })
}

/// The limits a viewer reported, by codec: codecs this build does not know and codecs without a
/// limit carry none.
fn decode_limits(decode: &[SurfaceDecodeLimit]) -> BTreeMap<StreamCodec, (u32, u32)> {
    decode
        .iter()
        .filter_map(|entry| Some((StreamCodec::from_wire_label(&entry.codec)?, entry.max?)))
        .collect()
}

/// What this device's hardware decoders take, as a viewer tells a host.
fn local_decode() -> Vec<SurfaceDecodeLimit> {
    let caps = media::realtime::local_capabilities();
    caps.hardware_decode
        .iter()
        .take(MAX_SURFACE_VIEW_DECODERS)
        .map(|codec| SurfaceDecodeLimit {
            codec: codec.wire_label().to_owned(),
            max: caps.hardware_decode_max.get(codec).copied(),
        })
        .collect()
}

/// The picture this device shows a window another member shares at: the largest monitor it has on,
/// within what the wire allows.
fn view_size(compositor: &Compositor) -> (u32, u32) {
    let side = |value: i32| {
        u32::try_from(value)
            .unwrap_or(MAX_SURFACE_VIEW_SIDE)
            .clamp(1, MAX_SURFACE_VIEW_SIDE)
    };
    compositor
        .outputs()
        .into_iter()
        .filter(|output| output.enabled)
        .filter_map(|output| output.rect)
        .map(|(_, _, width, height)| (side(width), side(height)))
        .max_by_key(|(width, height)| u64::from(*width).saturating_mul(u64::from(*height)))
        .unwrap_or((1920, 1080))
}

/// What the desk calls the window shown in `tab` , and the device it says hosts it: empty when the
/// desk shows no such tab.
fn tab_title(tree: &DesktopTree, tab: TabId) -> (String, String) {
    shown_tab(tree, tab)
        .map(|shown| {
            let sharer = match &shown.tile {
                TileContent::HostedWindow { host_label, .. } => host_label.clone(),
                _ => String::new(),
            };
            (shown.title.clone(), sharer)
        })
        .unwrap_or_default()
}

/// Point every shared window's tile at the colour of the display it stands on, so its pictures are
/// prepared for that panel, and follow it when the window moves to another display.
fn land_displays(desk: &DesktopHandle, displays: &crate::displays::Displays) {
    for tab in desk.peer_surface_tabs() {
        let monitor = desk.peer_surface_display(tab);
        desk.set_peer_display(tab, displays.colour(monitor.as_deref()));
    }
}

/// The tab `tab` as the desk shows it.
fn shown_tab(tree: &DesktopTree, tab: TabId) -> Option<&Tab> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|shown| shown.id == tab)
}

/// The window of another device shown in `tab` : its host's key and app.
fn hosted_window(tree: &DesktopTree, tab: TabId) -> Option<(String, String)> {
    shown_tab(tree, tab).and_then(|shown| match &shown.tile {
        TileContent::HostedWindow { host, app_id, .. } => Some((host.clone(), app_id.clone())),
        _ => None,
    })
}

/// The device a window is hosted by, as the key that hosts it: what the desktop model names a
/// hosted window's device by.
fn device_key(host: &str) -> Option<EndpointId> {
    hex::decode(host)
        .ok() // ignored-failure: a host that is no device key hosts no window
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .and_then(|bytes| EndpointId::from_bytes(&bytes).ok()) // ignored-failure: 32 bytes that are no endpoint name no device to host a window with
}

/// Whether the desk shows the tab `tab` anywhere.
fn shown(tree: &DesktopTree, tab: TabId) -> bool {
    shown_tab(tree, tab).is_some()
}

/// The devices a workspace's windows are hosted on, by the salt each mints its ids under and its
/// key: this device under `salt` , and each of `devices` (the ones heard in the workspace) under
/// the salt its key derives. The desktop model finds a shared window's host with it.
fn owners_of(
    salt: u16,
    publisher: EndpointId,
    devices: &[(EndpointId, String)],
) -> BTreeMap<u16, String> {
    let mut owners = BTreeMap::from([(salt, hex::encode(publisher.as_bytes()))]);
    for (device, _) in devices {
        owners.insert(
            tilo_desktop_model::ids::device_salt_of_key(device.as_bytes()),
            hex::encode(device.as_bytes()),
        );
    }
    owners
}

/// The desk without the tab `tab` , and without the strip `strip` when that was made for it and
/// holds nothing else.
fn remove_governed_tile(tree: &mut DesktopTree, tab: TabId, strip: Option<StripId>) {
    for workspace in &mut tree.workspaces {
        for held in &mut workspace.strips {
            held.window_groups.retain_mut(|group| {
                group.tabs.retain(|shown| shown.id != tab);
                group.focus_history.retain(|id| *id != tab);
                if group.active_tab == tab
                    && let Some(first) = group.tabs.first()
                {
                    group.active_tab = first.id;
                }
                !group.tabs.is_empty()
            });
        }
        if let Some(strip) = strip {
            workspace
                .strips
                .retain(|held| held.id != strip || !held.window_groups.is_empty());
        }
    }
}

/// A viewer's pointer, in this device's terms: the point on the picture it watches, in the layout
/// the window stands at.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the wire carries input offsets as surface pixels, which are integers"
)]
fn remote_point(
    compositor: &Compositor,
    window: &str,
    size: (u32, u32),
    x: f64,
    y: f64,
) -> Option<(f64, f64)> {
    let (left, top, width, height) = compositor.shareable_window_rect(window)?;
    let scale_x = f64::from(width.max(1)) / f64::from(size.0.max(1));
    let scale_y = f64::from(height.max(1)) / f64::from(size.1.max(1));
    Some((f64::from(left) + x * scale_x, f64::from(top) + y * scale_y))
}

/// The session's resource: the shares as they stand, and the actions that change them.
pub(crate) struct WindowShareService {
    shares: Rc<RefCell<WindowShares>>,
    /// The revision the resource was last read at.
    seen: u64,
}

impl WindowShareService {
    pub(crate) const fn new(shares: Rc<RefCell<WindowShares>>) -> Self {
        Self { shares, seen: 0 }
    }
}

impl Service for WindowShareService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![shares_resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == SHARES_RESOURCE).then(|| self.shares.borrow().reading())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != SHARES_RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let address = ActionAddress::new(resource, action);
        let encoded = tilo_actions::door::encode_json(&[shares_resource()], &address, arguments)?;
        let command = (encoded.type_id == OPERATION)
            .then(|| serde_json::from_slice::<ShareCommand>(&encoded.payload).ok())
            .flatten()
            .ok_or_else(|| DoorError::Failed("not a window-share command".into()))?;
        self.shares.borrow_mut().queue(command);
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let revision = self.shares.borrow().revision;
        let changed = revision != self.seen;
        self.seen = revision;
        changed
    }
}

/// `id` 's action, carrying a [`ShareCommand`].
#[expect(
    clippy::expect_used,
    reason = "a share command always serializes to JSON"
)]
fn action(
    id: &'static str,
    label: String,
    params: Vec<ActionParam>,
    build: impl Fn(&tilo_actions::ActionArgs) -> ShareCommand + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label,
        String::new(),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| {
            EncodedAction::encode_operation(OPERATION, &build(args))
                .expect("a share command always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

/// The window shares' resource and its actions.
fn shares_resource() -> ResourceSpec {
    ResourceSpec::new(SHARES_RESOURCE, "session", t("session-window-share"))
        .with_action(share_action())
        .with_action(stop_action())
        .with_action(allow_input_action())
        .with_action(watch_action())
        .with_action(unwatch_action())
}

/// The tab argument: the window of the desk a share is about, named by its id as the desk's own
/// resources name it. Ids carry a device's salt and do not fit a JSON integer, so they travel as
/// text.
fn tab_argument() -> ActionParam {
    ActionParam::required_owned("tab", ParamType::Text, t("session-window-share-param-tab"))
}

/// The workspace argument: the workspace a window is shared with or watched in. It is named `room`
/// on the wire, the key the actions have always taken.
fn workspace_argument() -> ActionParam {
    ActionParam::required_owned(
        "room",
        ParamType::Text,
        t("session-window-share-param-workspace"),
    )
}

/// The tab an action names, by its decimal id.
fn tab_of(args: &tilo_actions::ActionArgs) -> TabId {
    args.text("tab")
        // ignored-failure: text that is no tab id names no tab
        .and_then(|tab| tab.trim().parse().ok())
        .unwrap_or(0)
}

/// The text argument `name` , or nothing.
fn text_of(args: &tilo_actions::ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().to_owned()
}

/// Share the window `window` of `app_id` as `tab` of `workspace` .
fn share_action() -> ActionSpec {
    action(
        "share",
        t("session-window-share-share"),
        vec![
            ActionParam::required_owned(
                "window",
                ParamType::Text,
                t("session-window-share-param-window"),
            ),
            tab_argument(),
            ActionParam::required_owned(
                "app_id",
                ParamType::Text,
                t("session-window-share-param-app"),
            ),
            workspace_argument(),
        ],
        |args| ShareCommand::Share {
            window: text_of(args, "window"),
            tab: tab_of(args),
            app_id: text_of(args, "app_id"),
            workspace: text_of(args, "room"),
        },
    )
}

/// Stop sharing `tab` .
fn stop_action() -> ActionSpec {
    action(
        "stop",
        t("session-window-share-stop"),
        vec![tab_argument()],
        |args| ShareCommand::Stop { tab: tab_of(args) },
    )
}

/// Let the members viewing `tab` control it, or stop them.
fn allow_input_action() -> ActionSpec {
    action(
        "allow-remote-input",
        t("session-window-share-allow-input"),
        vec![
            tab_argument(),
            ActionParam::optional_owned(
                "on",
                ParamType::Boolean,
                t("session-window-share-param-on"),
            ),
        ],
        |args| ShareCommand::AllowInput {
            tab: tab_of(args),
            on: args.boolean("on"),
        },
    )
}

/// Show `tab` , another member's window, from `workspace` .
fn watch_action() -> ActionSpec {
    action(
        "watch",
        t("session-window-share-watch"),
        vec![tab_argument(), workspace_argument()],
        |args| ShareCommand::Watch {
            tab: tab_of(args),
            workspace: text_of(args, "room"),
        },
    )
}

/// Stop showing `tab` .
fn unwatch_action() -> ActionSpec {
    action(
        "unwatch",
        t("session-window-share-unwatch"),
        vec![tab_argument(), workspace_argument()],
        |args| ShareCommand::Unwatch {
            tab: tab_of(args),
            workspace: text_of(args, "room"),
        },
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(test)]
mod window_tracks {
    use super::*;

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const TAB: TabId = 0x5EED_0000_0000_0007;

    fn host(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// The announcement of a window's page stream, as its host sends it.
    fn announcement(tab: TabId, codec: &str) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamStart {
            surface: tab,
            source_peer: neighbour_wire::peer_handle(&[9; 32]),
            width: 800,
            height: 600,
            codec: codec.to_owned(),
            ts: 0,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 8,
            app_id: "so.tilo.browser".to_owned(),
            tab,
        }
    }

    fn received(tab: TabId, codec: &str, host: EndpointId) -> Received {
        let msg = announcement(tab, codec);
        Received {
            tab,
            host,
            ws: tilo_network_actor::workspace_topic_id(
                tilo_desktop_model::WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
            ),
            shown: true,
            start: PeerStreamStart::announced(&msg, 11, "Laptop", "Browser", false)
                .expect("a stream start"),
            page: codec == ui::webview::paint::stream::wire::CODEC,
        }
    }

    fn watching() -> BTreeMap<(String, TabId), Watched> {
        let mut watching = BTreeMap::new();
        watching.insert(
            (WORKSPACE.to_owned(), TAB),
            Watched {
                host: host(5),
                asked_at: Instant::now(),
            },
        );
        watching
    }

    #[test]
    fn a_pages_tab_is_asked_for_while_the_desk_holds_it_and_watched() {
        let mut streams = BTreeMap::new();
        streams.insert(
            (neighbour_wire::peer_handle(&[9; 32]), TAB),
            received(TAB, ui::webview::paint::stream::wire::CODEC, host(5)),
        );
        let wanted = tracks_to_show(&streams, &watching(), |tab| tab == TAB);
        assert_eq!(
            wanted.get(WORKSPACE).and_then(|hosts| hosts.get(&host(5))),
            Some(&BTreeSet::from([Track::Page(TAB)])),
            "the plane is asked for that window's page"
        );
    }

    #[test]
    fn a_window_the_desk_stops_showing_or_watching_is_no_longer_asked_for() {
        let mut streams = BTreeMap::new();
        streams.insert(
            (neighbour_wire::peer_handle(&[9; 32]), TAB),
            received(TAB, ui::webview::paint::stream::wire::CODEC, host(5)),
        );
        assert!(
            tracks_to_show(&streams, &watching(), |_| false).is_empty(),
            "the tab left the desk"
        );
        assert!(
            tracks_to_show(&streams, &BTreeMap::new(), |tab| tab == TAB).is_empty(),
            "nothing watches that window here"
        );
    }

    #[test]
    fn a_watched_window_without_a_page_is_asked_for_by_its_pictures() {
        let wanted = tracks_to_show(&BTreeMap::new(), &watching(), |tab| tab == TAB);
        assert_eq!(
            wanted.get(WORKSPACE).and_then(|hosts| hosts.get(&host(5))),
            Some(&BTreeSet::from([Track::Window(TAB, Layer::High)])),
            "the plane is asked for that window's pictures"
        );
    }
}
