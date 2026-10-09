//! Canonical records created by the governing host's compiled application factory.
//! This is a host-local operation; incoming member documents cannot mint ownership.

use super::{CollaborationError, Rooms, system_time};
use tilo_agent_participant::{AppCatalogue, ConfinedReplica, Seed};
use tilo_user_identity::DeviceCert;

impl Rooms {
    /// Author an existing native primary record as its exact current governor.
    /// The registry and synchronous change come from trusted host code. This
    /// host-local API accepts no incoming member snapshot or factory. It keeps
    /// the existing graph and publishes only its admitted incremental changes.
    ///
    /// # Errors
    /// Refuses other certificates, stale custody, undeclared/non-primary apps,
    /// missing records, sealed records, failed changes or oversized publication.
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
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(governor, now, "application.author")?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application authoring requires exact governor custody",
            ));
        }
        let definition = catalogue
            .get(app)
            .filter(|definition| definition.keeps_room_record())
            .ok_or_else(|| {
                CollaborationError::new("application has no registered primary record")
            })?;
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "application governor differs from current room authority",
            ));
        }
        governed
            .peer()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        governed.peer().check_document_read(
            governor.device_public_key,
            app,
            "",
            system_time(now),
        )?;
        let heads = governed
            .peer()
            .plaintext_complete_heads(app, "")
            .ok_or_else(|| CollaborationError::new("native primary is not complete plaintext"))?;
        let bytes = governed
            .peer()
            .document(definition.app_id, "")
            .ok_or_else(|| CollaborationError::new("native primary record is not prepared"))?;
        let mut document = automerge::AutoCommit::load(&bytes)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let result = change(&mut document)?;
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
        candidate.publish_incremental(app, "", changes, &self.inner.host.key, system_time(now))?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        Ok(result)
    }

    /// Prepare a registered application's primary record with exact governor custody.
    /// The catalogue comes from this host's compiled, trusted application registry.
    /// An existing record remains byte-identical. No member-supplied factory or
    /// snapshot crosses this API; return only after durable signed publication.
    ///
    /// # Errors
    /// Refuses other devices, stale governance, undeclared applications, factories
    /// without a primary record, invalid genesis or failed publication.
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
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(governor, now, "application.prepare")?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application creation requires exact governor custody",
            ));
        }
        let definition = catalogue
            .get(app)
            .ok_or_else(|| CollaborationError::new("application is not registered on this host"))?;
        if !definition.keeps_room_record() {
            return Err(CollaborationError::new(
                "application has no primary room record",
            ));
        }
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "application governor differs from current room authority",
            ));
        }
        governed
            .peer()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        if governed.peer().document(app, "").is_some() {
            return Ok(false);
        }
        // The factory is the app's own code: it runs in a confined app
        // worker, held only while its genesis is taken and admitted here
        // before it is signed.
        let genesis = {
            let source = ConfinedReplica::open(
                catalogue,
                app,
                room,
                &hex::encode(governor.user_public_key),
                false,
                Seed::New,
            )
            .map_err(|error| CollaborationError::new(error.to_string()))?;
            if source.record() != "@tilo:package-primary" {
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
            "",
            automerge::AutoCommit::new().save(),
            &self.inner.host.key,
            system_time(now),
        )?;
        candidate.publish_document(app, "", genesis, &self.inner.host.key, system_time(now))?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        Ok(true)
    }
}
