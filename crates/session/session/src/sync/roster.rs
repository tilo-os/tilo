//! Who is here with this person: the workspaces this session takes part
//! in, what its own presence says of them, and everyone else it heard on
//! each workspace's live plane, as their presence and their certificate
//! tell it.
//!
//! Nothing here is drawn. What this holds is what the session says and keeps;
//! the roster it feeds is the desktop's (`tilo_desktop::here`).

use collab_types::presence::{
    Availability, DESKTOP_APP, Payload, PointerSpace, Presence, PresenceKey,
};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_desktop::here::{Focus, LiveMember, Wave};
use tilo_network_actor::{NetEvent, WorkspaceId};

use crate::content;

/// What this session says of itself here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnPresence {
    /// The person's name, as everyone here sees it.
    pub label: String,
    /// The person (user root, hex) this device is certified as.
    pub user: Option<String>,
    /// What they say they are doing, in their own words.
    pub status: Option<String>,
    /// How available they say they are.
    pub availability: Availability,
    /// The voice channel they are in here, if any: what everyone else's
    /// roster says beside their face.
    pub call: Option<InCall>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
}

/// A device's call, as its presence carries it to everyone in the workspace:
/// the channel it is in, and what it sends of the person there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InCall {
    /// The channel's id and, once someone named it, its name.
    pub call: ui::CallPresence,
    /// This device's camera is on in the call.
    pub camera: bool,
    /// This device shares its screen in the call.
    pub screen: bool,
}

/// A call in progress in a workspace, as the people in it say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCall {
    /// The workspace the channel is in, as its id travels.
    pub workspace: String,
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when
    /// nobody named it.
    pub name: String,
    /// Who is in it, in the order they arrived.
    pub people: Vec<LiveCaller>,
}

/// Someone in a call, as their device's presence names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCaller {
    /// Their device's publisher.
    pub id: String,
    /// The name everyone here calls them: the person's.
    pub label: String,
    /// The person behind the device (user root, hex), once its certificate
    /// was heard.
    pub user: Option<String>,
    /// The device's camera is on in the call.
    pub camera: bool,
    /// The device shares its screen in the call.
    pub screen: bool,
}

impl LiveCall {
    /// `peer` is in this call, as their presence says: one more in it, and
    /// the name they give it when nobody named it first.
    fn join(&mut self, peer: &Peer, presence: &InCall) {
        if self.name.trim().is_empty() {
            self.name.clone_from(&presence.call.name);
        }
        self.people.push(LiveCaller {
            id: peer.publisher.to_string(),
            label: peer.label.clone(),
            user: peer.user.clone(),
            camera: presence.camera,
            screen: presence.screen,
        });
    }

    /// Whether `person` is in this call, on any of their devices: what a
    /// Join is not offered for.
    pub(super) fn joined_by(&self, person: Option<&str>) -> bool {
        let Some(person) = person else {
            return false;
        };
        self.people
            .iter()
            .any(|caller| caller.user.as_deref() == Some(person))
    }
}

/// Someone else here, as their presence and their certificate tell it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Peer {
    /// Their device's publisher: the endpoint id every workspace member names it
    /// by.
    pub(super) publisher: EndpointId,
    /// The person behind the device (user root, hex), once its certificate
    /// has been heard.
    pub(super) user: Option<String>,
    /// The name everyone here calls them, as their presence says it: the
    /// person's, never the device's.
    pub(super) label: String,
    /// What the device calls itself, from its certificate: what tells two
    /// devices of one person apart.
    pub(super) device: String,
    /// What they say they are doing, in their own words.
    pub(super) status: Option<String>,
    /// How available they say they are.
    pub(super) availability: Availability,
    /// Whose view they follow, by that member's publisher.
    pub(super) following: Option<String>,
    /// The last wave they sent at someone here.
    pub(super) wave: Option<Wave>,
    /// The call they are in, as their presence says: the channel's id,
    /// what it is called, and what their device sends there.
    pub(super) call: Option<InCall>,
    /// Where their pointer is: the tab of this workspace its tile shows, and the
    /// position on that tile in logical pixels from its corner.
    pub(super) pointer: Option<(u64, f64, f64)>,
    /// Their own view of their desktop, as a shell — or an agent, which says
    /// its face, where it works and what it is doing the same way — said it.
    pub(super) shell: Option<Box<collab_types::presence::ShellPresence>>,
    /// They said they are here and have not left: what makes them a member.
    pub(super) here: bool,
}

