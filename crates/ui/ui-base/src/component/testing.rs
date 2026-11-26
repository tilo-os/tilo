//! The probe the component tests read an instance's parts from. A described widget reads what a host
//! gives it through [`Source`]; the probe is that host as a test gives it: values by prop, the widget
//! state to restore, the binding it offers, the events it handles queued as intents, and the components
//! in its parts.

#![allow(
    dead_code,
    reason = "each test file compiles this module and uses the helpers its cases need"
)]

use std::collections::BTreeMap;

use super::{Bound, BoundValue, Element, Event, Fired, Intent, LocalState, Source, Value};

/// An instance as a test gives it: values by prop, the events it handles queued on `queue`, and the
/// components in its parts.
pub struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<&'static str>,
    parts: Vec<(&'static str, Node)>,
    bound: Option<BoundValue>,
    queue: crate::Intents<Fired>,
}

impl Node {
    /// An instance of the component `kind` keyed `key`, its events queued on `queue`.
    pub fn new(kind: &'static str, key: &str, queue: &crate::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            parts: Vec::new(),
            bound: None,
            queue: queue.clone(),
        }
    }

    /// Give the prop `name` the value `value`.
    #[must_use]
    pub fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    /// Handle the event `event`.
    #[must_use]
    pub fn on(mut self, event: &'static str) -> Self {
        self.events.push(event);
        self
    }

    /// Offer `bound` as the instance's binding.
    #[must_use]
    pub fn bound(mut self, bound: BoundValue) -> Self {
        self.bound = Some(bound);
        self
    }

    /// Hold `child` in the part `part`.
    #[must_use]
    pub fn holding(mut self, part: &'static str, child: Node) -> Self {
        self.parts.push((part, child));
        self
    }
}

impl Source for Node {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn state(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, name: &str) -> Event {
        if self.events.contains(&name) {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        self.bound.clone()
    }

    fn children(&mut self, _part: &str) -> Vec<Element> {
        Vec::new()
    }

    fn part_kinds(&mut self, part: &str) -> Vec<Option<String>> {
        self.parts
            .iter()
            .filter(|(name, _)| *name == part)
            .map(|(_, child)| Some(child.kind.to_owned()))
            .collect()
    }

    fn components(
        &mut self,
        part: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for (name, child) in &mut self.parts {
            if *name == part {
                read(child)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

/// A text value.
pub fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// A value naming the resource `id`.
pub fn resource(id: &str) -> Value {
    Value::Resource(id.to_owned())
}

/// The event `event` of the instance `key`, as the queue holds it.
pub fn fired(key: &str, event: &str, value: Option<Value>) -> Fired {
    Fired {
        key: key.to_owned(),
        event: event.to_owned(),
        value,
        gesture: None,
    }
}

/// The edits a control makes, queued as the `value` event of the instance `key`.
pub fn edits(queue: &crate::Intents<Fired>, key: &str) -> Option<Intent> {
    Some(Intent::new(queue, key, "value"))
}

/// A switch's binding, on or off.
pub fn flag_bound(key: &str, on: bool, edits: Option<Intent>) -> BoundValue {
    BoundValue::Bool(Bound {
        state: LocalState::new(on),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
        identity: None,
    })
}

/// A number's binding.
pub fn number_bound(key: &str, number: f64, edits: Option<Intent>) -> BoundValue {
    BoundValue::Float(Bound {
        state: LocalState::new(number),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
        identity: None,
    })
}

/// A text's binding.
pub fn text_bound(key: &str, value: &str, edits: Option<Intent>) -> BoundValue {
    BoundValue::Text(Bound {
        state: LocalState::new(value.to_owned()),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
        identity: None,
    })
}
