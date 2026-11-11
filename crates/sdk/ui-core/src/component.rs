//! Components: the toolkit's widgets as described, composable data.
//!
//! A described widget has **props**: a plain struct with `#[derive(Component)]`, and the derive
//! generates its [`Description`] from it (props, slots, events, bindings, state and semantics, each
//! with its doc text), so the description cannot drift from the widget: a prop or an event whose
//! type has no description does not compile, and an event is an [`Event`], data, never a closure.
//!
//! The widget takes its props through [`Described`]: built from them, and takes changed props in
//! place. Its Rust builder is the same widget; the props are the declarative form, which the
//! generic host adapter, tilolang (the package `ui`) and serialized graphs use. Each widget takes
//! its props apart field by field, so a prop it does not take does not compile either.
//!
//! The [`catalogue`] of every described widget ships as data
//! (`tilo_ui_graph::component::SHIPPED`), so tools that do not link the toolkit read it; a test
//! fails while the shipped one is not the one the toolkit generates.
//!
//! ```
//! use tilo_ui::component::{Component, Type};
//! use tilo_ui::widget::ListRowProps;
//!
//! let row = ListRowProps::describe();
//! assert_eq!(row.name, "ListRow");
//! assert_eq!(row.prop("title").map(|p| &p.ty), Some(&Type::Text));
//! assert!(row.event("activate").is_some());
//! ```
//!
//! A prop whose type has no description does not compile, so a closure cannot be one:
//!
//! ```compile_fail,E0277
//! use tilo_ui::component::Component;
//!
//! /// A row.
//! #[derive(Component, Default)]
//! #[component(name = "Row", role = "ListItem")]
//! struct RowProps {
//!     /// What runs when it is pressed.
//!     pressed: Option<Box<dyn FnMut()>>,
//! }
//! ```
//!
//! nor can an event, which is an [`Event`]:
//!
//! ```compile_fail,E0308
//! use tilo_ui::component::Component;
//!
//! /// A row.
//! #[derive(Component, Default)]
//! #[component(name = "Row", role = "ListItem")]
//! struct RowProps {
//!     /// What runs when it is pressed.
//!     #[component(event)]
//!     pressed: Option<Box<dyn FnMut()>>,
//! }
//! ```
//!
//! and every part is documented, since its doc text is what an author is shown:
//!
//! ```compile_fail
//! use tilo_ui::component::Component;
//!
//! /// A row.
//! #[derive(Component, Default)]
//! #[component(name = "Row", role = "ListItem")]
//! struct RowProps {
//!     title: String,
//! }
//! ```

use crate::LocalState;
use crate::icons::Icon;
use crate::widget::Element;

pub use tilo_ui_graph::component::{
    Accepts, Binding as BindingDescription, Catalogue, Description, Event as EventDescription,
    FORMAT_VERSION, Merge, Offered, Picture, Prop, Semantics, Slot, State as StateDescription,
    Type, Value,
};
pub use tilo_ui_macros::{Component, not_a_component};

/// Why an instance's props could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub component: String,
    /// The prop, slot, binding or state.
    pub part: String,
    pub message: String,
}

impl Error {
    #[must_use]
    pub fn missing(component: &str, part: &str) -> Self {
        Self {
            component: component.to_owned(),
            part: part.to_owned(),
            message: "is required".to_owned(),
        }
    }

    #[must_use]
    pub fn value(component: &str, part: &str, message: String) -> Self {
        Self {
            component: component.to_owned(),
            part: part.to_owned(),
            message,
        }
    }

