//! An app's view of one record, attached and never drawn: how a host
//! without a screen — an agent host, a relay, a room service — performs
//! actions.
//!
//! Nothing is drawn if no one is looking, but no action is performed out of
//! sight. An [`AttachedView`] mounts the app's real component over the
//! record exactly as a window does, lays it out and never paints it; every
//! operation goes through the view's bound dispatcher, the door a person's
//! click takes, admitted for the participant as invoker and checked against
//! its permissions. Where the action landed is where the participant's
//! presence shows ([`AttachedView::landed`]), and the person's devices draw
//! the same record from synced state when someone looks.
//!
//! Sync is the caller's: [`AttachedView::take_changes`] returns this
//! participant's own changes since the last call (the whole document the
//! first time, for a record it created) and [`AttachedView::merge`] takes a
//! peer's, through the record store's replication frontier as a browser
//! page's mounts do.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::{ActionEnvelope, ActionOutput, ActionTransport};
use ui_core::application::{
    ApplicationDefinition, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui_core::application_view::InstanceViewStore;
use ui_core::collaboration::DocumentMutation;
use ui_core::headless::FrameHarness;
use ui_core::permissions::{PermissionContext, UserId};
use ui_core::portable_storage::{PortableStorage, StorageKey};
use ui_core::record::{RecordCheckout, RecordOwners, RecordStore};
use ui_core::state::DocumentFrontier;

/// The component instance a view mounts.
const INSTANCE: u64 = 1;

/// Where the view lays out. Never drawn; it only gives the view a size, as
/// a window would.
const VIEWPORT: (u32, u32) = (1280, 720);

/// Device name the view's storage is kept under.
const DEVICE: &str = "attached-view";

/// The actions the view applied, as its transport replicates them.
#[derive(Clone, Default)]
struct Landed(Rc<RefCell<Vec<ActionEnvelope>>>);

impl ActionTransport for Landed {
    fn replicate(&mut self, env: &ActionEnvelope) {
        self.0.borrow_mut().push(env.clone());
    }
}

/// One record held in its app's view, attached and never drawn. See the
/// [module docs](self).
pub struct AttachedView {
    definition: &'static ApplicationDefinition,
    checkout: RecordCheckout,
    /// The app's view of the record: mounted, laid out, never drawn.
    frame: Box<FrameHarness>,
    /// The record store the view reads and writes.
    records: RecordStore,
    permissions: Rc<RefCell<PermissionContext>>,
    /// Who acts through the view: the invoker its operations are admitted for.
    me: String,
    /// The invoker as admitted: its kind and the person it acts for.
    invoker: tilo_actions::ActionInvoker,
    /// The task the invoker is carrying out, recorded with what it does.
    context: Option<tilo_actions::ActionContext>,
    landed: Landed,
    /// Everything taken or merged so far: what lies past it is this
    /// participant's own.
    frontier: DocumentFrontier,
    /// The whole document is still to be sent: the participant created it.
    fresh: bool,
    /// This participant's changes set aside, untaken, while a peer's merged.
    unsent: Vec<Vec<u8>>,
}

impl AttachedView {
    /// Mount `definition`'s view over the record `document` holds in
    /// `room`, for `me` with `permissions`. `created` when `me` just created
    /// the record, so the first [`Self::take_changes`] sends it whole.
    ///
    /// # Errors
    /// Returns why the record could not be held or the view would not open
    /// over it.
    pub fn open(
        definition: &'static ApplicationDefinition,
        room: &str,
        checkout: RecordCheckout,
        document: Vec<u8>,
        me: &str,
        permissions: PermissionContext,
        created: bool,
    ) -> Result<Self, String> {
        Self::open_for(
            definition,
            room,
            checkout,
            document,
            &tilo_actions::ActionInvoker::actor(me),
            permissions,
            created,
        )
    }

    /// [`Self::open`] for `invoker` — an agent, say, which may act for a
    /// person — rather than a person by their id alone. Its changes are
    /// written under the record writer's actor for its handle on this host.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_for(
        definition: &'static ApplicationDefinition,
        room: &str,
        checkout: RecordCheckout,
        document: Vec<u8>,
        invoker: &tilo_actions::ActionInvoker,
        permissions: PermissionContext,
        created: bool,
    ) -> Result<Self, String> {
        let me = invoker.actor.as_str();
        let cache = PortableStorage::new("attached-views", DEVICE)?;
        let key = if checkout.id.is_package_primary() {
            StorageKey::Primary {
                package: definition.app_id.into(),
                workspace: room.to_owned(),
            }
        } else {
            StorageKey::Record {
                package: definition.app_id.into(),
                workspace: room.to_owned(),
                record: checkout.id.as_str().into(),
            }
        };
        cache.hydrate(key, document, Some(me.as_bytes().to_vec()))?;
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id, room)
            .map_err(|error| error.to_string())?;
        let views =
            Rc::new(InstanceViewStore::portable(cache, room).map_err(|error| error.to_string())?);
        let permissions = Rc::new(RefCell::new(permissions));
        let target = LaunchTarget::Record {
            checkout: checkout.clone(),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records: records.clone(),
                views,
                device_id: DEVICE.into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: UserId(me.to_owned()),
                action_context: tilo_actions::ActionContext::default()
                    .with_invoker(invoker.clone()),
            },
            &target,
        )
        .map_err(|error| error.to_string())?;
        let component = definition
            .instantiate(context, target, RestoreInput::None)
            .map_err(|error| error.to_string())?;
        let mut frame = FrameHarness::from_component(
            component,
            VIEWPORT,
            ComponentRunnerInputs {
                theme: ui_core::Theme::default(),
                text_renderer: ui_core::TextRenderer::new(),
            },
        )
        .map_err(|error| error.to_string())?;
        let landed = Landed::default();
        frame.set_action_transport(Box::new(landed.clone()))?;
        let mut frontier = DocumentFrontier::default();
        if let Some(heads) = records.with(&checkout.id, ui_core::automerge::AutoCommit::get_heads) {
            frontier.advance(heads);
        }
        Ok(Self {
            definition,
            checkout,
            frame: Box::new(frame),
            records,
            permissions,
            me: me.to_owned(),
            invoker: invoker.clone(),
            context: None,
            landed,
            frontier,
            fresh: created,
            unsent: Vec::new(),
        })
    }

    /// The app whose view this is.
    #[must_use]
    pub const fn definition(&self) -> &'static ApplicationDefinition {
        self.definition
    }

    /// The record the view holds.
    #[must_use]
    pub const fn checkout(&self) -> &RecordCheckout {
        &self.checkout
    }

    /// Who acts through the view.
    #[must_use]
    pub fn me(&self) -> &str {
        &self.me
    }

    /// The participant's permissions changed: a role granted or taken away.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        *self.permissions.borrow_mut() = permissions;
        self.frame.relayout();
    }

    /// The tree the view publishes, as the participant may reach it.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
        let permissions = self.permissions.borrow();
        self.frame
            .resources()
            .into_iter()
            .filter_map(|resource| permissions.permitted_resource(self.definition.app_id, resource))
            .collect()
    }

    /// Every document operation the participant may perform through the
    /// view now, with its typed parameters.
    #[must_use]
    pub fn operations(&self) -> Vec<ActionDescriptor> {
        tilo_actions::door::survey(&self.resources())
            .into_iter()
            .filter(|descriptor| descriptor.kind == tilo_actions::OperationKind::Document)
            .collect()
    }

    /// Perform the operation `address` names through the view, for the
    /// participant: permission, confirmation and strict argument binding
    /// exactly as for a person's click (`confirmed` when the person it acts
    /// for confirmed it). The change it made leaves with
    /// [`Self::take_changes`].
    ///
    /// # Errors
    /// The door's refusal: an address the view does not offer the
    /// participant, a needed confirmation, bad arguments, or the action's
    /// own failure.
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        // The task may name the person the participant acts for now.
        let invoker = self
            .context
            .as_ref()
            .and_then(|context| context.invoker.clone())
            .filter(|invoker| invoker.actor == self.invoker.actor)
            .unwrap_or_else(|| self.invoker.clone());
        self.invoke_as(invoker, address, arguments, confirmed)
    }

    /// [`Self::invoke`] for `invoker`, whom the host admitted for this one
    /// invocation (an agent's, delivered to the app that owns the record):
    /// the operation and every edit it makes are theirs. Any operation the
    /// view offers — a document action, or an edit at the invoker's own
    /// caret.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &mut self,
        invoker: tilo_actions::ActionInvoker,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        if !tilo_actions::door::survey(&self.resources())
            .iter()
            .any(|entry| &entry.address == address)
        {
            return Err(DoorError::Unknown(address.clone()));
        }
        self.landed.0.borrow_mut().clear();
        // History names the app the view is of, as its window does, within
        // whatever task the participant carries out.
        let ambient = self.context.clone().unwrap_or_default().enriched_by(
            &tilo_actions::ActionContext::default()
                .with_app(self.definition.app_id, self.definition.title),
        );
        let (_, started) = self.frame.start_bound_job_within(
            Some(invoker),
            Some(&ambient),
            &ui_core::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: address.clone(),
            },
            arguments,
            confirmed,
        )?;
        self.frame.relayout();
        Ok(started.completed().unwrap_or_default())
    }

    /// Perform `invoke`, an action of the record's resource `R` built from
    /// its typed reference (`Note::EDIT.with(..)`, `Invoke::edit(..)`),
    /// through the view as [`Self::invoke`] does.
    ///
    /// # Errors
    /// As [`Self::invoke`], and when `invoke` does not bind or the view
    /// publishes no `R`.
    pub fn invoke_typed<R: tilo_resource::Resource>(
        &mut self,
        invoke: &tilo_resource::Invoke<R>,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(DoorError::InvalidArguments(refusal.to_owned()));
        }
        let resource = self
            .resources()
            .into_iter()
            .find(|resource| resource.kind == R::KIND)
            .ok_or_else(|| DoorError::Unknown(ActionAddress::new(R::KIND, invoke.id)))?;
        self.invoke(
            &ActionAddress::new(resource.id.as_str(), invoke.id),
            &invoke.arguments,
            confirmed,
        )
    }

    /// The task the participant carries out from now on (`None`: none),
    /// recorded with each thing it does.
    pub fn set_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.context = context;
    }

    /// Record one instruction that changed several records as one run in
    /// this record's history: a marker, no content. Leaves with
    /// [`Self::take_changes`]; whether anything was recorded.
    pub fn record_run(
        &mut self,
        label: &str,
        issued_by: Option<&str>,
        decision: Option<&str>,
        steps: &[ui_core::RunStep],
    ) -> bool {
        self.records
            .with(&self.checkout.id, |doc| {
                let by = doc.get_actor().to_hex_string();
                let recorded = ui_core::record_run_with_details(
                    doc,
                    label,
                    Some(&by),
                    issued_by,
                    Some(label),
                    decision,
                    steps,
                );
                if recorded.is_some() {
                    tilo_resource::history::commit_stamped(doc);
                }
                recorded.is_some()
            })
            .unwrap_or(false)
    }

    /// The last action the view applied for the participant's latest
    /// invocation: where its presence shows.
    #[must_use]
    pub fn landed(&self) -> Option<ActionEnvelope> {
        self.landed.0.borrow().last().cloned()
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> R) -> Option<R> {
        self.records.read(&self.checkout.id, read)
    }

    /// The participant's changes since the last call, or `None`. The first
    /// call on a record the participant created returns it whole.
    pub fn take_changes(&mut self) -> Option<DocumentMutation> {
        if std::mem::take(&mut self.fresh) {
            self.unsent.clear();
            let (bytes, heads) = self.records.replication_snapshot(&self.checkout.id).ok()?;
            self.frontier.advance(heads);
            return Some(DocumentMutation::Snapshot(bytes));
        }
        self.set_aside_own();
        let own: Vec<u8> = std::mem::take(&mut self.unsent).concat();
        (!own.is_empty()).then_some(DocumentMutation::Incremental(own))
    }

    /// Merge a peer's change into the view's record. Incremental changes
    /// that build on nothing this record holds belong to another record (or
    /// arrived ahead of what they build on) and are left alone: `Ok(false)`.
    ///
    /// # Errors
    /// Returns why the record store refused the peer's document.
    pub fn merge(&mut self, mutation: &DocumentMutation) -> Result<bool, String> {
        if let DocumentMutation::Incremental(bytes) = mutation {
            let mut incoming = ui_core::automerge::AutoCommit::new();
            if tilo_automerge::ApplyIncoming::load_incoming(&mut incoming, bytes).is_err() {
                return Ok(false);
            }
            let missing = incoming.get_missing_deps(&[]);
            let known = self
                .records
                .with(&self.checkout.id, |doc| {
                    missing
                        .iter()
                        .all(|hash| doc.get_change_by_hash(hash).is_some())
                })
                .unwrap_or(false);
            if !known {
                return Ok(false);
            }
        }
        // What this participant did and has not taken stays its own.
        if !self.fresh {
            self.set_aside_own();
        }
        let (changed, observed) = self
            .records
            .merge_replica(&self.checkout, mutation.clone())?;
        self.frontier.observe(observed);
        self.frame.relayout();
        Ok(changed)
    }

    /// Move this participant's changes past the frontier into `unsent`.
    fn set_aside_own(&mut self) {
        if let Ok((delta, heads)) = self
            .records
            .replication_delta(&self.checkout.id, &mut self.frontier)
        {
            self.frontier.advance(heads);
            if !delta.is_empty() {
                self.unsent.push(delta);
            }
        }
    }
}
