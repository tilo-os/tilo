//! The context a view builds its tree in: the events its controls fire, the view's
//! own state, and what a place shows it.
//!
//! A view is a function of a resource, built again whenever what it read changes.
//! [`ViewCx`] names controls to the host (`key`), attaches what an event does
//! (`action`, `open`, `set`), and keeps the view's own `window` and `lens` state,
//! which is the person's alone.
//!
//! Events are data, never closures: an event carries the action it runs or the
//! state it sets, and the host reports it by the node's key and name.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::node::{Binding, Event, Node, Principal, ResourceRef, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Value(Value),
    Event,
    State(String),
}

impl Arg {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Value(Value::Text(value.into()))
    }

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

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Effect {
    Action {
        action: String,
        args: Vec<Arg>,
    },
    On {
        target: ResourceRef,
        action: String,
        args: Vec<Arg>,
    },
    Open {
        target: ResourceRef,
        view: Option<String>,
    },
    OpenEvent {
        view: Option<String>,
    },
    State {
        name: String,
    },
    #[default]
    None,
}

/// The context a `#[view]` function builds with, made by the SDK when it runs a
/// view for a place. It keeps the events the view gave its controls by key, so the
/// host can tell the view what the person did.
pub struct ViewCx {
    resource: ResourceRef,
    view: String,
    events: RefCell<BTreeMap<(String, String), Effect>>,
    state: RefCell<BTreeMap<String, Value>>,
    person: Principal,
}

impl ViewCx {
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

    #[must_use]
    pub fn resource(&self) -> &ResourceRef {
        &self.resource
    }

    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }

    #[must_use]
    pub fn person(&self) -> &Principal {
        &self.person
    }

    /// A key for a node of this view, unique among the view's nodes: the view's name
    /// and the caller's suffix. Patches name nodes by it, so two nodes may never share
    /// one in a tree.
    #[must_use]
    pub fn key(&self, suffix: impl AsRef<str>) -> String {
        format!("{}@{}", self.view, suffix.as_ref())
    }

    #[must_use]
    pub fn action(&self, node: &Node, event: &str, action: impl Into<String>) -> Event {
        self.action_with(node, event, action, Vec::new())
    }

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

    #[must_use]
    pub fn set(&self, node: &Node, event: &str, name: impl Into<String>) -> Event {
        self.attach(node, event, Effect::State { name: name.into() })
    }

    /// Attach `effect` to the event `event` of `node` and hand back what the host reads:
    /// the resource and action the event runs, which the event carries as data.
    fn attach(&self, node: &Node, event: &str, effect: Effect) -> Event {
        let invoked = |resource: &ResourceRef, action: &str| {
            Some(crate::node::patch::Invocation {
                resource: resource.id_string(),
                action: action.to_owned(),
                declared: action.to_owned(),
            })
        };
        let wire = match &effect {
            Effect::Action { action, .. } => Event {
                toggles: None,
                invokes: invoked(&self.resource, action),
            },
            Effect::On { target, action, .. } => Event {
                toggles: None,
                invokes: invoked(target, action),
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

    pub fn record(&self, key: &str, event: &str, effect: Effect) {
        self.events
            .borrow_mut()
            .insert((key.to_owned(), event.to_owned()), effect);
    }

    #[must_use]
    pub fn effect(&self, key: &str, event: &str) -> Option<Effect> {
        self.events
            .borrow()
            .get(&(key.to_owned(), event.to_owned()))
            .cloned()
    }

    #[must_use]
    pub fn recorded(&self) -> BTreeMap<(String, String), Effect> {
        self.events.borrow().clone()
    }

    #[must_use]
    pub fn state(&self, name: &str) -> Option<Value> {
        self.state.borrow().get(name).cloned()
    }

    #[must_use]
    pub fn state_text(&self, name: &str, fallback: &str) -> String {
        match self.state(name) {
            Some(Value::Text(text)) => text,
            _ => fallback.to_owned(),
        }
    }

    #[must_use]
    pub fn state_flag(&self, name: &str, fallback: bool) -> bool {
        match self.state(name) {
            Some(Value::Bool(flag)) => flag,
            _ => fallback,
        }
    }

    pub fn set_state(&self, name: &str, value: Value) {
        self.state.borrow_mut().insert(name.to_owned(), value);
    }

    #[must_use]
    pub fn state_now(&self) -> BTreeMap<String, Value> {
        self.state.borrow().clone()
    }

    #[must_use]
    pub fn bind_field(
        target: &ResourceRef,
        field: impl Into<String>,
        name: impl Into<String>,
    ) -> Binding {
        Binding {
            field: Some((target.id_string(), field.into())),
            kind: Some(target.ty.clone()),
            name: name.into(),
        }
    }

    #[must_use]
    pub fn bind_state(name: impl Into<String>) -> Binding {
        Binding {
            field: None,
            kind: None,
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
            Some(crate::node::patch::Invocation {
                resource: "tube.channel:abc".to_owned(),
                action: "subscribe".to_owned(),
                declared: "subscribe".to_owned(),
            })
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
        let wired = cx.action_with(
            &node,
            "submit",
            "post",
            vec![Arg::text("#general"), Arg::Event],
        );
        assert_eq!(
            wired.invokes,
            Some(crate::node::patch::Invocation {
                resource: "tube.channel:abc".to_owned(),
                action: "post".to_owned(),
                declared: "post".to_owned(),
            })
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
