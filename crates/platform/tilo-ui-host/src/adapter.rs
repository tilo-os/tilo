//! The one host adapter: a view's node of a described component (`ui.ListRow`) as the
//! toolkit's widget.
//!
//! The adapter knows no widget. It reads each node through the component's description:
//!
//! - a **prop** is the node's prop of that name, converted to the description's type (a date
//!   reads the person's way, a participant by what the workspace calls them);
//! - an **event** the view gave the node is an intent queued for the window, which fires the
//!   action reference the view attached through the door, or applies its view-state update; a
//!   component that presents a resource opens it when activated, unless the view says otherwise;
//! - a **binding** goes through the model-binding path: the reconciler keeps the value in step
//!   with what it is bound to, and each edit is reported for the runtime to merge (text is
//!   spliced, never written over);
//! - a **slot**'s content is the node's children: a named slot's are those of its `slot` child,
//!   the default slot's are the node's own. A slot that accepts a component reads that
//!   component's props; one that accepts views holds each child built, in a [`Pane`] the
//!   reconciler reaches again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::{ResourceRef, UiNode, Value};
use ui::component::{
    Accepts, Bound, BoundValue, Catalogue, Description, Event, Fired, Intent, Merge, Source, Type,
    Value as Prop,
};
use ui::widget::Element;

#[cfg(test)]
use crate::pane::Pane;
use crate::reconcile::{Reconciler, UiEvent, presence_path};

/// What a node of a described component is called: `ui.` and the component's name.
pub const KIND_PREFIX: &str = "ui.";

/// The kind of a `slot` node: the content of a component's named slot.
pub const SLOT: &str = "slot";

/// The toolkit's components, as it describes them.
#[must_use]
pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(ui::component::catalogue)
}

/// The component a node kind names (`ui.ListRow`: `ListRow`).
#[must_use]
pub fn component_name(kind: &str) -> Option<&str> {
    kind.strip_prefix(KIND_PREFIX)
}

/// The kind of the component `name`'s nodes.
#[must_use]
pub fn kind(name: &str) -> String {
    format!("{KIND_PREFIX}{name}")
}

/// The description of the component a node is of.
#[must_use]
pub fn description_of(node: &UiNode) -> Option<&'static Description> {
    catalogue().get(component_name(&node.kind)?)
}

/// Where the rows of a virtual list come from: the host renders them as the list shows them.
/// Given the list node's key, its rows; `None` when the host renders no rows for it.
pub type RowsOf = Rc<dyn Fn(&str) -> Option<ui::component::SharedRows>>;

/// What a built component's events do, kept by its key until its node is built again.
struct Built {
    component: String,
    /// The events its view gave it.
    events: Vec<String>,
    /// The resource it presents, which it opens when activated without an event of its own.
    presents: Option<ResourceRef>,
    /// How an edit through its binding merges, when it has one.
    merge: Option<Merge>,
}

/// The events the components of one view report, and what each component's events do. Clones
/// share both.
#[derive(Clone, Default)]
pub struct Adapter {
    fired: ui::Intents<Fired>,
    built: Rc<RefCell<HashMap<String, Built>>>,
    /// The resources components were given, by id: what an event that carries one reports.
    resources: Rc<RefCell<HashMap<String, ResourceRef>>>,
    /// The actions the app publishes at the window, as the window offers them: what an event
    /// that runs one of them is bound to.
    published: Rc<RefCell<Vec<tilo_actions::ResourceSpec>>>,
}

impl Adapter {
    /// Offer the actions the app publishes (`resources`): an event that runs one of them is
    /// bound to the action itself, so pressing the control and anyone else running the action
    /// take one path, and what the window publishes names the control that is bound to it.
    pub fn published(&self, resources: Vec<tilo_actions::ResourceSpec>) {
        *self.published.borrow_mut() = resources;
    }
    /// What the components reported since the last call, as what the window applies.
    #[must_use]
    pub fn take_events(&self) -> Vec<UiEvent> {
        let built = self.built.borrow();
        self.fired
            .take()
            .into_iter()
            .filter_map(|fired| {
                let component = built.get(&fired.key)?;
                let description = catalogue().get(&component.component)?;
                if description.binding(&fired.event).is_some() {
                    return match fired.value {
                        Some(Prop::Text(text)) => Some(UiEvent::Edit(fired.key, text)),
                        Some(Prop::Bool(checked)) => Some(UiEvent::Toggle(fired.key, checked)),
                        // A number's edit goes as its text: the runtime reads it back as the
                        // number the bound field holds (a number, a date's ISO text).
                        Some(Prop::Int(number)) => {
                            Some(UiEvent::Edit(fired.key, number.to_string()))
                        }
                        Some(Prop::Float(number)) => {
                            Some(UiEvent::Edit(fired.key, number.to_string()))
                        }
                        Some(Prop::Rich(rich)) => Some(UiEvent::EditRich(fired.key, rich)),
                        _ => None,
                    };
                }
                if component.events.contains(&fired.event) {
                    let value = fired.value.as_ref().and_then(|value| self.value_of(value));
                    return Some(UiEvent::Fire(fired.key, fired.event, value));
                }
                // A reference in rich text the view gave nothing of its own to do: open it.
                if fired.event == ui::widget::OPEN_REFERENCE {
                    return match fired.value {
                        Some(Prop::Text(reference)) => Some(UiEvent::OpenReference(reference)),
                        _ => None,
                    };
                }
                component.presents.clone().map(UiEvent::Open)
            })
            .collect()
    }

