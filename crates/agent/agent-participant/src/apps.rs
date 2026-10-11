//! Apps as an agent uses them: each record a replica of its resource,
//! edited through the resource's own door, with nothing rendered.
//!
//! [`Replica`] holds one record of any app of the catalogue as a framework
//! [`Participant`] of the app's resource kind ([`ui::framework::hosted_kinds`]):
//! what the agent may do is exactly what the resource publishes — its
//! declared and bound document operations — checked against the agent's
//! permissions, applied under its own actor and attributed to it, exactly as
//! a person's window applies them. Views and effects are the app's own to
//! run where it is open: an agent's confined worker mounts the record's
//! component for the effects it runs (`worker/effects.rs`).
//!
//! Durable state moves as Automerge changes: [`Replica::take_mutation`]
//! returns this participant's own changes since the last call (never ones it
//! received), and [`Replica::apply`] merges another participant's.

use collab_types::DocumentMutation;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_browser_peer::transport::presence_anchors;
use tilo_relay_protocol::live::LiveAnchor;
use ui::application::ApplicationDefinition;
use ui::collaboration::PresencePayload;
use ui::framework::{Participant, ResourceSelection};
use ui::permissions::{Actor, PermissionContext, Principal, Role};
use ui::portable_storage::PortableStorage;
use ui::record::{RecordCheckout, RecordId, RecordOwners};
use ui::tilo_resource::ResourceKind;

/// The apps an agent can open, by app id.
#[derive(Clone)]
pub struct AppCatalogue {
    apps: Vec<&'static ApplicationDefinition>,
}

/// Every app the browser desktop registers (from the app manifests): what a
/// browser session can mount, and so what an agent in it can open.
#[must_use]
pub fn component_apps() -> Vec<&'static ApplicationDefinition> {
    crate::web_apps::web_apps()
}

impl AppCatalogue {
    /// `apps`.
    #[must_use]
    pub fn new(apps: Vec<&'static ApplicationDefinition>) -> Self {
        Self { apps }
    }

    /// [`component_apps`].
    #[must_use]
    pub fn native() -> Self {
        Self::new(component_apps())
    }

    /// The definition of `app_id`.
    #[must_use]
    pub fn get(&self, app_id: &str) -> Option<&'static ApplicationDefinition> {
        self.apps
            .iter()
            .copied()
            .find(|definition| definition.app_id == app_id)
    }

    /// Every app, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &'static ApplicationDefinition> + '_ {
        self.apps.iter().copied()
    }

    /// Read existing resource view descriptors without mounting or initializing
    /// an application. The document remains owned by its original room.
    #[must_use]
    pub fn resource_views(
        &self,
        app_id: &str,
        doc: &automerge::AutoCommit,
        wanted: &tilo_actions::remote::ViewResourceQuery,
        permissions: &PermissionContext,
    ) -> Vec<ActionDescriptor> {
        let Some(kind) = self.kind(app_id) else {
            return Vec::new();
        };
        discover_views(kind, app_id, doc, wanted, permissions)
    }

    /// What the record `doc` of `app_id` is called, as its app names it in
    /// the palette and in history (a document's first heading): its root
    /// resource's label. `None` when the app gives it no name of its own
    /// (its resource type's generic label) or `doc` is not such a record.
    #[must_use]
    pub fn resource_title(&self, app_id: &str, doc: &automerge::AutoCommit) -> Option<String> {
        let kind = self.kind(app_id)?;
        let state = kind.read(doc).ok()?;
        let name = kind.name(state.as_ref());
        let name = name.trim();
        (!name.is_empty() && name != kind.label()).then(|| name.to_owned())
    }

    /// The resource type `app_id`'s records hold.
    fn kind(&self, app_id: &str) -> Option<ResourceKind> {
        let definition = self.get(app_id)?;
        ui::framework::hosted_kinds()
            .get(definition.record_kind.as_str())
            .copied()
    }
}

/// Which record a [`Replica`] holds.
pub enum Seed {
    /// A new record, its resource's genesis.
    New,
    /// An existing record, from the durable changes the host replayed.
    Existing {
        /// The record's id.
        record: RecordId,
        /// Its Automerge changes (a snapshot, then increments), in order.
        changes: Vec<Vec<u8>>,
    },
}

/// Why a record could not be held or merged.
#[derive(Debug)]
pub enum AppError {
    /// No such app in the catalogue, or no resource type for it.
    UnknownApp(String),
    /// The new record could not be created.
    Launch(String),
    /// The record's durable changes could not be merged.
    Merge(String),
    /// The app worker that holds the record did not start, could not
    /// confine itself, or stopped ([`crate::worker`]).
    Confinement(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApp(app) => write!(f, "no app {app} can be opened here"),
            Self::Launch(reason) => write!(f, "the record could not be created: {reason}"),
            Self::Merge(reason) => write!(f, "the record did not merge: {reason}"),
            Self::Confinement(reason) => write!(f, "the app worker failed: {reason}"),
        }
    }
}

