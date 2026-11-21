//! Every kind every app publishes, through the reader a written view reads with.
//!
//! A view reads what an app publishes, and an app gets that by publishing. So for every app whose
//! surface this build carries, and every kind of that surface that stores fields, a value filled
//! from the declared types alone (every field, every case of every enum whose cases carry
//! values, once with every option empty, and once as a store keeps a number at 0 and a flag
//! that is off: with no key for it) is laid out as a read answers a stored value,
//! handed through [`App::reading`], and read back field by field as a view's code reads it
//! ([`Runtime::read`]). What does not arrive whole is a stop: it traps, comes empty, or loses
//! something.
//!
//! The stops known today are the lines of `sweep_known.txt`. The sweep is red for a stop that
//! list does not hold (a new app, a new field or a new type a view cannot read) and for a line
//! whose stop is gone, so the list only shrinks. A second sweep reads one kind that declares
//! every shape a surface can, each as a field of its own, so one shape's stop never hides
//! another's.
//!
//! What it does not walk: whether a view can name the kind at all. The table says it beside
//! each kind and judges nothing by it.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value as Json, json};
use tilo_actions::surface::{FieldType, NamedType, Surface, TypeField};
use tilo_actions::{Actor, MemberId};
use tilolang_tilo::catalogue;
use tilolang_tilo::runtime::value::EnumValue;
use tilolang_tilo::runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};
use tilolang_tilo::tilolang::PackageMeta;

use crate::apps::{App, Root};

/// The stops known today, one a line: `CLASS | app | kind | path`.
const KNOWN: &str = include_str!("sweep_known.txt");

/// The app [`SHAPES`] is published as.
const SHAPES_APP: &str = "so.tilo.sweep";

/// One kind that declares every shape a surface can, each as a field of its own.
const SHAPES: &str = r#"
[[resources]]
kind = "sweep.shapes"
label = "Shapes"
version = 1
fields = [
  { path = "text", type = "text" },
  { path = "rich", type = "rich_text" },
  { path = "int", type = "int" },
  { path = "float", type = "float" },
  { path = "bool", type = "bool" },
  { path = "date", type = "date" },
  { path = "time", type = "time" },
  { path = "actor", type = "actor" },
  { path = "plain_case", type = { named = "Plain" } },
  { path = "carrying_case", type = { named = "Carrying" } },
  { path = "part", type = { named = "Part" } },
  { path = "option_text", type = { option = "text" } },
  { path = "option_time", type = { option = "time" } },
  { path = "option_date", type = { option = "date" } },
  { path = "option_actor", type = { option = "actor" } },
  { path = "option_part", type = { option = { named = "Part" } } },
  { path = "option_case", type = { option = { named = "Carrying" } } },
  { path = "option_list", type = { option = { list = "float" } } },
  { path = "option_map", type = { option = { map = "float" } } },
  { path = "option_rich", type = { option = "rich_text" } },
  { path = "list_text", type = { list = "text" } },
  { path = "list_int", type = { list = "int" } },
  { path = "list_actor", type = { list = "actor" } },
  { path = "list_part", type = { list = { named = "Part" } } },
  { path = "list_case", type = { list = { named = "Carrying" } } },
  { path = "list_list", type = { list = { list = "float" } } },
  { path = "set_text", type = { set = "text" } },
  { path = "set_date", type = { set = "date" } },
  { path = "set_part", type = { set = { named = "Part" } } },
  { path = "collection", type = { collection = { named = "Part" } }, stored = "collection" },
  { path = "keyed", type = { collection = { named = "Part" } }, stored = "keyed" },
  { path = "collection_text", type = { collection = "text" }, stored = "collection" },
  { path = "map_text", type = { map = "text" } },
  { path = "map_int", type = { map = "int" } },
  { path = "map_float", type = { map = "float" } },
  { path = "map_part", type = { map = { named = "Part" } } },
  { path = "map_map", type = { map = { map = "int" } } },
  { path = "map_rich", type = { map = "rich_text" } },
  { path = "reference", type = { resource = "sweep.shapes" } },
  { path = "named_view", type = { named = "View" } },
  { path = "map_bytes", type = { map = { named = "Bytes" } } },
  { path = "any", type = "any" },
  { path = "reactions", type = "reactions" },
]

[[resources.types]]
name = "Plain"
cases = [
  { name = "first" },
  { name = "second" },
]