    /// What an event carries, as the runtime's value: a resource by the id it was given as.
    fn value_of(&self, value: &Prop) -> Option<Value> {
        Some(match value {
            Prop::Text(text) | Prop::Choice(text) | Prop::Icon(text) => Value::Text(text.clone()),
            Prop::Int(number) => Value::Int(*number),
            Prop::Float(number) => Value::Float(*number),
            Prop::Bool(flag) => Value::Bool(*flag),
            // A resource the view gave as a prop comes back whole; one no prop named — what a
            // drag carried from another view, or a drop target takes from any runtime — comes
            // back by the address the component fired, which is all the toolkit knows of it.
            // Its type is read where the drop lands, from the record or the reading that holds
            // it, so the value is never lost at this boundary.
            Prop::Resource(id) => {
                Value::Ref(self.resources.borrow().get(id).cloned().unwrap_or_else(|| {
                    ResourceRef {
                        id: id.clone(),
                        ty: String::new(),
                    }
                }))
            }
            Prop::Principal { identity, name } => Value::Principal(crate::Principal {
                id: identity.clone(),
                name: name.clone(),
            }),
            // What a drop carries: the card and its place.
            Prop::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.value_of(item))
                    .collect::<Option<_>>()?,
            ),
            Prop::None
            | Prop::Rich(_)
            | Prop::Map(_)
            | Prop::Action(_)
            | Prop::Picture(_)
            | Prop::Case { .. } => {
                return None;
            }
        })
    }

    /// Keep the resource `target` a component was given, by its id.
    fn given(&self, target: &ResourceRef) {
        self.resources
            .borrow_mut()
            .entry(target.id_string())
            .or_insert_with(|| target.clone());
    }

    /// Keep what the events of `node`, about to be built, do.
    fn remember(&self, node: &UiNode, description: &Description) {
        self.built.borrow_mut().insert(
            node.key.clone(),
            Built {
                component: description.name.clone(),
                events: node.events.keys().cloned().collect(),
                presents: presented(node, description),
                merge: bound(node, description).map(|binding| binding.merge),
            },
        );
    }

    /// How an edit through the bound control `key` merges into what it is bound to, as the
    /// component that built it says: `None` for a key no component with a binding built.
    #[must_use]
    pub fn merge_of(&self, key: &str) -> Option<Merge> {
        self.built.borrow().get(key)?.merge
    }
}

/// The binding of `node`'s component an edit it reports is for: the one its `bind` prop names,
/// or the only one it has.
fn bound<'a>(
    node: &UiNode,
    description: &'a Description,
) -> Option<&'a ui::component::BindingDescription> {
    match node.props.get("bind") {
        Some(Value::Text(name)) => description.binding(name),
        _ if description.bindings.len() == 1 => description.bindings.first(),
        _ => None,
    }
}

/// The resource `node` presents, when its component presents one.
fn presented(node: &UiNode, description: &Description) -> Option<ResourceRef> {
    let prop = description.semantics.presents.as_ref()?;
    match node.props.get(prop) {
        Some(Value::Ref(reference)) => Some(reference.clone()),
        _ => None,
    }
}

/// The nodes in the slot `slot` of `node`, a view call's or a loop row's fragment seen through:
/// the components a slot that takes components holds, wherever the view's calls and loops put
/// them.
#[must_use]
pub fn slot_parts<'a>(node: &'a UiNode, description: &Description, slot: &str) -> Vec<&'a UiNode> {
    fn through<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        if node.kind == "fragment" {
            for child in &node.children {
                through(child, out);
            }
        } else {
            out.push(node);
        }
    }
    let mut out = Vec::new();
    for child in slot_nodes(node, description, slot) {
        through(child, &mut out);
    }
    out
}

