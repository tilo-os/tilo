//! What each type's methods are called: one list per type, read by the lowering that checks a
//! call (`call.rs`, `text_math.rs`) and by the messages that say what a type has. A field that
//! is a container is checked against the same list, so a method a container field's read accepts
//! is one its value has.

use crate::types::Ty;

/// The text methods, in the order a reader meets them: how a text is counted, read and cut, how
/// it is cased, how it is asked about, and what it becomes.
pub const TEXT: &[&str] = &[
    "len",
    "is_empty",
    "trim",
    "lower",
    "upper",
    "starts_with",
    "ends_with",
    "contains",
    "find",
    "slice",
    "replace",
    "split",
    "matches",
    "to_int",
];

/// What a whole number can be asked.
pub const INT: &[&str] = &["min", "max", "clamp", "abs", "to_float", "to_text"];

/// What a fractional number can be asked.
pub const FLOAT: &[&str] = &[
    "min", "max", "clamp", "abs", "round", "floor", "ceil", "to_int", "to_text",
];

/// `Bool.to_text`.
pub const BOOL: &[&str] = &["to_text"];

/// The methods every list has.
const LIST: &[&str] = &[
    "len",
    "is_empty",
    "first",
    "last",
    "at",
    "previous",
    "map",
    "flat_map",
    "filter",
    "any",
    "all",
    "find",
    "count",
    "counted",
    "index_of",
    "contains",
    "sort_by",
    "count_by",
    "group",
    "take",
    "each",
    "single",
    "descendants",
    "ancestors",
    "matching",
    "of",
];

/// The methods a set has: the list's, less the ones that need an order or a nesting.
const SET: &[&str] = &[
    "len", "is_empty", "first", "last", "map", "flat_map", "filter", "any", "all", "find", "count",
    "counted", "index_of", "contains", "sort_by", "count_by", "group", "take", "each", "single",
];

/// The methods a collection's items have, as a view reads them: the list's, less `of`, which
/// reads a list's own item, and without `first`/`last` counting a stored order the store keeps.
const COLLECTION: &[&str] = &[
    "len",
    "is_empty",
    "first",
    "last",
    "at",
    "previous",
    "map",
    "flat_map",
    "filter",
    "any",
    "all",
    "find",
    "count",
    "counted",
    "index_of",
    "contains",
    "sort_by",
    "count_by",
    "group",
    "take",
    "each",
    "single",
    "descendants",
    "ancestors",
    "matching",
];

/// The methods a map field's value has, read with the key they ask for.
const MAP: &[&str] = &["get", "keys"];

/// What a day can be asked: the day of the week it is, the day, month and year it names, and
/// the first day of the week it stands in.
const DAY: &[&str] = &["weekday", "day", "month", "year", "start_of_week"];

/// What a moment can be asked: the day it falls on and the time of day it is then.
const MOMENT: &[&str] = &["day", "time_of_day"];

/// What a length of time can be asked: the seconds it is, which is how a record keeps one.
const LENGTH: &[&str] = &["seconds"];

/// The methods a value of `ty` has, by name; `None` for a type whose methods a message says in
/// words of their own ([`described`]).
#[must_use]
pub fn names(ty: &Ty) -> Option<Vec<&'static str>> {
    let names: Vec<&'static str> = match ty {
        Ty::Text => TEXT.to_vec(),
        Ty::Int => INT.to_vec(),
        Ty::Float => FLOAT.to_vec(),
        Ty::Bool => BOOL.to_vec(),
        Ty::List(item) => {
            let mut names = LIST.to_vec();
            names.extend(aggregates(item));
            names
        }
        Ty::Set(_) => SET.to_vec(),
        Ty::Collection(_) => COLLECTION.to_vec(),
        Ty::Map(_) => MAP.to_vec(),
        Ty::Date => DAY.to_vec(),
        Ty::Time => MOMENT.to_vec(),
        Ty::Duration => LENGTH.to_vec(),
        _ => return None,
    };
    Some(names)
}

/// What a list of `item` holds that sums, joins or averages, which only some items do: text
/// joins, numbers sum and average.
fn aggregates(item: &Ty) -> Vec<&'static str> {
    match item {
        Ty::Text => vec!["join"],
        Ty::Int | Ty::Float => vec!["sum", "avg"],
        _ => Vec::new(),
    }
}

/// The methods a value of `ty` has, as a message names them, telling apart the types whose
/// methods are a sentence rather than a list.
#[must_use]
pub fn described(ty: &Ty) -> String {
    match ty {
        Ty::RichText => "is_empty (and the members excerpt, text, references, people)".to_owned(),
        Ty::Bytes => "text, len".to_owned(),
        Ty::File => "text (and the members name, media, size, url)".to_owned(),
        Ty::Json => "a document whose shape code asks as it reads, `match` on `.object(fields)`, `.array(items)`, `.string(text)`, `.number(number)`, `.bool(flag)`, `.null`".to_owned(),
        Ty::Response => "text (and the fields ok, status, status_text, headers, body)".to_owned(),
        Ty::Record(_) => "id (and fields)".to_owned(),
        // A day and a moment also come from a text, by way of the global reader rather than a
        // method of the value.
        Ty::Date => format!("{} (and `Date.parse`)", DAY.join(", ")),
        Ty::Time => format!("{} (and `Time.parse`)", MOMENT.join(", ")),
        Ty::Option(_) | Ty::Result(..) | Ty::Enum(_) => {
            "none; take it apart with `match`, such as `match x { .some(v) => v, .none => 0 }`"
                .to_owned()
        }
        other => names(other).map_or_else(|| "none".to_owned(), |names| names.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_names_what_its_items_can_be_asked() {
        assert!(
            names(&Ty::List(Box::new(Ty::Text)))
                .unwrap()
                .contains(&"join")
        );
        assert!(
            !names(&Ty::List(Box::new(Ty::Int)))
                .unwrap()
                .contains(&"join")
        );
        assert!(
            names(&Ty::List(Box::new(Ty::Int)))
                .unwrap()
                .contains(&"sum")
        );
        assert!(
            !names(&Ty::List(Box::new(Ty::Text)))
                .unwrap()
                .contains(&"sum")
        );
        assert_eq!(names(&Ty::Unit), None);
        assert!(described(&Ty::Text).starts_with("len, is_empty, trim"));
    }
}