[[resources.types]]
name = "Carrying"
cases = [
  { name = "bare" },
  { name = "one", fields = [{ name = "0", type = "text" }] },
  { name = "two", fields = [{ name = "0", type = "int" }, { name = "1", type = "bool" }] },
  { name = "labelled", fields = [{ name = "label", type = "text" }, { name = "amount", type = { option = "float" } }] },
  { name = "who", fields = [{ name = "0", type = "actor" }] },
  { name = "listed", fields = [{ name = "0", type = { list = "float" } }] },
  { name = "holding", fields = [{ name = "0", type = { named = "Leaf" } }] },
]

[[resources.types]]
name = "Leaf"
fields = [
  { name = "title", type = "text" },
]

[[resources.types]]
name = "Part"
fields = [
  { name = "title", type = "text" },
  { name = "count", type = "int" },
  { name = "done", type = "bool" },
]

# Two types named as the app language names two of its own: an app names its types without
# knowing which names the language holds. One holds what only reads by its declared type.
[[resources.types]]
name = "View"
fields = [
  { name = "source", type = "text" },
  { name = "by", type = "actor" },
  { name = "parts", type = { collection = { named = "Part" } } },
]

[[resources.types]]
name = "Bytes"
fields = [
  { name = "received", type = "int" },
  { name = "sent", type = "int" },
]
"#;

/// What a view should read of a filled value.
enum Want {
    /// A value that reads as this.
    Plain(Json),
    /// An option that holds nothing.
    Nothing,
    /// The items of a list, a set or a collection, in their order.
    Items(Vec<Want>),
    /// The entries of a map, by their keys.
    Entries(Vec<(String, Want)>),
    /// A declared type's fields.
    Fields(Vec<Field>),
    /// An enum's case and the values it carries.
    Case(String, Vec<Field>),
    /// A type the app language has no type for: no view reads it.
    Untyped,
}

/// One field of a declared type or of an enum's case.
struct Field {
    /// Its name as a view writes it.
    name: String,
    want: Want,
    holds: Holds,
}

/// What a field's type holds somewhere inside, which tells one stop from another where both trap
/// with the same words.
#[derive(Clone, Copy, Default)]
struct Holds {
    map: bool,
    rich: bool,
}

/// A field's name as a view writes it: the values of a case that numbers them are `value`,
/// `value1`, and a key with hyphens is written with underscores (`catalogue`'s own rule).
fn written(name: &str) -> String {
    match name.parse::<u32>() {
        Ok(0) => "value".to_owned(),
        Ok(index) => format!("value{index}"),
        Err(_) => name.replace('-', "_"),
    }
}

/// Fills a value of a declared type as a read answers a stored one, and says what a view should
/// read of it.
struct Filler<'a> {
    /// The named types of the app's surface.
    named: &'a BTreeMap<String, NamedType>,
    /// How the kind keeps each path that says so (`collection`, `keyed`).
    kept: BTreeMap<&'a str, &'a str>,
    /// Which case every enum takes.
    turn: usize,
    /// Whether every option holds nothing.
    empty: bool,
    /// Whether every number stands at 0 and every flag is off, with no key for one that is a
    /// declared type's field: a store keeps what an absent key reads as by leaving it out
    /// (`tilo_resource`'s `write_fields_leaving`), and a read answers what the store keeps.
    sparse: bool,
    /// The last id an item was given: no two items of one value share one.
    ids: Cell<usize>,
}

