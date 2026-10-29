//! Component descriptions: what each described toolkit widget takes and does, as data.
//!
//! A description is generated from the widget's Rust definition (`#[derive(Component)]` in the
//! toolkit), so it cannot drift from the widget. It is read by everything that composes
//! widgets without being Rust: tilolang reads the catalogue as its package `ui`, the generic
//! host adapter builds widgets from it, and a serialized graph names component instances by it.
//!
//! The parts of a description:
//!
//! - **props**: typed values;
//! - **slots**: child content, typed by what may go in it;
//! - **events**: data, never closures; each takes an action reference or a view-state update;
//! - **bindings**: two-way to a resource field, with the merge an edit is applied with;
//! - **state**: what the widget keeps between frames, which is lens state;
//! - **semantics**: its accessibility role, and which props name it and the resource it shows.
//!
//! The catalogue is versioned with the toolkit: [`FORMAT_VERSION`] is the shape of this format,
//! and [`Catalogue::toolkit`] the toolkit version the descriptions were generated from.

use serde::{Deserialize, Serialize};

/// The version of the description format: a change to the shape of [`Description`] bumps it.
pub const FORMAT_VERSION: u32 = 3;

/// Every described component of one toolkit version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    /// [`FORMAT_VERSION`] when it was written.
    pub format: u32,
    /// The toolkit version the descriptions come from.
    pub toolkit: String,
    pub components: Vec<Description>,
}

impl Catalogue {
    /// The description of the component `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Description> {
        self.components.iter().find(|c| c.name == name)
    }

    /// The names of every component, for messages.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.components.iter().map(|c| c.name.as_str()).collect()
    }

    /// Read a catalogue written by [`Self::to_json`].
    ///
    /// # Errors
    /// The text is not a catalogue, or one of another format version.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let catalogue: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if catalogue.format != FORMAT_VERSION {
            return Err(format!(
                "component catalogue format {} is not the supported {FORMAT_VERSION}",
                catalogue.format
            ));
        }
        Ok(catalogue)
    }

    /// The catalogue as pretty JSON, one stable text per catalogue.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }
}

/// The catalogue of the toolkit this crate ships with, as the toolkit generated it
/// (`components.json`; the toolkit's tests fail while it is stale).
///
/// # Panics
/// Never for the shipped file, which the toolkit's tests read back.
#[must_use]
pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CATALOGUE.get_or_init(|| {
        #[expect(
            clippy::expect_used,
            reason = "the shipped catalogue is written by the toolkit and read back by its tests"
        )]
        Catalogue::from_json(SHIPPED).expect("the shipped component catalogue reads")
    })
}

/// The text of the shipped catalogue.
pub const SHIPPED: &str = include_str!("../components.json");

/// One described component.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// The widget's name, as code calls it (`ListRow`).
    pub name: String,
    /// The widget's doc text.
    pub doc: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<Binding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<State>,
    pub semantics: Semantics,
}

impl Description {
    #[must_use]
    pub fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    #[must_use]
    pub fn slot(&self, name: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// The slot a component's block fills when it names none.
    #[must_use]
    pub fn default_slot(&self) -> Option<&Slot> {
        self.slots.iter().find(|s| s.default)
    }

    #[must_use]
    pub fn event(&self, name: &str) -> Option<&Event> {
        self.events.iter().find(|e| e.name == name)
    }

    #[must_use]
    pub fn binding(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.name == name)
    }

    #[must_use]
    pub fn state(&self, name: &str) -> Option<&State> {
        self.state.iter().find(|s| s.name == name)
    }

    /// The props given by position, in order.
    pub fn positional(&self) -> impl Iterator<Item = &Prop> {
        self.props.iter().filter(|p| p.positional)
    }

    /// The first line of the doc text, for messages.
    #[must_use]
    pub fn summary(&self) -> &str {
        first_line(&self.doc)
    }
}

/// The first line of a doc text.
#[must_use]
pub fn first_line(doc: &str) -> &str {
    doc.lines().next().unwrap_or("").trim()
}