/// The nodes in the slot `slot` of `node`: those of its `slot` child of that name, and for the
/// default slot those given without a name too.
#[must_use]
pub fn slot_nodes<'a>(node: &'a UiNode, description: &Description, slot: &str) -> Vec<&'a UiNode> {
    let unnamed = description.slot(slot).is_some_and(|s| s.default);
    let named = node.children.iter().filter(|child| {
        child.kind == SLOT
            && matches!(child.props.get("name"), Some(Value::Text(name)) if name == slot)
    });
    node.children
        .iter()
        .filter(|child| unnamed && child.kind != SLOT)
        .chain(named.flat_map(|child| child.children.iter()))
        .collect()
}

/// A node of a described component, as the toolkit reads an instance.
pub struct NodeSource<'a> {
    node: &'a UiNode,
    description: &'static Description,
    reconciler: &'a mut Reconciler,
    /// Whether the views in its slots are built: not when its props are read to take them in
    /// place, where the reconciler reaches the views it has.
    build_views: bool,
}

impl<'a> NodeSource<'a> {
    /// `node`, to build its widget.
    pub fn new(
        node: &'a UiNode,
        description: &'static Description,
        reconciler: &'a mut Reconciler,
    ) -> Self {
        reconciler.adapter().remember(node, description);
        Self {
            node,
            description,
            reconciler,
            build_views: true,
        }
    }

    /// `node`, to read its props into a widget built before.
    pub fn in_place(
        node: &'a UiNode,
        description: &'static Description,
        reconciler: &'a mut Reconciler,
    ) -> Self {
        Self {
            build_views: false,
            ..Self::new(node, description, reconciler)
        }
    }

    fn value(&self, name: &str, ty: &Type) -> Option<Prop> {
        // The tone of a date fact is the presenter's, not the view's: a component that declares
        // `<prop>_tone` reads it off the date `<prop>` holds, so a day gone by is overdue in
        // every list, card and chip alike.
        if let (Type::Choice { name: choice, .. }, Some(base)) = (ty, name.strip_suffix("_tone"))
            && choice == "Tone"
            && !self.node.props.contains_key(name)
            && let Some(tone) = crate::date::shown_tone(self.node, base)
        {
            let case = match tone {
                ui::widget::Tone::Danger => "danger",
                ui::widget::Tone::Warning => "warning",
                ui::widget::Tone::Success => "success",
                ui::widget::Tone::Info => "info",
                ui::widget::Tone::Neutral | ui::widget::Tone::Presence(_) => "neutral",
            };
            return Some(Prop::Choice(case.to_owned()));
        }
        let value = self.node.props.get(name)?;
        let text = match ty {
            Type::Option(inner) => **inner == Type::Text && !matches!(value, Value::Opt(None)),
            other => *other == Type::Text,
        };
        if text {
            // A date or a time reads as briefly as is still clear, as lists show them.
            return crate::date::shown_brief(self.node, name).map(Prop::Text);
        }
        convert(value, ty, self.reconciler)
    }
}

/// The value a view gave the control `node` as its `name` rather than bound it to
/// (`ui.Slider(value: level)` over a device's reading): the control shows it, and what the
/// person does with it goes to the event the view handles, never back into the value.
fn shown_value(
    node: &UiNode,
    description: &Description,
    reconciler: &mut Reconciler,
    name: &str,
) -> Option<BoundValue> {
    fn shown<T>(state: ui::LocalState<T>) -> Bound<T> {
        Bound {
            state,
            edits: None,
            name: String::new(),
            path: Vec::new(),
        }
    }
    let given = node.props.get(name)?;
    let key = &node.key;
    Some(match (&description.binding(name)?.ty, given) {
        (Type::Float, Value::Float(number)) => {
            let state = reconciler.number_state(key);
            reconciler.set_number(key, *number);
            BoundValue::Float(shown(state))
        }
        (Type::Int, Value::Int(number)) => {
            let state = reconciler.count_state(key);
            reconciler.set_count(key, *number);
            BoundValue::Int(shown(state))
        }
        (Type::Bool, Value::Bool(flag)) => {
            let state = reconciler.flag_state(key);
            reconciler.set_flag(key, *flag);
            BoundValue::Bool(shown(state))
        }
        (Type::Text, Value::Text(text)) => {
            let state = reconciler.text_state(key);
            reconciler.set_text(key, text);
            BoundValue::Text(shown(state))
        }
        _ => return None,
    })
}

