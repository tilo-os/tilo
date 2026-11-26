//! The probe the component tests read an instance's parts from. A described widget reads what a host
//! gives it through [`Source`]; the probe is that host as a test gives it: values by prop, the widget
//! state to restore, the binding it offers, the events it handles queued as intents, and the components
//! in its parts.

#![allow(
    dead_code,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "each test file compiles this module and uses the helpers its cases need"
)]

use std::collections::BTreeMap;

use tilo_ui_base::LocalState;
use tilo_ui_base::widget::Element;
use tilo_ui_base::{
    component::Bound, component::BoundValue, component::Event, component::Fired, component::Intent,
    component::Registry, component::SharedRows, component::Source, component::Type,
    component::Value,
};

/// An instance as a test gives it: values by prop, the events it handles queued on `queue`, the
/// components and views in its parts, and the rows it hands out.
pub struct Node {
    pub kind: &'static str,
    pub key: String,
    pub props: BTreeMap<String, Value>,
    pub events: Vec<&'static str>,
    pub parts: Vec<(&'static str, Node)>,
    pub bound: Option<BoundValue>,
    pub queue: tilo_ui_base::Intents<Fired>,
    /// Views in a part, handed out once.
    pub views: Vec<(&'static str, Element)>,
    /// A plain text view this many times in every part, handed out each time it is asked.
    pub fillers: usize,
    pub rows: Option<SharedRows>,
    /// Every event is handled, not only those named with [`Node::on`].
    pub every_event: bool,
    /// The props are also the widget state to restore.
    pub restores: bool,
}

/// An instance keyed `key` with `props`, that restores no state and handles no event.
pub fn given(key: &str, props: &[(&str, Value)]) -> Node {
    let mut node = Node::new("", key, &tilo_ui_base::Intents::default());
    node.restores = false;
    for (name, value) in props {
        node.props.insert((*name).to_owned(), value.clone());
    }
    node
}

impl Node {
    /// An instance of the component `kind` keyed `key`, its events queued on `queue`.
    pub fn new(kind: &'static str, key: &str, queue: &tilo_ui_base::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            parts: Vec::new(),
            bound: None,
            queue: queue.clone(),
            views: Vec::new(),
            fillers: 0,
            rows: None,
            every_event: false,
            restores: true,
        }
    }

    /// Put the view `view` in the part `part`.
    #[must_use]
    pub fn view(mut self, part: &'static str, view: Element) -> Self {
        self.views.push((part, view));
        self
    }

    /// Hand out `rows` for every part that asks for them.
    #[must_use]
    pub fn rows(mut self, rows: SharedRows) -> Self {
        self.rows = Some(rows);
        self
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
        self.props.get(name).filter(|_| self.restores).cloned()
    }

    fn event(&mut self, name: &str) -> Event {
        if self.every_event || self.events.contains(&name) {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        self.bound.clone()
    }

    fn children(&mut self, part: &str) -> Vec<Element> {
        let mut children: Vec<Element> = self
            .views
            .iter_mut()
            .filter(|(name, _)| *name == part)
            .map(|(_, view)| std::mem::replace(view, Box::new(crate::widget::Absent::new())))
            .collect();
        children.extend(
            (0..self.fillers).map(|_| Box::new(crate::widget::Text::new("view")) as Element),
        );
        children
    }

    fn rows(&mut self, _part: &str) -> Option<SharedRows> {
        self.rows.clone()
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
pub fn edits(queue: &tilo_ui_base::Intents<Fired>, key: &str) -> Option<Intent> {
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

pub fn given_with_views(key: &str, props: &[(&str, Value)], views: usize) -> Node {
    Node {
        fillers: views,
        ..given(key, props)
    }
}

pub fn sample(ty: &Type) -> Value {
    match ty {
        Type::Text | Type::Enum => Value::Text("x".into()),
        Type::Int => Value::Int(1),
        Type::Float => Value::Float(1.0),
        Type::Bool => Value::Bool(true),
        Type::Actor => Value::Actor {
            identity: tilo_actions::MemberId::person("ada").into(),
            name: "Ada".into(),
            hue: None,
            picture: None,
        },
        Type::Resource => Value::Resource("r".into()),
        Type::Link => Value::Link {
            reference: "tl:a.b".into(),
            title: "x".into(),
        },
        Type::Time => Value::Text("2026-06-30T08:42:00Z".into()),
        Type::Icon => Value::Icon("pin".into()),
        Type::Picture => Value::Picture(tilo_ui_graph::component::Picture {
            src: "tilo-blob://dot".into(),
        }),
        Type::File => Value::File(tilo_ui_graph::component::File {
            name: "notes.md".into(),
            media: "text/markdown".into(),
            size: 12,
            url: "tilo-blob://kept".into(),
        }),
        Type::RichText => Value::Rich(tilo_ui_graph::rich::RichText::default()),
        Type::Choice { cases, .. } => Value::Choice(cases.first().cloned().unwrap_or_default()),
        Type::Option(_) => Value::None,
        Type::List(_) | Type::Set(_) => Value::List(Vec::new()),
        Type::Map { .. } => Value::Map(Vec::new()),
        Type::Action => Value::Action(tilo_ui_graph::component::Offered::default()),
        Type::Cases { name } => {
            let described = Registry::of(&[&crate::component_catalogue::TOOLKIT])
                .catalogue()
                .enums
                .into_iter()
                .find(|described| &described.name == name)
                .unwrap_or_else(|| panic!("`{name}` is described"));
            let case = &described.cases[0];
            Value::Case {
                case: case.name.clone(),
                fields: case
                    .fields
                    .iter()
                    .filter(|field| field.default.is_none())
                    .map(|field| (field.name.clone(), sample(&field.ty)))
                    .collect(),
            }
        }
    }
}
