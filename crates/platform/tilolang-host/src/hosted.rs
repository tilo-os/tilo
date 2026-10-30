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
//!   its invoker is built through the published spec and applied as that invoker's step. It
//!   offers an action directory on the connection too, answering it from what it published
//!   ([`PublishedDirectory`]): what makes the instance one the door and agents can browse.
//! - **What every app publishes:** on its app's resource the instance publishes the history
//!   verbs (Undo, Redo, History, ...: `collaboration::history_source::verbs`), `read`
//!   (`tilo_resource::read`) and "Show this window" (`tilo_actions::remote::activate_spec`), and
//!   answers them as a framework app does: history on its record, `read` through the runtime,
//!   and "Show this window" by asking its window to come forward. The person's Undo on a notice
//!   in the window is the same `undo`, as theirs: there is one undo.
//! - **The window:** the instance sends it patches and hears what the person did over the patch
//!   connection.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::action_directory::PublishedDirectory;
use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit, VIEW_TYPE};
use collaboration::{CollabEvent, CollabRequest, SurfacePublication};
use tilo_actions::{
    ActionAddress, ActionContext, ActionDescriptor, ActionInvoker, ActionRegistry, ActorKind,
    EncodedAction, InvocationId, InvocationUpdate, ResourceSpec,
};
use tilo_render_protocol::patch::Transport;
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::room::{RoomError, RoomRecord};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform, Publication};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::HOST_RESOURCE;
use tilolang_tilo::publish::package_resource_id;
use tilolang_tilo::publish::{
    Caller, Committer, Planner, Publications, host_resource, publish_package,
};
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

/// What a [`Surface`]'s resources are built from: the published records' revision and the
/// loaded packages.
type SurfaceInputs = (u64, Vec<tilolang_tilo::runtime::PackageId>);

/// What the instance publishes on its connection: the room's records with their actions, kept up
/// to date change by change.
struct Surface {
    /// The instance's app: its resource carries what every app publishes.
    app: String,
    /// "Show this window", in the person's language.
    activate: tilo_actions::ActionSpec,
    host: Host,
    planner: Arc<dyn Planner>,
    committer: Arc<dyn Committer>,
    publications: RefCell<Publications>,
}