    /// The component `name` is not described.
    #[must_use]
    pub fn unknown(name: &str) -> Self {
        Self {
            component: name.to_owned(),
            part: String::new(),
            message: "is not a described component".to_owned(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.part.is_empty() {
            write!(f, "{} {}", self.component, self.message)
        } else {
            write!(f, "{}.{} {}", self.component, self.part, self.message)
        }
    }
}

impl std::error::Error for Error {}

/// A Rust type a prop (or state, or an event's value) may have: its [`Type`] in a description,
/// and how a [`Value`] of it reads.
pub trait PropType: Sized {
    fn ty() -> Type;

    /// # Errors
    /// The value is of another type.
    fn from_value(value: &Value) -> Result<Self, String>;

    /// The value as the instance `source` gives it: what it reads beside the value itself (the
    /// event an offered action fires) is the instance's.
    ///
    /// # Errors
    /// The value is of another type.
    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        let _ = source;
        Self::from_value(value)
    }
}

pub(crate) fn wrong(want: &Type, found: &Value) -> String {
    format!("takes {}, not {}", want.show(), found.kind())
}

impl PropType for String {
    fn ty() -> Type {
        Type::Text
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl PropType for i64 {
    fn ty() -> Type {
        Type::Int
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Int(number) => Ok(*number),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl PropType for f64 {
    fn ty() -> Type {
        Type::Float
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "an Int given for a Float prop is a size or a weight, far inside f64"
    )]
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Float(number) => Ok(*number),
            Value::Int(number) => Ok(*number as f64),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl PropType for bool {
    fn ty() -> Type {
        Type::Bool
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(flag) => Ok(*flag),
            Value::Int(number) => Ok(*number != 0),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl<T: PropType> PropType for Option<T> {
    fn ty() -> Type {
        Type::Option(Box::new(T::ty()))
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::None => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }

    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        match value {
            Value::None => Ok(None),
            other => T::read(other, source).map(Some),
        }
    }
}

impl<T: PropType> PropType for Vec<T> {
    fn ty() -> Type {
        Type::List(Box::new(T::ty()))
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(items) => items.iter().map(T::from_value).collect(),
            other => Err(wrong(&Self::ty(), other)),
        }
    }

    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        match value {
            Value::List(items) => items.iter().map(|item| T::read(item, source)).collect(),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl<T: PropType + Ord> PropType for std::collections::BTreeSet<T> {
    fn ty() -> Type {
        Type::Set(Box::new(T::ty()))
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(items) => items.iter().map(T::from_value).collect(),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

impl<K: PropType + Ord, V: PropType> PropType for std::collections::BTreeMap<K, V> {
    fn ty() -> Type {
        Type::Map {
            key: Box::new(K::ty()),
            value: Box::new(V::ty()),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Map(entries) => entries
                .iter()
                .map(|(key, value)| Ok((K::from_value(key)?, V::from_value(value)?)))
                .collect(),
            other => Err(wrong(&Self::ty(), other)),
        }
    }

    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        match value {
            Value::Map(entries) => entries
                .iter()
                .map(|(key, value)| Ok((K::read(key, source)?, V::read(value, source)?)))
                .collect(),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// An action offered to the person, as a widget takes one: how it shows, and the event
/// choosing it fires (data, never a closure).
#[derive(Clone, Debug, Default)]
pub struct OfferedAction {
    pub label: String,
    pub icon: Option<Icon>,
    pub primary: bool,
    pub destructive: bool,
    /// Whether the state it toggles is on, when it toggles one.
    pub on: Option<bool>,
    /// The plain key that runs it while its window has the keyboard (and, on a row's action,
    /// while that row is selected), as the app declared it.
    pub key: Option<String>,
    pub run: Event,
}

impl PropType for OfferedAction {
    fn ty() -> Type {
        Type::Action
    }

    /// An action read without its instance runs nothing.
    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Action(offered) = value else {
            return Err(wrong(&Self::ty(), value));
        };
        Ok(Self {
            label: offered.label.clone(),
            icon: offered
                .icon
                .as_deref()
                .and_then(|name| Icon::from_name(&name.replace('_', "-"))),
            primary: offered.primary,
            destructive: offered.destructive,
            on: offered.on,
            key: offered.key.clone(),
            run: Event::None,
        })
    }

    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        let mut action = Self::from_value(value)?;
        if let Value::Action(offered) = value {
            action.run = source.event(&offered.event);
        }
        Ok(action)
    }
}

impl PropType for Icon {
    fn ty() -> Type {
        Type::Icon
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Icon(name) | Value::Text(name) => Self::from_name(&name.replace('_', "-"))
                .ok_or_else(|| format!("names no icon: {name}")),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// A participant, as a component shows one: their identity, what the workspace calls them, and the
/// presence hue they wear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub identity: String,
    pub name: String,
    /// The hue their chip and avatar wear. A value names an identity and a name only, so a person
    /// read from one wears the hue their identity derives ([`PresenceHue::of_identity`]); one a
    /// host knows more of is given the hue the member list announced.
    pub hue: crate::tokens::PresenceHue,
}

impl Default for Person {
    fn default() -> Self {
        Self {
            identity: String::new(),
            name: String::new(),
            hue: crate::tokens::PresenceHue::from_slot(0),
        }
    }
}

impl PartialOrd for Person {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Person {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.identity.as_str(), self.name.as_str(), self.hue.slot()).cmp(&(
            other.identity.as_str(),
            other.name.as_str(),
            other.hue.slot(),
        ))
    }
}

impl PropType for Person {
    fn ty() -> Type {
        Type::Principal
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Principal { identity, name } => Ok(Self {
                hue: crate::tokens::PresenceHue::of_identity(identity),
                identity: identity.clone(),
                name: name.clone(),
            }),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// A case of one of the app's own enums, by its name, as a binding says it: which cases it
/// holds is the field the binding names, so a description says only that it takes one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnumCase;

impl PropType for EnumCase {
    fn ty() -> Type {
        Type::Enum
    }

    /// A case is read from the field it binds, never from a prop: nothing places one as a value.
    fn from_value(value: &Value) -> Result<Self, String> {
        Err(wrong(&Self::ty(), value))
    }
}

/// A moment, as a component shows one: the instant, in seconds since the Unix epoch. The
/// component decides how it reads (a time of day, a day's divider).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment(pub i64);

impl PropType for Moment {
    fn ty() -> Type {
        Type::Time
    }

    /// A moment is an instant (`YYYY-MM-DDTHH:MM:SSZ`), or the day it falls on (`YYYY-MM-DD`,
    /// its midnight): that is what a day's field binds.
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Text(at) => moment_of(at).ok_or_else(|| format!("is no moment: {at}")),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// The instant `at` is, in seconds since the Unix epoch: its instant, or the midnight of the
/// day it names.
fn moment_of(at: &str) -> Option<Moment> {
    if let Ok(instant) = chrono::DateTime::parse_from_rfc3339(at) {
        return Some(Moment(instant.timestamp()));
    }
    let day = chrono::NaiveDate::parse_from_str(at.get(..10)?, "%Y-%m-%d").ok()?;
    day.and_hms_opt(0, 0, 0)
        .map(|midnight| Moment(midnight.and_utc().timestamp()))
}

/// The day a moment's text names: its date, or the day an instant falls on.
#[must_use]
pub fn moment_day(text: &str) -> Option<chrono::NaiveDate> {
    // An instant falls on the day it is in the person's zone, which is the day they picked.
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(at.with_timezone(&chrono::Local).date_naive());
    }
    chrono::NaiveDate::parse_from_str(text.get(..10).unwrap_or(text), "%Y-%m-%d").ok()
}

/// The id of a resource a component presents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceId(pub String);

impl PropType for ResourceId {
    fn ty() -> Type {
        Type::Resource
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Resource(id) => Ok(Self(id.clone())),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// A record a view names, as a component offers it: what refers to it, and how it reads for the
/// person now. What the component writes of it names the record, never the text, so a record
/// renamed later reads by its new title wherever it was linked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// What refers to it: the catalogue's reference, the value a `mention` mark carries.
    pub reference: String,
    /// How it reads for the person now: its title, or the phrase for one they cannot read.
    pub title: String,
}

impl PropType for Link {
    fn ty() -> Type {
        Type::Link
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Link { reference, title } => Ok(Self {
                reference: reference.clone(),
                title: title.clone(),
            }),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// One of the design system's closed sets (a role, a size) as a prop: [`Type::Choice`] of its
/// cases' snake-case names. Crate-wide, so a component declares its choices beside itself:
/// `crate::component::choice!(MyRole, "MyRole", { A => "a", B => "b" });`.
macro_rules! choice {
    ($ty:ty, $name:literal, { $($case:ident => $text:literal),+ $(,)? }) => {
        impl $crate::component::PropType for $ty {
            fn ty() -> $crate::component::Type {
                $crate::component::Type::Choice {
                    name: $name.into(),
                    cases: vec![$($text.into()),+],
                }
            }

            fn from_value(value: &$crate::component::Value) -> Result<Self, String> {
                let ($crate::component::Value::Choice(case) | $crate::component::Value::Text(case)) = value else {
                    return Err($crate::component::wrong(&<Self as $crate::component::PropType>::ty(), value));
                };
                match case.as_str() {
                    $($text => Ok(<$ty>::$case),)+
                    other => Err(format!("has no case `{other}`")),
                }
            }
        }
    };
}

#[allow(
    unused_imports,
    reason = "a component declares its choices beside itself; none outside this file may yet"
)]
pub(crate) use choice;

/// A slot of any number of one part, whose props the slot's owner reads (a stat group's tiles,
/// a trail's segments): `crate::component::part_slot!(FactProps);` beside the part.
macro_rules! part_slot {
    ($props:ty) => {
        impl $crate::component::SlotType for Vec<$props> {
            const MANY: bool = true;

            fn accepts() -> $crate::component::Accepts {
                $crate::component::Accepts::Component(
                    <$props as $crate::component::Component>::NAME.into(),
                )
            }

            fn fill(
                &mut self,
                source: &mut dyn $crate::component::Source,
                slot: &str,
            ) -> Result<(), String> {
                let mut read = Vec::new();
                source.components(slot, &mut |child| {
                    read.push(
                        <$props as $crate::component::Component>::from_source(child)
                            .map_err(|error| error.to_string())?,
                    );
                    Ok(())
                })?;
                *self = read;
                Ok(())
            }
        }
    };
}

pub(crate) use part_slot;

choice!(crate::tokens::TextRole, "TextRole", {
    Display => "display",
    LargeTitle => "large_title",
    Title => "title",
    Heading => "heading",
    Subheading => "subheading",
    Body => "body",
    Label => "label",
    Caption => "caption",
    Reading => "reading",
    Code => "code",
});

choice!(crate::widget::ButtonRole, "ButtonRole", {
    Primary => "primary",
    Secondary => "secondary",
    Destructive => "destructive",
    Plain => "plain",
    Key => "key",
    Bar => "bar",
});

choice!(crate::widget::ControlSize, "ControlSize", {
    Small => "small",
    Regular => "regular",
    Large => "large",
});

/// What happens when one of a component's events happens. Data, never a closure: an action
/// reference, invoked through the door, or a view intent its host applies (a view-state update,
/// or the action reference the host's view attached to the event).
#[derive(Clone, Default)]
pub enum Event {
    /// Nothing happens.
    #[default]
    None,
    /// An exported action, invoked through the door; the host resolves its policy first.
    Action(tilo_actions::ActionBinding<()>),
    /// A view intent, queued for the host.
    View(Intent),
}

impl Event {
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// The action the event runs, when it is bound to one.
    #[must_use]
    pub fn invokes(&self) -> Option<&tilo_actions::ActionAddress> {
        match self {
            Self::None | Self::View(_) => None,
            Self::Action(binding) => Some(binding.address()),
        }
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Action(_) => f.write_str("Action(..)"),
            Self::View(intent) => write!(f, "View({:?})", intent.fired),
        }
    }
}

/// An event a component reports to its host: which instance (by key) and which event, with the
/// value the event carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub key: String,
    pub event: String,
    pub value: Option<Value>,
}

/// A view intent: [`Fired`] queued on the host's [`crate::Intents`].
#[derive(Clone)]
pub struct Intent {
    queue: crate::Intents<Fired>,
    fired: Fired,
}

impl Intent {
    /// The intent that queues `event` of the instance `key` on `queue`.
    #[must_use]
    pub fn new(queue: &crate::Intents<Fired>, key: &str, event: &str) -> Self {
        Self {
            queue: queue.clone(),
            fired: Fired {
                key: key.to_owned(),
                event: event.to_owned(),
                value: None,
            },
        }
    }

    /// Queue the intent, carrying `value`.
    pub fn fire(&self, value: Option<Value>) {
        self.queue.push(Fired {
            value,
            ..self.fired.clone()
        });
    }

    /// A handler that queues the intent each time it runs, for a widget's own activation path.
    pub fn sender(&self) -> impl FnMut() + 'static {
        let intent = self.clone();
        move || intent.fire(None)
    }
}

impl Event {
    /// Queue a view intent, carrying `value`. An action is run through its widget's emitter
    /// ([`Self::handler`]), which knows whether it is available.
    pub fn fire(&self, value: Option<Value>) {
        if let Self::View(intent) = self {
            intent.fire(value);
        }
    }

    /// The event as the handler of a widget that runs one when it happens: `None` for
    /// [`Event::None`].
    #[must_use]
    pub fn handler(self) -> Option<Box<dyn FnMut()>> {
        match self {
            Self::None => None,
            Self::View(intent) => Some(Box::new(intent.sender())),
            Self::Action(binding) => {
                let mut emitter = crate::widget::ActionEmitter::new(binding);
                Some(Box::new(move || {
                    if let Err(error) = emitter.emit(()) {
                        tracing::warn!(%error, "a component's action refused its arguments");
                    }
                }))
            }
        }
    }
}

/// A two-way binding as a widget takes it: the value through the widget's model-binding path
/// (a [`LocalState`] its host keeps in step with what it is bound to), and where each edit is
/// reported, for the host to apply with the binding's merge.
#[derive(Clone)]
pub struct Bound<T> {
    pub state: LocalState<T>,
    pub edits: Option<Intent>,
    /// What the bound value is called, for assistive technology: the field's name.
    pub name: String,
    /// Where the bound value lives, for presence: collaborators at the same path see each
    /// other's carets and selections in it.
    pub path: Vec<crate::presence::PathSegment>,
}

impl<T: Default + 'static> Default for Bound<T> {
    fn default() -> Self {
        Self {
            state: LocalState::new(T::default()),
            edits: None,
            name: String::new(),
            path: Vec::new(),
        }
    }
}

impl<T: Clone + 'static> Bound<T> {
    /// Report an edit to the value.
    pub fn edited(&self, value: Value) {
        if let Some(edits) = &self.edits {
            edits.fire(Some(value));
        }
    }
}

/// A binding as a host hands it over, of one of the types bindings take.
#[derive(Clone)]
pub enum BoundValue {
    Text(Bound<String>),
    Bool(Bound<bool>),
    Int(Bound<i64>),
    Float(Bound<f64>),
    /// Rich text, edited by its paragraphs and marks.
    Rich(Bound<tilo_ui_graph::rich::RichText>),
}

/// A Rust type a binding may have.
pub trait BindType: Sized {
    fn ty() -> Type;

    /// # Errors
    /// The host bound a value of another type.
    fn from_bound(bound: BoundValue) -> Result<Self, String>;
}

impl BindType for Bound<String> {
    fn ty() -> Type {
        Type::Text
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        match bound {
            BoundValue::Text(bound) => Ok(bound),
            other => Err(other.not_text()),
        }
    }
}

impl BindType for Bound<bool> {
    fn ty() -> Type {
        Type::Bool
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        match bound {
            BoundValue::Bool(bound) => Ok(bound),
            other => Err(other.not_bool()),
        }
    }
}

impl BindType for Bound<i64> {
    fn ty() -> Type {
        Type::Int
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        match bound {
            BoundValue::Int(bound) => Ok(bound),
            other => Err(other.not_int()),
        }
    }
}

impl BindType for Bound<f64> {
    fn ty() -> Type {
        Type::Float
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        match bound {
            BoundValue::Float(bound) => Ok(bound),
            other => Err(other.not_float()),
        }
    }
}

impl BindType for Bound<tilo_ui_graph::rich::RichText> {
    fn ty() -> Type {
        Type::RichText
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        match bound {
            BoundValue::Rich(bound) => Ok(bound),
            other => Err(other.not_rich()),
        }
    }
}

/// What a binding of another type is, for the message saying it fits neither.
impl BoundValue {
    fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "Text",
            Self::Bool(_) => "Bool",
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::Rich(_) => "RichText",
        }
    }

    fn not_text(&self) -> String {
        format!("binds Text, not {}", self.kind())
    }

    fn not_bool(&self) -> String {
        format!("binds a Bool, not {}", self.kind())
    }

    fn not_int(&self) -> String {
        format!("binds an Int, not {}", self.kind())
    }

    fn not_float(&self) -> String {
        format!("binds a Float, not {}", self.kind())
    }

    fn not_rich(&self) -> String {
        format!("binds RichText, not {}", self.kind())
    }
}

/// A binding a widget may go without: `None` when the instance binds nothing.
impl<B: BindType> BindType for Option<B> {
    fn ty() -> Type {
        B::ty()
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        B::from_bound(bound).map(Some)
    }
}

/// A Rust type a slot may have: [`Option<Element>`] for one view, [`Vec<Element>`] for any
/// number, `Option<P>` of another component's props for that component only (the derive
/// implements it).
pub trait SlotType {
    /// Whether it holds any number of children.
    const MANY: bool;

    fn accepts() -> Accepts;

    /// Take the slot `slot`'s content from `source`.
    ///
    /// # Errors
    /// The content does not fit the slot.
    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String>;
}

impl SlotType for Option<Element> {
    const MANY: bool = false;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut children = source.children(slot);
        if children.len() > 1 {
            return Err(format!("holds one view, not {}", children.len()));
        }
        *self = children.pop();
        Ok(())
    }
}

impl SlotType for Vec<Element> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        *self = source.children(slot);
        Ok(())
    }
}

/// An instance of a component as its host gives it: the values of its props, its events and
/// bindings as data, and its slots' content.
pub trait Source {
    /// The instance's stable key.
    fn key(&self) -> &str;