impl Peer {
    pub(super) fn new(publisher: EndpointId) -> Self {
        Self {
            publisher,
            user: None,
            label: String::new(),
            device: String::new(),
            status: None,
            availability: Availability::default(),
            following: None,
            wave: None,
            call: None,
            pointer: None,
            shell: None,
            here: false,
        }
    }

    /// What the desktop lists of them: an agent's face and the record it
    /// works in come from its shell frame, a person's from their presence
    /// and their own words. What they are called is the person's name; the
    /// device they say it from is named beside it.
    pub(super) fn member(&self) -> LiveMember {
        let shell = self.shell.as_deref();
        let agent = shell.is_some_and(|shell| shell.agent);
        let label = if self.label.trim().is_empty() {
            shell.map(|shell| shell.label.clone()).unwrap_or_default()
        } else {
            self.label.clone()
        };
        LiveMember {
            publisher: self.publisher.to_string(),
            kind: if agent { "agent" } else { "person" }.into(),
            label,
            device: self.device.clone(),
            focus: shell
                .and_then(collab_types::presence::ShellPresence::focus)
                .map(|(app, record)| Focus {
                    app: app.to_owned(),
                    record: hex::encode(record),
                }),
            activity: shell
                .and_then(|shell| shell.activity.clone())
                .or_else(|| self.status.clone()),
            place: shell.and_then(|shell| shell.place.clone()),
            user: self.user.clone(),
            wave: self.wave.clone(),
            layout: None,
            following: self.following.clone(),
            availability: (if agent {
                None
            } else {
                Some(self.availability.id().to_owned())
            }),
            call: self.call.as_ref().map(|call| call.call.encode()),
        }
    }
}

/// One workspace this session takes part in, and who it heard is here.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// The workspace's topic.
    pub(super) ws: WorkspaceId,
    /// The workspace, as its id travels: the uuid string every publish names.
    pub(super) id: String,
    /// Everyone heard here, in the order they arrived.
    pub(super) peers: Vec<Peer>,
    /// The devices a record's content of this workspace reaches this one
    /// from: those heard on its topic, and its hosts.
    pub(super) content: content::Topology,
    /// How many times what this workspace knows of its people changed: what a
    /// surface drawing them compares against the revision it drew, so a change
    /// draws one frame and a desk that holds still draws none.
    pub(super) revision: u64,
}

impl Workspace {
    /// Change what is known of `publisher`, starting their entry the first
    /// time they say anything.
    pub(super) fn with(&mut self, publisher: EndpointId, change: impl FnOnce(&mut Peer)) {
        if let Some(index) = self
            .peers
            .iter()
            .position(|peer| peer.publisher == publisher)
        {
            let changed = self.peers.get_mut(index).is_some_and(|peer| {
                let before = peer.clone();
                change(peer);
                *peer != before
            });
            if changed {
                self.touch();
            }
            return;
        }
        let mut peer = Peer::new(publisher);
        change(&mut peer);
        self.peers.push(peer);
        self.touch();
    }

