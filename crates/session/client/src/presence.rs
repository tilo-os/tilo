//! Who is here: the workspaces this session takes part in, the board of what each member said of
//! themselves there, and this device's own word on it. Every host drives the same board and says
//! what it owes on its own transport; nothing here knows one.

use crate::plane::{Event, Topic};
use collab_types::presence::{Availability, Presence, Shown};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_live::presence;
use tilo_live::presence::{Board, Device};

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
    /// The voice channel they are in here, if any: what everyone else's roster says beside their
    /// face.
    pub call: Option<super::InCall>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
}

impl OwnPresence {
    /// What the board says of this device: the person's own word, and what only its certificate and
    /// its last wave add.
    pub fn word(&self, device: &str, wave: Option<presence::Wave>) -> presence::Own {
        presence::Own {
            label: self.label.clone(),
            device: device.to_owned(),
            user: self.user.clone(),
            status: self.status.clone(),
            availability: self.availability,
            call: self.call.clone(),
            following: self.following.clone(),
            wave,
            pointer: None,
            shell: None,
        }
    }
}

/// The clock presence arrives and is drawn on: one clock for what a board heard and what a surface
/// draws, so a pointer's motion is measured against the arrivals it was heard on.
#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64 for the life of a session"
)]
pub fn now_ms() -> f64 {
    ui::time::unix_now_ms() as f64
}

/// One workspace this session takes part in: its topic, and the board of who is here.
#[derive(Debug)]
pub struct Workspace {
    /// The workspace's topic.
    pub ws: Topic,
    /// The workspace, as its id travels: the uuid string every publish names.
    pub id: String,
    /// What each member here said of themselves, and what this device says.
    pub board: Board,
}

impl Workspace {
    pub fn new(ws: Topic, id: String, publisher: Device) -> Self {
        Self {
            board: Board::new(id.clone(), publisher),
            ws,
            id,
        }
    }

    /// The calls in progress here, as those in them say.
    pub fn calls(&self) -> Vec<super::LiveCall> {
        self.board.calls()
    }

    /// Everyone here, as a surface that draws them reads them, at `now_ms`.
    pub fn shown(&self, now_ms: f64) -> Vec<Shown> {
        self.board
            .here()
            .map(|member| presence::shown(member, now_ms))
            .collect()
    }

    /// Their device's certificate arrived: only the sender's own, validly signed one says who is
    /// here, and what everyone calls them is the person's own word, never their device's name.
    pub fn certify(&mut self, from: EndpointId, cert: &tilo_user_identity::DeviceCert) {
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
        self.board.certify(
            *from.as_bytes(),
            hex::encode(cert.user_public_key),
            &cert.device_label,
        );
    }
}

/// The workspaces this session takes part in, and the one in front.
#[derive(Debug)]
pub struct Plane {
    /// This device's publisher.
    pub publisher: EndpointId,
    pub workspaces: Vec<Workspace>,
    /// The workspace in front: whose roster the desktop shows.
    pub active: Option<Topic>,
}

impl Plane {
    pub fn new(publisher: EndpointId) -> Self {
        Self {
            publisher,
            workspaces: Vec::new(),
            active: None,
        }
    }

    pub fn workspace(&mut self, ws: Topic) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.ws == ws)
    }

    /// The workspace in front, if it is one this session takes part in.
    fn front(&self) -> Option<&Workspace> {
        let active = self.active;
        self.workspaces
            .iter()
            .find(|workspace| Some(workspace.ws) == active)
    }

    fn front_mut(&mut self) -> Option<&mut Workspace> {
        let active = self.active;
        self.workspaces
            .iter_mut()
            .find(|workspace| Some(workspace.ws) == active)
    }

    /// Take one message a peer published on `ws`.
    pub fn message(&mut self, ws: Topic, from: EndpointId, msg: NeighbourMsg) {
        if from == self.publisher {
            return;
        }
        if let NeighbourMsg::IdentityHello { cert, .. } = msg
            && let Some(workspace) = self.workspace(ws)
        {
            workspace.certify(from, &cert);
        }
    }

    /// Take one thing the plane heard that its boards keep.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Msg { ws, peer, msg } => self.message(ws, peer, *msg),
            Event::PeerLeft { ws, peer } => {
                if let Some(workspace) = self.workspace(ws) {
                    workspace.board.depart(*peer.as_bytes());
                }
            }
            _ => {}
        }
    }

    /// How many times what the workspace in front knows of its people changed.
    pub fn revision(&self) -> u64 {
        self.front()
            .map_or(0, |workspace| workspace.board.revision())
    }

    /// How many times what `workspace` knows of its people changed.
    pub fn revision_of(&self, workspace: &str) -> u64 {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map_or(0, |held| held.board.revision())
    }

    /// Everyone here in `workspace`, as a surface that draws them reads them.
    pub fn shown(&self, workspace: &str, now_ms: f64) -> Vec<Shown> {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map_or_else(Vec::new, |held| held.shown(now_ms))
    }

    /// When what this device says is next due, on the clock presence is heard on, if anything is.
    pub fn next_due(&self, now_ms: f64) -> Option<f64> {
        self.front()
            .and_then(|workspace| workspace.board.next_due(now_ms))
    }

    /// Say `word` in the workspace in front and take what it owes now, each frame with its object
    /// on that workspace's presence track.
    pub fn say_in_front(
        &mut self,
        word: &presence::Own,
        now_ms: f64,
    ) -> Option<(DeskWorkspaceId, Vec<(u32, Presence)>)> {
        let workspace = self.front_mut()?;
        workspace.board.say(word);
        let desk = DeskWorkspaceId::parse(&workspace.id)?;
        Some((desk, workspace.board.flush(now_ms)))
    }

    /// Take what the workspace in front's board owes at `now_ms`.
    pub fn due_in_front(&mut self, now_ms: f64) -> Option<(DeskWorkspaceId, Vec<(u32, Presence)>)> {
        let workspace = self.front_mut()?;
        let desk = DeskWorkspaceId::parse(&workspace.id)?;
        Some((desk, workspace.board.due(now_ms)?))
    }

    /// Take what `workspace`'s board owes at `now_ms`, as a host that names one workspace at a time
    /// asks with its own timer: the keepalives, the repeats and the loss repairs it holds.
    pub fn due_in(
        &mut self,
        workspace: &str,
        now_ms: f64,
    ) -> Option<(DeskWorkspaceId, Vec<(u32, Presence)>)> {
        let held = self
            .workspaces
            .iter_mut()
            .find(|held| held.id == workspace)?;
        let desk = DeskWorkspaceId::parse(&held.id)?;
        Some((desk, held.board.due(now_ms)?))
    }

    /// When `workspace`'s board next owes a word, on the clock presence is heard on.
    pub fn next_due_in(&self, workspace: &str, now_ms: f64) -> Option<f64> {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .and_then(|held| held.board.next_due(now_ms))
    }
}
