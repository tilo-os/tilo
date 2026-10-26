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

/// What a bot's changes to `record` build on: the record as the room holds
/// it (`existing`) and the heads the room has of it (all of a `sealed` one,
/// the complete plaintext of any other), or nothing for a part of a chain
/// the room has not got yet, which the bot starts (then `true`).
fn bot_base(
    peer: &tilo_peer_state::StatePeer,
    sealed: bool,
    (app, record): (&str, &str),
    existing: Option<Vec<u8>>,
) -> Result<(automerge::AutoCommit, Vec<automerge::ChangeHash>, bool), CollaborationError> {
    let Some(bytes) = existing else {
        let part = ui::record::RecordId::parse(record).is_some_and(|id| id.chain_part().is_some());
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

/// Start `record`, a new part of a chain, in `candidate` as the room's
/// host, signed with `key`: the genesis `document` begins with, which holds
/// nothing and is the same on every device. Sealed with `keys` for a record
/// members write sealed. The heads after it.
fn start_part(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&super::sealed_records::RecordKeys>,
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
        super::sealed_records::start_sealed(candidate, keys, (app, record), genesis, key, at)?;
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

impl Rooms {
    /// Author an existing native record (`""` for the primary) as its exact
    /// current governor. The registry and synchronous change come from
    /// trusted host code. This host-local API accepts no incoming member
    /// snapshot or factory. It keeps the existing graph and publishes only
    /// its admitted incremental changes, sealed for a record members write
    /// sealed (`super::sealed_records`).
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
        governed.absorb_layout((app, record), &mut document);
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
        let keys = self
            .sealing_keys(&governed, app, record)
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
        let published = publish_authored(
            &mut candidate,
            keys.as_ref(),
            (app, record),
            (&mut document, heads),
            (&self.inner.host.key, system_time(now)),
        )?;
        if !published {
            return Ok(result);
        }
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        governed.absorb_layout((app, record), &mut document);
        self.settle(&mut governed).await?;
        Ok(result)
    }

    /// Publish the changes a bot's run made to a native record (`""` for
    /// the primary), signed by the bot's own device `bot`, which this
    /// governor holds for a bot it installed. Every member admits them as
    /// the bot's: within its grants, and showing another name only when
    /// the room lets the bot. A part of a chain the room has not got yet
    /// (the next part of a chat's messages) is started by the host, its genesis the
    /// same everywhere, and what the bot wrote in it published as the bot's.
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
        let keys = self
            .sealing_keys(&governed, app, record)
            .await
            .map_err(CollaborationError::new)?;
        let existing = governed
            .readable_document(app_id, record)
            .await
            .map_err(CollaborationError::new)?;
        // A part of a chain the room has not got yet (the next part of a
        // chat's messages) is one the bot starts. Its genesis holds nothing
        // and is the same on every device (it meets anyone else's start of
        // it), so the room's host starts the part, as it prepares any record,
        // and what the bot wrote in it is the bot's.
        let (mut document, heads, starts_part) =
            bot_base(governed.peer(), keys.is_some(), (app, record), existing)?;
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
            if record.is_empty() != source.record_id().is_package_primary() {
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

/// Publish what `document` holds beyond `heads` (all of it for a record not
/// yet held), sealed under `keys` where the record's channel has them; false
/// when it holds nothing new.
fn publish_authored(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&super::sealed_records::RecordKeys>,
    (app, record): (&str, &str),
    (document, heads): (
        &mut automerge::AutoCommit,
        Option<Vec<automerge::ChangeHash>>,
    ),
    (key, now): (&ed25519_dalek::SigningKey, std::time::SystemTime),
) -> Result<bool, CollaborationError> {
    if document.get_heads() == heads.clone().unwrap_or_default() {
        return Ok(false);
    }
    let bytes = match (&heads, keys) {
        (Some(heads), _) => document.save_after(heads),
        (None, Some(_)) => document.save_after(&[]),
        (None, None) => document.save(),
    };
    if bytes.len() > 64 * 1024 {
        return Err(CollaborationError::new(
            "record authoring exceeds publication limit",
        ));
    }
    match (heads, keys) {
        (Some(_), keys) => {
            super::sealed_records::publish_changes(
                candidate,
                keys,
                (app, record),
                bytes,
                key,
                now,
            )?;
        }
        (None, Some(keys)) => {
            super::sealed_records::start_sealed(candidate, keys, (app, record), bytes, key, now)?;
        }
        (None, None) => {
            candidate
                .publication_mut()
                .publish_document(app, record, bytes, key, now)?;
        }
    }
    Ok(true)
}

/// Publish an app's `genesis` of a new `record` into `candidate` as the
/// room's host: sealed under `keys` where the record's channel has them, so
/// nothing it holds is ever written in the clear.
fn publish_genesis(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&super::sealed_records::RecordKeys>,
    (app, record): (&str, &str),
    genesis: Vec<u8>,
    (key, now): (&ed25519_dalek::SigningKey, std::time::SystemTime),
) -> Result<(), CollaborationError> {
    if let Some(keys) = keys {
        let changes = automerge::AutoCommit::load(&genesis)
            .map_err(|error| CollaborationError::new(error.to_string()))?
            .save_after(&[]);
        super::sealed_records::start_sealed(candidate, keys, (app, record), changes, key, now)?;
    } else {
        // Establish ownership through the same governor-only StatePeer path
        // as native prepare_application_record, then merge the standard
        // factory.
        candidate.publication_mut().publish_document(
            app,
            record,
            automerge::AutoCommit::new().save(),
            key,
            now,
        )?;
        candidate
            .publication_mut()
            .publish_document(app, record, genesis, key, now)?;
    }
    Ok(())
}
