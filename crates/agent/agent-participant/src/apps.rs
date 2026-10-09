//! Apps as an agent uses them: each record held in the app's own view,
//! attached and never drawn, acted on through that view.
//!
//! [`Replica`] mounts the app's real component over one record, as a window
//! does, on a host without a screen: the record's view is live, and every
//! operation goes through the same door a person's click takes, checked
//! against the participant's permissions and attributed to it. Nothing is
//! performed out of sight of a view; what the participant did is where its
//! presence shows ([`Replica::presence`]). The person's devices draw the
//! same record from synced state when someone looks.
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
use ui::framework::{AttachedView, ResourceSelection};
use ui::permissions::{Actor, PermissionContext, Principal, Role};
use ui::portable_storage::PortableStorage;
use ui::record::{RecordCheckout, RecordId, RecordOwners};
use ui::tilo_resource::{Place, ResourceKind};

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
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApp(app) => write!(f, "no app {app} can be opened here"),
            Self::Launch(reason) => write!(f, "the record could not be created: {reason}"),
            Self::Merge(reason) => write!(f, "the record did not merge: {reason}"),
        }
    }
}

impl std::error::Error for AppError {}

/// One record of one app, held in the app's own view
/// ([`AttachedView`]): mounted on this host, never drawn.
pub struct Replica {
    app_id: &'static str,
    kind: ResourceKind,
    view: AttachedView,
    /// Where its last operation left it.
    place: Option<Place>,
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
    /// its changes do not hold a record of the app's kind, or the app's view
    /// would not open over it.
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
        let (checkout, bytes, created) = match seed {
            Seed::New => {
                let checkout = if definition.keeps_room_record() {
                    RecordCheckout::package_primary(definition.record_kind.clone())
                } else {
                    RecordCheckout::new(RecordId::minted(), definition.record_kind.clone())
                };
                let bytes = genesis(definition, room, &checkout, kind, participant)?;
                (checkout, bytes, true)
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
                let checkout = RecordCheckout::new(record, definition.record_kind.clone());
                (checkout, doc.save(), false)
            }
        };
        {
            let doc = ui::tilo_resource::Doc::load(&bytes)
                .map_err(|error| AppError::Merge(error.to_string()))?;
            kind.read(&doc)
                .map_err(|error| AppError::Merge(error.to_string()))?;
        }
        let view = AttachedView::open(
            definition,
            room,
            checkout,
            bytes,
            participant,
            permissions,
            created,
        )
        .map_err(AppError::Launch)?;
        Ok(Self {
            app_id: definition.app_id,
            kind,
            view,
            place: None,
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
        self.view.checkout().id.as_str()
    }

    /// Collaboration route of this record. A package-primary storage key is
    /// internal custody; its existing room-level route has an empty record id.
    #[must_use]
    pub fn wire_record(&self) -> &str {
        let id = &self.view.checkout().id;
        if id.is_package_primary() {
            ""
        } else {
            id.as_str()
        }
    }

    /// The record's id.
    #[must_use]
    pub const fn record_id(&self) -> &RecordId {
        &self.view.checkout().id
    }

    /// Where the participant's last operation left it, as the live plane's
    /// selection anchors every collaborator's window resolves against the
    /// record's own items: the paragraph it wrote and the caret at its end,
    /// the item it acted on. `None` before any operation, or for one that
    /// lands nowhere in particular.
    #[must_use]
    pub fn presence(&self) -> Option<(LiveAnchor, LiveAnchor)> {
        let place = self.place.clone()?;
        let (key, payload) =
            ResourceSelection::from(place).presence_of(self.kind.kind(), self.app_id);
        let (start, end) = match payload {
            PresencePayload::Caret { pos } => (pos, pos),
            PresencePayload::Selection { start, end } => (start, end),
            _ => return None,
        };
        presence_anchors(&key, start, end)
    }

    /// Every document operation the participant may perform through the
    /// view now, as inert descriptors.
    #[must_use]
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        self.view.operations()
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
        self.read(|doc| {
            discover_views(
                self.kind,
                self.app_id,
                doc,
                &tilo_actions::remote::ViewResourceQuery {
                    id: id.to_owned(),
                    kind: kind.to_owned(),
                },
                permissions,
            )
        })
        .unwrap_or_default()
    }

    /// Perform one published operation through the view, the door a
    /// person's click takes: permission, confirmation and strict argument
    /// binding exactly as for a person. The change it made is taken with
    /// [`Self::take_mutation`].
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
        let output = self.view.invoke(address, arguments, confirm)?;
        let landed = self.view.landed();
        self.place = landed
            .and_then(|envelope| self.read(|doc| self.kind.lands(doc, &envelope.action)))
            .flatten()
            .or_else(|| {
                (address.resource != self.kind.kind())
                    .then(|| Place::Item(address.resource.clone()))
            });
        Ok(output)
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&automerge::AutoCommit) -> R) -> Option<R> {
        self.view.read(read)
    }

    /// The tree the record's view publishes to this participant now.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
        self.view.resources()
    }

    /// This participant's changes since the last call, or `None`. The first
    /// call on a new record returns the whole document as a snapshot.
    pub fn take_mutation(&mut self) -> Option<DocumentMutation> {
        self.view.take_changes()
    }

    /// Merge another participant's durable change into the view's record.
    /// Changes that build on nothing this record holds belong to another
    /// record and are left alone; one already merged changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when a whole document is not one of this
    /// record's kind.
    pub fn apply(&mut self, mutation: &DocumentMutation) -> Result<(), AppError> {
        if let DocumentMutation::Snapshot(bytes) = mutation {
            let holds = ui::tilo_resource::Doc::load(bytes)
                .is_ok_and(|incoming| self.kind.holds(&incoming));
            if !holds {
                return Err(AppError::Merge("the document is not this record".into()));
            }
        }
        self.view.merge(mutation).map_err(AppError::Merge)?;
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
