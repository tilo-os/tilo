//! The actions an app publishes for its window, as the window's own command palette offers them. An
//! app whose instance runs beside its window sends them
//! ([`ToUi::Actions`](tilo_render_protocol::patch::ToUi::Actions)); the window offers them as
//! [`PublishesResources`], where the palette, the action directory and every other reader of its
//! surface find them, and choosing one hands its address back to the app, which runs it through the
//! action API it publishes. So a window offers the same palette whichever way its app runs, and neither
//! side knows how the other works.

use std::collections::HashSet;
use std::sync::Arc;

use tilo_actions::{
    ActionAddress, ActionArgs, ActionDescriptor, ActionOutput, ActionParam, ActionSpec,
    EncodedAction, ExecutionLocus, InputSpec, OperationKind, ParamType, ResourceSpec,
};
use ui::automerge::AutoCommit;
use ui::collaboration::{SurfaceDelta, SurfacePublication, SurfaceTable};
use ui::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui::resources::PublishesResources;

use crate::window::AppWindow;

/// The operation of the window's declaration of the actions its app published: choosing one hands
/// the action's address to the app, which runs it as the person's step.
pub const HOSTED_ACTION: &str = "tilo.tilolang.hosted.action";

/// What the person chose from the window's command palette, as that operation's payload: the
/// action's address, and the arguments the palette collected.
#[derive(serde::Serialize, serde::Deserialize)]
struct Chosen {
    resource: String,
    action: String,
    arguments: serde_json::Value,
}

