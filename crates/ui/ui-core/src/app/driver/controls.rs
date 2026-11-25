//! The controls a driven app's exported accessibility tree carries, and what a person, a screen
//! reader or an agent does to one: the shared description of an operable control.

use accesskit::Role;
use vello::kurbo::Rect;

/// Resource id prefix of a projected control (`ui:<percent-encoded node>`).
pub const CONTROL_RESOURCE_PREFIX: &str = "ui:";

/// One control as the exported accessibility tree carries it now.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// The exported node id, in decimal.
    pub id: String,
    /// Exported role.
    pub role: Role,
    /// Accessible name, already localized.
    pub name: Option<String>,
    /// The row title an accessible name starts with, for a list row named by
    /// its title and its secondary line: the line a person finds it by.
    pub title: Option<String>,
    /// A field's contents, a slider's readout.
    pub value: Option<String>,
    /// Where it is on the surface, in logical pixels.
    pub bounds: Option<Rect>,
    /// Holds keyboard focus.
    pub focused: bool,
    /// Present but inoperable.
    pub disabled: bool,
    /// The accessibility verbs it answers (`activate`, `focus`, `set-value`, …).
    pub verbs: Vec<String>,
}

/// What a person can do to a control.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlInput {
    /// Click, tap or press Enter on it: its `activate` verb, or a click at its centre when it has none.
    Press,
    /// Move keyboard focus to it.
    Focus,
    /// Replace its value (a field, a slider): its `set-value` verb.
    SetValue(String),
    /// Focus it, then type text at its caret through the input-method path.
    TypeText(String),
    /// Point at it and turn the wheel by line deltas.
    Scroll { dx: f64, dy: f64 },
    /// Check or uncheck it.
    Toggle,
    /// Step it up.
    Increment,
    /// Step it down.
    Decrement,
    /// Open it.
    Expand,
    /// Close it.
    Collapse,
}

/// An agent's invocation of `action` on resource `resource`, as the broker reports it while it applies:
/// what a driven app places as the action's locus.
#[must_use]
pub fn agent_acting(resource: &str, action: &str) -> tilo_actions::SemanticInteraction {
    tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::Acting::agent("agent", "ada"),
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: action.into(),
            label: action.into(),
            app: None,
            step: None,
            creates: None,
            did: None,
        },
        resource: tilo_actions::ActionResource {
            id: resource.into(),
            kind: resource.into(),
            label: resource.into(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    }
}