    /// The value of the prop `name`, when the instance gives it.
    fn prop(&mut self, name: &str) -> Option<Value>;

    /// The widget state `name` to restore, when there is some.
    fn state(&mut self, name: &str) -> Option<Value> {
        let _ = name;
        None
    }

    /// What the event `name` does: [`Event::None`] when the instance gives it nothing.
    fn event(&mut self, name: &str) -> Event;

    /// The binding `name`, when the instance binds it.
    fn binding(&mut self, name: &str) -> Option<BoundValue>;

    /// The views in the slot `slot`, built.
    fn children(&mut self, slot: &str) -> Vec<Element>;

    /// Each component in the slot `slot`, in order, read by `read`. Returns how many there
    /// were.
    ///
    /// # Errors
    /// What `read` returned, or the slot holds something other than a component.
    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String>;

    /// The component each child of the slot `slot` is an instance of, in order: `None` for a
    /// child that is a view of another kind.
    fn slot_kinds(&mut self, slot: &str) -> Vec<Option<String>> {
        let _ = slot;
        Vec::new()
    }

    /// The rows of the slot `slot` when its host renders them only as they are shown (a loop
    /// over a collection of any length): the widget reads them from there rather than from
    /// [`Self::components`].
    fn rows(&mut self, slot: &str) -> Option<SharedRows> {
        let _ = slot;
        None
    }
}

/// Rows its host renders only when the widget shows them: a loop over ten thousand items costs
/// the rows on screen. The host keeps the keys of every row, renders the rows the widget asks
/// for and forgets the others, and queues what changed (rows inserted, removed or moved, and
/// rendered rows that show something else now) for the widget to take.
pub trait RowSource {
    /// How many rows there are, rendered or not.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The key of the row at `index`.
    fn key_at(&self, index: usize) -> Option<String>;

