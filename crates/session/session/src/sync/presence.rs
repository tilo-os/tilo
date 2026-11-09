//! Who is here: the workspaces this session takes part in, the board of what each member said of
//! themselves there, and this device's own word on it. Every host drives the same board and says
//! what it owes on its own transport; nothing here knows one.

use crate::platform::plane::Topic;
use collab_types::presence::{Availability, Presence, Shown};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_desktop::here::LiveMember;
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
    pub(super) fn word(&self, device: &str, wave: Option<presence::Wave>) -> presence::Own {
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
pub(super) fn now_ms() -> f64 {
    ui::time::unix_now_ms() as f64
}

/// One workspace this session takes part in: its topic, and the board of who is here.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// The workspace's topic.
    pub(super) ws: Topic,
    /// The workspace, as its id travels: the uuid string every publish names.
    pub(super) id: String,
    /// What each member here said of themselves, and what this device says.
    pub(super) board: Board,
}

impl Workspace {
    pub(super) fn new(ws: Topic, id: String, publisher: Device) -> Self {
        Self {
            board: Board::new(id.clone(), publisher),
            ws,
            id,
        }
    }

    /// Who is here, as the desktop lists them, on the clock presence is heard on.
    pub(super) fn members(&self, now_ms: f64) -> Vec<LiveMember> {
        self.board
            .here()
            .map(|member| LiveMember::of(&presence::shown(member, now_ms)))
            .collect()
    }

    /// The calls in progress here, as those in them say.
    pub(super) fn calls(&self) -> Vec<super::LiveCall> {
        self.board.calls()
    }

    /// Everyone here, as a surface that draws them reads them, at `now_ms`.
    pub(super) fn shown(&self, now_ms: f64) -> Vec<Shown> {
        self.board
            .here()
            .map(|member| presence::shown(member, now_ms))
            .collect()
    }

    /// Their device's certificate arrived: only the sender's own, validly signed one says who is
    /// here, and what everyone calls them is the person's own word, never their device's name.
    pub(super) fn certify(&mut self, from: EndpointId, cert: &tilo_user_identity::DeviceCert) {
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
pub(crate) struct Plane {
    /// This device's publisher.
    pub(super) publisher: EndpointId,
    pub(super) workspaces: Vec<Workspace>,
    /// The workspace in front: whose roster the desktop shows.
    pub(super) active: Option<Topic>,
}

impl Plane {
    pub(super) fn new(publisher: EndpointId) -> Self {
        Self {
            publisher,
            workspaces: Vec::new(),
            active: None,
        }
    }

    pub(super) fn workspace(&mut self, ws: Topic) -> Option<&mut Workspace> {
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
    pub(super) fn message(&mut self, ws: Topic, from: EndpointId, msg: NeighbourMsg) {
        if from == self.publisher {
            return;
        }
        if let NeighbourMsg::IdentityHello { cert, .. } = msg
            && let Some(workspace) = self.workspace(ws)
        {
            workspace.certify(from, &cert);
        }
    }

    /// How many times what the workspace in front knows of its people changed.
    pub(super) fn revision(&self) -> u64 {
        self.front()
            .map_or(0, |workspace| workspace.board.revision())
    }

    /// Everyone here in `workspace`, as a surface that draws them reads them.
    pub(super) fn shown(&self, workspace: &str, now_ms: f64) -> Vec<Shown> {
        self.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map_or_else(Vec::new, |held| held.shown(now_ms))
    }

    /// When what this device says is next due, on the clock presence is heard on, if anything is.
    pub(super) fn next_due(&self, now_ms: f64) -> Option<f64> {
        self.front()
            .and_then(|workspace| workspace.board.next_due(now_ms))
    }

    /// Say `word` in the workspace in front and take what it owes now, each frame with its object
    /// on that workspace's presence track.
    pub(super) fn say_in_front(
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
    pub(super) fn due_in_front(
        &mut self,
        now_ms: f64,
    ) -> Option<(DeskWorkspaceId, Vec<(u32, Presence)>)> {
        let workspace = self.front_mut()?;
        let desk = DeskWorkspaceId::parse(&workspace.id)?;
        Some((desk, workspace.board.due(now_ms)?))
    }
}
