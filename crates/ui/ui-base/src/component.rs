//! Components: the toolkit's widgets as described, composable data.
//!
//! A described widget has **props**: a plain struct with `#[derive(Component)]`, and the derive
//! generates its [`Description`] from it (props, parts, events, bindings, state and semantics, each
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
//! use crate::component::{Component, Type};
//! use crate::widget::ListRowProps;
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
//! use crate::component::Component;
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
//! use crate::component::Component;
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
//! use crate::component::Component;
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
    Accepts, Binding as BindingDescription, Catalogue, Description, EnumDescription,
    Event as EventDescription, FORMAT_VERSION, File, Merge, Offered, Part, Picture, Prop,
    Semantics, State as StateDescription, Type, Value,
};
pub use tilo_ui_macros::{Component, not_a_component};

/// Why an instance's props could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub component: String,
    /// The prop, part, binding or state.
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

/// A Rust type a prop (or state, or an event's value) may have: its [`Type`] in a description, and how
/// a [`Value`] of it reads.
pub trait PropType: Sized {
    fn ty() -> Type;

    fn from_value(value: &Value) -> Result<Self, String>;

    /// The value as the instance `source` gives it: what it reads beside the value itself (the event an
    /// offered action fires) is the instance's.
    fn read(value: &Value, source: &mut dyn Source) -> Result<Self, String> {
        let _ = source;
        Self::from_value(value)
    }
}

/// The field `name` of a case's `fields`, read as a `T`: `None` when the case leaves it out or holds
/// nothing there, so a field with a default reads as its default.
pub fn case_field<T: PropType>(
    fields: &[(String, Value)],
    name: &str,
) -> Result<Option<T>, String> {
    match fields.iter().find(|(field, _)| field == name) {
        None | Some((_, Value::None)) => Ok(None),
        Some((_, value)) => T::from_value(value)
            .map(Some)
            .map_err(|error| format!("`{name}` {error}")),
    }
}

/// A field of a case as its set's description lists it.
pub fn case_field_description(
    name: &str,
    ty: Type,
    default: Option<Value>,
    doc: &str,
) -> tilo_ui_graph::component::FieldDescription {
    tilo_ui_graph::component::FieldDescription {
        name: name.to_owned(),
        ty,
        default,
        doc: doc.to_owned(),
    }
}

pub fn wrong(want: &Type, found: &Value) -> String {
    format!("takes {}, not {}", want.show(), found.kind())
}

impl PropType for tilo_ui_graph::rich::RichText {
    fn ty() -> Type {
        Type::RichText
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Rich(rich) => Ok(rich.clone()),
            other => Err(format!("takes RichText, not {}", other.kind())),
        }
    }
}

impl PropType for tilo_ui_graph::component::File {
    fn ty() -> Type {
        Type::File
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::File(file) => Ok(file.clone()),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
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
    /// The plain key that runs it while its window has the keyboard (and, on a row's action, while that
    /// row is selected), as the app declared it.
    pub key: Option<String>,
    pub run: Event,
}

impl PropType for OfferedAction {
    fn ty() -> Type {
        Type::Action
    }

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

/// Who acted, as a component shows them: their identity, what the workspace calls them, and the
/// presence hue they wear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub identity: tilo_actions::Actor,
    pub name: String,
    /// The hue their chip and avatar wear.
    pub hue: crate::tokens::PresenceHue,
    /// The address of their picture (a `data:` or `tilo-blob://` URL), when they set one: every
    /// face drawn for them shows it. `None` shows their initials.
    pub picture: Option<String>,
}

impl Person {
    /// `identity`, called `name`, in the hue their identity gives them.
    #[must_use]
    pub fn new(identity: impl Into<tilo_actions::Actor>, name: impl Into<String>) -> Self {
        let identity = identity.into();
        Self {
            hue: crate::tokens::PresenceHue::of_member(&identity),
            identity,
            name: name.into(),
            picture: None,
        }
    }
}

impl Person {
    /// Someone a view knows only by `name`: a person by that name when it can be one's id, else
    /// a name that carries no member, which still wears its initials in the hue it derives.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        let identity = tilo_action_policy::PersonId::parse(&name).map_or_else(
            |_| tilo_actions::Actor::automation(name.clone()),
            |person| tilo_actions::MemberId::Person(person).into(),
        );
        Self::new(identity, name)
    }
}