    /// Where the row `key` stands.
    fn index_of(&self, key: &str) -> Option<usize>;

    /// Render the rows of `range` not rendered yet, and forget those rendered outside it.
    /// Render the rows of `range` as they are shown, and the document of the row `open` — the
    /// row standing open into its item where it stands, when one does.
    fn request_range(&mut self, range: std::ops::Range<usize>, open: Option<String>);

    /// Whether the row `key` is rendered now, its content there to read: a row named but not
    /// rendered yet is on its way, and the list shows a placeholder in its slot until it
    /// arrives ([`crate::widget::virtual_list::ListModel::row_state`]).
    fn rendered(&self, key: &str) -> bool;

    /// Read the rendered row `key` as an instance of a component: `Ok(false)` when it is not
    /// rendered.
    ///
    /// # Errors
    /// What `read` returned.
    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String>;

    /// The key of the row showing the resource `id`, rendered or not.
    fn key_presenting(&self, id: &str) -> Option<String>;

    /// What changed since it was asked last, each change against the rows the one before it
    /// left.
    fn take_changes(&mut self) -> Vec<crate::widget::virtual_list::ListModelChange<String>>;

    /// Whether changes are waiting.
    fn has_changes(&self) -> bool;
}

/// Rows a host and the widget showing them share.
pub type SharedRows = std::rc::Rc<std::cell::RefCell<dyn RowSource>>;

/// A widget's props: the struct its instances are made of. Derive it.
pub trait Component: Default + Sized {
    /// The component's name, as code calls it.
    const NAME: &'static str;

