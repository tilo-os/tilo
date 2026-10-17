//! Canonical records created by the governing host's compiled application factory.
//! This is a host-local operation; incoming member documents cannot mint ownership.
//!
//! A record is the app's primary (`""`, for an app that keeps one record
//! per room) or a named record of the room: any app's, under an id the
//! caller chooses, such as the room's default record every member's first
//! window of the app opens.

use super::authority::RoomOp;
use super::{CollaborationError, Rooms, system_time};
use crate::host_authority::HostOperation;
use tilo_agent_participant::{AppCatalogue, ConfinedReplica, Seed};
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
    if record.is_empty() && !definition.keeps_room_record() {
        return Err(CollaborationError::new(
            "application has no primary room record",
        ));
    }
    Ok(definition.app_id)
}

impl Rooms {
    /// Author an existing native record (`""` for the primary) as its exact
    /// current governor. The registry and synchronous change come from
    /// trusted host code. This host-local API accepts no incoming member
    /// snapshot or factory. It keeps the existing graph and publishes only
    /// its admitted incremental changes, sealed for a record members write
    /// sealed ([`super::sealed_records`]).
    ///
    /// # Errors
    /// Refuses other certificates, stale custody, undeclared apps, a primary
    /// of an app without one, missing records, sealed records whose channel
    /// this relay does not steward, failed changes or oversized publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn author_application<R>(
        &self,
        room: &str,
        governor: &DeviceCert,
        catalogue: &AppCatalogue,
        app: &str,
        record: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            RoomOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application authoring requires exact governor custody",
            ));
        }
        let app_id = app_id_for(catalogue, app, record)?;
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "application governor differs from current room authority",
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
        let keys = governed
            .record_keys(app, record)
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
        super::sealed_records::publish_changes(
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
        governed.absorb_desktop((app, record), &mut document);
        self.settle(&mut governed).await?;
        Ok(result)
    }

    /// Author a room record no app factory makes, such as the shared
    /// desktop members' pages keep, as its exact current governor: an
    /// existing record keeps its graph and gains only the change, and an
    /// absent one starts from it. Members' pages admit it like any member's
    /// change to that record.
    ///
    /// # Errors
    /// Refuses other certificates, stale custody, incomplete records, sealed
    /// ones whose channel this relay does not steward, failed changes or
    /// oversized publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn author_room_record<R>(
        &self,
        room: &str,
        governor: &DeviceCert,
        (app, record): (&str, &str),
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            RoomOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "record authoring requires exact governor custody",
            ));
        }
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "record governor differs from current room authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        let keys = governed
            .record_keys(app, record)
            .await
            .map_err(CollaborationError::new)?;
        let existing = governed
            .readable_document(app, record)
            .await
            .map_err(CollaborationError::new)?;
        let (mut document, heads) = match existing {
            Some(bytes) => {
                let mut document = automerge::AutoCommit::load(&bytes)
                    .map_err(|error| CollaborationError::new(error.to_string()))?;
                let heads = match &keys {
                    Some(_) => document.get_heads(),
                    None => governed
                        .peer()
                        .plaintext_complete_heads(app, record)
                        .ok_or_else(|| {
                            CollaborationError::new("record is not complete plaintext")
                        })?,
                };
                (document, Some(heads))
            }
            None => (automerge::AutoCommit::new(), None),
        };
        let result = change(&mut document)?;
        if automerge::transaction::Transactable::pending_ops(&document) != 0 {
            document.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
        }
        let mut candidate = governed.peer().clone();
        match heads {
            Some(heads) if document.get_heads() == heads => return Ok(result),
            Some(heads) => {
                let changes = document.save_after(&heads);
                if changes.len() > 64 * 1024 {
                    return Err(CollaborationError::new(
                        "record authoring exceeds publication limit",
                    ));
                }
                super::sealed_records::publish_changes(
                    &mut candidate,
                    keys.as_ref(),
                    (app, record),
                    changes,
                    &self.inner.host.key,
                    system_time(now),
                )?;
            }
            None if document.get_heads().is_empty() => return Ok(result),
            None => {
                let bytes = document.save();
                if bytes.len() > 64 * 1024 {
                    return Err(CollaborationError::new(
                        "record authoring exceeds publication limit",
                    ));
                }
                candidate.publish_document(
                    app,
                    record,
                    bytes,
                    &self.inner.host.key,
                    system_time(now),
                )?;
            }
        }
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        governed.absorb_desktop((app, record), &mut document);
        self.settle(&mut governed).await?;
        Ok(result)
    }

    /// Publish the changes a bot's run made to a native record (`""` for
    /// the primary), signed by the bot's own device `bot`, which this
    /// governor holds for a bot it installed. Every member admits them as
    /// the bot's: within its grants, and showing another name only when
    /// the room lets the bot.
    ///
    /// # Errors
    /// Refuses another governor's certificate, stale custody, undeclared
    /// apps, missing records, sealed ones whose channel this relay does not
    /// steward, a bot the room does not let act, a change outside its
    /// grants, or oversized publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn publish_as_bot(
        &self,
        room: &str,
        governor: &DeviceCert,
        bot: &ed25519_dalek::SigningKey,
        catalogue: &AppCatalogue,
        app: &str,
        record: &str,
        changes: &[u8],
    ) -> Result<(), CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            RoomOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "bot publication requires exact governor custody",
            ));
        }
        let app_id = app_id_for(catalogue, app, record)?;
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "application governor differs from current room authority",
            ));
        }
        // A record members write sealed is read through its channel's keys,
        // and the bot's changes reach it sealed, decided as the bot's own.
        let keys = governed
            .record_keys(app, record)
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
        tilo_automerge::ApplyIncoming::load_incoming(&mut document, changes)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        // Only what the room lacks: a run's replica also holds the record's
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
        let mut candidate = governed.peer().clone();
        match &keys {
            Some(keys) => {
                super::sealed_records::publish_changes_for(
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
                candidate.publish_incremental(app, record, fresh, bot, system_time(now))?;
            }
        }
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        // The room decides the bot's changes like any member's: one it
        // refused or holds back never reaches the record.
        let wanted = document.get_heads();
        let taken = governed
            .readable_document(app_id, record)
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .is_some_and(|mut kept| {
                wanted
                    .iter()
                    .all(|head| kept.get_change_by_hash(head).is_some())
            });
        if !taken {
            return Err(CollaborationError::new(
                "the room did not take the bot's change",
            ));
        }
        Ok(())
    }

    /// Prepare a registered application's record (`""` for the primary)
    /// with exact governor custody, from the app's own factory. The
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
        reason = "the governor transaction retains its room lock through durable signed publication"
    )]
    pub async fn prepare_application(
        &self,
        room: &str,
        governor: &DeviceCert,
        catalogue: &AppCatalogue,
        app: &str,
        record: &str,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            RoomOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application creation requires exact governor custody",
            ));
        }
        app_id_for(catalogue, app, record)?;
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "application governor differs from current room authority",
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
        // A record a room route names (the room's default record) is
        // created under that id: a genesis naming another id would be
        // published there, and no member could open it.
        let seed =
            if record.is_empty() {
                Seed::New
            } else {
                Seed::NewAt(ui::record::RecordId::parse(record).ok_or_else(|| {
                    CollaborationError::new("application record is not a record id")
                })?)
            };
        let genesis = {
            let source = ConfinedReplica::open(
                catalogue,
                app,
                room,
                &hex::encode(governor.user_public_key),
                &tilo_agent_participant::ReplicaActor::Owner,
                seed,
            )
            .map_err(|error| CollaborationError::new(error.to_string()))?;
            if record.is_empty() != (source.record() == "@tilo:package-primary") {
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
        let mut candidate = governed.peer().clone();
        // Establish ownership through the same governor-only StatePeer path as
        // native prepare_application_record, then merge the standard factory.
        candidate.publish_document(
            app,
            record,
            automerge::AutoCommit::new().save(),
            &self.inner.host.key,
            system_time(now),
        )?;
        candidate.publish_document(app, record, genesis, &self.inner.host.key, system_time(now))?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        Ok(true)
    }
}