impl Filler<'_> {
    fn id(&self) -> usize {
        let next = self.ids.get().saturating_add(1);
        self.ids.set(next);
        next
    }

    /// Whether a declared type's field of type `ty` has no key in what is stored
    /// ([`Self::sparse`]).
    fn left_out(&self, ty: &FieldType) -> bool {
        self.sparse && matches!(ty, FieldType::Int | FieldType::Float | FieldType::Bool)
    }

    /// Whether the app language has a type for `ty` (`catalogue`'s own rule).
    fn typed(&self, ty: &FieldType) -> bool {
        match ty {
            FieldType::Any | FieldType::Reactions | FieldType::Outbox(_) => false,
            FieldType::Named(name) => self.named.contains_key(name),
            FieldType::Option(inner)
            | FieldType::List(inner)
            | FieldType::Set(inner)
            | FieldType::Collection(inner)
            | FieldType::Map(inner) => self.typed(inner),
            FieldType::Union(cases) => cases
                .iter()
                .all(|case| matches!(case, FieldType::Resource(_))),
            FieldType::Text
            | FieldType::RichText
            | FieldType::Int
            | FieldType::Float
            | FieldType::Bool
            | FieldType::Date
            | FieldType::Time
            | FieldType::Identifier
            | FieldType::Actor
            | FieldType::Resource(_)
            | FieldType::Duration => true,
        }
    }

    fn holds(&self, ty: &FieldType, open: &mut Vec<String>) -> Holds {
        match ty {
            FieldType::RichText => Holds {
                map: false,
                rich: true,
            },
            FieldType::Map(inner) => Holds {
                map: true,
                ..self.holds(inner, open)
            },
            FieldType::Option(inner)
            | FieldType::List(inner)
            | FieldType::Set(inner)
            | FieldType::Collection(inner) => self.holds(inner, open),
            FieldType::Named(name) => {
                let Some(declared) = self.named.get(name) else {
                    return Holds::default();
                };
                if open.iter().any(|held| held == name) {
                    return Holds::default();
                }
                open.push(name.clone());
                let mut all = Holds::default();
                let carried = declared.cases.iter().flat_map(|case| case.fields.iter());
                for field in declared.fields.iter().chain(carried) {
                    let inside = self.holds(&field.ty, open);
                    all = Holds {
                        map: all.map || inside.map,
                        rich: all.rich || inside.rich,
                    };
                }
                open.pop();
                all
            }
            _ => Holds::default(),
        }
    }

    /// The kind `surface` describes, filled: what a read answers, and what a view should read.
    fn kind(&self, surface: &Surface) -> (Json, Vec<Field>) {
        let mut stored = Map::new();
        let mut wanted = Vec::new();
        for field in surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
        {
            let (value, want) = self
                .fill(&field.ty, &field.path, 1, &mut Vec::new())
                .unwrap_or((Json::Null, Want::Nothing));
            if !self.left_out(&field.ty) {
                stored.insert(field.path.clone(), value);
            }
            wanted.push(Field {
                name: written(&field.path),
                want,
                holds: self.holds(&field.ty, &mut Vec::new()),
            });
        }
        (Json::Object(stored), wanted)
    }

    /// A value of type `ty` at the path `at`, marked `mark` among its neighbours; `None` where a
    /// type that holds itself has to end.
    fn fill(
        &self,
        ty: &FieldType,
        at: &str,
        mark: usize,
        open: &mut Vec<String>,
    ) -> Option<(Json, Want)> {
        if !self.typed(ty) {
            return Some((json!({ "untyped": true }), Want::Untyped));
        }
        let plain = |value: Json| Some((value.clone(), Want::Plain(value)));
        match ty {
            // What a store leaves out of a declared type's fields, as its absent key reads.
            FieldType::Int if self.sparse => plain(json!(0)),
            FieldType::Float if self.sparse => plain(json!(0.0)),
            FieldType::Bool if self.sparse => plain(json!(false)),
            FieldType::Text => plain(json!(format!("{at} #{mark}"))),
            FieldType::Int => plain(json!(mark.saturating_add(7))),
            FieldType::Float => plain(json!(1.5)),
            FieldType::Bool => plain(json!(true)),
            FieldType::Date => plain(json!("2026-11-18")),
            FieldType::Time => plain(json!("2026-11-18T04:30:00Z")),
            FieldType::Identifier => plain(json!("mailto:ada@example.test")),
            FieldType::Duration => plain(json!(90)),
            FieldType::Resource(_) => plain(json!(format!("r{mark}"))),
            FieldType::Union(cases) => {
                // A union's value names which of its types it is: an id on its own does not say.
                let id = format!("r{mark}");
                let kind = cases.iter().find_map(|case| match case {
                    FieldType::Resource(kind) => Some(kind.as_str()),
                    _ => None,
                });
                match kind.and_then(type_key_of_kind) {
                    Some(ty) => Some((json!({ "ty": ty, "id": id }), Want::Plain(json!(id)))),
                    None => plain(json!(id)),
                }
            }
            FieldType::RichText => {
                // As a read answers a stored rich text: its paragraphs a list in their order,
                // each with its id, its kind, its words and its attributes. The ids here do
                // not stand in the order of the paragraphs, so the order read is the list's.
                let first = format!("{at} #{mark}");
                let second = format!("{at} #{mark}, its second paragraph");
                let blocks = json!([
                    { "id": "b2", "kind": "paragraph", "text": first, "attrs": {} },
                    { "id": "b1", "kind": "paragraph", "text": second, "attrs": {} },
                ]);
                let stored = json!({ "schema": "tilo.richtext", "blocks": blocks, "styles": {} });
                Some((stored, Want::Plain(json!(format!("{first}\n{second}")))))
            }
            FieldType::Actor => {
                // Written whole as one text of JSON (`tilo_resource`'s `json_scalar`): an
                // actor's own, or a member's, which a surface declares as an actor too.
                let member = MemberId::person("ada");
                let who = Actor::Member(member.clone());
                let kept = if mark.is_multiple_of(2) {
                    serde_json::to_string(&member)
                } else {
                    serde_json::to_string(&who)
                };
                let kept = Json::String(kept.unwrap_or_default());
                Some((kept, Want::Plain(json!({ "actor": who }))))
            }
            FieldType::Named(name) => self.named_value(name, at, mark, open),
            FieldType::Option(inner) => {
                let filled = if self.empty {
                    None
                } else {
                    self.fill(inner, at, mark, open)
                };
                Some(filled.unwrap_or((Json::Null, Want::Nothing)))
            }
            FieldType::List(_)
            | FieldType::Set(_)
            | FieldType::Collection(_)
            | FieldType::Map(_) => Some(self.gathered(ty, at, open)),
            FieldType::Any | FieldType::Reactions | FieldType::Outbox(_) => {
                Some((json!({ "untyped": true }), Want::Untyped))
            }
        }
    }

    /// Two values of type `ty` inside `at`, fewer where a type that holds itself ends.
    fn items(&self, ty: &FieldType, at: &str, open: &mut Vec<String>) -> Vec<(Json, Want)> {
        let inside = format!("{at}.*");
        [1, 2]
            .into_iter()
            .filter_map(|mark| self.fill(ty, &inside, mark, open))
            .collect()
    }

    /// A list, a set, a collection or a map at `at`, as each is stored.
    fn gathered(&self, ty: &FieldType, at: &str, open: &mut Vec<String>) -> (Json, Want) {
        match ty {
            FieldType::Set(inner) => {
                // A map keyed by each element's JSON form, a text as itself.
                let keyed: BTreeMap<String, (Json, Want)> = self
                    .items(inner, at, open)
                    .into_iter()
                    .map(|(stored, want)| {
                        let key = match &stored {
                            Json::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        (key, (stored, want))
                    })
                    .collect();
                let mut stored = Map::new();
                let mut wanted = Vec::new();
                for (key, (value, want)) in keyed {
                    stored.insert(key, value);
                    wanted.push(want);
                }
                (Json::Object(stored), Want::Items(wanted))
            }
            FieldType::Collection(inner) => {
                // Its items by id, each under its place in the order; a keyed one by key.
                let keyed = self.kept.get(at).is_some_and(|kept| *kept == "keyed");
                let mut stored = Map::new();
                let mut wanted = Vec::new();
                for (item, want) in self.items(inner, at, open) {
                    let id = self.id();
                    if keyed {
                        stored.insert(format!("k{id:06}"), item);
                    } else {
                        let entry = json!({ "k": format!("a{id:06}"), "v": item });
                        stored.insert(format!("i{id:06}"), entry);
                    }
                    wanted.push(want);
                }
                (Json::Object(stored), Want::Items(wanted))
            }
            FieldType::Map(inner) => {
                let mut stored = Map::new();
                let mut wanted = Vec::new();
                for (mark, (value, want)) in self.items(inner, at, open).into_iter().enumerate() {
                    let key = format!("m{mark}");
                    stored.insert(key.clone(), value);
                    wanted.push((key, want));
                }
                (Json::Object(stored), Want::Entries(wanted))
            }
            other => {
                let inner = match other {
                    FieldType::List(inner) => inner.as_ref(),
                    other => other,
                };
                let (stored, wanted): (Vec<Json>, Vec<Want>) =
                    self.items(inner, at, open).into_iter().unzip();
                (Json::Array(stored), Want::Items(wanted))
            }
        }
    }

    /// A value of the named type `name`: a declared type's fields, or an enum's case.
    fn named_value(
        &self,
        name: &str,
        at: &str,
        mark: usize,
        open: &mut Vec<String>,
    ) -> Option<(Json, Want)> {
        let declared = self.named.get(name)?;
        if open.iter().filter(|held| held.as_str() == name).count() >= 2 {
            return None;
        }
        open.push(name.to_owned());
        let filled = if declared.is_enum() {
            self.case_value(declared, at, mark, open)
        } else {
            self.fields_value(&declared.fields, at, mark, open, true)
                .map(|(stored, wanted)| (Json::Object(stored), Want::Fields(wanted)))
        };
        open.pop();
        filled
    }

    /// A value of each of `fields`, by name. `leave`: they are a declared type's own, which a
    /// store keeps without a key for what stands at its start ([`Self::left_out`]); the values
    /// a case carries are each kept.
    fn fields_value(
        &self,
        fields: &[TypeField],
        at: &str,
        mark: usize,
        open: &mut Vec<String>,
        leave: bool,
    ) -> Option<(Map<String, Json>, Vec<Field>)> {
        let mut stored = Map::new();
        let mut wanted = Vec::new();
        for field in fields {
            let inside = format!("{at}.{}", field.name);
            let (value, want) = self.fill(&field.ty, &inside, mark, open)?;
            if !(leave && self.left_out(&field.ty)) {
                stored.insert(field.name.clone(), value);
            }
            wanted.push(Field {
                name: written(&field.name),
                want,
                holds: self.holds(&field.ty, &mut Vec::new()),
            });
        }
        Some((stored, wanted))
    }

    /// The case of this turn, or the first after it whose values end: an enum of plain cases
    /// is stored as its case's name, one whose cases carry values as a map of the name under
    /// `$variant` beside the case's values by name.
    fn case_value(
        &self,
        declared: &NamedType,
        at: &str,
        mark: usize,
        open: &mut Vec<String>,
    ) -> Option<(Json, Want)> {
        let carries = declared.cases.iter().any(|case| !case.fields.is_empty());
        let count = declared.cases.len();
        let first = self.turn.checked_rem(count).unwrap_or(0);
        declared
            .cases
            .iter()
            .cycle()
            .skip(first)
            .take(count)
            .find_map(|case| {
                let inside = format!("{at}.{}", case.name);
                let (mut stored, wanted) =
                    self.fields_value(&case.fields, &inside, mark, open, false)?;
                let value = if carries {
                    stored.insert("$variant".to_owned(), json!(case.name));
                    Json::Object(stored)
                } else {
                    json!(case.name)
                };
                Some((value, Want::Case(case.name.clone(), wanted)))
            })
    }
}

