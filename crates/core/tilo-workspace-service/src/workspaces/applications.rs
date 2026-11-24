//! An app's records, and the actions a host runs on them.
//!
//! A record is the app's primary (`""`, for an app that keeps one record per workspace) or a
//! named record of the workspace: any app's, under an id the caller chooses, such as the
//! workspace's default record every member's first window of the app opens. The app's own
//! process opens a record or founds it at the id a call names; what this host authors is the
//! change it is asked to publish.

use super::{CollaborationError, Workspaces, system_time};
use tilo_user_identity::DeviceCert;

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
    /// Whether this host governs `workspace`'s applications: of its holders,
    /// the one its governance names to make the compacted base, else the
    /// device it was made on. That one authors its records' setup.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub async fn governs_applications(&self, workspace: &str) -> Result<bool, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governs = state
            .lock()
            .await
            .governs_applications(&self.inner.host.cert);
        Ok(governs)
    }

    /// Author an existing native record (`""` for the primary) as its exact
    /// current governor. The registry and synchronous change come from
    /// trusted host code. This host-local API accepts no incoming member
    /// snapshot or factory. It keeps the existing graph and publishes only
    /// its admitted incremental changes, sealed for a record members write
    /// sealed (`super::sealed_documents`).
    ///
    /// # Errors
    /// Refuses other certificates, stale ownership, undeclared apps, a primary
    /// of an app without one, missing records, sealed records whose channel
    /// this server does not take part in, failed changes or oversized
    /// publication.
    pub async fn author_application<R>(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        app: &str,
        record: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application authoring requires exact governor ownership",
            ));
        }
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if !governed.governs_applications(governor) {
            return Err(CollaborationError::new(
                "application governor differs from current workspace authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        governed.peer().check_document_read(
            governor.device_public_key,
            app,
            record,
            system_time(now),
        )?;
        // A record members write sealed is read through its channel's keys
        // and written sealed; any other must be complete plaintext.
        let keys = self
            .sealing_keys(&governed, app, record)
            .await
            .map_err(CollaborationError::new)?;
        let bytes = governed
            .readable_document(app, record)
            .await
            .map_err(CollaborationError::new)?
            .ok_or_else(|| CollaborationError::new("native record is not prepared"))?;
        let mut document = automerge::AutoCommit::load(&bytes)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let heads = match &keys {
            Some(_) => document.get_heads(),
            None => governed
                .peer()
                .plaintext_complete_heads(app, record)
                .ok_or_else(|| {
                    CollaborationError::new("native record is not complete plaintext")
                })?,
        };
        let result = change(&mut document)?;
        // The governor writes the record directly: an edit of the whole
        // record, named as such.
        if automerge::transaction::Transactable::pending_ops(&document) != 0 {
            document.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
        }
        let changes = document.save_after(&heads);
        if changes.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "native authoring exceeds publication limit",
            ));
        }
        if document.get_heads() == heads {
            return Ok(result);
        }
        let mut candidate = governed.peer().clone();
        let written = super::sealed_documents::seal_changes(
            &mut candidate,
            super::sealed_documents::sealed_with(keys.as_ref())?,
            (app, record),
            changes,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = governed.collaboration().clone();
        governed.keep(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        self.carry_written(governed.workspace_id(), &[written])?;
        drop(governed);
        // What this device wrote itself reaches whoever follows the record, as a member's write
        // does.
        self.wrote(workspace, app, record);
        Ok(result)
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
