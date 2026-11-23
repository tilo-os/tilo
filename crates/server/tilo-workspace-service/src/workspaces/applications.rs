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

/// What a bot's changes to `record` build on: the record as the workspace holds
/// it (`existing`) and the heads the workspace has of it (all of a `sealed` one,
/// the complete plaintext of any other), or nothing for a part of a chain
/// the workspace has not got yet, which the bot starts (then `true`).
fn bot_base(
    peer: &tilo_peer_state::StatePeer,
    sealed: bool,
    (app, record): (&str, &str),
    existing: Option<Vec<u8>>,
) -> Result<(automerge::AutoCommit, Vec<automerge::ChangeHash>, bool), CollaborationError> {
    let Some(bytes) = existing else {
        let part = collab_types::document::DocumentId::parse(record)
            .is_some_and(|id| id.chain_part().is_some());
        return if part {
            Ok((automerge::AutoCommit::new(), Vec::new(), true))
        } else {
            Err(CollaborationError::new("native record is not prepared"))
        };
    };
    let mut document = automerge::AutoCommit::load(&bytes)
        .map_err(|error| CollaborationError::new(error.to_string()))?;
    let heads = if sealed {
        document.get_heads()
    } else {
        peer.plaintext_complete_heads(app, record)
            .ok_or_else(|| CollaborationError::new("native record is not complete plaintext"))?
    };
    Ok((document, heads, false))
}

/// Start `record`, a new part of a chain, in `candidate` as the workspace's
/// host, signed with `key`: the genesis `document` begins with, which holds
/// nothing and is the same on every device. Sealed with `keys` for a record
/// members write sealed. The heads after it.
fn start_part(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&super::sealed_documents::DocumentKeys>,
    (app, record): (&str, &str),
    document: &mut automerge::AutoCommit,
    (key, at): (&ed25519_dalek::SigningKey, std::time::SystemTime),
) -> Result<Vec<automerge::ChangeHash>, CollaborationError> {
    let (hash, genesis) = document
        .get_changes(&[])
        .into_iter()
        .find(|change| change.deps().is_empty())
        .map(|change| (change.hash(), change.raw_bytes().to_vec()))
        .ok_or_else(|| CollaborationError::new("the new part has no genesis"))?;
    if let Some(keys) = keys {
        super::sealed_documents::start_sealed(candidate, keys, (app, record), genesis, key, at)?;
    } else {
        let mut started = automerge::AutoCommit::new();
        tilo_automerge::ApplyIncoming::load_incoming(&mut started, &genesis)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        candidate
            .publication_mut()
            .publish_document(app, record, started.save(), key, at)?;
    }
    Ok(vec![hash])
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
        Ok(result)
    }

    /// Current access of a bot device, as every other device decides its signed writes.
    ///
    /// # Errors
    /// The workspace does not admit the device.
    pub async fn bot_access(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Result<tilo_action_policy::MemberAccess, CollaborationError> {
        let state = self.workspace(workspace).await?;
        state
            .lock()
            .await
            .peer()
            .permissions(device, tilo_clock::wall())
            .map_err(|error| CollaborationError::new(error.to_string()))
    }

    /// Publish the changes a bot's run made to a native record (`""` for
    /// the primary), signed by the bot's own device `bot`, which this
    /// governor holds for a bot it installed. Every member admits them as
    /// the bot's: within its grants, and showing another name only when
    /// the workspace lets the bot. A part of a chain the workspace has not got yet
    /// (the next part of a chat's messages) is started by the host, its genesis the
    /// same everywhere, and what the bot wrote in it published as the bot's.
    ///
    /// # Errors
    /// Refuses another governor's certificate, stale ownership, undeclared
    /// apps, missing records, sealed ones whose channel this server does not
    /// take part in, a bot the workspace does not let act, a change outside its
    /// grants, or oversized publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn publish_as_bot(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        bot: &ed25519_dalek::SigningKey,
        catalogue: &AppCatalogue,
        app: &str,
        record: &str,
        changes: &[u8],
    ) -> Result<(), CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "bot publication requires exact governor ownership",
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
        // A record members write sealed is read through its channel's keys,
        // and the bot's changes reach it sealed, decided as the bot's own.
        let keys = self
            .sealing_keys(&governed, app, record)
            .await
            .map_err(CollaborationError::new)?;
        let existing = governed
            .readable_document(app_id, record)
            .await
            .map_err(CollaborationError::new)?;
        // A part of a chain the workspace has not got yet (the next part of a
        // chat's messages) is one the bot starts. Its genesis holds nothing
        // and is the same on every device (it meets anyone else's start of
        // it), so the workspace's host starts the part, as it prepares any record,
        // and what the bot wrote in it is the bot's.
        let (mut document, heads, starts_part) =
            bot_base(&governed.peer(), keys.is_some(), (app, record), existing)?;
        tilo_automerge::ApplyIncoming::load_incoming(&mut document, changes)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let mut candidate = governed.peer().clone();
        let heads = if starts_part {
            start_part(
                &mut candidate,
                keys.as_ref(),
                (app, record),
                &mut document,
                (&self.inner.host.key, system_time(now)),
            )?
        } else {
            heads
        };
        // Only what the workspace lacks: a run's replica also holds the record's
        // history it started from.
        let fresh = document.save_after(&heads);
        if fresh.is_empty() || document.get_heads() == heads {
            return Ok(());
        }
        if fresh.len() > 64 * 1024 {
            return Err(CollaborationError::new(
                "bot publication exceeds publication limit",
            ));
        }
        match &keys {
            Some(keys) => {
                super::sealed_documents::publish_changes_for(
                    &mut candidate,
                    keys,
                    bot,
                    (app, record),
                    fresh,
                    &self.inner.host.key,
                    system_time(now),
                )?;
            }
            None => {
                candidate.publication_mut().publish_incremental(
                    app,
                    record,
                    fresh,
                    bot,
                    system_time(now),
                )?;
            }
        }
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        // The workspace decides the bot's changes like any member's: one it
        // refused or holds back never reaches the record.
        let wanted = document.get_heads();
        let taken = governed
            .readable_document(app_id, record)
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .is_some_and(|kept| wanted.iter().all(|head| kept.has_change(head)));
        if !taken {
            return Err(CollaborationError::new(
                "the workspace did not take the bot's change",
            ));
        }
        Ok(())
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
}