/// `actor` as a bound control's text holds it: its serde form, as the runtime reads it back.
#[must_use]
pub fn actor_text(actor: &tilo_actions::Actor) -> String {
    serde_json::to_string(actor).unwrap_or_default()
}

/// The actor [`actor_text`] wrote, `None` for nobody.
#[must_use]
pub fn actor_of_text(text: &str) -> Option<tilo_actions::Actor> {
    serde_json::from_str(text).ok()
}

impl PartialOrd for Person {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Person {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            &self.identity,
            self.name.as_str(),
            self.hue.slot(),
            self.picture.as_deref(),
        )
            .cmp(&(
                &other.identity,
                other.name.as_str(),
                other.hue.slot(),
                other.picture.as_deref(),
            ))
    }
}

impl PropType for Person {
    fn ty() -> Type {
        Type::Actor
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            // The colour the workspace paints them in travels with them; a sender that knows no
            // members leaves it to the rule every surface derives it by.
            Value::Actor {
                identity,
                name,
                hue,
                picture,
            } => Ok(Self {
                hue: hue.map_or_else(
                    || crate::tokens::PresenceHue::of_member(identity),
                    |slot| crate::tokens::PresenceHue::from_slot(usize::from(slot)),
                ),
                picture: picture.clone(),
                ..Self::new(identity.clone(), name.clone())
            }),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// A case of one of the app enums, by its name, as a binding says it: which cases it has is the field
/// the binding names, so a description says only that it takes one.
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

/// A moment, as a component shows one: the instant, in seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment(pub i64);

impl PropType for Moment {
    fn ty() -> Type {
        Type::Time
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Text(at) => moment_of(at).ok_or_else(|| format!("is no moment: {at}")),
            other => Err(wrong(&Self::ty(), other)),
        }
    }
}

/// The instant `at` is, in seconds since the Unix epoch: its instant, or the midnight of the day it
/// names.
fn moment_of(at: &str) -> Option<Moment> {
    if let Ok(instant) = at.parse::<jiff::Timestamp>() {
        return Some(Moment(instant.as_second()));
    }
    let day = jiff::civil::Date::strptime("%Y-%m-%d", at.get(..10)?).ok()?;
    let midnight = day.to_zoned(jiff::tz::TimeZone::UTC).ok()?;
    Some(Moment(midnight.timestamp().as_second()))
}

/// The day a moment's text names: its date, or the day an instant falls on.
#[must_use]
pub fn moment_day(text: &str) -> Option<jiff::civil::Date> {
    if let Ok(at) = text.parse::<jiff::Timestamp>() {
        return Some(crate::clock::at(at).date());
    }
    jiff::civil::Date::strptime("%Y-%m-%d", text.get(..10).unwrap_or(text)).ok()
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
    /// What refers to it: the catalogue's reference, the value a `mention` mark has.
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

#[doc(hidden)]
#[macro_export]
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

/// What one child of a part of many is read as: a component's props, or one of the kinds of
/// component the part takes ([`Accepts::OneOf`]).
pub trait PartItem: Sized {
    fn accepts() -> Accepts;

    /// Read every child of the part `part` from `source`.
    fn read_all(source: &mut dyn Source, part: &str) -> Result<Vec<Self>, String>;
}

/// [`PartItem`] for a component's props: a part of many of it reads each child's props.
#[doc(hidden)]
#[macro_export]
macro_rules! part_type {
    ($props:ty) => {
        impl $crate::component::PartItem for $props {
            fn accepts() -> $crate::component::Accepts {
                $crate::component::Accepts::Component(
                    <$props as $crate::component::Component>::NAME.into(),
                )
            }

            fn read_all(
                source: &mut dyn $crate::component::Source,
                part: &str,
            ) -> Result<Vec<Self>, String> {
                let mut read = Vec::new();
                source.components(part, &mut |child| {
                    read.push(
                        <$props as $crate::component::Component>::from_source(child)
                            .map_err(|error| error.to_string())?,
                    );
                    Ok(())
                })?;
                Ok(read)
            }
        }
    };
}

impl<T: PartItem> PartType for Vec<T> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        T::accepts()
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        *self = T::read_all(source, part)?;
        Ok(())
    }
}

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

choice!(crate::a11y::Politeness, "Politeness", {
    Polite => "polite",
    Assertive => "assertive",
});