impl Surface {
    fn new(app: &str, activate: tilo_actions::ActionSpec, host: &Host, wake: Waker) -> Self {
        let handle = Arc::new(
            host.handle()
                .with_caller(|| CALLER.with(|caller| caller.borrow().clone())),
        );
        let mut publications = Publications::new(host);
        publications.set_waker(wake);
        let planner: Arc<dyn Planner> = handle.clone();
        let surface = Self {
            app: app.to_owned(),
            activate,
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

    /// What [`Self::resources`] is built from, besides what never changes for this surface: the
    /// published records' revision and the loaded packages.
    fn inputs(&self) -> SurfaceInputs {
        (
            self.publications.borrow().revision(),
            self.host
                .packages()
                .into_iter()
                .map(|package| package.id)
                .collect(),
        )
    }

    /// Every resource the room's packages hold, each with its actions; the host's own resource;
    /// and each package's actions that take no record, the instance's own app's with what every
    /// app publishes. Built now, so an invocation resolves against what the record holds now.
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut out = vec![host_resource(&self.committer)];
        out.extend(
            self.publications
                .borrow()
                .trees()
                .values()
                .map(|tree| (**tree).clone()),
        );
        for package in self.host.packages() {
            let mut resource = publish_package(&package.meta, &self.planner);
            if package.meta.app == self.app {
                resource = with_app_actions(resource, &self.activate);
            }
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

/// `resource` with what every app publishes, those it does not offer itself: the history verbs
/// of its record, `read` and "Show this window".
fn with_app_actions(
    mut resource: ResourceSpec,
    activate: &tilo_actions::ActionSpec,
) -> ResourceSpec {
    let every = verbs::specs()
        .into_iter()
        .chain([tilo_resource::read::spec(None), activate.clone()]);
    for spec in every {
        if !resource
            .actions
            .iter()
            .any(|existing| existing.id == spec.id)
        {
            resource = resource.with_action(spec);
        }
    }
    resource
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
        let activate = tilo_actions::remote::activate_spec(
            self.phrases.say("app-activate-window", &[]),
            self.phrases.say("app-activate-window-description", &[]),
        );
        let surface = Rc::new(Surface::new(&self.bundle.meta.app, activate, host, wake));
        *self.surface.borrow_mut() = Some(Rc::clone(&surface));
        Some(Box::new(Held(surface)))
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
    /// The app's package: what a read of a past version runs.
    bundle: Arc<Bundle>,
    /// The app's resource, by its id and kind: where its history verbs are published.
    resource: (String, String),
    instance: Instance,
    record: RoomRecord,
    surface: SurfaceSlot,
    /// What the broker holds of the instance's surface, and the descriptors it was built from.
    published: Option<(Vec<ActionDescriptor>, SurfacePublication)>,
    /// The directory the instance answers its connection's directory requests from: what it
    /// last published, browsable by level and name, as the door and agents read it.
    directory: PublishedDirectory,
    /// Whether labels moved since [`Self::directory`] was built ([`Self::relabel`]): it is
    /// built again when it is next asked, not on every keystroke.
    directory_behind: bool,
    /// The surface and its inputs ([`Surface::inputs`]) the broker's copy was built from: while
    /// they are the same, a turn publishes without building anything.
    published_from: Option<(Rc<Surface>, SurfaceInputs)>,
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
        let resource = (
            package_resource_id(&bundle.meta.app),
            bundle.meta.package_kind(),
        );
        let record = record.speaking(phrases.locale());
        let platform = HostPlatform {
            bundle: Arc::clone(&bundle),
            phrases,
            wake,
            surface: Rc::clone(&surface),
        };
        let mut instance = Instance::new(
            Config {
                app: spec.app.clone(),
                me: spec.person.clone(),
                mode: Mode::Live,
                device: None,
                index_dir: kept.indexes.as_ref().map(|indexes| {
                    tilolang_tilo::index::record_dir(
                        indexes,
                        &spec.room,
                        record.route().record.as_deref(),
                    )
                }),
                record_loader: None,
                sink: None,
            },
            Rc::new(platform),
            // People by what the window calls them, once it said (`ToApp::Names`).
            Rc::new(tilolang_tilo::names::Unnamed),
            window,
        );
        // The device that governs the room is its records' origin: the package's migrations
        // run there, once.
        instance.set_origin(record.governs());
        let registry = ActionRegistry::with(tilolang_tilo::instance::room::register);
        Self {
            spec,
            bundle,
            resource,
            instance,
            record,
            surface,
            published: None,
            directory: PublishedDirectory::default(),
            directory_behind: false,
            published_from: None,
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

    /// The package the instance runs.
    #[must_use]
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Keep the package's types with an origin in step with `origin`'s service.
    pub fn add_origin(&mut self, origin: tilolang_tilo::origin::SharedOrigin) {
        self.instance.add_origin(origin);
    }

    /// Run the package's scenario `scenario` in the instance on its next frame, as a capture
    /// stages one: its changes are the person's steps on the instance's record.
    pub fn stage(&mut self, scenario: &str) {
        self.instance.stage(Some(scenario));
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
            let (resource, kind) = &self.resource;
            match self.record.undo(&mut self.instance, resource, kind) {
                Ok(undone) => changed |= undone,
                Err(error) => tracing::warn!(%error, "the undo did not reach the room"),
            }
        }
        match self.record.commit(&mut self.instance) {
            Ok(committed) => changed |= !committed.is_empty(),
            Err(error) => {
                tracing::warn!(app = self.spec.app, %error, "a change did not reach the room");
            }
        }
        for event in events {
            changed |= self.delivered(event);
        }
        self.publish();
        self.answer_directory();
        changed
    }

    /// Offer the instance's action directory on its connection, once it is connected, and
    /// answer what was asked of it since the last frame from what the instance published.
    fn answer_directory(&mut self) {
        let manager = self.record.manager_mut();
        manager.publish_action_directory(self.spec.app.clone());
        let requests = manager.take_action_directory_requests();
        if !requests.is_empty() && self.directory_behind {
            self.catch_up_directory();
        }
        for request in requests {
            self.directory.answer(request);
        }
    }

    /// Build the directory again from what the instance publishes now, after labels moved.
    fn catch_up_directory(&mut self) {
        self.directory_behind = false;
        let (Some(surface), Some((descriptors, _))) =
            (self.surface.borrow().clone(), self.published.as_ref())
        else {
            return;
        };
        self.directory.publish(&surface.resources(), descriptors);
    }

    /// Run what the broker delivered on the instance's connection. Every event that waits on an
    /// answer from the instance is answered: an invocation with its outcome, a handoff step with
    /// what the host does of it. The rest says something the instance does not keep (presence,
    /// cursors, the room's context, results of requests it never makes).
    fn delivered(&mut self, event: CollabEvent) -> bool {
        let (id, invocation, resource, action, argument, actor, context) = match event {
            CollabEvent::Invocation {
                id,
                invocation,
                resource,
                action,
                argument,
                actor,
                context,
            } => (id, invocation, resource, action, argument, actor, context),
            CollabEvent::Handoff { request } => {
                self.handoff(&request);
                return true;
            }
            CollabEvent::InvocationCancellation { id } => {
                // An invocation runs to its end in the frame it arrives in and is answered
                // there: by the time its cancellation arrives nothing of it is left to stop.
                tracing::debug!(id, "a cancellation of an invocation already answered");
                return false;
            }
            CollabEvent::TaskRun(_)
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::TaskRunControl { .. }
            | CollabEvent::WorkspaceControl { .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "a task or workspace request reached an app instance, which does not run them"
                );
                return false;
            }
            _ => return false,
        };
        let address = ActionAddress::new(resource, action);
        // The invoker is who the broker admitted, with the participant and the person it acts
        // for that the broker set in the delivery's context; nothing else is trusted.
        let invoker = ActionInvoker::delivered(&actor, context.as_deref());
        // A member's own record is that person's alone: anyone else is refused before anything
        // of it is read.
        let person = match invoker.kind {
            ActorKind::Person => Some(invoker.actor.as_str()),
            _ => invoker.on_behalf_of.as_deref(),
        };
        if !tilolang_tilo::reaches(self.record.route().record.as_deref(), person) {
            let update = failed(
                invocation,
                Refused::new("permission_denied", "this is another member's own record"),
            );
            self.answer_invocation(id, update);
            return true;
        }
        let attributed = context
            .as_deref()
            .map(|context| context.clone().with_invoker(invoker.clone()));
        let update = match self.invoke(&address, argument.as_deref(), invoker, attributed.as_ref())
        {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => failed(invocation, error),
        };
        self.answer_invocation(id, update);
        true
    }

    /// Hand the broker the outcome of the invocation `id`. An answer the connection does not
    /// take leaves its invoker waiting until the broker ends the invocation with the connection:
    /// that is said here.
    fn answer_invocation(&mut self, id: u64, update: InvocationUpdate) {
        if let Err(error) = self
            .record
            .manager_mut()
            .try_send(CollabRequest::UpdateInvocation { id, update })
        {
            tracing::error!(
                app = self.spec.app,
                instance = self.spec.instance,
                id,
                %error,
                "an invocation's outcome did not reach the broker"
            );
        }
    }

    /// Answer the shell's handoff step `request`: a checkpoint writes the record's on-disk copy
    /// and is done; a retirement is refused with why, as the tilolang host does not move a
    /// record to another workspace; an owner view is presentation the instance has no window
    /// of its own for, and wants no answer.
    fn handoff(&mut self, request: &collaboration::handoff::HandoffRequest) {
        use collaboration::handoff::{HandoffReport, HandoffRequest};
        let report = match request {
            HandoffRequest::Checkpoint { token } => {
                self.record.flush();
                HandoffReport::Done {
                    token: *token,
                    heads: Vec::new(),
                }
            }
            HandoffRequest::Retire { token, .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "the shell asked a tilolang instance to retire its record, which it cannot"
                );
                HandoffReport::Failed {
                    token: *token,
                    reason: "a tilolang app's instance does not move its record to another \
                             workspace"
                        .to_owned(),
                }
            }
            HandoffRequest::OwnerView { .. } => return,
        };
        if let Err(error) = self
            .record
            .manager_mut()
            .try_send(CollabRequest::ReportHandoff {
                message: report.encode(),
            })
        {
            tracing::error!(
                app = self.spec.app,
                instance = self.spec.instance,
                %error,
                "a handoff step's answer did not reach the shell"
            );
        }
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
        if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
            return Err(Refused::new(
                "action_invalid_arguments",
                String::from_utf8_lossy(&encoded.payload),
            ));
        }
        if encoded.type_id == EDIT_TYPE {
            // A history verb, on the record as the invoker's own step.
            let edit: HistoryEdit = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            let value = self
                .record
                .history(
                    &mut self.instance,
                    &edit,
                    &address.resource,
                    &self.resource.1,
                    invoker,
                    ambient,
                )
                .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            return Ok(tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            });
        }
        if encoded.type_id == VIEW_TYPE {
            return self
                .record
                .view_history(&encoded.payload)
                .map_err(|error| Refused::new("action_failed", error));
        }
        if encoded.type_id == tilo_resource::read::TYPE {
            return self
                .read(&encoded.payload)
                .map_err(|error| Refused::new("action_failed", error));
        }
        if encoded.type_id == tilo_actions::remote::APPLICATION_ACTIVATE_TYPE {
            self.instance.activate_window();
            return Ok(tilo_actions::remote::activation_requested());
        }
        if encoded.type_id == ACTION_TYPE {
            let action: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            self.record
                .apply_invoked(&mut self.instance, &action, invoker, ambient)
                .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            return Ok(returned(&host, &action));
        }
        if address.resource == HOST_RESOURCE
            && encoded.type_id == <HostEffect as tilo_resource::Effect>::OPERATION
        {
            let effect = <HostEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
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
                    .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            self.instance.open_requests().borrow_mut().push(anchor);
            return Ok(tilo_actions::ActionOutput::default());
        }
        Err(Refused::new(
            "action_unknown",
            format!("the tilolang host does not run `{}`", encoded.type_id),
        ))
    }

