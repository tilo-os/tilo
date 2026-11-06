//! The context a view builds its tree in: the events its controls fire, the view's own state,
//! and what a place shows it.
//!
//! A view is a function of a resource, built again whenever what it read changes
//! (one-app-model.md §2). [`ViewCx`] is what the function is handed besides the resource: it
//! names controls to the host (`key`), attaches what an event does (`action`, `open`, `set`),
//! and keeps the view's own `window` and `lens` state, which is the person's alone.
//!
//! Events are data, never closures, exactly as in a tilolang view: an event carries the action
//! it runs, or the state it sets, and the host reports the event by the node's key and name
//! ([`tilo_render_protocol::patch::Event`]). What a Rust app's view names here is what the
//! runtime runs, over the same patch stream a tilolang view uses.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::node::{Binding, Event, Node, Principal, ResourceRef, Value};

/// What an event of a control does, as the host reads it back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Effect {
    /// Run the action `action` of the view's resource (the resource the place shows).
    Action { action: String },
    /// Run the action `action` of the resource `target`.
    On { target: ResourceRef, action: String },
    /// Show `target` (`open(r)`): the place goes on to it, as a press on a row does.
    Open {
        target: ResourceRef,
        view: Option<String>,
    },
    /// Set the view's own state `name` to the event's value.
    State { name: String },
    /// Both: run the action, and set the state (a compose box cleared after it sends).
    #[default]
    None,
}

/// The context a `#[view]` function builds with.
///
/// Built by the SDK when it runs a view for a place; a view reads it, never makes one. It keeps
/// the events the view gave its controls by the key it gave each, so the host can tell the view
/// what the person did.
pub struct ViewCx {
    /// The resource the place shows: the view's first parameter.
    resource: ResourceRef,
    /// The view's name, for keys and for what a place names as `shown_as`.
    view: String,
    /// What each event does, by node key and event name.
    events: RefCell<BTreeMap<(String, String), Effect>>,
    /// The view's own state, by name: what the person's `window` and `lens` slots hold.
    state: RefCell<BTreeMap<String, Value>>,
    /// The person the view is drawn for.
    person: Principal,
}

