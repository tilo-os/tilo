//! One app instance in a tilolang host: the app's runtime over its room record, on the broker
//! connection the session opened for that app's launch, driving its window with component
//! patches.
//!
//! What the window's own process did before (the framework's root) is the instance's here:
//!
//! - **The record:** a [`RoomRecord`] holds it with its undo log and save state, on the instance's
//!   connection. Each change the runtime plans is committed to it as a step of the person at the
//!   window and sent on the route; changes from the room come back into it.
//! - **Actions:** the instance publishes the app's resources and actions on its connection, as
//!   the toolkit publishes an app's ([`tilo_actions::door::survey`], deltas through
//!   [`SurfacePublication`]), and runs what the broker delivers: an action the broker admitted for
//!   its invoker is built through the published spec and applied as that invoker's step.
//! - **The window:** the instance sends it patches and hears what the person did over the patch
//!   connection. The person's Undo on a notice takes their last step back.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, SurfacePublication};
use tilo_actions::{
    ActionAddress, ActionContext, ActionDescriptor, ActionInvoker, ActionRegistry, ActorKind,
    EncodedAction, InvocationId, InvocationUpdate,
};
use tilo_render_protocol::patch::Transport;
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::room::{RoomError, RoomRecord};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform, Publication};
use tilolang_tilo::publish::HOST_RESOURCE;
use tilolang_tilo::publish::{
    Caller, Committer, Planner, Publications, host_resource, publish_package,
};
use tilolang_tilo::runtime::{NameHost, Principal};
use tilolang_tilo::{HostEffect, OpenEffect};

use crate::phrases::Phrases;

thread_local! {
    /// The caller of the invocation being built on this thread, as the broker delivered it.
    static CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
}

/// Who invokes, as the runtime reads the invoker the broker admitted.
#[must_use]
pub fn caller_of(invoker: &ActionInvoker) -> Caller {
    Caller {
        actor: invoker.actor.clone(),
        agent: invoker.kind != ActorKind::Person,
        on_behalf_of: invoker.on_behalf_of.clone(),
    }
}

/// What one instance is: the app, the room, the window and who is at it, as the session named
/// them for the app's launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub app: String,
    pub room: String,
    /// The window's instance.
    pub instance: u64,
    /// The participant at the window.
    pub person: String,
}

/// What the instance publishes on its connection: the room's records with their actions, kept up
/// to date change by change.
struct Surface {
    host: Host,
    planner: Arc<dyn Planner>,
    committer: Arc<dyn Committer>,
    publications: RefCell<Publications>,
}

impl Surface {
    fn new(host: &Host, wake: Waker) -> Self {
        let handle = Arc::new(
            host.handle()
                .with_caller(|| CALLER.with(|caller| caller.borrow().clone())),
        );
        let mut publications = Publications::new(host);
        publications.set_waker(wake);
        let planner: Arc<dyn Planner> = handle.clone();
        let surface = Self {
            host: host.clone(),
            planner,
            committer: handle,
            publications: RefCell::new(publications),
        };
        surface.update();
        surface
    }

    fn update(&self) {
        self.publications
            .borrow_mut()
            .update(&self.host, &self.planner);
    }

    /// Every resource the room's packages hold, each with its actions; the host's own resource;
    /// and each package's actions that take no record. Built now, so an invocation resolves
    /// against what the record holds now.
    fn resources(&self) -> Vec<tilo_actions::Resource> {
        let mut out = vec![host_resource(&self.committer)];
        out.extend(self.publications.borrow().trees().values().cloned());
        for package in self.host.packages() {
            let resource = publish_package(&package.meta, &self.planner);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }
}

/// The instance's hold on its surface: while the instance publishes, it is the instance's.
struct Held(Rc<Surface>);

impl Publication for Held {
    fn update(&self) {
        self.0.update();
    }

    fn pending(&self) -> bool {
        self.0.publications.borrow().is_building()
    }
}

type SurfaceSlot = Rc<RefCell<Option<Rc<Surface>>>>;

/// What the instance asks of the host: its package, the platform's phrases and clock, waking the
/// host, and publishing on its connection.
struct HostPlatform {
    bundle: Arc<Bundle>,
    phrases: Rc<Phrases>,
    wake: Waker,
    surface: SurfaceSlot,
}

impl Platform for HostPlatform {
    fn load(&self, host: &Host) -> u64 {
        if let Err(error) = host.load_bundle(&self.bundle) {
            tracing::error!(app = self.bundle.meta.app, %error, "the package did not load");
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        self.phrases.say(id, args)
    }

    fn now(&self) -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    fn waker(&self) -> Waker {
        Arc::clone(&self.wake)
    }

    fn publish(&self, host: &Host, wake: Waker) -> Option<Box<dyn Publication>> {
        let surface = Rc::new(Surface::new(host, wake));
        *self.surface.borrow_mut() = Some(Rc::clone(&surface));
        Some(Box::new(Held(surface)))
    }
}

/// A principal as the room names them: by the id their changes carry.
struct ById;

impl NameHost for ById {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
    }
}

/// Where an instance keeps what it keeps: its room record's on-disk copy and its indexes, under
/// its package's directory in the host's storage.
pub struct Kept {
    pub documents: Rc<dyn durable_store::DurableStore>,
    pub indexes: Option<std::path::PathBuf>,
}

/// One app instance. See the module docs.
pub struct Hosted {
    spec: Spec,
    instance: Instance,
    record: RoomRecord,
    surface: SurfaceSlot,
    /// What the broker holds of the instance's surface, and the descriptors it was built from.
    published: Option<(Vec<ActionDescriptor>, SurfacePublication)>,
    registry: ActionRegistry,
    /// The identity-service connection the instance's connection signs through, held for as
    /// long as the instance runs.
    #[allow(
        dead_code,
        reason = "held open, never read: the connection signs through it"
    )]
    identity: Option<std::os::fd::OwnedFd>,
}