    /// Answer `read` (`tilo_resource::read`): what the record holds, through the runtime, now or
    /// at a past version.
    fn read(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let args: serde_json::Value = if payload.is_empty() {
            serde_json::Value::Null
        } else {
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
        };
        let item = args.get("item").and_then(serde_json::Value::as_str);
        let value = if let Some(version) = args.get("version").and_then(serde_json::Value::as_str) {
            let doc = self.record.doc();
            let heads = collaboration::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            // A runtime of its own over the frozen record, as a framework app reads a past
            // version; a read runs no action.
            let past = Host::new(
                &frozen,
                tilolang_tilo::runtime::Principal::new("reader"),
                tilolang_tilo::authority::holding_nothing("reader"),
            )
            .map_err(|error| error.to_string())?;
            past.load_bundle(&self.bundle)
                .map_err(|error| error.to_string())?;
            verbs::review::bounded(serde_json::json!({
                "version": heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("+"),
                "heads": heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "content": past.read_content(item)?,
            }))?
        } else {
            let host = self
                .instance
                .host()
                .ok_or_else(|| "the app has not started yet".to_owned())?;
            host.sync_from(self.record.doc());
            host.read_content(item)?
        };
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    /// Publish what the instance offers, when it changed since the broker last took it.
    fn publish(&mut self) {
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        // Nothing the surface is built from changed since the broker took it: a keystroke into
        // a field nothing published reads, and every turn that changed nothing, cost no more
        // with ten thousand records or items than with one.
        let inputs = surface.inputs();
        if self
            .published_from
            .as_ref()
            .is_some_and(|(from, at)| Rc::ptr_eq(from, &surface) && *at == inputs)
        {
            return;
        }
        if self.relabel(&surface, inputs.clone()) {
            return;
        }
        let _phase = tilo_resource::profiling::Phase::start("tl.host.publish");
        let resources = surface.resources();
        let handlers: HashSet<String> = HashSet::from([
            <HostEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            <OpenEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            VIEW_TYPE.to_owned(),
            tilo_resource::read::TYPE.to_owned(),
            tilo_actions::remote::APPLICATION_ACTIVATE_TYPE.to_owned(),
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
            self.directory.withdraw();
            self.directory_behind = false;
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
                    .send(CollabRequest::PublishActionSurface { delta })
                {
                    self.published = Some((Vec::new(), state));
                    self.published_from = None;
                }
            }
            return;
        }
        let descriptors = tilo_actions::door::survey(&resources);
        self.directory.publish(&resources, &descriptors);
        self.directory_behind = false;
        if self
            .published
            .as_ref()
            .is_some_and(|(held, _)| *held == descriptors)
        {
            self.published_from = Some((surface, inputs));
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
                .send(CollabRequest::PublishActionSurface { delta })
        {
            self.published = Some((descriptors, state));
            self.published_from = Some((surface, inputs));
        }
    }

    /// Publish a change to the surface that moved only labels (a note's title typed into) as
    /// those resources alone: their descriptors patched, and a delta of them, at the cost of
    /// what changed whatever the notebook. `inputs` are the surface's ([`Surface::inputs`]).
    /// Whether this turn's publishing is done; when the surface changed in more than labels
    /// since the broker took it, the caller builds it whole.
    ///
    /// Nothing is validated again: no rule reads a label.
    fn relabel(&mut self, surface: &Rc<Surface>, inputs: SurfaceInputs) -> bool {
        let Some((from, (published_at, published_packages))) = &self.published_from else {
            return false;
        };
        if !Rc::ptr_eq(from, surface) || *published_packages != inputs.1 {
            return false;
        }
        let Some(labels) = surface.publications.borrow().labels_since(*published_at) else {
            return false;
        };
        let Some((descriptors, state)) = &mut self.published else {
            return false;
        };
        if labels
            .iter()
            .any(|(resource, _)| state.indices(resource).is_none())
        {
            return false;
        }
        let _phase = tilo_resource::profiling::Phase::start("tl.host.relabel");
        for (resource, label) in &labels {
            for index in state.indices(resource).unwrap_or_default() {
                if let Some(descriptor) = descriptors.get_mut(*index) {
                    descriptor.resource_label.clone_from(label);
                }
            }
        }
        self.directory_behind |= !labels.is_empty();
        let changed: Vec<&str> = labels
            .iter()
            .map(|(resource, _)| resource.as_str())
            .collect();
        let Some(delta) = state.relabel(descriptors, &changed) else {
            return false;
        };
        if !delta.is_empty()
            && !self
                .record
                .manager_mut()
                .send(CollabRequest::PublishActionSurface { delta })
        {
            // What the broker holds is no longer what was patched: a later turn sends the
            // surface whole, as a reset.
            self.published = None;
            self.published_from = None;
            return true;
        }
        self.published_from = Some((Rc::clone(surface), inputs));
        true
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
    resources: &[tilo_actions::ResourceSpec],
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

/// What the action `action` answers: the resource it returned, by its published id, which a
/// task's program goes on with.
fn returned(host: &tilolang_tilo::host::Host, action: &DynAction) -> tilo_actions::ActionOutput {
    let returned = host
        .take_returned(&action.action_id)
        .map(|made| tilolang_tilo::publish::resource_id(&action.app, &made.id_string()));
    tilo_actions::ActionOutput {
        value: None,
        resources: returned.into_iter().collect(),
    }
}