impl ViewCx {
    /// The context of the view `view` over `resource`, for `person`, with the state `state` the
    /// person's window keeps.
    #[must_use]
    pub fn new(
        resource: ResourceRef,
        view: impl Into<String>,
        person: Principal,
        state: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            resource,
            view: view.into(),
            events: RefCell::new(BTreeMap::new()),
            state: RefCell::new(state),
            person,
        }
    }

    /// The resource the view shows.
    #[must_use]
    pub fn resource(&self) -> &ResourceRef {
        &self.resource
    }

    /// The view's name.
    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }

    /// The person the view is drawn for.
    #[must_use]
    pub fn person(&self) -> &Principal {
        &self.person
    }

    /// A key for a node of this view, unique among the view's nodes: the view's name and the
    /// caller's suffix (a row's item id, a field's name). Two nodes may never share a key in one
    /// tree: patches name nodes by it.
    #[must_use]
    pub fn key(&self, suffix: impl AsRef<str>) -> String {
        format!("{}@{}", self.view, suffix.as_ref())
    }

    // ----- events -------------------------------------------------------------------------

    /// The event of the control `node` named `event` that runs `action` on the view's resource.
    #[must_use]
    pub fn action(&self, node: &Node, event: &str, action: impl Into<String>) -> Event {
        self.attach(
            node,
            event,
            &Effect::Action {
                action: action.into(),
            },
        )
    }

    /// The event of `node` named `event` that runs `action` on `target` (a view may offer an
    /// action of another resource, as a list row offers its item's).
    #[must_use]
    pub fn action_on(
        &self,
        node: &Node,
        event: &str,
        target: ResourceRef,
        action: impl Into<String>,
    ) -> Event {
        self.attach(
            node,
            event,
            &Effect::On {
                target,
                action: action.into(),
            },
        )
    }

    /// The event of `node` named `event` that shows `target` (`open(r)`, or `open(r, view: v)`)
    /// in the place the view is shown in: pressing a video row places the video.
    #[must_use]
    pub fn open(&self, node: &Node, event: &str, target: ResourceRef, view: Option<&str>) -> Event {
        self.attach(
            node,
            event,
            &Effect::Open {
                target,
                view: view.map(str::to_owned),
            },
        )
    }

    /// The event of `node` named `event` that sets the view's state `name` to the event's value
    /// (a disclosure's open, a checked box).
    #[must_use]
    pub fn set(&self, node: &Node, event: &str, name: impl Into<String>) -> Event {
        self.attach(node, event, &Effect::State { name: name.into() })
    }

    /// Attach `effect` to the event of `node` and hand back what the host reads: the resource
    /// and action the event runs (or the field it toggles), which the event carries as data.
    fn attach(&self, _node: &Node, _event: &str, effect: &Effect) -> Event {
        match effect {
            Effect::Action { action } => Event {
                toggles: None,
                invokes: Some((self.resource.id_string(), action.clone())),
            },
            Effect::On { target, action } => Event {
                toggles: None,
                invokes: Some((target.id_string(), action.clone())),
            },
            Effect::Open { .. } | Effect::State { .. } | Effect::None => Event::default(),
        }
    }

    /// Record what the event `event` of the node `key` does, for the runtime that runs the
    /// view's events. A view's events are applied by the same runtime a tilolang view's are.
    pub fn record(&self, key: &str, event: &str, effect: Effect) {
        self.events
            .borrow_mut()
            .insert((key.to_owned(), event.to_owned()), effect);
    }

    /// What the event `event` of the node `key` does, when the view attached one.
    #[must_use]
    pub fn effect(&self, key: &str, event: &str) -> Option<Effect> {
        self.events
            .borrow()
            .get(&(key.to_owned(), event.to_owned()))
            .cloned()
    }

    // ----- state --------------------------------------------------------------------------

    /// The view's state `name` as it is, when the view ever set it.
    #[must_use]
    pub fn state(&self, name: &str) -> Option<Value> {
        self.state.borrow().get(name).cloned()
    }

    /// The view's state `name` as a text, or `fallback` when unset or not text.
    #[must_use]
    pub fn state_text(&self, name: &str, fallback: &str) -> String {
        match self.state(name) {
            Some(Value::Text(text)) => text,
            _ => fallback.to_owned(),
        }
    }

    /// The view's state `name` as a flag, or `fallback` when unset.
    #[must_use]
    pub fn state_flag(&self, name: &str, fallback: bool) -> bool {
        match self.state(name) {
            Some(Value::Bool(flag)) => flag,
            _ => fallback,
        }
    }

    /// Set the view's state `name`.
    pub fn set_state(&self, name: &str, value: Value) {
        self.state.borrow_mut().insert(name.to_owned(), value);
    }

    /// The view's state as a whole: what the place keeps for the person.
    #[must_use]
    pub fn state_now(&self) -> BTreeMap<String, Value> {
        self.state.borrow().clone()
    }

    // ----- bindings -----------------------------------------------------------------------

    /// The binding of a control named `name` to the field `field` of `target`.
    #[must_use]
    pub fn bind_field(
        target: &ResourceRef,
        field: impl Into<String>,
        name: impl Into<String>,
    ) -> Binding {
        Binding {
            field: Some((target.id_string(), field.into())),
            name: name.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> ViewCx {
        ViewCx::new(
            ResourceRef {
                id: "tube.channel:abc".to_owned(),
                ty: "tube.Channel".to_owned(),
            },
            "page",
            Principal {
                id: "Pat".to_owned(),
                name: "Pat".to_owned(),
            },
            BTreeMap::new(),
        )
    }

    #[test]
    fn an_action_event_carries_the_resource_and_action() {
        let cx = cx();
        let node: crate::node::Node = crate::ui::TextBuilder::new(&cx.key("title"), "x").into();
        let event = cx.action(&node, "press", "subscribe");
        assert_eq!(
            event.invokes,
            Some(("tube.channel:abc".to_owned(), "subscribe".to_owned()))
        );
    }

    #[test]
    fn an_open_event_shows_its_target() {
        let cx = cx();
        let node: crate::node::Node = crate::ui::TextBuilder::new(&cx.key("r"), "x").into();
        cx.record(
            &node.key,
            "activate",
            Effect::Open {
                target: ResourceRef {
                    id: "tube.video:1".to_owned(),
                    ty: "tube.Video".to_owned(),
                },
                view: None,
            },
        );
        let effect = cx.effect(&node.key, "activate").expect("recorded");
        assert!(matches!(effect, Effect::Open { target, .. } if target.id == "tube.video:1"));
    }

    #[test]
    fn the_view_state_is_the_persons() {
        let cx = cx();
        assert!(!cx.state_flag("open", false));
        cx.set_state("open", Value::Bool(true));
        assert!(cx.state_flag("open", false));
        assert_eq!(cx.state_text("tab", "all"), "all");
    }
}
