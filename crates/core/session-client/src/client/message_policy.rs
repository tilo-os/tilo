//! The check every message from another member passes: the workspace it arrived in decides, by its
//! own policy, what the member may say there.
//! The table of what each message is lives in [`crate::message_policy`]; here is where the question is asked
//! — with the sender's person, the workspaces the session shows, and each workspace's policy — and
//! what a refusal says.

use crate::message_policy::{self, WorkspaceAction};
use crate::network::Event;

use super::Client;

/// The shell a session shows, as the check reads it: its workspaces' policies hold every
/// message from another member to what the workspace grants them.
pub trait Shell {
    /// Whether the workspace `workspace` of this shell lets `person` do `required` at `now` (Unix
    /// seconds). A shell that holds no such workspace admits nothing.
    fn allows(&self, workspace: &str, person: &str, required: &WorkspaceAction, now: u64) -> bool;
}

impl Client {
    /// Why `event`'s message is refused by the workspace it arrived in: `None` when it is admitted,
    /// and when nothing here decides — a client that takes part as nobody, or a message that is no
    /// workspace's traffic ([`check::required_workspace_action`]).
    pub(crate) fn refusal(&self, event: &Event) -> Option<String> {
        let Event::Msg { ws, peer, msg } = event else {
            return None;
        };
        // A session whose device no person certified is a member of no workspace's governance: a
        // stand-in client has no policy to be held to.
        self.person()?;
        let required: WorkspaceAction = message_policy::required_workspace_action(msg)?;
        if *peer == self.publisher() {
            return None;
        }
        let (workspace, known) = self.heard_member(*ws, *peer)?;
        // Who the device is: the certificate it said here, or, for a member that announces none — a
        // browser of the workspace — the identity the workspace's governance admitted its device
        // as.
        let person = known.or_else(|| self.governed_person(&workspace, *peer));
        let Some(person) = person else {
            return Some("the device is not known here".to_owned());
        };
        let shell = self.shell.borrow().clone()?;
        let now = tilo_clock::unix_now();
        if shell.allows(&workspace, &person, &required, now) {
            return None;
        }
        Some(format!(
            "the workspace's policy does not let {person} do {required:?}"
        ))
    }
}