/// `value` as a value of the description's `ty`.
fn convert(value: &Value, ty: &Type, reconciler: &Reconciler) -> Option<Prop> {
    Some(match (ty, value) {
        (Type::Option(_), Value::Opt(None)) => Prop::None,
        (Type::Option(inner), Value::Opt(Some(held))) => convert(held, inner, reconciler)?,
        (Type::Option(inner), other) => convert(other, inner, reconciler)?,
        (_, Value::Opt(Some(held))) => convert(held, ty, reconciler)?,
        (Type::Text, other) => Prop::Text(crate::reconcile::display(other)),
        (Type::Int, Value::Int(number)) => Prop::Int(*number),
        (Type::Float, Value::Float(number)) => Prop::Float(*number),
        #[expect(
            clippy::cast_precision_loss,
            reason = "a whole number given for a size or a weight, far inside f64"
        )]
        (Type::Float, Value::Int(number)) => Prop::Float(*number as f64),
        (Type::Bool, Value::Bool(flag)) => Prop::Bool(*flag),
        (Type::Bool, Value::Int(number)) => Prop::Bool(*number != 0),
        (Type::Principal, Value::Principal(who)) => Prop::Principal {
            identity: who.id.clone(),
            name: who.name.clone(),
        },
        (Type::Resource, Value::Ref(reference)) => {
            reconciler.adapter().given(reference);
            Prop::Resource(reference.id.clone())
        }
        (Type::Icon, Value::Text(name)) => Prop::Icon(name.replace('_', "-")),
        (Type::Picture, Value::Picture(picture)) => Prop::Picture(ui::component::Picture {
            src: picture.src.clone(),
        }),
        // A moment reaches the component as the instant it is, never as text read the
        // person's way: the component decides how to show it.
        (Type::Time, Value::Text(at)) => Prop::Text(at.clone()),
        (Type::Choice { .. }, Value::Text(case) | Value::Enum(case)) => Prop::Choice(case.clone()),
        (Type::Cases { name }, Value::Case { case, fields }) => {
            let described = catalogue().enums.iter().find(|set| &set.name == name)?;
            let shape = described.cases.iter().find(|c| &c.name == case)?;
            Prop::Case {
                case: case.clone(),
                fields: fields
                    .iter()
                    .map(|(field, held)| {
                        let ty = &shape.fields.iter().find(|f| &f.name == field)?.ty;
                        Some((field.clone(), convert(held, ty, reconciler)?))
                    })
                    .collect::<Option<Vec<_>>>()?,
            }
        }
        (Type::List(inner) | Type::Set(inner), Value::List(items)) => Prop::List(
            items
                .iter()
                .map(|item| convert(item, inner, reconciler))
                .collect::<Option<Vec<_>>>()?,
        ),
        (Type::Map { key, value }, Value::Map(entries)) => Prop::Map(
            entries
                .iter()
                .map(|(k, v)| Some((convert(k, key, reconciler)?, convert(v, value, reconciler)?)))
                .collect::<Option<Vec<_>>>()?,
        ),
        (Type::Action, Value::Action(offered)) => Prop::Action(offered.clone()),
        (Type::RichText, Value::Rich(rich)) => Prop::Rich(rich.clone()),
        _ => return None,
    })
}

impl NodeSource<'_> {
    /// The action the event `name` of the node runs, when it runs one the app published and
    /// that action takes nothing: the control is bound to it, so it offers the action the way
    /// every control offers one. An event that runs no action, an action the app has not
    /// published yet, and an event that is not its control's own action
    /// ([`ui::component::Event::press`], or one of the actions a control offers by a prop)
    /// are the view's own.
    fn published(&self, name: &str) -> Option<tilo_actions::ActionBinding<()>> {
        if !self.presses(name) {
            return None;
        }
        let (resource, action) = self.node.events.get(name)?.invokes.as_ref()?;
        bound_to(
            &self.reconciler.adapter().published.borrow(),
            resource,
            action,
        )
    }

    /// Whether the event `name` is a press of its control: the event the description declares
    /// for it, or one of the actions the control offers by a prop (`add`, `actions`), whose
    /// event runs the action it offers.
    fn presses(&self, name: &str) -> bool {
        if self
            .description
            .event(name)
            .is_some_and(|event| event.press)
        {
            return true;
        }
        tilo_render_protocol::patch::validate::offers(self.description, name)
    }
}