    fn describe() -> Description;

    /// The props `source` gives.
    ///
    /// # Errors
    /// A required prop is missing, or a value is of another type.
    fn from_source(source: &mut dyn Source) -> Result<Self, Error>;
}

/// The views in one slot of a widget, which a host inserts, removes, moves and replaces by
/// key: a change of a slot's content costs what changed, and is described to the window
/// ([`crate::widget::StructureChange::Described`]) rather than rebuilding the widget.
pub trait SlotViews {
    /// How many views the slot holds.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element>;

    fn insert(&mut self, index: usize, view: Element);

    fn remove(&mut self, index: usize);

    fn move_view(&mut self, from: usize, to: usize);

    fn replace(&mut self, index: usize, view: Element);
}

impl<L: crate::widget::arrange::Layout + 'static> SlotViews
    for crate::widget::arrange::Arranged<L>
{
    fn len(&self) -> usize {
        Self::len(self)
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        self.child_mut(index)
    }

    fn insert(&mut self, index: usize, view: Element) {
        Self::insert(self, index, crate::widget::arrange::Slot::new(view));
    }

    fn remove(&mut self, index: usize) {
        let _ = Self::remove(self, index);
    }

    fn move_view(&mut self, from: usize, to: usize) {
        self.move_slot(from, to);
    }

    fn replace(&mut self, index: usize, view: Element) {
        let _ = Self::replace(self, index, view);
    }
}

/// A widget made from props.
///
/// A described widget answers its own type through [`crate::widget::Widget::as_any_mut`]: the
/// adapter reaches it with that to change its props in place, and a widget that answers no
/// type is built again on every change it is told about. Declare the answer inside its
/// [`Widget`](crate::widget::Widget) impl with [`crate::described!`].
pub trait Described: crate::widget::Widget + Sized + 'static {
    type Props: Component;

