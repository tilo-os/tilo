//! Canonical records created by the governing host's compiled application factory.
//! This is a host-local operation; incoming member documents cannot mint ownership.
//!
//! A record is the app's primary (`""`, for an app that keeps one record
//! per workspace) or a named record of the workspace: any app's, under an id the
//! caller chooses, such as the workspace's default record every member's first
//! window of the app opens.

use super::genesis::publish_genesis;
use super::{CollaborationError, Workspaces, system_time};
use tilo_agent_session::{AppCatalogue, ConfinedReplica, Seed};
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

/// The app's id, when it has a record `record`: the primary only
/// for an app that keeps one, a named record for any app.
fn app_id_for(
    catalogue: &AppCatalogue,
    app: &str,
    record: &str,
) -> Result<&'static str, CollaborationError> {
    let definition = catalogue
        .get(app)
        .ok_or_else(|| CollaborationError::new("application is not registered on this host"))?;
    if record.is_empty() && !definition.keeps_workspace_resource() {
        return Err(CollaborationError::new(
            "application has no primary workspace record",
        ));
    }
    Ok(definition.app_id())
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
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn author_application<R>(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        catalogue: &AppCatalogue,
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
        let app_id = app_id_for(catalogue, app, record)?;
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
            .readable_document(app_id, record)
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
        super::sealed_documents::publish_changes(
            &mut candidate,
            keys.as_ref(),
            (app, record),
            changes,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        drop(governed);
        // What this device wrote itself reaches whoever follows the record, as a member's write
        // does.
        self.wrote(workspace, app, record);
        Ok(result)
    }

    /// Prepare a registered application's record (`""` for the primary)
    /// with exact governor ownership, from the app's own factory. The
    /// catalogue comes from this host's compiled, trusted application
    /// registry. An existing record remains byte-identical. No
    /// member-supplied factory or snapshot crosses this API; return only
    /// after durable signed publication.
    ///
    /// # Errors
    /// Refuses other devices, stale governance, undeclared applications, a
    /// primary of an app without one, invalid genesis or failed publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor transaction retains its workspace lock through durable signed publication"
    )]
    pub async fn prepare_application(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        catalogue: &AppCatalogue,
        app: &str,
        record: &str,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application creation requires exact governor ownership",
            ));
        }
        app_id_for(catalogue, app, record)?;
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
        if governed.peer().holds_document(app, record) {
            return Ok(false);
        }
        // The factory is the app's own code: it runs in a confined app
        // worker, held only while its genesis is taken and admitted here
        // before it is signed.
        // A record a workspace route names (the workspace's default record) is
        // created under that id: a genesis naming another id would be
        // published there, and no member could open it.
        let seed = if record.is_empty() {
            Seed::New
        } else {
            Seed::NewAt(
                collab_types::document::DocumentId::parse(record).ok_or_else(|| {
                    CollaborationError::new("application record is not a record id")
                })?,
            )
        };
        let genesis = {
            let source = ConfinedReplica::open(
                catalogue,
                app,
                workspace,
                &tilo_actions::MemberId::person(hex::encode(governor.user_public_key)),
                &tilo_agent_session::ReplicaActor::Owner,
                seed,
                false,
            )
            .map_err(|error| CollaborationError::new(error.to_string()))?;
            if record.is_empty() != source.document_id().is_package_primary() {
                return Err(CollaborationError::new(
                    "factory primary route differs from its declaration",
                ));
            }
            let Ok(Some(collab_types::DocumentMutation::Snapshot(genesis))) =
                source.take_mutation()
            else {
                return Err(CollaborationError::new(
                    "factory did not produce a genesis snapshot",
                ));
            };
            genesis
        };
        if genesis.is_empty() || genesis.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "factory genesis exceeds native bootstrap limit",
            ));
        }
        let keys = self
            .sealing_keys(&governed, app, record)
            .await
            .map_err(CollaborationError::new)?;
        let mut candidate = governed.peer().clone();
        publish_genesis(
            &mut candidate,
            keys.as_ref(),
            (app, record),
            genesis,
            (&self.inner.host.key, system_time(now)),
        )?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        Ok(true)
    }

    /// Run one of an app's actions on `record`, its native record, as `invoker`, and publish
    /// what it changed as the governor's: a record this device holds without running the app is
    /// still changed by the app's own action ([`ConfinedReplica`]), so its fields and their
    /// schema stay the app's. Answers what the action answered.
    ///
    /// # Errors
    ///
    /// The record is not there, the app has no such action or refuses it, or the changes cannot
    /// be published.
    pub(crate) async fn apply_app_action(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        catalogue: &AppCatalogue,
        invoker: &tilo_actions::ActionInvoker,
        asked: RecordAction<'_>,
    ) -> Result<tilo_actions::ActionOutput, CollaborationError> {
        use tilo_actions::api::ActionAddress;
        let RecordAction {
            app,
            record,
            resource,
            action,
            arguments,
        } = asked;
        let record_id = collab_types::document::DocumentId::parse(record)
            .ok_or_else(|| CollaborationError::new("application record is not a record id"))?;
        let bytes = self
            .resource_document(workspace, app, record)
            .await?
            .ok_or_else(|| CollaborationError::new("native record is not prepared"))?;
        let replica = ConfinedReplica::open(
            catalogue,
            app,
            workspace,
            &tilo_actions::MemberId::person(hex::encode(governor.user_public_key)),
            &tilo_agent_session::ReplicaActor::Owner,
            Seed::Existing {
                record: record_id,
                changes: vec![bytes],
            },
            false,
        )
        .map_err(|error| CollaborationError::new(error.to_string()))?;
        let address = ActionAddress::new(resource, action);
        let output = replica
            .invoke_with(Some(invoker), None, &address, &arguments, true)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let changes = match replica
            .take_mutation()
            .map_err(|error| CollaborationError::new(error.to_string()))?
        {
            Some(collab_types::DocumentMutation::Incremental(changes)) => changes,
            Some(collab_types::DocumentMutation::Snapshot(_)) | None => {
                return Err(CollaborationError::new(
                    "the app recorded no change of the record",
                ));
            }
        };
        self.author_application(workspace, governor, catalogue, app, record, |document| {
            tilo_automerge::ApplyIncoming::load_incoming(document, &changes)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(output)
    }
}
