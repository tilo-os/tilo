//! The probe the component tests read an instance's parts from.
//!
//! A described widget reads what a host gives it through [`Source`]; the probe is that host as a
//! test gives it: values by prop, the widget state to restore, the binding it offers, the events
//! it handles queued as intents, and the components in its slots.

#![allow(
    dead_code,
    reason = "each test file compiles this module and uses the helpers its cases need"
)]

use std::collections::BTreeMap;

use super::*;
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::Widget;

/// An instance as a test gives it: values by prop, the events it handles queued on `queue`,
/// and the components in its slots.
pub(super) struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<&'static str>,
    slots: Vec<(&'static str, Node)>,
    bound: Option<BoundValue>,
    queue: crate::Intents<Fired>,
}

impl Node {
    pub(super) fn new(kind: &'static str, key: &str, queue: &crate::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            slots: Vec::new(),
            bound: None,
            queue: queue.clone(),
        }
    }

    pub(super) fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    pub(super) fn on(mut self, event: &'static str) -> Self {
        self.events.push(event);
        self
    }

    pub(super) fn bound(mut self, bound: BoundValue) -> Self {
        self.bound = Some(bound);
        self
    }

    pub(super) fn holding(mut self, slot: &'static str, child: Node) -> Self {
        self.slots.push((slot, child));
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

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn slot_kinds(&mut self, slot: &str) -> Vec<Option<String>> {
        self.slots
            .iter()
            .filter(|(name, _)| *name == slot)
            .map(|(_, child)| Some(child.kind.to_owned()))
            .collect()
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for (name, child) in &mut self.slots {
            if *name == slot {
                read(child)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

pub(super) fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

pub(super) fn resource(id: &str) -> Value {
    Value::Resource(id.to_owned())
}

/// The widget at a desktop-sized window, laid out in the light theme.
pub(super) fn harness<W: Widget + 'static>(root: W) -> FrameHarness {
    FrameHarness::for_root(crate::State::new(), root, (900, 600), Theme::light())
}

pub(super) fn fired(key: &str, event: &str, value: Option<Value>) -> Fired {
    Fired {
        key: key.to_owned(),
        event: event.to_owned(),
        value,
    }
}

/// The edits a control makes, queued as the `value` event of the instance `key`.
pub(super) fn edits(queue: &crate::Intents<Fired>, key: &str) -> Option<Intent> {
    Some(Intent::new(queue, key, "value"))
}

pub(super) fn flag_bound(key: &str, on: bool, edits: Option<Intent>) -> BoundValue {
    BoundValue::Bool(Bound {
        state: LocalState::new(on),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
    })
}

pub(super) fn number_bound(key: &str, number: f64, edits: Option<Intent>) -> BoundValue {
    BoundValue::Float(Bound {
        state: LocalState::new(number),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
    })
}

pub(super) fn text_bound(key: &str, value: &str, edits: Option<Intent>) -> BoundValue {
    BoundValue::Text(Bound {
        state: LocalState::new(value.to_owned()),
        edits,
        name: key.to_owned(),
        path: Vec::new(),
    })
}
