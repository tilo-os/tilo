//! The actions an app publishes for its window, as the window's own command palette offers
//! them.
//!
//! An app whose instance runs beside its window sends what it offers
//! ([`ToUi::Actions`](tilo_render_protocol::patch::ToUi::Actions)): its actions as the
//! workspace's action directory lists them for the person at the window, with the labels, keys,
//! icons, arguments and confirmation the app gives them. The window offers them the one way
//! every window offers its actions: as [`PublishesResources`]. The command palette, the action
//! directory and every other reader of the window's surface find them there, and choosing one
//! hands its address back to the app, which runs it through the door it publishes.
//!
//! So a window offers the same palette whichever way its app runs: an app in the window's own
//! process reads the actions off its widgets, and an app in a host sends them, and neither
//! knows how the other works.

use tilo_actions::{
    ActionAddress, ActionArgs, ActionDescriptor, ActionOutput, ActionParam, ActionSpec,
    EncodedAction, InputSpec, OperationKind, ParamType, ResourceSpec,
};
use ui::automerge::AutoCommit;
use ui::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui::resources::PublishesResources;

use crate::window::AppWindow;
use tilo_render_protocol::patch as wire;

/// The operation type of the window's own declaration of the actions its app published:
/// choosing one hands the action's address to the app, which runs it as the person's step.
pub const HOSTED_ACTION: &str = "tilo.tilolang.hosted.action";

/// What the person chose from the window's command palette, as that operation's payload: the
/// action's address in the app's published surface, and the arguments the palette collected.
#[derive(serde::Serialize, serde::Deserialize)]
struct Chosen {
    resource: String,
    action: String,
    arguments: serde_json::Value,
}

impl Chosen {
    /// The arguments as the door's binder reads them: the canonical JSON object the palette
    /// collected, or nothing for an action that takes none.
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

/// The actions the app sent the window, as [`ToUi::Actions`] carries them: its directory's
/// descriptor JSON.
fn read(bytes: &[u8]) -> Result<Vec<ActionDescriptor>, String> {
    serde_json::from_slice(bytes)
        .map_err(|error| format!("the app's action catalogue did not read back: {error}"))
}

/// Take what the app offers the window now into `held`, the actions it holds: every action;
/// the actions of the resources whose labels moved, from an app of an earlier build; or what
/// changed ([`wire::ActionCatalogue::Changed`]).
pub(crate) fn take(
    held: &mut Vec<ActionDescriptor>,
    catalogue: wire::ActionCatalogue,
) -> Result<(), String> {
    match catalogue {
        wire::ActionCatalogue::All(actions) => *held = read(&actions)?,
        wire::ActionCatalogue::Relabeled(actions) => replace(held, read(&actions)?),
        wire::ActionCatalogue::Changed { actions, resources } => {
            replace(held, read(&actions)?);
            let order: std::collections::HashMap<&str, usize> = resources
                .iter()
                .enumerate()
                .map(|(at, resource)| (resource.as_str(), at))
                .collect();
            held.retain(|action| order.contains_key(action.address.resource.as_str()));
            held.sort_by_key(|action| order.get(action.address.resource.as_str()).copied());
        }
    }
    Ok(())
}

/// Every action of each resource `incoming` names, in place of what `held` holds of that
/// resource: where it sits, or after the rest for a resource it did not hold.
fn replace(held: &mut Vec<ActionDescriptor>, mut incoming: Vec<ActionDescriptor>) {
    while let Some(first) = incoming.first() {
        let resource = first.address.resource.clone();
        let theirs: Vec<_> = incoming
            .extract_if(.., |action| action.address.resource == resource)
            .collect();
        let at = held
            .iter()
            .position(|action| action.address.resource == resource)
            .unwrap_or(held.len());
        held.retain(|action| action.address.resource != resource);
        held.splice(at..at, theirs);
    }
}

impl AppWindow {
    /// The actions the app published for this window, as the resources the window's palette,
    /// action directory and agents offer: one resource per published one, in the order the app
    /// offered them, each action with everything the app said of it.
    #[must_use]
    pub fn catalogue(&self) -> Vec<ResourceSpec> {
        let mut resources: Vec<ResourceSpec> = Vec::new();
        for descriptor in self.actions() {
            match resources
                .iter_mut()
                .find(|resource| resource.id.as_str() == descriptor.address.resource)
            {
                Some(resource) => resource.actions.push(offered(descriptor)),
                None => resources.push(
                    ResourceSpec::new(
                        descriptor.address.resource.clone(),
                        descriptor.resource_kind.clone(),
                        descriptor.resource_label.clone(),
                    )
                    .with_action(offered(descriptor)),
                ),
            }
        }
        resources
    }
}

/// The window's declaration of the published action `descriptor`: everything the palette shows
/// of it, and the operation that hands its address to the app when the person chooses it.
fn offered(descriptor: &ActionDescriptor) -> ActionSpec {
    let address = descriptor.address.clone();
    // The app says what an invocation of its action takes, and the window lists and collects it
    // by that same declaration: one that takes nothing runs from the palette at once, and one
    // that takes words is asked them in the app's own words and handed over under the parameter
    // the app named them by.
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
        move |_target, args: &ActionArgs| {
            let chosen = Chosen {
                resource: address.resource.clone(),
                action: address.action.clone(),
                arguments: serde_json::Value::Object(args.to_json()),
            };
            EncodedAction::encode_operation(HOSTED_ACTION, &chosen).unwrap_or_else(|error| {
                // Handing over the address cannot fail; an empty payload is refused where the
                // choice is handed over, and says so in the window.
                tracing::error!(%error, "a chosen action did not encode");
                EncodedAction {
                    type_id: HOSTED_ACTION.to_owned(),
                    payload: Vec::new(),
                }
            })
        },
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
    // Whether it can run now, as the app says: an action whose precondition does not hold is
    // not offered, so the window does not offer it either.
    ty.contract
        .availability
        .clone_from(&descriptor.contract.availability);
    // The window offers the app's actions to whoever the app offers them to: one it keeps for
    // agents stays out of what a person is shown here, as it is everywhere else.
    ty.contract.set_audience(descriptor.contract.audience());
    spec
}

/// The window offers the app's published actions as its own: the one surface the command
/// palette, the action directory and every other reader of the window's widgets see.
impl PublishesResources for AppWindow {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        let mut all = ui::resources::resources(ui::widget::Decorator::inner(self), doc);
        all.extend(self.catalogue());
        all
    }
}

/// Choosing one hands its address back to the app over the window's patch connection: the app
/// runs it as the person's own step, through the same door an invocation from anywhere takes.
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