/// A typed value a component takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prop {
    pub name: String,
    pub doc: String,
    #[serde(rename = "type")]
    pub ty: Type,
    /// Whether every instance gives it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Whether it may be given by position, in declaration order.
    #[serde(default, skip_serializing_if = "is_false")]
    pub positional: bool,
}

/// Child content a component takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub name: String,
    pub doc: String,
    /// What may go in it.
    pub accepts: Accepts,
    /// Whether it holds any number of children (else at most one).
    #[serde(default, skip_serializing_if = "is_false")]
    pub many: bool,
    /// Whether it is what the component's block fills when no slot is named.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default: bool,
}

/// What a slot accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accepts {
    /// Any view.
    View,
    /// Only the component of this name, whose props the slot's owner reads.
    Component(String),
    /// Any of the components of these names, in any order (a menu's commands, separators and
    /// emoji bar), whose props the slot's owner reads.
    OneOf(Vec<String>),
}

impl Accepts {
    /// Whether the slot holds components whose props its owner reads, rather than views.
    #[must_use]
    pub fn reads_components(&self) -> bool {
        !matches!(self, Self::View)
    }

    /// Whether a component of the name `name` may stand in the slot.
    #[must_use]
    pub fn takes(&self, name: &str) -> bool {
        match self {
            Self::View => true,
            Self::Component(want) => want == name,
            Self::OneOf(names) => names.iter().any(|want| want == name),
        }
    }

    /// What it takes, for messages: `a view`, `` `ui.MenuItem` `` or a list of them.
    #[must_use]
    pub fn show(&self) -> String {
        match self {
            Self::View => "a view".to_owned(),
            Self::Component(name) => format!("`ui.{name}`"),
            Self::OneOf(names) => names
                .iter()
                .map(|name| format!("`ui.{name}`"))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// Something that happens to a component. What it does is data: an action reference, invoked
/// through the door, or a view-state update; never a closure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub name: String,
    pub doc: String,
    /// The value the event carries (the key a list selected), when it carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Type>,
    /// Where the value lands, when the event says: a drop's place among the other items of
    /// what it was dropped on, counted from 0. An event with one carries `[value, at]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Type>,
}

/// A two-way binding to a resource field or view state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    pub doc: String,
    #[serde(rename = "type")]
    pub ty: Type,
    pub merge: Merge,
}

/// How an edit through a binding is applied to what it is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Merge {
    /// The value is replaced: a flag, a choice.
    Register,
    /// Text: the edit is spliced into it, never written over it, so concurrent edits merge.
    Splice,
    /// Rich text: its paragraphs spliced and its marks set, never written over.
    Rich,
}

/// What a widget keeps between frames (scroll, selection, caret, expansion). It is lens state:
/// personal, shown as presence, resumed on the person's other devices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub name: String,
    pub doc: String,
    #[serde(rename = "type")]
    pub ty: Type,
}

/// How assistive technology and tests see a component.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Semantics {
    /// The accessibility role, as `accesskit` names it.
    pub role: String,
    /// The prop that names the component to assistive technology, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The prop holding the resource the component presents, when it presents one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presents: Option<String>,
    /// The component draws nothing where it stands: it gives the view it stands in something
    /// to do (a context menu, a toolbar on hover, a swipe). Wherever a view's body puts it, it
    /// belongs to that view; a list item holds those of its row.
    #[serde(default, skip_serializing_if = "is_false")]
    pub attaches: bool,
}

