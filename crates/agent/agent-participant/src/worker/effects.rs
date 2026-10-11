//! An agent's effects, run in the worker that holds its record.
//!
//! A record's document operations run on the replica alone; its effects —
//! exporting a picture, writing out a file — are the app's own, published
//! and run only where its component is mounted. An agent's worker mounts the record's
//! component headless ([`EffectSession`]), from the replica as it stands,
//! under the agent's permissions and with the worker's content store, and
//! runs the effect there for the agent, as the host runs an invocation a
//! collaborator was admitted for. What the effect keeps as content leaves
//! the worker the way every operation's content does, and its output — the
//! content's address among it — goes back to the agent.
//!
//! The record's views — looking at a picture, inspecting what a record
//! holds — are run the same way: they read the record as the component
//! shows it and answer with what they found (a rendered picture kept as
//! content and named by address, never inlined), so an agent can learn what
//! an edit needs, an artwork's size say, before it plans the edit.
//!
//! An agent runs only the effects and views that read the record and that
//! any participant may run: one needing more than read access, or one
//! reaching the files of the device it runs on, is not offered and is
//! refused, and the component refuses any edit a read submits. Nor does an
//! agent name where an effect writes: its `path` parameter is not offered,
//! and an invocation naming one is refused, so what an effect produces is
//! kept as content.

use automerge::ChangeHash;
use tilo_actions::door::{self, ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::{
    ActionContract, ActionInvoker, ActionOutput, ActionSpec, InvocationUpdate, LOCAL_DEVICE_ONLY,
    OperationKind, PermissionRequirement, Resource, ResultSpec,
};
use tilo_content::ContentStore;
use ui::bound_dispatch::BoundActionAddress;
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, UserId};

use super::serve::{INSTANCE, Mount, Source, mount_component};
use crate::apps::{AppCatalogue, Replica};

/// The parameter an effect is told where on the device to write by.
const PATH: &str = "path";

/// The device the mounted component runs as. It is never the agent, so the
/// door's own refusal of device-only effects to anyone else holds too.
const DEVICE: &str = "agent-effects";

/// The longest an effect's job runs before it is given up on: well inside
/// the supervisor's limit on one request, so the worker answers first.
const EFFECT_TIMEOUT: ui::time::Duration = ui::time::Duration::from_secs(45);

/// Whether an operation of `kind` under `contract` is one an agent runs in
/// the mounted component: an effect, or a view that answers with what it
/// finds. A view answering nothing only moves the window (a control's
/// fold, a scroll), which a headless window shows no one; a document
/// operation runs on the replica.
const fn mounted(kind: OperationKind, contract: &ActionContract) -> bool {
    match kind {
        OperationKind::Effect => true,
        OperationKind::View => !matches!(contract.result, ResultSpec::None),
        OperationKind::Document => false,
    }
}

/// The effects and views of `resources` an agent may run, without the
/// parameter that would write to the device.
fn offered(resources: &[Resource]) -> Vec<ActionDescriptor> {
    door::survey(resources)
        .into_iter()
        .filter(|descriptor| {
            mounted(descriptor.kind, &descriptor.contract)
                && PermissionRequirement::Read.covers(descriptor.contract.permission)
                && !device_only(&descriptor.contract.tags)
        })
        .map(|mut descriptor| {
            descriptor.params.retain(|param| param.name != PATH);
            descriptor
        })
        .collect()
}

/// Refuse what an agent may not run: an effect or view reaching the
/// device, one needing more than read access with these `arguments`, or
/// one told to write to a path.
fn admit(
    spec: &ActionSpec,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), DoorError> {
    if device_only(&spec.contract.tags) {
        return Err(DoorError::Unavailable {
            code: "local_device_only".into(),
            reason: format!(
                "{} uses the files or hardware of a person's device; an agent does not run it",
                spec.label
            ),
        });
    }
    if !PermissionRequirement::Read.covers(spec.permission_for_json(arguments)) {
        return Err(DoorError::Unavailable {
            code: "effect_writes".into(),
            reason: format!(
                "{} changes more than it reads; an agent runs only effects and views that read \
                 the record",
                spec.label
            ),
        });
    }
    if arguments.contains_key(PATH) {
        return Err(DoorError::Unavailable {
            code: "effect_path".into(),
            reason: "an agent's effect writes to no path; leave `path` out and what it makes is \
                     kept as content"
                .into(),
        });
    }
    Ok(())
}

fn device_only(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == LOCAL_DEVICE_ONLY)
}