impl Hosted {
    /// The instance `spec` of `bundle`, over the room record its connection `manager` joined,
    /// kept in `kept`, sending its window patches over `window`.
    ///
    /// # Errors
    /// The room record could not be opened on the connection.
    pub fn open(
        spec: Spec,
        manager: collaboration::CollaborationManager,
        bundle: Arc<Bundle>,
        kept: &Kept,
        window: Box<dyn Transport>,
        phrases: Rc<Phrases>,
        wake: Waker,
    ) -> Result<Self, RoomError> {
        let record = RoomRecord::open(manager, &kept.documents, spec.person.clone())?;
        Ok(Self::with_record(
            spec, record, bundle, kept, window, phrases, wake,
        ))
    }

    /// The instance `spec` of `bundle` over `record`, which it holds from now on.
    #[must_use]
    pub fn with_record(
        spec: Spec,
        record: RoomRecord,
        bundle: Arc<Bundle>,
        kept: &Kept,
        window: Box<dyn Transport>,
        phrases: Rc<Phrases>,
        wake: Waker,
    ) -> Self {
        let surface: SurfaceSlot = Rc::default();
        let platform = HostPlatform {
            bundle,
            phrases,
            wake,
            surface: Rc::clone(&surface),
        };
        let instance = Instance::new(
            Config {
                app: spec.app.clone(),
                me: spec.person.clone(),
                mode: Mode::Live,
                device: None,
                index_dir: kept
                    .indexes
                    .as_ref()
                    .map(|indexes| tilolang_tilo::index::room_dir(indexes, &spec.room)),
                record_loader: None,
                sink: None,
            },
            Rc::new(platform),
            Rc::new(ById),
            window,
        );
        let mut registry = ActionRegistry::new();
        tilolang_tilo::action::register_actions(&mut registry);
        Self {
            spec,
            instance,
            record,
            surface,
            published: None,
            registry,
            identity: None,
        }
    }

    /// What the broker was last handed of the instance's actions.
    #[must_use]
    pub fn published(&self) -> &[ActionDescriptor] {
        self.published
            .as_ref()
            .map_or(&[], |(descriptors, _)| descriptors.as_slice())
    }

    /// Keep the identity-service connection the instance's connection signs through for as long
    /// as the instance runs.
    #[must_use]
    pub fn holding_identity(mut self, identity: std::os::fd::OwnedFd) -> Self {
        self.identity = Some(identity);
        self
    }

    /// What the instance is.
    #[must_use]
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The instance's runtime and views.
    #[must_use]
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// The room record the instance holds.
    #[must_use]
    pub fn record(&self) -> &RoomRecord {
        &self.record
    }

