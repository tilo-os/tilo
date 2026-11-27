//! A workspace a person's own device asks a server of theirs to make, where that server does
//! not found their workspaces: a device that keeps their root founds it.
//!
//! The server keeps the make as its record in the person's own workspace, which both keep, for
//! a device that keeps the root, asked as itself, a device of theirs
//! ([`Workspaces::found_at_their_root`]): the same record and runner every other device of
//! theirs makes a workspace through ([`crate::workspaces::at_root::found_request`]). That device
//! founds it and lists it with the person's others, and the server takes it in from that list as
//! every device of theirs does. The server founds nothing itself: it keeps the workspace as the
//! person's own once it holds it, and answers with what their root's device answered.

use std::time::Duration;

use serde_json::Value;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_server_protocol::workspaces::WorkspaceLabels;
use tilo_user_identity::DeviceCert;

use super::persons_own;
use crate::workspaces::providers::Answered;
use crate::workspaces::{NotAtRoot, Workspaces};

/// How long the server waits for a device that keeps the person's root to take the make in.
const REACHED_WITHIN: Duration = Duration::from_secs(30);
/// How long it waits for the person's answer there, where their grants leave it to them.
const ANSWERED_WITHIN: Duration = Duration::from_secs(5 * 60);

impl Workspaces {
    /// Have a device that keeps `device`'s person's root make the workspace `request` asks for,
    /// as `caller` asked it here, where this host does not found it ([`Self::found_for`]): what
    /// that device answered, once the workspace it made is kept here. `None` where this host
    /// founds it itself, or the call is not asked by a device of this host's own person: it is
    /// then decided here as any other.
    ///
    /// # Errors
    /// No device that keeps the root took it in, the person did not answer or said no there,
    /// or it was refused.
    pub(super) async fn found_at_their_root(
        &self,
        (caller, device): (&Caller, &DeviceCert),
        request: &InvokeRequest,
        (args, now): (&super::super::Args, u64),
    ) -> Result<Option<Answered>, ActionCallError> {
        let person = device.user_public_key;
        let labels: Option<WorkspaceLabels> = args.object("labels")?;
        if self.theirs_already(person, args, labels.as_ref()).await? {
            return Ok(None);
        }
        let here = self.device_cert();
        let as_here = persons_own((caller, device)) && here.user_public_key == person;
        if caller.forwarder().is_some()
            || !as_here
            || caller.is_in_process()
            || self.founding_here(&person).await.is_ok()
            || !self.let_in(&person, now).await
        {
            return Ok(None);
        }
        let name = args
            .text("name")?
            .map_or_else(|| crate::i18n::workspace_name(None), str::to_owned);
        let labels = labels.unwrap_or_default();
        // The make's id is the call's: the same call asked again makes the same workspace.
        let id = crate::workspaces::at_root::made_id(request.invocation);
        let mut asked = crate::workspaces::at_root::found_request(&name, &labels, Some(&id));
        asked.invocation = request.invocation;
        let own = tilo_server_protocol::servers::person_workspace_id(&person);
        let asking =
            Caller::authenticated(person, here.device_public_key).in_workspace(own.clone());
        let facts = crate::workspaces::at_root::found_question(&name, &here.device_label);
        let ended = self
            .ask_at_root(
                &own,
                (&asking, &asked),
                &facts,
                (REACHED_WITHIN, ANSWERED_WITHIN),
            )
            .await;
        let made = match ended {
            Ok(InvocationUpdate::Succeeded { output, .. }) => output.value.unwrap_or(Value::Null),
            Ok(InvocationUpdate::Failed { error, .. }) => {
                return Err(ActionCallError::refused(error.message));
            }
            Ok(InvocationUpdate::Cancelled { message, .. }) => {
                return Err(ActionCallError::refused(message.unwrap_or_default()));
            }
            Ok(other) => {
                return Err(ActionCallError::refused(format!("it ended as {other:?}")));
            }
            Err(NotAtRoot::Unreached(_)) => {
                return Err(ActionCallError::Unavailable {
                    reason: "the device that keeps your identity didn't take it up".into(),
                });
            }
            Err(NotAtRoot::Unanswered(_)) => {
                return Err(ActionCallError::refused(
                    "you didn't answer on the device that keeps your identity",
                ));
            }
            Err(NotAtRoot::Unkept(why)) => return Err(ActionCallError::refused(why)),
        };
        let workspace = made
            .get("workspace")
            .and_then(|made| made.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ActionCallError::refused("the device that keeps your identity made no workspace")
            })?
            .to_owned();
        self.held_within(&workspace, REACHED_WITHIN)
            .await
            .map_err(ActionCallError::refused)?;
        if let Some(own) = self.own() {
            own.record(&person, &workspace)
                .map_err(ActionCallError::refused)?;
        }
        if !labels.is_empty() {
            self.label(&workspace, &labels)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
        }
        let summary = self
            .summary(&workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        super::answer(&tilo_server_protocol::workspaces::CreatedWorkspace {
            workspace: super::listing(&summary),
            invitation: None,
        })
        .map(Answered::Value)
        .map(Some)
    }

    /// Whether what `args` asks of `person` is theirs already, and answered here: a workspace
    /// the ask names, the first of theirs where it names none, or the one a retried ask labelled.
    async fn theirs_already(
        &self,
        person: [u8; 32],
        args: &super::super::Args,
        labels: Option<&WorkspaceLabels>,
    ) -> Result<bool, ActionCallError> {
        let name = args.text("name")?;
        if args.text("workspace")?.is_some() {
            return Ok(true);
        }
        let Some(own) = self.own() else {
            return Ok(false);
        };
        Ok((name.is_none() && own.any_of(&person))
            || self
                .own_labelled_for(person, (name, None), labels, own)
                .await
                .is_some())
    }
}