/// One record's component, mounted headless from its replica for the
/// agent's effects. It is mounted again once the replica has moved on.
pub(super) struct EffectSession {
    frame: FrameHarness,
    /// The replica's heads the component was mounted from.
    heads: Vec<ChangeHash>,
    /// The agent the effects run for.
    agent: String,
}

/// The held record an [`EffectSession`] mounts.
pub(super) struct Held<'a> {
    pub(super) app_id: &'a str,
    pub(super) room: &'a str,
    pub(super) replica: &'a mut Replica,
}

impl EffectSession {
    /// Mount `held`'s component from the replica as it stands, under the
    /// agent's permissions, lent `content`.
    pub(super) fn mount(
        catalogue: &AppCatalogue,
        held: Held<'_>,
        content: ContentStore,
        text_renderer: ui::TextRenderer,
    ) -> Result<Self, String> {
        let Held {
            app_id,
            room,
            replica,
        } = held;
        let heads = replica.heads();
        let agent = replica.participant_id().to_owned();
        let permissions: PermissionContext = replica.permissions().clone();
        let frame = mount_component(
            catalogue,
            Mount {
                source: Source {
                    app_id,
                    room,
                    record: replica.record_id().clone(),
                    bytes: replica.snapshot(),
                },
                device: DEVICE,
                permissions: std::rc::Rc::new(std::cell::RefCell::new(permissions)),
                author: UserId(DEVICE.into()),
                content: Some(content),
            },
            text_renderer,
        )?;
        Ok(Self {
            frame,
            heads,
            agent,
        })
    }

    /// Whether the component shows the replica at `heads`.
    pub(super) fn shows(&self, heads: &[ChangeHash]) -> bool {
        self.heads == heads
    }

    /// What an agent's survey adds to its record's document operations:
    /// the effects and views the component publishes that the agent may
    /// run.
    pub(super) fn offered(&self) -> Vec<ActionDescriptor> {
        match self.frame.bound_resources(None) {
            Ok(resources) => offered(&resources),
            Err(error) => {
                tracing::warn!(%error, "app worker: the component published nothing");
                Vec::new()
            }
        }
    }

    /// Run the effect or view at `address` for the agent, if it is one the
    /// agent may run, and wait, bounded, for what it ends as. The component
    /// admits the invocation for read access alone, so an edit the
    /// operation submits fails it.
    ///
    /// An effect runs as the agent. A view moves only the viewer's own
    /// window, which the door keeps to the window's own device: this
    /// headless window is mounted for the agent alone, so a view runs as
    /// that device — still admitted for read access alone, and never one
    /// reaching a device's files, which [`admit`] refused already.
    pub(super) fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        let resources = self.frame.bound_resources(Some(&address.resource))?;
        let Some(spec) =
            door::resolve(&resources, address).filter(|spec| mounted(spec.kind, &spec.contract))
        else {
            return Err(DoorError::Unknown(address.clone()));
        };
        admit(spec, arguments)?;
        let actor = if spec.kind == OperationKind::View {
            DEVICE.to_owned()
        } else {
            self.agent.clone()
        };
        self.run(&actor, address, arguments, confirmed)
    }

    fn run(
        &mut self,
        actor: &str,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        let (invocation, started) = self.frame.start_bound_job_for(
            Some(ActionInvoker::actor(actor)),
            &BoundActionAddress {
                instance_id: INSTANCE,
                address: address.clone(),
            },
            arguments,
            confirmed,
        )?;
        if let Some(output) = started.completed() {
            self.frame.relayout();
            return Ok(output);
        }
        let finished = |frame: &FrameHarness| {
            frame
                .job_updates(invocation)
                .iter()
                .find(|update| update.is_terminal())
                .cloned()
        };
        self.frame
            .wait_until(EFFECT_TIMEOUT, |frame| finished(frame).is_some());
        match finished(&self.frame) {
            Some(InvocationUpdate::Succeeded { output, .. }) => Ok(output),
            Some(InvocationUpdate::Failed { error, .. }) => Err(DoorError::Failed(error.message)),
            Some(_) => Err(DoorError::Failed(format!(
                "{} was cancelled",
                address.action
            ))),
            None => {
                let _ = self.frame.cancel_job(invocation);
                Err(DoorError::Failed(format!(
                    "{} did not finish within {} seconds",
                    address.action,
                    EFFECT_TIMEOUT.as_secs()
                )))
            }
        }
    }
}