/// The binding of the action (`resource`, `action`) of the actions `published`: an event that
/// runs it is bound to the action itself, so pressing the control and anyone else running the
/// action take the one path. `None` when the app publishes no such resource, or none of the
/// resource's actions is this one that takes no arguments.
pub(crate) fn bound_to(
    published: &[tilo_actions::ResourceSpec],
    resource: &str,
    action: &str,
) -> Option<tilo_actions::ActionBinding<()>> {
    // The app publishes the resource it names as its own id with the app's before it: the
    // action the view attached names the resource the same way the app's surface does.
    let held = published.iter().find(|held| {
        held.id
            .as_str()
            .rsplit_once(':')
            .is_some_and(|(_, id)| id == resource)
    })?;
    tilo_actions::ActionBinding::unit_for(held, action).ok()
}

impl Source for NodeSource<'_> {
    fn key(&self) -> &str {
        &self.node.key
    }

    fn prop(&mut self, name: &str) -> Option<Prop> {
        let ty = &self.description.prop(name)?.ty;
        self.value(name, ty)
    }

    fn state(&mut self, name: &str) -> Option<Prop> {
        let ty = &self.description.state(name)?.ty;
        self.value(name, ty)
    }

    fn event(&mut self, name: &str) -> Event {
        let given = self.node.events.contains_key(name);
        let opens = (name == "activate" && presented(self.node, self.description).is_some())
            || name == ui::widget::OPEN_REFERENCE;
        if given || opens {
            // An event that runs an action the app publishes runs it the one way every action
            // runs: the control is bound to it, and the host that owns the tree hands the
            // invocation to the app. A view event, and an action the app has not published,
            // stays the view's own.
            if let Some(binding) = self.published(name) {
                return Event::Action(binding);
            }
            Event::View(Intent::new(
                &self.reconciler.adapter().fired,
                &self.node.key,
                name,
            ))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, name: &str) -> Option<BoundValue> {
        let Some(binding) = self.node.binding.as_ref() else {
            if let Some(shown) = shown_value(self.node, self.description, self.reconciler, name) {
                return Some(shown);
            }
            // A text field the view gave no value is its own draft, keyed by where it stands:
            // the entry at the end of a list keeps what is typed into it across the app's own
            // changes, and commits it through its submit.
            let ty = &self.description.binding(name)?.ty;
            if *ty != Type::Text || self.node.kind != kind("TextInput") {
                return None;
            }
            let key = self.node.key.clone();
            return Some(BoundValue::Text(Bound {
                state: self.reconciler.text_state(&key),
                edits: None,
                name: String::new(),
                path: Vec::new(),
            }));
        };
        let named = match self.node.props.get("bind") {
            Some(Value::Text(bound)) => bound == name,
            _ => self.description.bindings.len() == 1,
        };
        if !named {
            return None;
        }
        let ty = &self.description.binding(name)?.ty;
        let edits = Some(Intent::new(
            &self.reconciler.adapter().fired,
            &self.node.key,
            name,
        ));
        let called = binding.name.clone();
        let path = presence_path(self.node);
        let key = self.node.key.clone();
        Some(match ty {
            // A moment is held as its ISO text (or the day it names), a person as their
            // identity, and a case as its name: what its widget shows, and what an edit carries.
            Type::Text | Type::Time | Type::Principal | Type::Enum => BoundValue::Text(Bound {
                state: self.reconciler.text_state(&key),
                edits,
                name: called,
                path,
            }),
            Type::Bool => BoundValue::Bool(Bound {
                state: self.reconciler.flag_state(&key),
                edits,
                name: called,
                path,
            }),
            Type::Int => BoundValue::Int(Bound {
                state: self.reconciler.count_state(&key),
                edits,
                name: called,
                path,
            }),
            Type::Float => BoundValue::Float(Bound {
                state: self.reconciler.number_state(&key),
                edits,
                name: called,
                path,
            }),
            Type::RichText => BoundValue::Rich(Bound {
                state: self.reconciler.rich_state(&key),
                edits,
                name: called,
                path,
            }),
            _ => return None,
        })
    }

    /// Each view built: a component as its widget (a header's button stays the button the
    /// header lays out), anything else in a pane the reconciler reaches again, each carrying
    /// the fit its node wrote so the component's slot sizes it as the view asked ([`Fitted`]).
    fn children(&mut self, slot: &str) -> Vec<Element> {
        if !self.build_views {
            return Vec::new();
        }
        slot_nodes(self.node, self.description, slot)
            .into_iter()
            .map(|child| self.reconciler.slot_view(child))
            .collect()
    }

    fn slot_kinds(&mut self, slot: &str) -> Vec<Option<String>> {
        slot_parts(self.node, self.description, slot)
            .into_iter()
            .map(|child| component_name(&child.kind).map(str::to_owned))
            .collect()
    }

    fn rows(&mut self, slot: &str) -> Option<ui::component::SharedRows> {
        let is_rows = self
            .description
            .slot(slot)
            .is_some_and(|s| s.default && s.many);
        if !is_rows || !crate::is_virtual(&self.node.kind) {
            return None;
        }
        self.reconciler.rows_of(&self.node.key)
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let nodes = slot_parts(self.node, self.description, slot);
        let mut count = 0usize;
        for child in nodes {
            let Some(description) = description_of(child) else {
                return Err(format!(
                    "holds a `{}`, which is not a component",
                    child.kind
                ));
            };
            let mut source = NodeSource {
                node: child,
                description,
                reconciler: &mut *self.reconciler,
                build_views: self.build_views,
            };
            source.reconciler.adapter().remember(child, description);
            read(&mut source)?;
            count = count.saturating_add(1);
        }
        Ok(count)
    }
}

