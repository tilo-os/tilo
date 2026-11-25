//! An app's records, and the actions a host runs on them.
//!
//! A record is the app's primary (`""`, for an app that keeps one record per workspace) or a
//! named record of the workspace: any app's, under an id the caller chooses, such as the
//! workspace's default record every member's first window of the app opens. The app's own
//! process opens a record or founds it at the id a call names.

use super::{CollaborationError, Workspaces};

/// One of an app's actions on one of its records, as the host runs it for a caller.
pub(crate) struct RecordAction<'a> {
    /// The app whose record it is, by its id.
    pub app: &'a str,
    /// The record, by its id.
    pub record: &'a str,
    /// The resource the action is published on.
    pub resource: &'a str,
    /// The action's id.
    pub action: &'a str,
    /// What it takes.
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl Workspaces {
    /// Whether this host administers `workspace`'s applications: of the devices
    /// keeping it, the one its governance names to make the compacted base,
    /// else the device it was made on. That one authors its records' setup.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub async fn administers_applications(
        &self,
        workspace: &str,
    ) -> Result<bool, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let administers = state
            .lock()
            .await
            .administers_applications(&self.inner.host.cert);
        Ok(administers)
    }

    /// Run one of an app's actions on `record` as `caller`: the invocation the workspace's own
    /// dispatch takes, so the app process that holds the record runs the action, started with no
    /// window where none runs, on `record` it opens or founds. Answers what the action answered.
    ///
    /// # Errors
    ///
    /// No app process here takes the call, the action is refused or fails, or it does not end in
    /// time. The app's own answer travels as it is, so the person's yes it waits for
    /// ([`tilo_actions::remote::ActionCallError::ConfirmationRequired`]) reaches the dispatch
    /// that keeps the call as their question.
    pub(crate) async fn apply_app_action(
        &self,
        workspace: &str,
        caller: &tilo_actions::remote::Caller,
        asked: RecordAction<'_>,
        confirmation: Option<tilo_actions::remote::Confirmation>,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, ReceiptStatus};

        /// How long the app has to take the call and answer it.
        const APP_ACTION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
        /// How often a call the app took is looked at again.
        const APP_ACTION_POLL: std::time::Duration = std::time::Duration::from_millis(20);

        let RecordAction {
            app,
            record,
            resource,
            action,
            arguments,
        } = asked;
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::application_resource_target(
                workspace, app, record, resource, action,
            ),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let invocation = request.invocation;
        let providers = self.providers();
        let answer = providers
            .invoke(caller, request)
            .ok_or_else(|| ActionCallError::Refused {
                reason: format!("no app process runs {app}'s {record} here"),
            })?;
        let receipt = tokio::task::spawn_blocking(move || answer.wait_timeout(APP_ACTION_WAIT))
            .await
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })?
            .map_err(|_| ActionCallError::Unavailable {
                reason: "the app did not take the call in time".into(),
            })?
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the app dropped the call".into(),
            })??;
        let mut status = receipt.status;
        // A call the app took rather than answered is followed until it settles.
        let deadline = tilo_clock::now()
            .checked_add(APP_ACTION_WAIT)
            .unwrap_or_else(tilo_clock::now);
        while status == ReceiptStatus::Accepted {
            if tilo_clock::now() >= deadline {
                return Err(ActionCallError::Unavailable {
                    reason: "the app did not answer in time".into(),
                });
            }
            tokio::time::sleep(APP_ACTION_POLL).await;
            if let Some(Some(state)) = providers.observe(caller, &invocation) {
                status = state.receipt().status;
            }
        }
        match status {
            ReceiptStatus::Completed { output } => Ok(output),
            ReceiptStatus::Accepted => Err(ActionCallError::Unavailable {
                reason: "the app did not answer".into(),
            }),
            // The app's own answer, its question for the person's yes among them.
            ReceiptStatus::Rejected { error } => Err(error),
            ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
                reason: error.message,
            }),
            ReceiptStatus::Cancelled { message } => Err(ActionCallError::Refused {
                reason: message.unwrap_or_else(|| "the app cancelled the call".into()),
            }),
        }
    }
}
