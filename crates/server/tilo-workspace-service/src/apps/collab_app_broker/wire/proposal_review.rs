//! A person's review of their assistant's proposals goes to the host that keeps
//! them. Only their own launched Assistant window may make this request here.

use tilo_actions::remote::{ActionCallError, RemoteActionMessage, workspace_initiative};
use tilo_app_id::apps::ASSISTANT;

use super::{
    AdmittedJoinIdentity, Answer, BrokerError, CrdtRoute, JoinedChannel, RouteAccess, refusal,
};

pub(super) fn addresses(message: &RemoteActionMessage) -> bool {
    matches!(message, RemoteActionMessage::Invoke(request)
        if request.target.provider == workspace_initiative::PROVIDER
            && request.target.address.resource == workspace_initiative::PROPOSALS
            && workspace_initiative::is_review_action(&request.target.address.action))
}

impl JoinedChannel {
    pub(super) fn review_proposal(
        &self,
        route: &CrdtRoute,
        current: &AdmittedJoinIdentity,
        message: RemoteActionMessage,
    ) -> Result<Answer<RemoteActionMessage>, BrokerError> {
        let launched = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref());
        let assistant_window = launched.is_some_and(|launch| {
            launch.app_id == ASSISTANT
                && launch.workspace_id == route.cluster_id
                && !launch.windowless
                && launch.current()
        });
        let direct = matches!(&message, RemoteActionMessage::Invoke(request)
            if request.on_behalf_of.is_none() && request.signed.is_none());
        if !direct
            || !assistant_window
            || route.app_id != ASSISTANT
            || !matches!(self.route_access, RouteAccess::Own)
            || !self.state.app_action_access(ASSISTANT).invoke
        {
            return refusal(&message, ActionCallError::NotPermitted);
        }
        // The host connection uses this device's own signing identity. It may
        // only carry the same person and device that the app channel proved.
        let person = current.caller(&route.cluster_id);
        if person.is_foreign_device()
            || current.certificate.as_ref().is_none_or(|cert| {
                self.state
                    .current_native_person(&route.cluster_id, cert)
                    .is_err()
            })
        {
            return refusal(&message, ActionCallError::NotPermitted);
        }
        let Some(host) = self
            .state
            .services
            .hosted_workspaces
            .as_ref()
            .and_then(|workspaces| workspaces.host_actions(&route.cluster_id))
        else {
            return refusal(
                &message,
                ActionCallError::Unavailable {
                    reason: "The workspace's host is unavailable for proposal review".into(),
                },
            );
        };
        let answer = tilo_actions::remote::serve(&host, &person, message).ok_or_else(|| {
            BrokerError::failed("only proposal review requests are answered".into())
        })?;
        Ok(Box::pin(async move {
            answer
                .await
                .ok_or_else(|| BrokerError::failed("the workspace's host did not answer".into()))
        }))
    }
}