/// What happens when one of a component's events happens. Data, never a closure: an action
/// reference, invoked through the API, or a view intent its host applies (a view-state update,
/// or the action reference the host's view attached to the event).
#[derive(Clone, Default)]
pub enum Event {
    /// Nothing happens.
    #[default]
    None,
    /// An exported action, invoked through the API; the host resolves its policy first.
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

/// An event a component reports to its host: which instance (by key) and which event, with the value
/// the event has.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub key: String,
    pub event: String,
    pub value: Option<Value>,
    /// The gesture this event is one part of, when the person's act made more than one change on
    /// screen: the picture a paste writes into a document, and the edit that records its name,
    /// are one step of theirs. Their changes name the same gesture, so one undo takes both back.
    /// `None` for an event that is an act of its own.
    pub gesture: Option<tilo_actions::ActionId>,
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
                gesture: None,
            },
        }
    }

    /// Queue the intent, carrying `value`.
    pub fn fire(&self, value: Option<Value>) {
        self.fire_in_gesture(value, None);
    }

    /// Queue the intent, carrying `value`, as one part of the act `gesture`: the changes the
    /// event's action makes and the edit it was made with are one step of the person's.
    pub fn fire_in_gesture(&self, value: Option<Value>, gesture: Option<tilo_actions::ActionId>) {
        self.queue.push(Fired {
            value,
            gesture,
            ..self.fired.clone()
        });
    }

    /// A handler that queues the intent each time it runs, for a widget activation path.
    pub fn sender(&self) -> impl FnMut() + 'static {
        let intent = self.clone();
        move || intent.fire(None)
    }

    /// The instance whose event it is, by its key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.fired.key
    }

    /// The event's name, as the component declares it.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.fired.event
    }
}

impl Event {
    /// Queue a view intent, carrying `value`.
    pub fn fire(&self, value: Option<Value>) {
        if let Self::View(intent) = self {
            intent.fire(value);
        }
    }

    /// Queue a view intent carrying `value` as one part of the person's act `gesture`, whose
    /// changes undo takes back together ([`Fired::gesture`]); an action event runs as it does.
    pub fn fire_in_gesture(&self, value: Option<Value>, gesture: Option<tilo_actions::ActionId>) {
        if let Self::View(intent) = self {
            intent.fire_in_gesture(value, gesture);
        }
    }

    /// The event as the handler of a widget that runs one when it happens: `None` for [`Event::None`].
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

/// A two-way binding as a widget takes it: the value through the widget's model-binding path (a
/// [`LocalState`] its host keeps in step with what it is bound to), and where each edit is reported,
/// for the host to apply with the binding's merge.
#[derive(Clone)]
pub struct Bound<T> {
    pub state: LocalState<T>,
    pub edits: Option<Intent>,
    /// What the bound value is called, for assistive technology: the field's name.
    pub name: String,
    /// Where the bound value lives, for presence: members at the same path see each other's
    /// carets and selections in it.
    pub path: Vec<tilo_presence::PathSegment>,
    /// What the value edits, as the identity anyone looks it up by: the kind of record, the one
    /// record and the declared id of the field. `None` for a view's own state, and for a host whose
    /// values answer to nothing but their names.
    pub identity: Option<String>,
}

impl<T: Default + 'static> Default for Bound<T> {
    fn default() -> Self {
        Self {
            state: LocalState::new(T::default()),
            edits: None,
            name: String::new(),
            path: Vec::new(),
            identity: None,
        }
    }
}

