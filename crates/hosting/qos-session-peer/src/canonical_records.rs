//! Installed canonical record factories shared by the real governor adapters.
//!
//! Prepared records are uncommitted data, not admission or durability witnesses.
//! The caller must keep its actual owner turn, current caller evidence and route
//! CAS through publication. This module owns neither a writer nor a signing key.
pub mod creation;

use collab_types::{
    ArtifactKind,
    caps::{Actor, CapDecision, Principal, ResourceRef, Verb, decide_capability},
};
use qos_peer_state::StatePeer;
use tilo_user_identity::DeviceCert;
use ui::{application::ApplicationDefinition, automerge::AutoCommit, record::RecordCheckout};
use web_time::SystemTime;

/// A finite installed entry. Remote labels cannot supply an initializer.
pub struct CheckedCanonicalApplication {
    app_id: &'static str,
    definition: fn() -> &'static ApplicationDefinition,
    initialize: fn(&mut AutoCommit) -> Result<(), String>,
}

static INSTALLED: [CheckedCanonicalApplication; 4] = [
    CheckedCanonicalApplication {
        app_id: "dev.qos.word-processor",
        definition: word_processor::definition,
        initialize: word_processor::initialize_empty_document,
    },
    CheckedCanonicalApplication {
        app_id: "dev.qos.notes",
        definition: notes::definition,
        initialize: notes::notebook::initialize_empty,
    },
    CheckedCanonicalApplication {
        app_id: "dev.qos.calculator",
        definition: calculator::definition,
        initialize: calculator::doc::initialize_empty,
    },
    CheckedCanonicalApplication {
        app_id: "dev.qos.chat",
        definition: chat::definition,
        initialize: initialize_chat,
    },
];

fn initialize_chat(doc: &mut AutoCommit) -> Result<(), String> {
    chat::doc::ChatDoc::seed_starter(doc, "native", "Native workspace peer", "NW");
    Ok(())
}

pub fn installed() -> impl ExactSizeIterator<Item = &'static CheckedCanonicalApplication> {
    INSTALLED.iter()
}

pub fn resolve(app: &str) -> Option<&'static CheckedCanonicalApplication> {
    installed().find(|entry| entry.app_id == app)
}

/// Result of a current factory decision, not a committed creation result.
/// No public constructor, serialized form, owner receipt or positive resource
/// qualification can be obtained from this value.
pub struct PreparedCanonicalRecord {
    app: &'static str,
    record: String,
    document: Option<Vec<u8>>,
}

impl PreparedCanonicalRecord {
    pub fn record(&self) -> &str {
        &self.record
    }

    /// Existing primary records have no new document. Newly prepared bytes must
    /// enter the actual owner's candidate together with its topology/result.
    pub fn into_new_document(self) -> Option<(String, String, Vec<u8>)> {
        self.document
            .map(|bytes| (self.app.to_owned(), self.record, bytes))
    }
}

impl CheckedCanonicalApplication {
    /// Exact durable route identity, retained independently of package renames.
    pub fn app_id(&self) -> &'static str {
        self.app_id
    }

    pub fn definition(&self) -> &'static ApplicationDefinition {
        (self.definition)()
    }

    /// Explicit factory schema revision, independent of display/package aliases.
    pub fn registry_revision(&self) -> [u8; 32] {
        let bytes =
            postcard::to_stdvec(&(self.app_id, self.definition().record_kind.as_str(), 1u16))
                .expect("fixed checked registry encoding");
        blake3::derive_key("qOS checked canonical factory v1", &bytes)
    }

    /// Prepare the actual host-allocated OpenTab route. The owner adapter has
    /// already checked the tab's current topology namespace and source command;
    /// this method does not authenticate a tab number or a remote invocation.
    pub fn prepare_for_tab(
        &self,
        peer: &StatePeer,
        caller: &DeviceCert,
        tab: u64,
        now: SystemTime,
    ) -> Result<PreparedCanonicalRecord, String> {
        if tab == 0 {
            return Err("canonical tab identity is zero".into());
        }
        let definition = self.definition();
        let checkout = qos_desktop::records::component_record(
            self.app_id,
            tab,
            definition.record_kind.clone(),
        );
        self.prepare(peer, caller, checkout, now)
    }

    fn prepare(
        &self,
        peer: &StatePeer,
        caller: &DeviceCert,
        checkout: RecordCheckout,
        now: SystemTime,
    ) -> Result<PreparedCanonicalRecord, String> {
        let app = self.app_id;
        if !peer.certificates().contains(caller) {
            return Err("canonical record caller is no longer admitted".into());
        }
        // Current membership, full certificate and security prerequisites are
        // repeated against this owner snapshot, even for existing primary reads.
        peer.permissions(caller.device_public_key, now)?;
        peer.validated_media_policy()?;
        let record = if checkout.id.is_package_primary() {
            String::new()
        } else {
            checkout.id.as_str().to_owned()
        };
        peer.check_document_read(caller.device_public_key, app, &record, now)?;
        if peer.document(app, &record).is_some() {
            if !checkout.id.is_package_primary() {
                return Err("new tab identity already has retained record custody".into());
            }
            return Ok(PreparedCanonicalRecord {
                app,
                record,
                document: None,
            });
        }
        authorize_creation(peer, caller, app, &record, now)?;

        // Preserve the existing checked SDK metadata/default initializer path.
        // It constructs only a detached portable candidate, never an owner or
        // canonical native saver. The actual owner publishes the returned bytes.
        let cache = ui::portable_storage::PortableStorage::new("desktop-command", peer.room())?;
        let records = ui::record::RecordOwners::portable(cache)
            .open(app, peer.room())
            .map_err(|e| e.to_string())?;
        records
            .reserve_record(&checkout)
            .map_err(|e| e.to_string())?
            .initialize(self.initialize)
            .map_err(|e| e.to_string())?;
        let bytes = records
            .read(&checkout.id, |doc| doc.clone().save())
            .ok_or("checked new record custody missing")?;
        Ok(PreparedCanonicalRecord {
            app,
            record,
            document: Some(bytes),
        })
    }
}

/// One current application creation policy for both governor adapters. The
/// normal governed capability evaluator remains the policy engine; governor
/// signing authority cannot replace the initiating person's rights.
pub(crate) fn authorize_creation(
    peer: &StatePeer,
    caller: &DeviceCert,
    app: &str,
    record: &str,
    now: SystemTime,
) -> Result<(), String> {
    let (role, caps, person) = peer.permissions(caller.device_public_key, now)?;
    let actor = Actor::person(Some(role), person)
        .with_principal(Principal::App(app.into()))
        .in_room(peer.room());
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "canonical creation time predates epoch")?
        .as_secs();
    let action = ResourceRef::Action {
        app: app.into(),
        action: "document.create".into(),
    };
    if decide_capability(&actor, &action, Verb::Write, &caps, Some(seconds)) != CapDecision::Allow {
        return Err("caller cannot create an application document".into());
    }
    let artifact = ResourceRef::Artifact {
        app: Some(app.into()),
        kind: ArtifactKind::Note,
        instance: collab_types::document_artifact_id(app, peer.room(), record),
    };
    if decide_capability(&actor, &artifact, Verb::Write, &caps, Some(seconds)) != CapDecision::Allow
    {
        return Err("caller cannot write the new application document".into());
    }
    Ok(())
}