impl std::error::Error for AppError {}

/// One record of one app, held as a replica of its resource.
pub struct Replica {
    app_id: &'static str,
    record: RecordId,
    participant: Participant,
    /// The whole document is still to be sent: the record is new here.
    fresh: bool,
    /// This participant's changes not taken yet.
    pending: Vec<Vec<u8>>,
}

impl Replica {
    /// Hold a record of `app_id` in `room` as `participant` (its author id).
    ///
    /// `agent` makes the participant an agent principal holding the editor
    /// role; otherwise it is a person holding the owner role, as a browser
    /// tab of the record's owner does.
    ///
    /// # Errors
    ///
    /// Returns why the app is unknown, the record could not be created, or
    /// its changes do not hold a record of the app's kind.
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        room: &str,
        participant: &str,
        agent: bool,
        seed: Seed,
    ) -> Result<Self, AppError> {
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let kind = catalogue
            .kind(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let actor = if agent {
            Actor {
                role: Some(Role::Editor),
                key: None,
                principals: vec![Principal::Agent(participant.to_owned())],
            }
        } else {
            Actor::person(Some(Role::Owner), participant)
        };
        let permissions = PermissionContext::from_capabilities(actor, Vec::new());
        let (record, bytes, fresh) = match seed {
            Seed::New => {
                let checkout = if definition.keeps_room_record() {
                    RecordCheckout::package_primary(definition.record_kind.clone())
                } else {
                    RecordCheckout::new(RecordId::minted(), definition.record_kind.clone())
                };
                (
                    checkout.id.clone(),
                    genesis(definition, room, &checkout, kind, participant)?,
                    true,
                )
            }
            Seed::Existing { record, changes } => {
                if definition.keeps_room_record() != record.is_package_primary() {
                    return Err(AppError::Merge(
                        "record route does not match the app's room scope".into(),
                    ));
                }
                let mut doc = automerge::AutoCommit::new();
                for change in &changes {
                    tilo_automerge::ApplyIncoming::load_incoming(&mut doc, change)
                        .map_err(|error| AppError::Merge(error.to_string()))?;
                }
                (record, doc.save(), false)
            }
        };
        let participant = Participant::from_snapshot(kind, &bytes, participant, permissions)
            .map_err(|error| AppError::Merge(error.to_string()))?;
        Ok(Self {
            app_id: definition.app_id,
            record,
            participant,
            fresh,
            pending: Vec::new(),
        })
    }

    /// The app's id.
    #[must_use]
    pub const fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// The record's id, as its durable route names it.
    #[must_use]
    pub fn record(&self) -> &str {
        self.record.as_str()
    }

    /// Collaboration route of this record. A package-primary storage key is
    /// internal custody; its existing room-level route has an empty record id.
    #[must_use]
    pub fn wire_record(&self) -> &str {
        if self.record.is_package_primary() {
            ""
        } else {
            self.record.as_str()
        }
    }

    /// The record's id.
    #[must_use]
    pub const fn record_id(&self) -> &RecordId {
        &self.record
    }

    /// Where the agent's last operation left it, as the live plane's
    /// selection anchors every collaborator's window resolves against the
    /// record's own items: the paragraph it wrote and the caret at its end,
    /// the item it acted on. `None` before any operation, or for one that
    /// lands nowhere in particular.
    #[must_use]
    pub fn presence(&self) -> Option<(LiveAnchor, LiveAnchor)> {
        let place = self.participant.place()?.clone();
        let (key, payload) =
            ResourceSelection::from(place).presence_of(self.participant.kind().kind(), self.app_id);
        let (start, end) = match payload {
            PresencePayload::Caret { pos } => (pos, pos),
            PresencePayload::Selection { start, end } => (start, end),
            _ => return None,
        };
        presence_anchors(&key, start, end)
    }

    /// Every document operation the participant may perform now, as inert
    /// descriptors.
    #[must_use]
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        self.participant.operations()
    }

    /// Discover one existing resource's local view metadata under the caller's
    /// current read policy. Views are inert; `invoke` remains document-only.
    #[must_use]
    pub fn resource_views(
        &self,
        id: &str,
        kind: &str,
        permissions: &PermissionContext,
    ) -> Vec<ActionDescriptor> {
        discover_views(
            *self.participant.kind(),
            self.app_id,
            self.participant.doc(),
            &tilo_actions::remote::ViewResourceQuery {
                id: id.to_owned(),
                kind: kind.to_owned(),
            },
            permissions,
        )
    }

    /// Perform one published operation through the resource's own door:
    /// permission, confirmation and strict argument binding exactly as for
    /// a person. The change it made is taken with [`Self::take_mutation`].
    ///
    /// # Errors
    ///
    /// Returns the door's refusal or the action's failure.
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, DoorError> {
        let changes = if confirm {
            self.participant.invoke_confirmed(address, arguments)?
        } else {
            self.participant.invoke(address, arguments)?
        };
        if let Some(changes) = changes {
            self.pending.push(changes);
        }
        Ok(ActionOutput::default())
    }

    /// The record's current heads.
    pub(crate) fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.participant.heads()
    }

    /// The whole record, saved from a copy so that what
    /// [`Self::take_mutation`] returns next is unchanged.
    pub(crate) fn snapshot(&self) -> Vec<u8> {
        self.participant.doc().clone().save()
    }

    /// The participant's permissions.
    pub(crate) fn permissions(&self) -> &PermissionContext {
        self.participant.permissions()
    }

    /// Who the participant is.
    pub(crate) fn participant_id(&self) -> &str {
        self.participant.me()
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&automerge::AutoCommit) -> R) -> Option<R> {
        Some(read(self.participant.doc()))
    }

    /// The tree the record publishes to this participant now.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
        self.participant.resources()
    }

    /// This participant's changes since the last call, or `None`. The first
    /// call on a new record returns the whole document as a snapshot.
    pub fn take_mutation(&mut self) -> Option<DocumentMutation> {
        if std::mem::take(&mut self.fresh) {
            self.pending.clear();
            return Some(DocumentMutation::Snapshot(self.participant.save()));
        }
        let own: Vec<u8> = std::mem::take(&mut self.pending).concat();
        (!own.is_empty()).then_some(DocumentMutation::Incremental(own))
    }

    /// Merge another participant's durable change. Changes that build on
    /// nothing this replica holds belong to another record and are left
    /// alone; one already merged changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when a whole document is not one of this
    /// record's kind.
    pub fn apply(&mut self, mutation: &DocumentMutation) -> Result<(), AppError> {
        match mutation {
            DocumentMutation::Snapshot(bytes) => {
                if !self.participant.merge_snapshot(bytes) {
                    return Err(AppError::Merge("the document is not this record".into()));
                }
            }
            DocumentMutation::Incremental(bytes) => {
                self.participant.merge_changes(bytes);
            }
        }
        Ok(())
    }
}