impl<T: Clone + 'static> Bound<T> {
    /// Report an edit to the value.
    pub fn edited(&self, value: Value) {
        self.edited_in(value, None);
    }

    /// Report an edit to the value as one part of the person's act `gesture`: what a control
    /// changed as part of a paste whose event names the same gesture ([`Fired::gesture`]), so
    /// the two changes are one step of theirs.
    pub fn edited_in(&self, value: Value, gesture: Option<tilo_actions::ActionId>) {
        if let Some(edits) = &self.edits {
            edits.fire_in_gesture(Some(value), gesture);
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

impl<B: BindType> BindType for Option<B> {
    fn ty() -> Type {
        B::ty()
    }

    fn from_bound(bound: BoundValue) -> Result<Self, String> {
        B::from_bound(bound).map(Some)
    }
}

/// A Rust type a part may have: [`Option<Element>`] for one view, [`Vec<Element>`] for any number,
/// `Option<P>` of another component's props for that component only (the derive implements it).
pub trait PartType {
    /// Whether it has any number of children.
    const MANY: bool;

    fn accepts() -> Accepts;

    /// Take the part `part`'s content from `source`.
    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String>;
}

/// A part that accepts only one component reads its props.
impl<P: Component> PartType for Option<P> {
    const MANY: bool = false;

    fn accepts() -> Accepts {
        Accepts::Component(P::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut read = None;
        let count = source.components(part, &mut |child| {
            read = Some(P::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        if count > 1 {
            return Err(format!("holds one {}, not {count}", P::NAME));
        }
        *self = read;
        Ok(())
    }
}

impl PartType for Option<Element> {
    const MANY: bool = false;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut children = source.children(part);
        if children.len() > 1 {
            return Err(format!("holds one view, not {}", children.len()));
        }
        *self = children.pop();
        Ok(())
    }
}

impl PartType for Vec<Element> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        *self = source.children(part);
        Ok(())
    }
}

/// An instance of a component as its host gives it: the values of its props, its events and bindings as
/// data, and its parts' content.
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

    /// The views in the part `part`, built.
    fn children(&mut self, part: &str) -> Vec<Element>;

    /// Each component in the part `part`, in order, read by `read`.
    fn components(
        &mut self,
        part: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String>;

    /// The component each child of the part `part` is an instance of, in order: `None` for a child that
    /// is a view of another kind.
    fn part_kinds(&mut self, part: &str) -> Vec<Option<String>> {
        let _ = part;
        Vec::new()
    }

    /// Whether the part `part` has anything: a view or a component, built or not.
    fn holds(&mut self, part: &str) -> bool {
        !self.part_kinds(part).is_empty()
    }

    /// The rows of the part `part` when its host renders them only as they are shown (a loop over a
    /// collection of any length): the widget reads them from there rather than from
    /// [`Self::components`].
    fn rows(&mut self, part: &str) -> Option<SharedRows> {
        let _ = part;
        None
    }

    /// What the control built from this source does, as the identity anyone looks it up by
    /// ([`crate::widget::Identified`]): the app action it runs, or the field it edits, in the
    /// app's own ids. `None` for a component that runs nothing, and for a host whose components
    /// answer to nothing but their names.
    fn identity(&mut self) -> Option<String> {
        None
    }
}

/// Rows its host renders only when the widget shows them: a loop over ten thousand items costs the rows
/// on screen.
pub trait RowSource {
    /// How many rows there are, rendered or not.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The key of the row at `index`.
    fn key_at(&self, index: usize) -> Option<String>;

    /// Where the row `key` is.
    fn index_of(&self, key: &str) -> Option<usize>;

    /// Render the rows of `range` not rendered yet, and forget those rendered outside it.
    fn request_range(&mut self, range: std::ops::Range<usize>, open: Option<String>);

    /// The person opened the menu of the row `key`, whose item offers one it did not carry
    /// (`offers_menu`): ask for the row again with it. Rows that carry their menus ask nothing.
    fn ask_menu(&mut self, _key: &str) {}

    /// Whether the row `key` is rendered now, its content there to read: a row named but not
    /// rendered yet is on its way, and the list shows a placeholder in its part until it
    /// arrives ([`crate::widget::virtual_list::ListModel::row_state`]).
    fn rendered(&self, key: &str) -> bool;

    /// Read the rendered row `key` as an instance of a component: `Ok(false)` when it is not rendered.
    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String>;

    /// Read the rendered row `key` for its props and what its parts hold, building none of its views:
    /// what a list reads of a row beside building it (its group, its place in a tree, the item it
    /// shows), which it asks for every row it has on every pass.
    fn read_props(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.read(key, read)
    }

    /// The rendered row `key` as it stands, by number: the same number for as long as what
    /// [`Self::read`] and [`Self::read_props`] give for the row stays the same, so a list keeps
    /// what it read of a row (its group, its menu) across frames until the number moves. `None`
    /// for a row not rendered, or from a source that makes no such promise: the list reads it
    /// again each time.
    fn revision(&self, _key: &str) -> Option<u64> {
        None
    }

    /// The key of the row showing the resource `id`, rendered or not.
    fn key_presenting(&self, id: &str) -> Option<String>;

    /// What changed since it was asked last, each change against the rows the one before it left.
    fn take_changes(&mut self) -> Vec<crate::collection_transition::ListModelChange<String>>;

    /// Whether changes are waiting.
    fn has_changes(&self) -> bool;
}

/// Rows a host and the widget showing them share.
pub type SharedRows = std::rc::Rc<std::cell::RefCell<dyn RowSource>>;

/// A widget's props: the struct its instances are made of.
pub trait Component: Default + Sized {
    /// The component's name, as code calls it.
    const NAME: &'static str;

    fn describe() -> Description;

    /// The props `source` gives. # Errors A required prop is missing, or a value is of another type.
    fn from_source(source: &mut dyn Source) -> Result<Self, Error>;
}

/// The views in one part of a widget, which a host inserts, removes, moves and replaces by
/// key: a change of a part's content costs what changed, and is described to the window
/// ([`crate::widget::StructureChange::Described`]) rather than rebuilding the widget.
pub trait PartViews {
    /// How many views the part has.
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

/// A widget made from props.
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

    /// The views of its part `part`, to change by key in place: `None` when a change of what the part
    /// has builds the widget again.
    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        let _ = part;
        None
    }

    /// The views its part `part` has for the item `key` — an item being a component the part takes,
    /// whose props the widget reads — in that item's part `of`, to change them by key where they
    /// stand.
    fn item_views(&mut self, part: &str, key: &str, of: &str) -> Option<&mut dyn PartViews> {
        let _ = (part, key, of);
        None
    }
}

/// Say that a described widget is its type, inside its [`Widget`](crate::widget::Widget)
/// impl: `described!();`.
///
/// Without it the adapter reaches the widget as `dyn Widget`, cannot downcast it, and builds it
/// again on every change it is told about, so what the person was doing with it — the focus in a
/// field, a drag, the menu a press opened — goes with it. Every described component of the
/// toolkit answers this; `component::tests` builds each one from the catalogue and has it to it.
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
    /// The views of one of its parts, to change by key in place.
    pub part_views: for<'a> fn(&'a mut Element, &str) -> Option<&'a mut dyn PartViews>,
    /// The views of one item of a part, to change by key in place.
    pub item_views: for<'a> fn(&'a mut Element, &str, &str, &str) -> Option<&'a mut dyn PartViews>,
}