/// Reads a filled kind back as a view's code does, and keeps what did not arrive.
struct Walk<'a> {
    runtime: &'a Runtime,
    meta: &'a PackageMeta,
    /// What did not arrive: where, the class of the stop, and why.
    stops: BTreeMap<(String, &'static str), String>,
}

impl Walk<'_> {
    fn stop(&mut self, at: &str, class: &'static str, why: String) {
        self.stops.entry((at.to_owned(), class)).or_insert(why);
    }

    /// Read each of `wanted` from `target`, a resource of a type the app declares.
    fn fields(&mut self, at: &str, wanted: &[Field], target: &ResourceRef) {
        let meta = self.meta;
        let Some(declared) = meta.types.iter().find(|held| held.id == target.ty) else {
            let why = format!("a `{}`, which the package does not declare", target.ty);
            self.stop(at, "WRONG", why);
            return;
        };
        for field in wanted {
            let inside = if at.is_empty() {
                field.name.clone()
            } else {
                format!("{at}.{}", field.name)
            };
            if matches!(field.want, Want::Untyped) {
                let why = "the app language has no type for it".to_owned();
                self.stop(&inside, "UNTYPED", why);
            } else if declared.fields.iter().any(|held| held.name == field.name) {
                match self.runtime.read(target, &field.name) {
                    Ok(value) => self.check(&inside, &field.want, &value),
                    Err(error) => {
                        let why = error.to_string();
                        self.stop(&inside, class_of(&why, field.holds), why);
                    }
                }
            } else {
                let why = "the package the surface reads as declares no such field".to_owned();
                self.stop(&inside, "NO FIELD", why);
            }
        }
    }

    /// Whether `value`, read at `at`, is what `want` says.
    fn check(&mut self, at: &str, want: &Want, value: &Value) {
        match (want, value) {
            (Want::Untyped, _) => {
                let why = "the app language has no type for it".to_owned();
                self.stop(at, "UNTYPED", why);
            }
            (want, Value::Opt(Some(inner))) => self.check(at, want, inner),
            (Want::Nothing, Value::Opt(None) | Value::Unit) => {}
            (Want::Plain(wanted), value) => match plain(value) {
                Some(read) if read == *wanted => {}
                Some(Json::Null) => {
                    self.stop(at, "EMPTY", format!("wanted {wanted}, read nothing"));
                }
                Some(read) => self.stop(at, "WRONG", format!("wanted {wanted}, read {read}")),
                None => {
                    let why = format!("wanted {wanted}, read a {}", kind_of(value));
                    self.stop(at, "WRONG", why);
                }
            },
            (Want::Items(wanted), Value::List(items)) => {
                if wanted.len() != items.len() {
                    let why = format!("wanted {} items, read {}", wanted.len(), items.len());
                    self.stop(at, "LOST", why);
                }
                let inside = format!("{at}.*");
                for (want, item) in wanted.iter().zip(items) {
                    self.check(&inside, want, item);
                }
            }
            (Want::Entries(wanted), Value::Map(_) | Value::List(_)) => {
                let entries = entries_of(value);
                if wanted.len() != entries.len() {
                    let why = format!("wanted {} entries, read {}", wanted.len(), entries.len());
                    self.stop(at, "LOST", why);
                }
                let inside = format!("{at}.*");
                for (key, want) in wanted {
                    let held = entries
                        .iter()
                        .find(|(held, _)| matches!(held, Value::Text(text) if text == key));
                    match held {
                        Some((_, value)) => self.check(&inside, want, value),
                        None => self.stop(&inside, "LOST", format!("no entry `{key}`")),
                    }
                }
            }
            (Want::Fields(wanted), Value::Ref(held)) => self.fields(at, wanted, &held.target),
            (Want::Case(name, carried), Value::Enum(held)) => self.case(at, name, carried, held),
            (want, value) => {
                let why = format!("wanted {}, read a {}", said_of(want), kind_of(value));
                self.stop(at, "WRONG", why);
            }
        }
    }

    /// Whether `held` is the case `name` with every value it carries.
    fn case(&mut self, at: &str, name: &str, carried: &[Field], held: &EnumValue) {
        let meta = self.meta;
        let variant = meta
            .enums
            .iter()
            .find(|declared| declared.id == held.ty)
            .and_then(|declared| {
                declared
                    .variants
                    .iter()
                    .find(|variant| variant.id == held.variant)
            });
        let Some(variant) = variant else {
            let why = format!(
                "the case `{}` of `{}`, which the package does not declare",
                held.variant, held.ty
            );
            self.stop(at, "WRONG", why);
            return;
        };
        if variant.name != name {
            let why = format!("wanted the case `{name}`, read `{}`", variant.name);
            self.stop(at, "WRONG", why);
            return;
        }
        for field in carried {
            let inside = format!("{at}.{name}.{}", field.name);
            let read = variant
                .fields
                .iter()
                .find(|declared| declared.name == field.name)
                .and_then(|declared| held.fields.get(&declared.id));
            match (&field.want, read) {
                (Want::Untyped, _) => {
                    let why = "the app language has no type for it".to_owned();
                    self.stop(&inside, "UNTYPED", why);
                }
                (Want::Nothing, None) => {}
                (want, Some(value)) => self.check(&inside, want, value),
                (_, None) => {
                    let why = "the case arrives without this value".to_owned();
                    self.stop(&inside, "LOST", why);
                }
            }
        }
    }
}

