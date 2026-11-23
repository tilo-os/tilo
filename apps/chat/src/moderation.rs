//! Banning someone from the workspace, from their message.
//!
//! Membership is the workspace's, not chat's: a ban is the workspace's own `ban`
//! action on its governance resource (`workspace.permissions:<workspace>`,
//! provider `tilo.workspace`), invoked through the host like any other
//! action, and decided there by the workspace's grants on banning (the
//! Moderator role holds one).

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};

use crate::app::ChatApp;

/// The workspace's governance verb that removes a person and keeps them out.
const BAN: &str = "ban";

/// Bans asked for and not answered yet, and the last refusal.
#[derive(Default)]
pub(crate) struct Moderation {
    requests: Vec<ui::collaboration::CollabRequest>,
    pending: Vec<InvocationId>,
    /// Why the last ban did not happen, until the next one is asked.
    pub(crate) refused: Option<String>,
}

/// Whether `member` names a person by their root key, the only id a
/// ban can name.
pub(crate) fn bannable(member: &ui::tilo_resource::MemberId) -> bool {
    member
        .person_id()
        .is_some_and(|root| root.len() == 64 && root.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The invocation banning `person` from `workspace`.
pub(crate) fn ban_request(workspace: &str, person: &str) -> InvokeRequest {
    let mut argument = serde_json::Map::new();
    argument.insert("person".into(), person.to_ascii_lowercase().into());
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, BAN),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

impl ChatApp {
    pub(crate) fn file_report(&self, report: &ui::tilo_resource::Invoke<chat_model::Moderation>) {
        let refused = self
            .dispatch
            .as_ref()
            .map(ui::framework::Dispatcher::refused);
        let action = report.id.to_owned();
        let finish = move |result: Result<(), ui::tilo_actions::ActionError>| {
            if let (Err(error), Some(refused)) = (result, refused) {
                refused.set(Some(ui::framework::Refused {
                    action,
                    item: None,
                    reason: error.to_string(),
                    by_workspace: false,
                }));
            }
        };
        if let Some(record) = self
            .moderation_record
            .as_ref()
            .and_then(|opener| opener.open(chat_model::MODERATION_DOCUMENT))
        {
            record.invoke_then(report, finish);
        } else {
            finish(Err(ui::tilo_actions::ActionError::Refused(crate::i18n::t(
                "chat-moderation-record-unavailable",
            ))));
        }
    }

    pub(crate) fn ban_author(&mut self, message: &str) {
        let Some(author) = self.message(message).map(|m| m.author.clone()) else {
            return;
        };
        if self.is_me(&author) || !bannable(&author) {
            return;
        }
        let request = ban_request(&self.perms.workspace(), author.id());
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
    fn a_ban_names_the_workspaces_governance_and_the_person() {
        let person = "ab".repeat(32);
        let request = ban_request("workspace-1", &person);
        assert_eq!(request.target.provider, "tilo.workspace");
        assert_eq!(
            request.target.address.resource,
            "workspace.permissions:workspace-1"
        );
        assert_eq!(request.target.address.action, "ban");
        let InvokeArgument::Json(argument) = request.argument else {
            panic!("a ban names its person");
        };
        assert_eq!(argument["person"], person);
        assert!(bannable(&ui::tilo_resource::MemberId::person(person)));
        assert!(!bannable(&ui::tilo_resource::MemberId::person("u_guest")));
    }
}