/// The [`Entry`] of the widget `W`.
#[must_use]
pub fn entry<W: Described>() -> Entry {
    fn build<W: Described>(source: &mut dyn Source) -> Result<Element, Error> {
        let key = source.key().to_owned();
        let props = W::Props::from_source(source)?;
        let mut widget = W::from_props(&key, props);
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
    fn part_views<'a, W: Described>(
        widget: &'a mut Element,
        part: &str,
    ) -> Option<&'a mut dyn PartViews> {
        widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<W>())?
            .part_views(part)
    }
    fn item_views<'a, W: Described>(
        widget: &'a mut Element,
        part: &str,
        key: &str,
        of: &str,
    ) -> Option<&'a mut dyn PartViews> {
        widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<W>())?
            .item_views(part, key, of)
    }
    Entry {
        name: W::Props::NAME,
        describe: W::Props::describe,
        build: build::<W>,
        update: update::<W>,
        part_views: part_views::<W>,
        item_views: item_views::<W>,
    }
}

/// The described widgets one crate declares: generated by its build script from each
/// `impl Described` in its sources (`tilo-component-scan`), so a widget described where it is
/// written is registered with no list to add it to.
pub struct Set {
    /// Every described widget's entry, in source order.
    pub entries: fn() -> Vec<Entry>,
    /// The components that stand only in a part of another, which reads their props: a property
    /// list's facts, a navigation list's places and sections, a menu's commands, separators and
    /// emoji bar, a row's swipe, the items of a virtual list, and what a view attaches to the
    /// element it is in (its context menu, hover toolbar and swipe).
    pub parts: &'static [fn() -> Description],
    /// The closed sets with fields its props name ([`Type::Cases`]).
    pub enums: fn() -> Vec<EnumDescription>,
}

/// The described widgets a window builds by name: the toolkit's and those of each crate of widgets
/// above it, composed by whatever builds views, so a component is found wherever it is declared.
pub struct Registry {
    sets: Vec<&'static Set>,
    entries: Vec<Entry>,
}

/// The components a layout itself runs for the view it shows: a view's menu opens from the area
/// around the layout, wherever nothing the press landed on took it
/// ([`crate::widget::ContextMenuArea`]). The other attachments belong to a row, which a part of
/// another component hands them to.
const LAYOUT_RUNS: [&str; 1] = ["ContextMenu"];