    /// The widget of `props`, with `key` its stable id.
    fn from_props(key: &str, props: Self::Props) -> Self;

    /// Take changed props in place, damaging what they change: `false` when this widget cannot
    /// and is to be built again.
    fn set_props(&mut self, props: Self::Props) -> bool {
        let _ = props;
        false
    }

    /// The views of its slot `slot`, to change by key in place: `None` when a change of what
    /// the slot holds builds the widget again.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        let _ = slot;
        None
    }

    /// The views its slot `slot` holds for the part `key` — a part being a component the slot
    /// takes, whose props the widget reads — in that part's own view slot `of`, to change them
    /// by key where they stand. `None` when a change inside a part builds the widget again.
    fn part_views(&mut self, slot: &str, key: &str, of: &str) -> Option<&mut dyn SlotViews> {
        let _ = (slot, key, of);
        None
    }
}

/// Say that a described widget is its own type, inside its [`Widget`](crate::widget::Widget)
/// impl: `described!();`.
///
/// Without it the adapter reaches the widget as `dyn Widget`, cannot downcast it, and builds it
/// again on every change it is told about, so what the person was doing with it — the focus in a
/// field, a drag, the menu a press opened — goes with it. Every described component of the
/// toolkit answers this; `component::tests` builds each one from the catalogue and holds it to it.
#[macro_export]
macro_rules! described {
    () => {
        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    };
}

