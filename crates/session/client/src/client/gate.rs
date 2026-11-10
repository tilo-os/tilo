//! The gate every message from another member passes: the workspace it arrived in decides, by its
//! own policy, what the member may say there.
//! The table of what each message is lives in [`crate::gate`]; here is where the question is asked
//! — with the sender's person, the desk the session shows, and the workspace's policy — and what a
//! refusal says.

use crate::gate::{self, WorkspaceAction};
use crate::plane::Event;

use super::Client;

/// The desk a session shows, as the gate reads it: the workspaces whose policies hold every
/// message from another member to what the workspace grants them.
pub trait Desk {
    /// Whether the workspace `workspace` of this desk lets `person` do `required` at `now` (Unix
    /// seconds). A desk that holds no such workspace admits nothing.
    fn allows(&self, workspace: &str, person: &str, required: &WorkspaceAction, now: u64) -> bool;
}

impl Client {
    /// Why `event`'s message is refused by the workspace it arrived in: `None` when it is admitted,
    /// and when nothing here decides — a plane that takes part as nobody, or a message that is no
    /// workspace's traffic ([`gate::required_workspace_action`]).
    pub(crate) fn refusal(&self, event: &Event) -> Option<String> {
        let Event::Msg { ws, peer, msg } = event else {
            return None;
        };
        // A session whose device no person certified is a member of no workspace's governance: a
        // stand-in plane has no policy to be held to.
        self.person()?;
        let required: WorkspaceAction = gate::required_workspace_action(msg)?;
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
        let person = known.or_else(|| {
            self.host
                .borrow()
                .as_ref()
                .and_then(|host| host.governed_person(&workspace, *peer))
        });
        let Some(person) = person else {
            return Some("the device is not known here".to_owned());
        };
        let desk = self.desk.borrow().clone()?;
        let now = tilo_clock::unix_now();
        if desk.allows(&workspace, &person, &required, now) {
            return None;
        }
        Some(format!(
            "the workspace's policy does not let {person} do {required:?}"
        ))
    }
}