impl Registry {
    /// The widgets of `sets`, in that order.
    #[must_use]
    pub fn of(sets: &[&'static Set]) -> Self {
        Self {
            sets: sets.to_vec(),
            entries: sets.iter().flat_map(|set| (set.entries)()).collect(),
        }
    }

    /// Every described widget's entry.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry of the component `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Build the component `name` of `source`.
    pub fn build(&self, name: &str, source: &mut dyn Source) -> Result<Element, Error> {
        let entry = self.get(name).ok_or_else(|| self.cannot_stand(name))?;
        let built = (entry.build)(source)?;
        // What the control does is part of the control: it says so however the window composes
        // around it, so a person, an assistive technology and an agent that names it reach the one
        // operation.
        Ok(match source.identity() {
            Some(identity) => Box::new(crate::widget::Identified::new(built, identity)),
            None => built,
        })
    }

    /// Bring `widget`, built for an instance of `name`, to `source`'s props in place: `Ok(false)`
    /// when it is to be built again.
    pub fn update(
        &self,
        name: &str,
        widget: &mut Element,
        source: &mut dyn Source,
    ) -> Result<bool, Error> {
        let entry = self.get(name).ok_or_else(|| self.cannot_stand(name))?;
        (entry.update)(widget, source)
    }

    /// Why no widget builds where `name` stands: nothing describes it, or it is one a view places
    /// only inside another — the components whose parts read it, or, for one that attaches to the
    /// element it stands in, that element.
    #[must_use]
    pub fn cannot_stand(&self, name: &str) -> Error {
        let catalogue = self.catalogue();
        let Some(description) = catalogue
            .components
            .iter()
            .find(|description| description.name == name)
        else {
            return Error::unknown(name);
        };
        // Where it may stand. A component a view attaches asks the element it stands in to run it:
        // a layout runs a view's menu ([`crate::widget::ContextMenuArea`]), and the other
        // attachments belong to a row, which a part of another component takes. One a part reads
        // stands in the parts whose owner reads it.
        let mut places: Vec<String> = Vec::new();
        if description.semantics.attaches && LAYOUT_RUNS.contains(&name) {
            places.push("a layout".to_owned());
        }
        places.extend(
            catalogue
                .components
                .iter()
                .filter(|owner| {
                    owner.parts.iter().any(|part| {
                        // A part that takes views takes every component of a view; only the parts
                        // that read a component's props are places it may stand.
                        part.accepts.reads_components() && part.accepts.takes(name)
                    })
                })
                .map(|owner| format!("`ui.{}`", owner.name)),
        );
        let inside = match places.split_last() {
            None => "in a part of another component".to_owned(),
            Some((last, [])) => format!("inside {last}"),
            Some((last, first)) => format!("inside {} or {last}", first.join(", ")),
        };
        Error::value(name, "", format!("can only stand {inside}"))
    }

    /// The views its part `part` has for the item `key`, in that item's part `of`, built for an
    /// instance of `name`, to change by key in place: `None` when the widget changes them only by
    /// being built again.
    pub fn item_views<'a>(
        &self,
        name: &str,
        widget: &'a mut Element,
        part: &str,
        key: &str,
        of: &str,
    ) -> Option<&'a mut dyn PartViews> {
        (self.get(name)?.item_views)(widget, part, key, of)
    }

    /// The views of the part `part` of `widget`, built for an instance of `name`, to change by key
    /// in place: `None` when the widget changes them only by being built again.
    pub fn part_views<'a>(
        &self,
        name: &str,
        widget: &'a mut Element,
        part: &str,
    ) -> Option<&'a mut dyn PartViews> {
        (self.get(name)?.part_views)(widget, part)
    }

    /// The catalogue of every described widget of these sets.
    #[must_use]
    pub fn catalogue(&self) -> Catalogue {
        Catalogue {
            format: FORMAT_VERSION,
            toolkit: env!("CARGO_PKG_VERSION").to_owned(),
            components: self
                .entries
                .iter()
                .map(|entry| (entry.describe)())
                .chain(
                    self.sets
                        .iter()
                        .flat_map(|set| set.parts.iter().map(|describe| describe())),
                )
                .collect(),
            enums: self.sets.iter().flat_map(|set| (set.enums)()).collect(),
            icons: crate::icons::Icon::ALL
                .iter()
                .map(|icon| icon.name().to_owned())
                .collect(),
        }
    }
}

pub mod gallery;