/// The runtime's name for the type of a tilolang package's resource kind (`tl.<app>.<Type>`):
/// `app#Type`, as a union's encoded reference spells it.
fn type_key_of_kind(kind: &str) -> Option<String> {
    let rest = kind.strip_prefix("tl.")?;
    let (app, _) = rest.rsplit_once('.')?;
    Some(format!("{app}#{}", catalogue::kind_type_name(kind)))
}

/// A value that is no list, map, case or resource, as the JSON a filled one is compared with.
fn plain(value: &Value) -> Option<Json> {
    Some(match value {
        Value::Unit | Value::Opt(None) => Json::Null,
        Value::Int(number) => json!(number),
        Value::Float(number) => json!(number),
        Value::Bool(flag) => json!(flag),
        Value::Text(text) => json!(text),
        Value::Rich(rich) => json!(rich.written_text()),
        Value::Actor(who) => json!({ "actor": who }),
        Value::Ref(held) => json!(held.target.id_string()),
        _ => return None,
    })
}

/// A map's entries as the runtime holds them: a map, or the list of key and value that a field
/// of a map reads as.
fn entries_of(value: &Value) -> Vec<(&Value, &Value)> {
    match value {
        Value::Map(entries) => entries.iter().map(|(key, held)| (key, held)).collect(),
        Value::List(pairs) => pairs
            .iter()
            .filter_map(|pair| match pair {
                Value::Tuple(both) => Some((both.first()?, both.get(1)?)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Unit => "nothing",
        Value::Int(_) | Value::Float(_) => "number",
        Value::Bool(_) => "flag",
        Value::Text(_) => "text",
        Value::Rich(_) => "rich text",
        Value::List(_) | Value::Tuple(_) => "list",
        Value::Opt(_) => "option",
        Value::Enum(_) => "case",
        Value::Ref(_) => "resource",
        Value::Actor(_) => "actor",
        Value::Map(_) => "map",
        _ => "value of another sort",
    }
}

fn said_of(want: &Want) -> String {
    match want {
        Want::Plain(value) => value.to_string(),
        Want::Nothing => "nothing".to_owned(),
        Want::Items(items) => format!("{} items", items.len()),
        Want::Entries(entries) => format!("{} entries", entries.len()),
        Want::Fields(_) => "a declared type's fields".to_owned(),
        Want::Case(name, _) => format!("the case `{name}`"),
        Want::Untyped => "nothing a view reads".to_owned(),
    }
}

/// The class of a stop a read named with `why`, of a field whose type holds `holds`.
fn class_of(why: &str, holds: Holds) -> &'static str {
    if why.contains("is not a case of") {
        "CASE WITH A VALUE"
    } else if why.contains("not an encoded value") || why.contains("bare array") {
        match (holds.map, holds.rich) {
            (true, true) => "MAP OR RICH TEXT",
            (true, false) => "MAP",
            (false, true) => "RICH TEXT",
            (false, false) => "ENCODING",
        }
    } else if why.contains("is not a Actor") || why.contains("is not an Actor") {
        "ACTOR"
    } else if why.contains("is not read from another app's reading") {
        "NAMED LIKE A BUILT-IN"
    } else if why.contains("is not a reference") || why.contains("is not a resource id") {
        "REFERENCE"
    } else if why.contains("is missing from the reading") {
        "MISSING"
    } else if why.contains(" is not a ") {
        "SHAPE"
    } else {
        "TRAP"
    }
}

/// The type name of the kind `kind` from its last two segments (`catalogue`'s own rule for a
/// kind whose short name a named type already has).
fn longer(kind: &str) -> String {
    let mut segments = kind.rsplit('.');
    let last = segments.next().unwrap_or(kind);
    let before = segments.next().unwrap_or_default();
    format!(
        "{}{}",
        catalogue::kind_type_name(before),
        catalogue::kind_type_name(last)
    )
}

/// How a view names the kind `kind` of the app a view calls `alias`, whose first kind and its
/// name are `first`: said in the table, judged by nothing here.
fn how_named(alias: &str, first: Option<&(String, String)>, kind: &str) -> String {
    match first {
        Some((held, name)) if held == kind => {
            if alias.chars().all(|c| c.is_alphanumeric() || c == '_') {
                format!("a view names it {alias}.{name}")
            } else {
                format!("NAME: no view can write `{alias}`")
            }
        }
        _ => "WHICH: no view names it".to_owned(),
    }
}

/// What the sweep found.
#[derive(Default)]
struct Table {
    /// One line a kind: what a view reads of it.
    kinds: Vec<String>,
    /// Every stop as its line in the list of known ones, with why.
    stops: BTreeMap<String, String>,
    /// The apps this build carries no surface of: no view names one.
    unpublished: Vec<String>,
}

impl Table {
    /// Walk every kind of the app `id` that people call `title`, from its surface file `text`.
    fn app(&mut self, id: &str, title: &str, text: &str) {
        let mut app = match App::of(id, title, text) {
            Ok(app) => app,
            Err(why) => {
                self.stops.insert(format!("SURFACE | {id} | * | *"), why);
                return;
            }
        };
        let surfaces = catalogue::parse_surfaces(text).unwrap_or_default();
        let runtime = Runtime::new(AmStore::new(), SimpleAuthority::new())
            .unwrap_or_else(|error| panic!("a runtime starts: {error}"));
        if let Err(error) = runtime.load_foreign(app.meta.clone()) {
            let why = error.to_string();
            self.stops.insert(format!("PACKAGE | {id} | * | *"), why);
            return;
        }
        let first = app
            .root
            .as_ref()
            .map(|root| (root.published.clone(), root.name.clone()));
        let taken: Vec<String> = surfaces
            .iter()
            .flat_map(|surface| surface.types.iter().map(|declared| declared.name.clone()))
            .collect();
        let mut named = BTreeMap::new();
        for surface in &surfaces {
            for declared in &surface.types {
                named.insert(declared.name.clone(), declared.clone());
            }
            if !surface.fields.is_empty() {
                let short = catalogue::kind_type_name(&surface.kind);
                let name = if taken.contains(&short) {
                    longer(&surface.kind)
                } else {
                    short
                };
                let how = how_named(&app.alias, first.as_ref(), &surface.kind);
                app.root = Some(Root {
                    ty: format!("{}#{name}", app.meta.app),
                    name,
                    published: surface.kind.clone(),
                });
                self.kind(id, &app, &runtime, surface, &named, &how);
            }
        }
    }

    /// Fill the kind `surface` describes, once for every case, once empty and once as a store
    /// keeps what stands at its start, and read each filling back through the app's reader.
    fn kind(
        &mut self,
        id: &str,
        app: &App,
        runtime: &Runtime,
        surface: &Surface,
        named: &BTreeMap<String, NamedType>,
        how: &str,
    ) {
        let Some(root) = &app.root else {
            return;
        };
        let target = ResourceRef::root(surface.kind.clone(), root.ty.clone());
        let turns = named
            .values()
            .filter(|declared| declared.cases.iter().any(|case| !case.fields.is_empty()))
            .map(|declared| declared.cases.len())
            .max()
            .unwrap_or(0)
            .clamp(2, 16);
        let mut walk = Walk {
            runtime,
            meta: &app.meta,
            stops: BTreeMap::new(),
        };
        for turn in 0..=turns.saturating_add(1) {
            let filler = Filler {
                named,
                kept: surface
                    .fields
                    .iter()
                    .map(|field| (field.path.as_str(), field.stored.as_str()))
                    .collect(),
                turn,
                empty: turn == turns,
                sparse: turn > turns,
                ids: Cell::new(0),
            };
            let (stored, wanted) = filler.kind(surface);
            let reading = app.reading(&stored);
            match runtime.set_reading(&target, &reading.to_string()) {
                Ok(_) => walk.fields("", &wanted, &target),
                Err(error) => walk.stop("*", "TRAP", error.to_string()),
            }
        }
        let tops: Vec<String> = surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
            .map(|field| written(&field.path))
            .collect();
        let stopped: BTreeSet<&str> = walk
            .stops
            .keys()
            .map(|(at, _)| at.split('.').next().unwrap_or_default())
            .collect();
        let whole = tops
            .iter()
            .filter(|top| !stopped.contains(top.as_str()))
            .count();
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, class) in walk.stops.keys() {
            let count = classes.entry(*class).or_insert(0);
            *count = count.saturating_add(1);
        }
        let classes = classes
            .iter()
            .map(|(class, count)| format!("{class} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.kinds.push(format!(
            "{id} | {} | {} | {how} | {whole} of {} whole | {classes}",
            surface.kind,
            root.name,
            tops.len()
        ));
        for ((at, class), why) in walk.stops {
            self.stops
                .entry(format!("{class} | {id} | {} | {at}", surface.kind))
                .or_insert(why);
        }
    }

    /// The table: every kind, every stop with why, and what differs from the known list.
    fn said(&self, new: &[&str], gone: &[&str]) -> String {
        let mut lines =
            vec!["KINDS: app | kind | type | how a view names it | fields | stops".to_owned()];
        lines.extend(self.kinds.iter().map(|kind| format!("KIND {kind}")));
        lines.extend(
            self.unpublished
                .iter()
                .map(|app| format!("NO SURFACE {app}")),
        );
        lines.extend(
            self.stops
                .iter()
                .map(|(stop, why)| format!("STOP {stop} || {why}")),
        );
        lines.extend(new.iter().map(|stop| format!("NEW {stop}")));
        lines.extend(gone.iter().map(|stop| format!("GONE {stop}")));
        lines.join("\n")
    }

    /// Red for a stop `known` does not hold, and for a known one that is gone.
    fn judge(&self, known: &BTreeSet<&str>) {
        let found: BTreeSet<&str> = self.stops.keys().map(String::as_str).collect();
        let new: Vec<&str> = found.difference(known).copied().collect();
        let gone: Vec<&str> = known.difference(&found).copied().collect();
        assert!(
            new.is_empty() && gone.is_empty(),
            "what a view reads differs from sweep_known.txt: {} stops it does not hold (NEW), {} of \
             its lines whose stop is gone (GONE)\n{}",
            new.len(),
            gone.len(),
            self.said(&new, &gone)
        );
    }
}

/// The stops of [`KNOWN`]: the shapes' own when `shapes`, the fleet's otherwise.
fn known(shapes: bool) -> BTreeSet<&'static str> {
    let own = format!(" | {SHAPES_APP} | ");
    KNOWN
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| line.contains(&own) == shapes)
        .collect()
}

#[test]
fn every_kind_every_app_publishes_reaches_a_view_whole_or_is_a_known_stop() {
    let mut table = Table::default();
    for app in app_fleet::apps() {
        match app_fleet::surface(app.id) {
            Some(surface) => table.app(app.id, app.name, surface),
            None => table.unpublished.push(app.id.to_owned()),
        }
    }
    assert!(
        table.kinds.len() >= 60,
        "the fleet's surfaces are walked: {} kinds",
        table.kinds.len()
    );
    table.judge(&known(false));
}

#[test]
fn every_shape_a_surface_declares_reaches_a_view_whole_or_is_a_known_stop() {
    let mut table = Table::default();
    table.app(SHAPES_APP, "Sweep", SHAPES);
    assert_eq!(table.kinds.len(), 1, "the shapes read as one kind");
    table.judge(&known(true));
}