impl Chosen {
    fn argument(&self) -> Option<String> {
        match &self.arguments {
            serde_json::Value::Object(arguments) if arguments.is_empty() => None,
            serde_json::Value::Null => None,
            arguments => Some(format!(
                "{}{arguments}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            )),
        }
    }
}

/// What the app offers at its window: the surface it publishes there, as the broker holds it,
/// with every action of every resource and the declaration the window's palette offers of each
/// resource.
#[derive(Default)]
pub(crate) struct Offered {
    surface: SurfaceTable,
    /// Each resource's actions together, in the order the resources were first published.
    actions: Vec<ActionDescriptor>,
    /// Each resource's declaration, in that same order.
    specs: Vec<ResourceSpec>,
}

impl Offered {
    pub(crate) fn actions(&self) -> &[ActionDescriptor] {
        &self.actions
    }

    pub(crate) fn specs(&self) -> &[ResourceSpec] {
        &self.specs
    }

    /// Take the change `bytes` of the surface the app publishes at the window
    /// ([`ToUi::Actions`](tilo_render_protocol::patch::ToUi::Actions), a [`SurfaceDelta`]): the
    /// resources it names are read again, and the rest stay as they are, so a change of one row
    /// costs that row with ten thousand on the screen.
    pub(crate) fn take(&mut self, bytes: &[u8]) -> Result<(), String> {
        let delta: SurfaceDelta = serde_json::from_slice(bytes)
            .map_err(|error| format!("the app's action catalogue did not read back: {error}"))?;
        self.surface.apply(&delta)?;
        let surface = &self.surface;
        let read = |resource: &str| -> Vec<ActionDescriptor> {
            surface
                .actions_of(resource)
                .iter()
                .map(|action| action.descriptor(ExecutionLocus::default()))
                .collect()
        };
        let mut actions = Vec::with_capacity(self.actions.len());
        let mut specs = Vec::with_capacity(self.specs.len());
        let mut push = |mine: Vec<ActionDescriptor>, spec: Option<ResourceSpec>| {
            if let Some(spec) = spec.or_else(|| declared(&mine)) {
                specs.push(spec);
                actions.extend(mine);
            }
        };
        let changed: HashSet<&str> = (delta.resources.iter())
            .map(|resource| resource.resource.as_str())
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        if !delta.reset {
            let removed: HashSet<&str> = delta.removed.iter().map(String::as_str).collect();
            let mut held = std::mem::take(&mut self.actions).into_iter().peekable();
            for spec in std::mem::take(&mut self.specs) {
                let mine: Vec<ActionDescriptor> = std::iter::from_fn(|| {
                    held.next_if(|action| action.address.resource == spec.id.as_str())
                })
                .collect();
                let name = spec.id.as_str();
                seen.insert(name.to_owned());
                if removed.contains(name) {
                    continue;
                }
                if changed.contains(name) {
                    push(read(name), None);
                } else {
                    push(mine, Some(spec));
                }
            }
        }
        for resource in &delta.resources {
            if seen.insert(resource.resource.clone()) {
                push(read(&resource.resource), None);
            }
        }
        self.actions = actions;
        self.specs = specs;
        Ok(())
    }
}

/// The declaration the window's palette offers of the resource whose actions are `actions`.
fn declared(actions: &[ActionDescriptor]) -> Option<ResourceSpec> {
    let first = actions.first()?;
    let mut spec = ResourceSpec::new(
        first.address.resource.clone(),
        first.resource_kind.clone(),
        first.resource_label.clone(),
    );
    for action in actions {
        spec = spec.with_action(offered(action));
    }
    Some(spec)
}

/// What the app publishes at its window when it offers `descriptors`, as one change from
/// nothing: what [`Offered::take`] reads.
#[must_use]
pub fn whole(descriptors: &[ActionDescriptor]) -> Vec<u8> {
    let (delta, _) = SurfacePublication::next(None, descriptors);
    serde_json::to_vec(&delta).unwrap_or_default()
}

impl AppWindow {
    /// The actions the app published for this window, as the resources its palette, action
    /// directory and agents offer: one per published action, in the order the app offered them.
    #[must_use]
    pub fn catalogue(&self) -> &[ResourceSpec] {
        self.offered.specs()
    }
}

fn offered(descriptor: &ActionDescriptor) -> ActionSpec {
    let address = descriptor.address.clone();
    // The app says what an invocation of its action takes, and the window lists and collects it by
    // that same declaration.
    let (params, placeholder) = match &descriptor.input {
        InputSpec::None => (Vec::new(), String::new()),
        InputSpec::Text { placeholder, .. } if !descriptor.params.is_empty() => {
            (descriptor.params.clone(), placeholder.clone())
        }
        InputSpec::Text {
            prompt,
            placeholder,
            ..
        } => {
            let name = descriptor
                .prompt_param()
                .unwrap_or(tilo_actions::PROMPTED_PARAM)
                .to_owned();
            (
                vec![ActionParam::required_owned(
                    name,
                    ParamType::Text,
                    prompt.clone(),
                )],
                placeholder.clone(),
            )
        }
    };
    let mut spec = ActionSpec::typed_operation_on(
        descriptor.address.action.clone(),
        descriptor.action_label.clone(),
        placeholder,
        OperationKind::Effect,
        HOSTED_ACTION,
        params,
        Arc::new(move |_target, args: &ActionArgs| {
            let chosen = Chosen {
                resource: address.resource.clone(),
                action: address.action.clone(),
                arguments: serde_json::Value::Object(args.to_json()),
            };
            EncodedAction::encode_operation(HOSTED_ACTION, &chosen).unwrap_or_else(|error| {
                // Encoding a chosen action cannot fail; an error says so where the choice is handed
                // over.
                tracing::error!(%error, "a chosen action did not encode");
                EncodedAction {
                    type_id: HOSTED_ACTION.to_owned(),
                    payload: Vec::new(),
                }
            })
        }),
    );
    let ty = spec.type_mut();
    ty.state.clone_from(&descriptor.state);
    ty.contract
        .confirmation
        .clone_from(&descriptor.contract.confirmation);
    if let Some(description) = &descriptor.description {
        ty.description = Some(description.clone());
    }
    ty.keywords.clone_from(&descriptor.keywords);
    ty.icon.clone_from(&descriptor.icon);
    // What the action does is what the app declares it does: an undoable change of its records
    // or a reach outside them. The window's own check of a press decides that need, so a person
    // whose role lets them change the workspace's records but asks before outside effects (an
    // editor) presses an undoable action as the app's own window lets them.
    ty.contract.kind = descriptor.contract.kind;
    ty.contract.field.clone_from(&descriptor.contract.field);
    ty.contract.risk = descriptor.contract.risk;
    ty.contract.tags.clone_from(&descriptor.contract.tags);
    // An action whose precondition does not hold is not offered, as the app says.
    ty.contract
        .availability
        .clone_from(&descriptor.contract.availability);
    // The window offers the app's actions to whoever the app offers them to: one kept for agents
    // stays out of what a person is shown.
    ty.contract.set_audience(descriptor.contract.audience());
    // One that acts on an item it was not given is offered where the item is.
    ty.contract.item.clone_from(&descriptor.contract.item);
    spec
}

impl PublishesResources for AppWindow {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        let mut all = ui::resources::resources(ui::widget::Decorator::inner(self), doc);
        all.extend(self.catalogue().iter().cloned());
        all
    }
}

impl ResourceOperations for AppWindow {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[HOSTED_ACTION]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(started) = ui::host_operation::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op
            .unscoped()
            .filter(|request| request.type_id == HOSTED_ACTION)
        else {
            return Ok(None);
        };
        let chosen: Chosen = serde_json::from_slice(request.payload)
            .map_err(|error| format!("a chosen action did not read back: {error}"))?;
        let argument = chosen.argument();
        self.invoke_action(ActionAddress::new(chosen.resource, chosen.action), argument);
        self.flush();
        Ok(Some(ResourceOperationStart::Completed(
            ActionOutput::default(),
        )))
    }
}
