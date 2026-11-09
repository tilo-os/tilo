//! The gate every message from another member passes: the workspace it arrived in decides, by its
//! own policy, what the member may say there.
//! The table of what each message is lives in [`crate::workspace_gate`]; here is where the question
//! is asked — with the sender's person, the desk the session shows, and the workspace's policy —
//! and what a refusal says.

#[cfg(target_os = "linux")]
use std::time::SystemTime;

use crate::platform::plane::Event;
#[cfg(target_os = "linux")]
use iroh::EndpointId;
#[cfg(target_os = "linux")]
use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_desktop_surface::DesktopHandle;

use crate::workspace_gate::{self, WorkspaceAction};

use super::Client;

impl Client {
    /// Read the policies of the workspaces this session takes part in from `desk` : the desk it
    /// shows, whose workspaces' policies every message from another member is held to
    /// ([`Client::hear`]).
    pub fn attach_desk(&self, desk: DesktopHandle) {
        *self.desk.borrow_mut() = Some(desk);
    }

    /// Why `event` 's message is refused by the workspace it arrived in: `None` when it is
    /// admitted, and when nothing here decides — a plane that takes part as nobody, or a message
    /// that is no workspace's traffic ([`workspace_gate::required_workspace_action`]).
    pub(super) fn refusal(&self, event: &Event) -> Option<String> {
        let Event::Msg { ws, peer, msg } = event else {
            return None;
        };
        // A session whose device no person certified is a member of no workspace's governance: a
        // stand-in plane has no policy to be held to.
        self.user?;
        let required: WorkspaceAction = workspace_gate::required_workspace_action(msg)?;
        let (workspace, known) = {
            let plane = self.plane.borrow();
            if *peer == plane.publisher {
                return None;
            }
            let workspace = plane.workspaces.iter().find(|held| held.ws == *ws)?;
            let known = workspace
                .board
                .member(*peer.as_bytes())
                .and_then(|known| known.user.clone());
            (workspace.id.clone(), known)
        };
        // Who the device is: the certificate it said here, or, for a member that announces none — a
        // browser of the workspace — the identity the workspace's governance admitted its device
        // as.
        #[cfg(target_os = "linux")]
        let person = known.or_else(|| self.governed_person(&workspace, *peer));
        #[cfg(not(target_os = "linux"))]
        let person = known;
        let Some(person) = person else {
            return Some("the device is not known here".to_owned());
        };
        let desk = self.desk.borrow().clone()?;
        let now = tilo_clock::unix_now();
        let allowed =
            desk.read(|tree| workspace_gate::allows(tree, &workspace, &person, &required, now));
        if allowed {
            return None;
        }
        Some(format!(
            "the workspace's policy does not let {person} do {required:?}"
        ))
    }

    /// The person the workspace `workspace` 's governance admitted `device` as, when the device
    /// announced no certificate of its own here.
    #[cfg(target_os = "linux")]
    fn governed_person(&self, workspace: &str, device: EndpointId) -> Option<String> {
        let engine = self.engine.as_ref()?;
        let identity = DeskWorkspaceId::parse(workspace)?;
        engine
            .owner()
            .with_current(identity, |view| {
                view.peer()
                    .permissions(*device.as_bytes(), SystemTime::now())
                    .map(|member| member.person)
                    .map_err(|error| error.to_string())
            })
            .ok()
    }
}