/// One described widget, as the generic adapter reaches it.
#[derive(Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub describe: fn() -> Description,
    pub build: fn(&mut dyn Source) -> Result<Element, Error>,
    /// Take the instance's props in place: `Ok(false)` when the widget is to be built again.
    pub update: fn(&mut Element, &mut dyn Source) -> Result<bool, Error>,
    /// The views of one of its slots, to change by key in place.
    pub slot_views: for<'a> fn(&'a mut Element, &str) -> Option<&'a mut dyn SlotViews>,
    /// The views of one part of a slot, to change by key in place.
    pub part_views: for<'a> fn(&'a mut Element, &str, &str, &str) -> Option<&'a mut dyn SlotViews>,
}

/// The [`Entry`] of the widget `W`.
#[must_use]
pub fn entry<W: Described>() -> Entry {
    fn build<W: Described>(source: &mut dyn Source) -> Result<Element, Error> {
        let key = source.key().to_owned();
        let props = W::Props::from_source(source)?;
        let mut widget = W::from_props(&key, props);
        // A widget just built owes the host no structure change: its props are read before the
        // host builds its layout, so the build realises what they set. A flag left over from
        // building it would rebuild the host's whole layout tree — a row's document opening
        // into its list, a note opened beside its tree, mounted where it stands.
        widget.take_structure_dirty();
        Ok(Box::new(widget))
    }
    fn update<W: Described>(widget: &mut Element, source: &mut dyn Source) -> Result<bool, Error> {
        let Some(widget) = widget.as_any_mut().and_then(|any| any.downcast_mut::<W>()) else {
            return Ok(false);
        };
        let props = W::Props::from_source(source)?;
        Ok(widget.set_props(props))
    }
    fn slot_views<'a, W: Described>(
        widget: &'a mut Element,
        slot: &str,
    ) -> Option<&'a mut dyn SlotViews> {
        widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<W>())?
            .slot_views(slot)
    }
    fn part_views<'a, W: Described>(
        widget: &'a mut Element,
        slot: &str,
        key: &str,
        of: &str,
    ) -> Option<&'a mut dyn SlotViews> {
        widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<W>())?
            .part_views(slot, key, of)
    }
    Entry {
        name: W::Props::NAME,
        describe: W::Props::describe,
        build: build::<W>,
        update: update::<W>,
        slot_views: slot_views::<W>,
        part_views: part_views::<W>,
    }
}

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// Every described widget: generated by the build script from each `impl Described` in the
/// toolkit's sources (`component_scan.rs`), so a widget described where it is written is
/// registered with no list to add it to.
#[must_use]
pub fn registry() -> &'static [Entry] {
    static REGISTRY: std::sync::OnceLock<Vec<Entry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(generated_entries)
}

