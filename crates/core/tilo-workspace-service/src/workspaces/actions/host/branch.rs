//! Branching a workspace of the caller's own person as a host operation
//! ([`tilo_server_protocol::workspaces::BRANCH_OWN_WORKSPACE`]): who may, and which branch a retry answers. What a
//! branch holds is the workspaces' (`crate::workspaces::branch`).

use serde_json::Value;
use tilo_actions::remote::ActionCallError;
use tilo_actions::{ActionParam, ParamType};
use tilo_peer_state::join::Role;
use tilo_server_protocol::workspaces::{AdminRefusal, WorkspaceLabels, check_labels};
use tilo_user_identity::DeviceCert;

use super::{Args, OWN_INVITATION_SECONDS, expiry};
use crate::workspaces::Workspaces;

/// What branching a workspace takes.
pub(super) fn params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "workspace",
            ParamType::Text,
            "The workspace of yours to branch, as listing workspaces names it.",
        ),
        ActionParam::required(
            "name",
            ParamType::Text,
            "The branch's name, 1 to 80 printable characters.",
        ),
        ActionParam::optional(
            "branch",
            ParamType::Text,
            "The branch's identity (a UUID): asked again under the same one, the branch made then is answered and nothing is copied twice. A new one when omitted.",
        ),
        ActionParam::optional(
            "labels",
            ParamType::Object,
            "What you label the branch with, as for a workspace you make: names (a-z, 0-9, `_`, `-`, `.`) to values, at most 16. The source's labels stay its own.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation your devices redeem to hold the branch stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

impl Workspaces {
    /// Branch the workspace `args` names, one of `device`'s person's own,
    /// already permitted to, and invite the person into the branch: the one made
    /// under the identity `branch` names when it is theirs already (a retry).
    pub(super) async fn branch_own_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionCallError> {
        let person = device.user_public_key;
        let source = args.required_text("workspace")?;
        let name = args.required_text("name")?;
        let branch = match args.text("branch")? {
            Some(branch) => uuid::Uuid::parse_str(branch).map_err(|_| {
                ActionCallError::invalid_request("`branch` is a workspace's identity, a UUID")
            })?,
            None => uuid::Uuid::new_v4(),
        }
        .to_string();
        let labels: Option<WorkspaceLabels> = args.object("labels")?;
        if let Some(labels) = &labels {
            check_labels(labels).map_err(ActionCallError::invalid_request)?;
        }
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        let own = self
            .own()
            .ok_or_else(|| ActionCallError::refused(&AdminRefusal::NotYourOwn.to_reason()))?;
        // A branch is a workspace made: a grant for one workspace makes none,
        // as it makes no other workspace.
        let makes = device
            .capabilities
            .agent
            .is_none_or(|scope| scope.makes_workspaces());
        // Only a workspace of the person's own, which they read whole, is
        // theirs to branch; anyone else's is not theirs, as for deleting it.
        if !makes || !own.is_of(&person, source) || !self.lists_to(device, source) {
            return Err(ActionCallError::refused(
                &AdminRefusal::NotYourOwn.to_reason(),
            ));
        }
        // A retry waits for the try it repeats, and answers the branch it made.
        let making = own.making(branch.clone()).await;
        // One a stop cut short is not made, however far it came.
        let summary = if own.is_of(&person, &branch) && !self.unfinished_branch(&branch) {
            self.summary(&branch)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?
        } else if !self.let_in(&person, now).await {
            return Err(ActionCallError::refused(
                self.not_let_in().await.to_reason(),
            ));
        } else {
            let made = self.make_branch(source, &branch, name, person).await?;
            // The branch holds what its source holds: running commands
            // unattended too. Not taken, its keys are asked, as before.
            if let Err(error) = self.branch_unattended(source, &made.workspace) {
                tracing::warn!(%source, branch = %made.workspace, %error, "a branch does not run commands unattended as its source does");
            }
            made
        };
        if let Some(labels) = labels {
            self.label(&summary.workspace, labels)
                .map_err(ActionCallError::refused)?;
        }
        drop(making);
        self.invite_own_person(
            summary,
            person,
            Role::Editor,
            request_id,
            expiry(now, seconds),
        )
        .await
    }

    #[cfg(feature = "agents")]
    async fn make_branch(
        &self,
        source: &str,
        branch: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<crate::workspaces::WorkspaceSummary, ActionCallError> {
        self.branch_own(source, branch, name, person)
            .await
            .map_err(|not_made| ActionCallError::refused(not_made.refusal()))
    }

    /// A host built without the apps' records reads no record whole to fork.
    #[cfg(not(feature = "agents"))]
    #[expect(
        clippy::unused_async,
        reason = "the same call as the host that branches"
    )]
    async fn make_branch(
        &self,
        _source: &str,
        _branch: &str,
        _name: &str,
        _person: [u8; 32],
    ) -> Result<crate::workspaces::WorkspaceSummary, ActionCallError> {
        Err(ActionCallError::refused(
            "this host does not branch workspaces",
        ))
    }
}