/// Device name a record created here is seeded on.
const DEVICE: &str = "agent";

/// A new record of `kind`: its genesis, with the identity its route names,
/// as every member creating it derives it, then what its app seeds a record
/// `creator` creates with.
fn genesis(
    definition: &ApplicationDefinition,
    room: &str,
    checkout: &RecordCheckout,
    kind: ResourceKind,
    creator: &str,
) -> Result<Vec<u8>, AppError> {
    let launch = |error: &dyn std::fmt::Display| AppError::Launch(error.to_string());
    let cache = PortableStorage::new("agent-records", DEVICE).map_err(|e| launch(&e))?;
    let records = RecordOwners::portable(cache)
        .open(definition.app_id, room)
        .map_err(|e| launch(&e))?;
    records
        .reserve_record(checkout)
        .map_err(|e| launch(&e))?
        .initialize_seeded(
            |doc| kind.initialize(doc).map_err(|error| error.to_string()),
            |doc| ui::framework::seed_record_in_room(kind.kind(), doc, creator, DEVICE, room),
        )
        .map_err(|e| launch(&e))?;
    records
        .read(&checkout.id, |doc| doc.clone().save())
        .ok_or_else(|| AppError::Launch("the new record did not open".into()))
}

fn discover_views(
    resource_kind: ResourceKind,
    app_id: &str,
    doc: &automerge::AutoCommit,
    wanted: &tilo_actions::remote::ViewResourceQuery,
    permissions: &PermissionContext,
) -> Vec<ActionDescriptor> {
    let published = match resource_kind.child(doc, &wanted.id) {
        Ok(Some(child)) if child.id.as_str() == wanted.id => child,
        _ => match resource_kind.publish(resource_kind.kind(), doc) {
            Ok(published) => published,
            Err(_) => return Vec::new(),
        },
    };
    let Some(permitted) = permissions.permitted_resource(app_id, published) else {
        return Vec::new();
    };
    tilo_actions::door::survey(&[permitted])
        .into_iter()
        .filter(|descriptor| {
            descriptor.address.resource.as_str() == wanted.id
                && descriptor.resource_kind == wanted.kind
                && descriptor.kind == tilo_actions::OperationKind::View
                && permissions.check(&ui::permissions::PermissionScope::action(
                    app_id,
                    descriptor.address.resource.clone(),
                    descriptor.address.action.clone(),
                    collab_types::caps::Verb::Read,
                ))
        })
        .collect()
}