/// The type of a prop, an event's value, a binding or a piece of state: the language's types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Type {
    Text,
    Int,
    Float,
    Bool,
    /// A participant: a person or an agent.
    Principal,
    /// A resource the component presents.
    Resource,
    /// A moment: a date and a time of day, as the language's `Time`.
    Time,
    /// One of the platform's icons, by its kebab-case name.
    Icon,
    /// Rich text, with how its references read for the person reading it.
    RichText,
    /// One of a closed set of cases: the design system's roles and tokens.
    Choice {
        name: String,
        cases: Vec<String>,
    },
    Option(Box<Type>),
    List(Box<Type>),
    /// Distinct values in no order the component relies on: who reacted with an emoji.
    Set(Box<Type>),
    /// Values by key: the people who reacted, by emoji.
    Map {
        key: Box<Type>,
        value: Box<Type>,
    },
    /// An action offered to the person, as a control shows it: what it is called, its icon and
    /// role, and what choosing it runs (an event of the instance, never a closure).
    Action,
}

impl Type {
    /// How the type reads in messages: `Option<Text>`, `TextRole`.
    #[must_use]
    pub fn show(&self) -> String {
        match self {
            Self::Text => "Text".into(),
            Self::Int => "Int".into(),
            Self::Float => "Float".into(),
            Self::Bool => "Bool".into(),
            Self::Principal => "Principal".into(),
            Self::Resource => "a resource".into(),
            Self::Time => "Time".into(),
            Self::Icon => "Icon".into(),
            Self::RichText => "RichText".into(),
            Self::Choice { name, .. } => name.clone(),
            Self::Option(inner) => format!("Option<{}>", inner.show()),
            Self::List(inner) => format!("List<{}>", inner.show()),
            Self::Set(inner) => format!("Set<{}>", inner.show()),
            Self::Map { key, value } => format!("Map<{}, {}>", key.show(), value.show()),
            Self::Action => "Action".into(),
        }
    }
}

/// A value of a [`Type`], as an instance of a component holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A participant: their identity, and what the room calls them.
    Principal {
        identity: String,
        name: String,
    },
    /// A resource, by its id.
    Resource(String),
    /// An icon's kebab-case name.
    Icon(String),
    /// A case of a choice, by name.
    Choice(String),
    /// Rich text, with how its references read for the person reading it.
    Rich(crate::rich::RichText),
    /// An `Option` that holds nothing.
    None,
    /// A list, or a set's values in the order they were given.
    List(Vec<Value>),
    /// A map's entries, each key once, in key order.
    Map(Vec<(Value, Value)>),
    /// An action offered to the person.
    Action(Offered),
}

/// An action offered to the person, as a component holds one: how a control shows it, and the
/// event of the instance that choosing it fires. Every field is always written: the wire's
/// encoding is not self-describing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offered {
    /// What it does, as a button or a menu names it.
    pub label: String,
    /// Its icon's kebab-case name, when it has one.
    pub icon: Option<String>,
    /// Whether it is its resource's primary action.
    pub primary: bool,
    /// Whether it takes something away: undone, not confirmed.
    pub destructive: bool,
    /// Whether the state it toggles is on, when it toggles one.
    pub on: Option<bool>,
    /// The event of the instance choosing it fires.
    pub event: String,
}

impl Value {
    /// The type the value is of, as far as the value says (`None` and an empty list say
    /// nothing).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "Text",
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::Bool(_) => "Bool",
            Self::Principal { .. } => "Principal",
            Self::Resource(_) => "a resource",
            Self::Icon(_) => "Icon",
            Self::Choice(_) => "a case",
            Self::Rich(_) => "RichText",
            Self::None => "nothing",
            Self::List(_) => "List",
            Self::Map(_) => "Map",
            Self::Action(_) => "Action",
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_false(flag: &bool) -> bool {
    !*flag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalogue_reads_and_names_each_component_once() {
        let catalogue = catalogue();
        let mut names = catalogue.names();
        let all = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all, "a component is described once");
        assert!(catalogue.get("ListRow").is_some());
    }

    #[test]
    fn a_catalogue_of_another_format_is_refused() {
        let other = Catalogue {
            format: FORMAT_VERSION + 1,
            toolkit: "0".into(),
            components: Vec::new(),
        };
        let text = serde_json::to_string(&other).unwrap_or_default();
        assert!(Catalogue::from_json(&text).is_err());
    }
}