/// The entry of the component `name`.
#[must_use]
pub fn get(name: &str) -> Option<&'static Entry> {
    registry().iter().find(|entry| entry.name == name)
}

/// Build the component `name` of `source`.
///
/// # Errors
/// `name` is not described, or `source`'s props do not fit it.
pub fn build(name: &str, source: &mut dyn Source) -> Result<Element, Error> {
    let entry = get(name).ok_or_else(|| Error::unknown(name))?;
    (entry.build)(source)
}

/// Bring `widget`, built for an instance of `name`, to `source`'s props in place: `Ok(false)`
/// when it is to be built again.
///
/// # Errors
/// `name` is not described, or `source`'s props do not fit it.
pub fn update(name: &str, widget: &mut Element, source: &mut dyn Source) -> Result<bool, Error> {
    let entry = get(name).ok_or_else(|| Error::unknown(name))?;
    (entry.update)(widget, source)
}

/// The components that stand only in a slot of another, which reads their props: a property
/// list's facts, a navigation list's places and sections, a menu's commands, separators and
/// emoji bar, a row's swipe, the items of a virtual list, and what a view attaches to the
/// element it stands in (its context menu, hover toolbar and swipe). Generated: every
/// `#[derive(Component)]` props that no described widget is built from.
#[must_use]
pub fn parts() -> &'static [fn() -> Description] {
    &GENERATED_PARTS
}

/// The views its slot `slot` holds for the part `key`, in the part's own view slot `of`, built
/// for an instance of `name`, to change by key in place: `None` when the widget changes them
/// only by being built again.
pub fn part_views<'a>(
    name: &str,
    widget: &'a mut Element,
    slot: &str,
    key: &str,
    of: &str,
) -> Option<&'a mut dyn SlotViews> {
    let entry = get(name)?;
    (entry.part_views)(widget, slot, key, of)
}

/// The views of the slot `slot` of `widget`, built for an instance of `name`, to change by key
/// in place: `None` when the widget changes them only by being built again.
pub fn slot_views<'a>(
    name: &str,
    widget: &'a mut Element,
    slot: &str,
) -> Option<&'a mut dyn SlotViews> {
    let entry = get(name)?;
    (entry.slot_views)(widget, slot)
}

/// The catalogue of every described widget, as this toolkit generates it.
#[must_use]
pub fn catalogue() -> Catalogue {
    Catalogue {
        format: FORMAT_VERSION,
        toolkit: env!("CARGO_PKG_VERSION").to_owned(),
        components: registry()
            .iter()
            .map(|entry| (entry.describe)())
            .chain(parts().iter().map(|describe| describe()))
            .collect(),
        enums: vec![crate::widget::drawing::shape_description()],
    }
}

pub mod gallery;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod coverage_tests;

#[cfg(test)]
mod feedback_tests;

#[cfg(test)]
mod data_tests;

#[cfg(test)]
mod controls_tests;

#[cfg(test)]
mod media_tests;

#[cfg(test)]
mod slot_fill_tests;