    /// Record that what is known here changed.
    pub(super) fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// They are here: this and what they say of themselves.
    pub(super) fn hear(&mut self, from: EndpointId, presence: &Presence) {
        if presence.app != DESKTOP_APP {
            return;
        }
        match (&presence.key, &presence.payload) {
            // A pointer moves many times a second, so it is compared where it
            // is rather than by copying the whole entry.
            (
                PresenceKey::Pointer {
                    space: PointerSpace::Tile { workspace, tab },
                },
                Payload::Pointer { x, y },
            ) => {
                if *workspace == self.id {
                    self.point(from, *tab, (f64::from(*x), f64::from(*y)));
                }
            }
            (
                PresenceKey::Pointer {
                    space: PointerSpace::Tile { workspace, .. },
                },
                Payload::Left,
            ) => {
                if *workspace == self.id {
                    self.clear_point(from);
                }
            }
            (PresenceKey::Workspace { id }, payload) if *id == self.id => {
                self.with(from, |peer| match payload {
                    Payload::Arrived => peer.here = true,
                    Payload::Left => peer.here = false,
                    Payload::Status { text, .. } => {
                        peer.status = Some(text.trim().to_owned()).filter(|text| !text.is_empty());
                    }
                    Payload::Availability {
                        state,
                        call,
                        camera,
                        screen,
                    } => {
                        peer.availability =
                            Availability::from_id(state).unwrap_or(Availability::Available);
                        peer.call =
                            call.as_deref()
                                .and_then(ui::CallPresence::decode)
                                .map(|call| InCall {
                                    call,
                                    camera: *camera,
                                    screen: *screen,
                                });
                    }
                    Payload::Following { target } => {
                        peer.following.clone_from(target);
                    }
                    Payload::Wave { seq, target } => {
                        peer.wave = Some(Wave {
                            seq: *seq,
                            target: target.clone(),
                        });
                    }
                    // The person's own name for themselves: what everyone
                    // here calls them, on whichever device they say it.
                    Payload::Person { name } => {
                        name.trim().clone_into(&mut peer.label);
                    }
                    // A shell — a page's desktop, or an agent's face — says
                    // where its person is and, for an agent, that it works
                    // here at all.
                    Payload::Shell(shell) => {
                        peer.here = true;
                        peer.shell = Some(shell.clone());
                    }
                    _ => {}
                });
            }
            _ => {}
        }
    }