/// Whether two nodes of a component give its widget the same data: the same props, binding
/// and events (an event name bound to another action is another event: the widget holds the
/// action it was built with), and in the slots that accept components, the same components by
/// the same rule.
#[must_use]
pub fn same_data(a: &UiNode, b: &UiNode, description: &Description) -> bool {
    // The rows of a virtual list change through the host's rows, never through the node.
    let virtual_rows = crate::is_virtual(&b.kind);
    a.props == b.props
        && a.binding == b.binding
        && a.events == b.events
        && description
            .slots
            .iter()
            .filter(|slot| slot.accepts.reads_components())
            .filter(|slot| !(virtual_rows && slot.default))
            .all(|slot| {
                let (old, new) = (
                    slot_parts(a, description, &slot.name),
                    slot_parts(b, description, &slot.name),
                );
                old.len() == new.len()
                    && old.iter().zip(&new).all(|(x, y)| {
                        x.kind == y.kind
                            && x.key == y.key
                            && description_of(y).is_some_and(|d| {
                                // A part's own views (a hover toolbar's buttons) are its data.
                                same_data(x, y, d) && views_of(x, d) == views_of(y, d)
                            })
                    })
            })
}

/// The views in a component node's slots, in order, by slot, key and kind: a component whose
/// views keep this shape keeps its widget, and its views are reconciled where they stand.
#[must_use]
pub fn views_of<'a>(node: &'a UiNode, description: &'a Description) -> Vec<(&'a str, &'a UiNode)> {
    description
        .slots
        .iter()
        .filter(|slot| slot.accepts == Accepts::View)
        .flat_map(|slot| {
            slot_nodes(node, description, &slot.name)
                .into_iter()
                .map(move |child| (slot.name.as_str(), child))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{Binding, Event, ResourceRef, UiNode, Value};
    use ui::widget::{Element, StructureChange, Widget};

    use super::*;
    use ui::component::Component as _;

    fn node(name: &str, key: &str, props: &[(&str, Value)]) -> UiNode {
        let mut node = UiNode::empty(&kind(name), key);
        for (prop, value) in props {
            node.props.insert((*prop).to_owned(), value.clone());
        }
        node
    }

    fn text(key: &str, value: &str) -> UiNode {
        node("Text", key, &[("value", Value::text(value))])
    }

    fn reconciler() -> Reconciler {
        Reconciler::new(Rc::default())
    }

    /// The identity of a widget, to tell the same one from one built again.
    fn identity(widget: &dyn Widget) -> usize {
        std::ptr::from_ref(widget).cast::<()>() as usize
    }

    /// The widgets the views of a component hold, in order: those in panes, and the texts the
    /// tests give each view, one letter each (a component's own title is a word).
    fn views(widget: &mut Element) -> Vec<usize> {
        fn walk(widget: &mut dyn Widget, out: &mut Vec<usize>) {
            if let Some(pane) = widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Pane>())
            {
                out.push(identity(pane.content_mut().as_ref()));
                return;
            }
            let lettered = widget
                .accessibility()
                .and_then(|node| node.name)
                .is_some_and(|name| name.chars().count() == 1);
            if lettered {
                out.push(identity(&*widget));
                return;
            }
            for child in widget.children_mut() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(widget.as_mut(), &mut out);
        out
    }

    fn label(widget: &Element) -> Option<String> {
        widget.accessibility().and_then(|node| node.name)
    }

    #[test]
    fn a_prop_patch_sets_the_widget_in_place_and_damages_it() {
        let mut reconciler = reconciler();
        let old = text("t", "1");
        let mut widget = reconciler.build(&old);
        let before = identity(widget.as_ref());
        let new = text("t", "2");
        assert!(
            reconciler.update_in(&mut widget, &old, &new).is_none(),
            "taken in place"
        );
        assert_eq!(identity(widget.as_ref()), before, "the same widget");
        assert_eq!(
            label(&widget).as_deref(),
            Some("2"),
            "showing the new value"
        );

        // A prop the widget cannot take in place builds it again.
        let mut titled = text("t", "2");
        titled.props.insert("role".to_owned(), Value::text("title"));
        assert!(reconciler.update_in(&mut widget, &new, &titled).is_some());
    }

    #[test]
    fn slot_patches_insert_remove_and_move_views_by_key() {
        let mut reconciler = reconciler();
        let section = |children: Vec<UiNode>| UiNode {
            children,
            ..node("Section", "s", &[("title", Value::text("Today"))])
        };
        let old = section(vec![text("a", "a"), text("b", "b"), text("c", "c")]);
        let mut widget = reconciler.build(&old);
        let section_before = identity(widget.as_ref());
        let [a, _, c] = views(&mut widget)[..] else {
            panic!("three views");
        };
        let _ = widget.take_structure_change();
        // A new view at the front, `b` gone, `c` moved before `a`.
        let new = section(vec![text("n", "n"), text("c", "c"), text("a", "a")]);
        assert!(
            reconciler.update_in(&mut widget, &old, &new).is_none(),
            "the section stays"
        );
        assert_eq!(identity(widget.as_ref()), section_before);
        let after = views(&mut widget);
        assert_eq!(after.len(), 3);
        assert_eq!(&after[1..], &[c, a], "kept views follow their keys");
        assert_eq!(
            widget.take_structure_change(),
            StructureChange::Described,
            "the change is described, not a rebuild"
        );

        // A named slot is a `slot` child: the card's body is its default slot.
        let card = |children: Vec<UiNode>| UiNode {
            children,
            ..node("Card", "card", &[("title", Value::text("Notes"))])
        };
        let old = card(vec![text("x", "x")]);
        let mut widget = reconciler.build(&old);
        let card_before = identity(widget.as_ref());
        let new = card(vec![text("x", "x"), text("y", "y")]);
        assert!(reconciler.update_in(&mut widget, &old, &new).is_none());
        assert_eq!(identity(widget.as_ref()), card_before);
        assert_eq!(views(&mut widget).len(), 2);
    }

    #[test]
    fn a_named_slot_holds_the_children_of_its_slot_node() {
        let mut row = node("ListRow", "row", &[("title", Value::text("Ada"))]);
        let mut avatar = UiNode::empty(SLOT, "row#avatar");
        avatar
            .props
            .insert("name".to_owned(), Value::text("avatar"));
        avatar.children.push(node(
            "Avatar",
            "row#avatar/who",
            &[
                ("name", Value::text("Ada")),
                ("glyph", Value::text("users")),
            ],
        ));
        row.children.push(avatar);
        let description = description_of(&row).expect("described");
        assert_eq!(slot_nodes(&row, description, "avatar").len(), 1);
        assert!(slot_nodes(&row, description, "trailing").is_empty());
        let mut reconciler = reconciler();
        let mut source = NodeSource::new(&row, description, &mut reconciler);
        let props = ui::widget::ListRowProps::from_source(&mut source).expect("reads");
        let avatar = props
            .avatar
            .expect("the avatar slot reads the Avatar's props");
        assert_eq!(avatar.name.as_deref(), Some("Ada"));
        assert_eq!(avatar.glyph, Some(ui::icons::Icon::Users));
    }

    #[test]
    fn a_drop_carries_its_resource_even_when_no_prop_gave_it() {
        // A drag that started in another view — the shell's place handle, a chip of another
        // app's transcript — reaches a drop target carrying its resource by its published
        // address alone. No prop of this view gave that resource, so the adapter has no map
        // entry for it; the event still carries the resource, by its address, and its type is
        // read where the drop lands. Without that the value vanished at this boundary and the
        // drop arrived at the runtime with nothing ("event `drop` carries no value").
        let mut reconciler = reconciler();
        let mut target = node("DropTarget", "composer", &[]);
        target.events.insert("drop".to_owned(), Event::default());
        let described = description_of(&target).expect("described");
        let event = NodeSource::new(&target, described, &mut reconciler).event("drop");
        event.fire(Some(ui::component::Value::Resource(
            "notes.pages:page_1".to_owned(),
        )));
        assert_eq!(
            reconciler.adapter().take_events(),
            vec![UiEvent::Fire(
                "composer".to_owned(),
                "drop".to_owned(),
                Some(Value::Ref(ResourceRef {
                    id: "notes.pages:page_1".to_owned(),
                    // The adapter does not know the type: it reads where the drop lands.
                    ty: String::new(),
                })),
            )],
            "the drop carries the resource the drag let go with, by its address"
        );
    }

    #[test]
    fn events_are_the_views_action_references_and_a_presented_resource_opens() {
        let mut reconciler = reconciler();
        let mut button = node("Button", "b", &[("label", Value::text("Send"))]);
        button.events.insert("press".to_owned(), Event::default());
        let description = description_of(&button).expect("described");
        let event = NodeSource::new(&button, description, &mut reconciler).event("press");
        event.fire(None);
        assert_eq!(
            reconciler.adapter().take_events(),
            vec![UiEvent::Fire("b".to_owned(), "press".to_owned(), None)],
            "the window fires the action reference the view attached"
        );

        let note = ResourceRef {
            id: "doc".to_owned(),
            ty: "note".to_owned(),
        };
        let row = node(
            "ListRow",
            "r",
            &[
                ("title", Value::text("Q3")),
                ("item", Value::Ref(note.clone())),
            ],
        );
        let description = description_of(&row).expect("described");
        let event = NodeSource::new(&row, description, &mut reconciler).event("activate");
        event.fire(None);
        assert_eq!(
            reconciler.adapter().take_events(),
            vec![UiEvent::Open(note)]
        );

        let plain = text("t", "x");
        let description = description_of(&plain).expect("described");
        let mut source = NodeSource::new(&plain, description, &mut reconciler);
        assert!(
            source.event("press").is_none(),
            "no event the view did not give"
        );
    }

    #[test]
    fn rich_text_builds_and_an_activated_reference_opens_what_it_refers_to() {
        let mut reconciler = reconciler();
        let rich = tilo_render_protocol::patch::rich::RichText {
            blocks: vec![tilo_render_protocol::patch::rich::Block {
                kind: "paragraph".to_owned(),
                text: "see Fix it".to_owned(),
                ..Default::default()
            }],
            embeds: Vec::new(),
        };
        let body = node("BlockEditor", "body", &[("value", Value::Rich(rich))]);
        let widget = reconciler.build(&body);
        assert_eq!(
            widget.name(),
            "BlockEditor",
            "rich text is the block editor's"
        );
        let description = description_of(&body).expect("described");
        let event =
            NodeSource::new(&body, description, &mut reconciler).event(ui::widget::OPEN_REFERENCE);
        event.fire(Some(ui::component::Value::Text(
            "{\"app\":\"a.b\"}".to_owned(),
        )));
        assert_eq!(
            reconciler.adapter().take_events(),
            vec![UiEvent::OpenReference("{\"app\":\"a.b\"}".to_owned())],
            "the view gave the event nothing of its own: the reference is opened"
        );
    }

    #[test]
    fn a_binding_goes_through_the_model_binding_path_and_reports_edits() {
        let mut reconciler = reconciler();
        let mut input = node("TextInput", "i", &[("bind", Value::text("value"))]);
        input.binding = Some(Binding {
            field: Some(("doc".to_owned(), "title".to_owned())),
            name: "Title".to_owned(),
        });
        let description = description_of(&input).expect("described");
        let bound = {
            let mut source = NodeSource::new(&input, description, &mut reconciler);
            source.binding("value")
        };
        let Some(ui::component::BoundValue::Text(bound)) = bound else {
            panic!("a text binding");
        };
        assert_eq!(bound.name, "Title", "named as the field is");
        reconciler.set_text("i", "Q3 plan");
        assert_eq!(
            bound.state.get(),
            "Q3 plan",
            "the value the window keeps in step"
        );
        bound.edited(ui::component::Value::Text("Q3 plans".to_owned()));
        assert_eq!(
            reconciler.adapter().take_events(),
            vec![UiEvent::Edit("i".to_owned(), "Q3 plans".to_owned())],
            "an edit is reported for the runtime to splice"
        );
    }

    #[test]
    fn same_data_compares_events_and_component_slots_by_value() {
        let description = catalogue().get("ListRow").expect("described");
        let mut a = node("ListRow", "r", &[("title", Value::text("x"))]);
        a.events.insert("activate".to_owned(), Event::default());
        let b = a.clone();
        assert!(same_data(&a, &b, description), "the same events");
        let mut bound = b.clone();
        bound.events.insert(
            "activate".to_owned(),
            Event {
                invokes: Some(("rows/r1".to_owned(), "open".to_owned())),
                ..Event::default()
            },
        );
        assert!(
            !same_data(&a, &bound, description),
            "an event bound to an action is not the event it was"
        );
        let mut b = b;
        b.props.insert("title".to_owned(), Value::text("y"));
        assert!(!same_data(&a, &b, description));
        let _ = BTreeMap::<String, Value>::new();
    }
}
