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

/// One argument of an action a view's event runs, besides the resource it is on.
///
/// A tilolang view hands an action what it read (`post(c, text)`); a Rust view does the same.
/// What the control's event carries at the press (a field's `submit` carries its text, a picked
/// emoji, the row a list activated) reaches the action where [`Arg::Event`] stands, exactly as a
/// tilolang view's `event` argument does, and the view's own state where [`Arg::State`] stands,
/// as a tilolang view's `draft`.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// A value the view read.
    Value(Value),
    /// What the event carries at the press.
    Event,
    /// The view's own state `name`, read at the press: the text a bound control holds, which a
    /// send button hands the action the composer's field is bound to.
    State(String),
}

impl Arg {
    /// A text argument: `Arg::text(channel)`.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Value(Value::Text(value.into()))
    }

    /// The view's own state `name`, read at the press (`Arg::state("Composer")`).
    #[must_use]
    pub fn state(name: impl Into<String>) -> Self {
        Self::State(name.into())
    }
}

impl From<Value> for Arg {
    fn from(value: Value) -> Self {
        Self::Value(value)
    }
}

/// What an event of a control does, as the host reads it back.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Effect {
    /// Run the action `action` of the view's resource (the resource the place shows), with
    /// `args` (in the order the action's parameters after its subject declare them).
    Action { action: String, args: Vec<Arg> },
    /// Run the action `action` of the resource `target`, with `args`.
    On {
        target: ResourceRef,
        action: String,
        args: Vec<Arg>,
    },
    /// Show `target` (`open(r)`): the place goes on to it, as a press on a row does.
    Open {
        target: ResourceRef,
        view: Option<String>,
    },
    /// Show the resource the event carries (`open(x)` on a list's `select(x)`): the row that was
    /// pressed, whatever it shows.
    OpenEvent { view: Option<String> },
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
        self.action_with(node, event, action, Vec::new())
    }

    /// [`Self::action`] with the arguments the action takes after its resource: what the view
    /// read (`post(c, text)`), or [`Arg::Event`] for what the event carries.
    #[must_use]
    pub fn action_with(
        &self,
        node: &Node,
        event: &str,
        action: impl Into<String>,
        args: Vec<Arg>,
    ) -> Event {
        self.attach(
            node,
            event,
            Effect::Action {
                action: action.into(),
                args,
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
        self.action_on_with(node, event, target, action, Vec::new())
    }

    /// [`Self::action_on`] with the arguments the action takes after its resource.
    #[must_use]
    pub fn action_on_with(
        &self,
        node: &Node,
        event: &str,
        target: ResourceRef,
        action: impl Into<String>,
        args: Vec<Arg>,
    ) -> Event {
        self.attach(
            node,
            event,
            Effect::On {
                target,
                action: action.into(),
                args,
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
            Effect::Open {
                target,
                view: view.map(str::to_owned),
            },
        )
    }

    /// The event of `node` named `event` that shows the resource the event carries, as a
    /// virtual list's `activate` does (`open(x)` on `activate(x)`): the row that was pressed,
    /// whatever it shows, is placed.
    #[must_use]
    pub fn open_event(&self, node: &Node, event: &str, view: Option<&str>) -> Event {
        self.attach(
            node,
            event,
            Effect::OpenEvent {
                view: view.map(str::to_owned),
            },
        )
    }

    /// The event of `node` named `event` that sets the view's state `name` to the event's value
    /// (a disclosure's open, a checked box).
    #[must_use]
    pub fn set(&self, node: &Node, event: &str, name: impl Into<String>) -> Event {
        self.attach(node, event, Effect::State { name: name.into() })
    }

    /// Attach `effect` to the event `event` of `node` and hand back what the host reads: the
    /// resource and action the event runs, which the event carries as data. What the event does
    /// in full (the place it opens, the state it sets) is kept for the runtime, by `node`'s key
    /// and the event's name, so a press runs through the same door a tilolang view's does.
    fn attach(&self, node: &Node, event: &str, effect: Effect) -> Event {
        let wire = match &effect {
            Effect::Action { action, .. } => Event {
                toggles: None,
                invokes: Some((self.resource.id_string(), action.clone())),
            },
            Effect::On { target, action, .. } => Event {
                toggles: None,
                invokes: Some((target.id_string(), action.clone())),
            },
            Effect::Open { .. }
            | Effect::OpenEvent { .. }
            | Effect::State { .. }
            | Effect::None => Event::default(),
        };
        self.events
            .borrow_mut()
            .insert((node.key.clone(), event.to_owned()), effect);
        wire
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

    /// Every event the view attached, by the node's key and the event's name: what the runtime
    /// reads back when a press reaches it.
    #[must_use]
    pub fn recorded(&self) -> BTreeMap<(String, String), Effect> {
        self.events.borrow().clone()
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

    /// The name of a binding to the control `name`, for assistive technology: what a control
    /// bound to the view's state announces.
    #[must_use]
    pub fn bind_state(name: impl Into<String>) -> Binding {
        Binding {
            field: None,
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

    #[test]
    fn an_actions_arguments_are_what_the_view_read_and_what_the_event_carries() {
        let cx = cx();
        let node: crate::node::Node = crate::ui::TextInputBuilder::new(&cx.key("draft")).into();
        cx.action_with(
            &node,
            "submit",
            "post",
            vec![Arg::text("#general"), Arg::Event],
        );
        let effect = cx.effect(&node.key, "submit").expect("recorded");
        assert_eq!(
            effect,
            Effect::Action {
                action: "post".to_owned(),
                args: vec![Arg::Value(Value::Text("#general".to_owned())), Arg::Event,],
            }
        );
    }
}
