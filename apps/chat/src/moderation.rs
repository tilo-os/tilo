//! Banning someone from the room, from their message.
//!
//! Membership is the room's, not chat's: a ban is the room's own `ban`
//! action on its governance resource (`workspace.permissions:<room>`,
//! provider `tilo.workspace`), invoked through the host like any other
//! action, and decided there by the room's policy (the owner, or a
//! Moderator for people ranked below Moderator).

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};

use crate::app::ChatApp;

/// The room's governance verb that removes a person and keeps them out.
const BAN: &str = "ban";

/// Bans asked for and not answered yet, and the last refusal.
#[derive(Default)]
pub(crate) struct Moderation {
    requests: Vec<ui::collaboration::CollabRequest>,
    pending: Vec<InvocationId>,
    /// Why the last ban did not happen, until the next one is asked.
    pub(crate) refused: Option<String>,
}

/// Whether `participant` names a person by their root key, the only id a
/// ban can name.
pub(crate) fn bannable(participant: &str) -> bool {
    participant.len() == 64 && participant.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The invocation banning `person` from `room`.
pub(crate) fn ban_request(room: &str, person: &str) -> InvokeRequest {
    let mut argument = serde_json::Map::new();
    argument.insert("person".into(), person.to_ascii_lowercase().into());
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, BAN),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

impl ChatApp {
    pub(crate) fn ban_author(&mut self, message: &str) {
        let Some(author) = self.message(message).map(|m| m.author.clone()) else {
            return;
        };
        if self.is_me(&author) || !bannable(&author) {
            return;
        }
        let request = ban_request(self.perms.room(), &author);
        self.moderation.refused = None;
        self.moderation.pending.push(request.invocation);
        self.moderation
            .requests
            .push(ui::collaboration::CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            });
    }

    pub(crate) fn moderation_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        std::mem::take(&mut self.moderation.requests)
    }

    /// Take the host's answer to a ban this window asked for.
    pub(crate) fn complete_moderation(&mut self, message: &RemoteActionMessage) {
        let RemoteActionMessage::Receipt(receipt) = message else {
            return;
        };
        let Some(index) = self
            .moderation
            .pending
            .iter()
            .position(|pending| *pending == receipt.invocation)
        else {
            return;
        };
        self.moderation.pending.remove(index);
        self.moderation.refused = match &receipt.status {
            ReceiptStatus::Failed { error } => Some(error.message.clone()),
            ReceiptStatus::Rejected { error } => Some(error.to_string()),
            _ => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ban_names_the_rooms_governance_and_the_person() {
        let person = "ab".repeat(32);
        let request = ban_request("room-1", &person);
        assert_eq!(request.target.provider, "tilo.workspace");
        assert_eq!(
            request.target.address.resource,
            "workspace.permissions:room-1"
        );
        assert_eq!(request.target.address.action, "ban");
        let InvokeArgument::Json(argument) = request.argument else {
            panic!("a ban names its person");
        };
        assert_eq!(argument["person"], person);
        assert!(bannable(&person));
        assert!(!bannable("u_guest"));
    }
}