    /// One frame: what the room sent taken in, the runtime brought up to date with the record,
    /// what the person did run, what was planned committed and sent, what the broker delivered
    /// run, and the surface published. Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let mut events = Vec::new();
        let mut changed = self
            .record
            .receive_and(&mut self.instance, |event| events.push(event));
        self.instance.sync(Some(self.record.doc()));
        changed |= self.instance.update();
        if self.instance.take_undo() {
            match self.record.undo(&mut self.instance) {
                Ok(undone) => changed |= undone,
                Err(error) => tracing::warn!(%error, "the undo did not reach the room"),
            }
        }
        match self.record.commit(&mut self.instance) {
            Ok(committed) => changed |= !committed.is_empty(),
            Err(error) => {
                tracing::warn!(app = self.spec.app, %error, "a change did not reach the room")
            }
        }
        for event in events {
            changed |= self.delivered(event);
        }
        self.publish();
        changed
    }

    /// Run what the broker delivered on the instance's connection.
    fn delivered(&mut self, event: CollabEvent) -> bool {
        let CollabEvent::Invocation {
            id,
            invocation,
            resource,
            action,
            argument,
            actor,
            context,
        } = event
        else {
            return false;
        };
        let address = ActionAddress::new(resource, action);
        // The invoker is who the broker admitted, with the participant and the person it acts
        // for that the broker set in the delivery's context; nothing else is trusted.
        let invoker = ActionInvoker::delivered(&actor, context.as_deref());
        let attributed = context
            .as_deref()
            .map(|context| context.clone().with_invoker(invoker.clone()));
        let update = match self.invoke(&address, argument.as_deref(), invoker, attributed.as_ref())
        {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => failed(invocation, error),
        };
        self.record
            .manager_mut()
            .send(CollabRequest::UpdateInvocation { id, update });
        true
    }

    /// Build the action `address` names for `invoker`, through the spec the instance publishes
    /// now, and run it: a record action is applied as the invoker's step; the host's own
    /// vocabulary is answered by the runtime, and what it commits is the invoker's.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        argument: Option<&str>,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        let surface = self
            .surface
            .borrow()
            .clone()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        let resources = surface.resources();
        let caller = caller_of(&invoker);
        CALLER.with(|slot| *slot.borrow_mut() = Some(caller.clone()));
        let encoded = encode(&resources, address, argument);
        CALLER.with(|slot| *slot.borrow_mut() = None);
        let encoded =
            encoded.map_err(|error| Refused::new(door_code(&error), error.to_string()))?;
        let host = self
            .instance
            .host()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        if encoded.type_id == ACTION_TYPE {
            let action: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            self.record
                .apply_invoked(&mut self.instance, &action, invoker, ambient)
                .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            return Ok(tilo_actions::ActionOutput::default());
        }
        if address.resource == HOST_RESOURCE
            && encoded.type_id == <HostEffect as tilo_resource::Effect>::OPERATION
        {
            let effect = <HostEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error))?;
            let first = host.next_ticket();
            let output = host
                .answer(effect, Some(&caller))
                .map_err(|error| Refused::new("action_failed", error))?;
            for committed in host.take_committed_from(first) {
                self.record
                    .apply_invoked(
                        &mut self.instance,
                        &committed.action,
                        invoker.clone(),
                        ambient,
                    )
                    .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            }
            return Ok(output);
        }
        if encoded.type_id == <OpenEffect as tilo_resource::Effect>::OPERATION {
            // A search hit, or an agent's `open`: the window shows what it names.
            let OpenEffect::Open { anchor } =
                <OpenEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                    .map_err(|error| Refused::new("action_invalid_arguments", error))?;
            self.instance.open_requests().borrow_mut().push(anchor);
            return Ok(tilo_actions::ActionOutput::default());
        }
        Err(Refused::new(
            "action_unknown",
            format!("the tilolang host does not run `{}`", encoded.type_id),
        ))
    }

    /// Publish what the instance offers, when it changed since the broker last took it.
    fn publish(&mut self) {
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        let resources = surface.resources();
        let handlers: HashSet<String> = HashSet::from([
            <HostEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            <OpenEffect as tilo_resource::Effect>::OPERATION.to_owned(),
        ]);
        let violations = tilo_actions::validate_surface(&resources, &self.registry, &handlers);
        if !violations.is_empty() {
            for violation in &violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing to publish an invalid resource/action surface"
                );
            }
            // A stale catalogue that looks valid is worse than none.
            if self
                .published
                .as_ref()
                .is_some_and(|(held, _)| !held.is_empty())
            {
                let (delta, state) = SurfacePublication::next(
                    self.published.as_ref().map(|(d, s)| (s, d.as_slice())),
                    &[],
                );
                if self
                    .record
                    .manager_mut()
                    .send(CollabRequest::PublishActionSurface {
                        app_id: self.spec.app.clone(),
                        delta,
                    })
                {
                    self.published = Some((Vec::new(), state));
                }
            }
            return;
        }
        let descriptors = tilo_actions::door::survey(&resources);
        if self
            .published
            .as_ref()
            .is_some_and(|(held, _)| *held == descriptors)
        {
            return;
        }
        let prior = self
            .published
            .as_ref()
            .map(|(held, state)| (state, held.as_slice()));
        let (delta, state) = SurfacePublication::next(prior, &descriptors);
        // A surface the connection did not take is offered again on the next frame.
        if delta.is_empty()
            || self
                .record
                .manager_mut()
                .send(CollabRequest::PublishActionSurface {
                    app_id: self.spec.app.clone(),
                    delta,
                })
        {
            self.published = Some((descriptors, state));
        }
    }

    /// Write what the record holds to its on-disk copy, as the instance ends.
    pub fn flush(&mut self) {
        self.record.flush();
    }
}

/// Why an invocation was refused, as the broker reports it to its invoker.
#[derive(Debug)]
struct Refused {
    code: String,
    message: String,
}

impl Refused {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

fn failed(invocation: InvocationId, refused: Refused) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: refused.code,
            message: refused.message,
            details: None,
            retryable: false,
        },
    }
}

/// The action `address` names, built through its published spec from `argument`: typed JSON
/// arguments, or the legacy text of a prompted action.
fn encode(
    resources: &[tilo_actions::Resource],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, tilo_actions::DoorError> {
    match argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)) {
        Some(json) => {
            let arguments: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)
                .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
            tilo_actions::door::encode_json(resources, address, &arguments)
        }
        None => tilo_actions::door::encode(resources, address, argument),
    }
}

fn door_code(error: &tilo_actions::DoorError) -> &'static str {
    match error {
        tilo_actions::DoorError::Unknown(_) => "action_unknown",
        tilo_actions::DoorError::Unavailable { .. } => "action_unavailable",
        _ => "action_invalid_arguments",
    }
}