    /// Where their pointer moved on the tile of `tab` of this workspace. A
    /// pointer over a tile is the device saying it is here: it is drawn
    /// whether or not the word that first said so reached this device.
    pub(super) fn point(&mut self, from: EndpointId, tab: u64, point: (f64, f64)) {
        let now = Some((tab, point.0, point.1));
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.publisher == from) {
            let arrived = !peer.here;
            peer.here = true;
            if peer.pointer != now || arrived {
                peer.pointer = now;
                self.touch();
            }
            return;
        }
        let mut peer = Peer::new(from);
        peer.here = true;
        peer.pointer = now;
        self.peers.push(peer);
        self.touch();
    }

    /// Their pointer left the tile it was on.
    pub(super) fn clear_point(&mut self, from: EndpointId) {
        let cleared = self
            .peers
            .iter_mut()
            .find(|peer| peer.publisher == from)
            .is_some_and(|peer| peer.pointer.take().is_some());
        if cleared {
            self.touch();
        }
    }

    /// Their device's certificate arrived: it names the device and the
    /// person it belongs to. Only the sender's own, validly signed
    /// certificate says who is there: one naming another device, or signed by
    /// nobody, is refused, so nobody here can be named as somebody else.
    /// What everyone calls them is the person's own word
    /// ([`Payload::Person`]), never their device's name.
    pub(super) fn certified(&mut self, from: EndpointId, cert: &tilo_user_identity::DeviceCert) {
        if cert.device_public_key != *from.as_bytes() {
            tracing::warn!(
                peer = %from,
                device = %hex::encode(cert.device_public_key),
                "refusing a certificate that is not the sender's own device"
            );
            return;
        }
        if let Err(error) =
            tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
        {
            tracing::warn!(peer = %from, %error, "refusing a device certificate");
            return;
        }
        self.with(from, |peer| {
            peer.user = Some(hex::encode(cert.user_public_key));
            cert.device_label.trim().clone_into(&mut peer.device);
        });
    }

    /// They left the workspace.
    pub(super) fn depart(&mut self, publisher: EndpointId) {
        let before = self.peers.len();
        self.peers.retain(|peer| peer.publisher != publisher);
        if self.peers.len() != before {
            self.touch();
        }
    }

    /// The calls in progress here, as those in them say: the people of one
    /// channel under its id and name, in the order they arrived.
    pub(super) fn calls(&self) -> Vec<LiveCall> {
        let mut calls: Vec<LiveCall> = Vec::new();
        for peer in self.peers.iter().filter(|peer| peer.here) {
            let Some(presence) = &peer.call else {
                continue;
            };
            let at = calls
                .iter()
                .position(|known| known.channel == presence.call.channel);
            if let Some(at) = at {
                if let Some(call) = calls.get_mut(at) {
                    call.join(peer, presence);
                }
            } else {
                let mut call = LiveCall {
                    workspace: self.id.clone(),
                    channel: presence.call.channel.clone(),
                    name: presence.call.name.clone(),
                    people: Vec::new(),
                };
                call.join(peer, presence);
                calls.push(call);
            }
        }
        calls
    }

    /// Who is here, as the desktop lists them.
    pub(super) fn members(&self) -> Vec<LiveMember> {
        self.peers
            .iter()
            .filter(|peer| peer.here)
            .map(Peer::member)
            .collect()
    }

    /// Everyone whose pointer is over `tab` now, by publisher.
    pub(super) fn pointers(&self, tab: u64) -> Vec<(String, f64, f64)> {
        self.peers
            .iter()
            .filter(|peer| peer.here)
            .filter_map(|peer| match peer.pointer {
                Some((at, x, y)) if at == tab => Some((peer.publisher.to_string(), x, y)),
                _ => None,
            })
            .collect()
    }
}

/// The workspaces this session takes part in, and the one in front.
#[derive(Debug)]
pub(crate) struct Plane {
    /// This device's publisher.
    pub(super) publisher: EndpointId,
    pub(super) workspaces: Vec<Workspace>,
    /// The workspace in front: whose roster the desktop shows.
    pub(super) active: Option<WorkspaceId>,
}

impl Plane {
    pub(super) fn workspace(&mut self, ws: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.ws == ws)
    }

    /// Take one event from the plane.
    pub(super) fn apply(&mut self, event: NetEvent) {
        match event {
            NetEvent::Msg { ws, peer, msg, .. } => self.message(ws, peer, msg),
            NetEvent::PeerLeft { ws, peer, .. } => {
                if let Some(workspace) = self.workspace(ws) {
                    workspace.depart(peer);
                }
            }
            _ => {}
        }
    }

    /// Take one message a peer published on `ws`.
    pub(super) fn message(&mut self, ws: WorkspaceId, from: EndpointId, msg: NeighbourMsg) {
        if from == self.publisher {
            return;
        }
        if let NeighbourMsg::IdentityHello { cert, .. } = msg
            && let Some(workspace) = self.workspace(ws)
        {
            workspace.certified(from, &cert);
        }
    }

    /// How many times what the workspace in front knows of its people
    /// changed: a surface drawing that workspace draws again when this moved.
    pub(super) fn revision(&self) -> u64 {
        let active = self.active;
        self.workspaces
            .iter()
            .find(|workspace| Some(workspace.ws) == active)
            .map_or(0, |workspace| workspace.revision)
    }

    /// Everyone whose pointer is over `tab` of `workspace` now, by publisher, if
    /// this session takes part in that workspace.
    pub(super) fn pointers(&self, workspace: &str, tab: u64) -> Vec<(String, f64, f64)> {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| held.pointers(tab))
            .unwrap_or_default()
    }
}
