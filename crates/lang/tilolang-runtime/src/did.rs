//! What history says a change reads with, worked out from the change itself: the type the field
//! declares, and what it held before and after.
//!
//! One place decides it, so the index store and the Am store read a change the same way.
//!
//! The words are the platform's: [`crate::words::Words`] resolves each key to the reader's
//! language, and the source words beside the key are what a host with no catalogue reads, as
//! the words an action declares for itself (`did:`) are. The words name the field they are of
//! (`$field`), in the case a sentence goes on with ("set priority to").

use crate::registry::FieldInfo;
use crate::value::Value;
use tilolang::types::Ty;

/// Which of a change's two values its words speak of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    /// What the field held before: "moved it out of" a project it left.
    Before,
    /// What it holds after: "moved it to" the day it is due.
    After,
    /// Neither: "unassigned it".
    None,
}

impl Shows {
    /// The word `host.Change.shows` carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::None => "",
        }
    }
}

/// A change in words a sentence goes on with after who made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    /// The key of the words in the platform's catalogue.
    pub key: &'static str,
    /// The words themselves, as the catalogue's source words hold them, with `{ $field }` where
    /// the field's name goes (`set { $field } to`).
    pub source: &'static str,
    /// The field the words name, in a sentence ("priority"), when they name one.
    pub field: Option<String>,
    /// Which of the change's values the words speak of.
    pub shows: Shows,
}

/// What a change to `field` reads with, `named` being the field's name in a sentence.
#[must_use]
pub fn said(field: &FieldInfo, named: &str, before: &Value, after: &Value) -> Said {
    let gone = kind(after) == "none";
    let was_gone = kind(before) == "none";
    let mut said = by_type(&field.ty, named, after, gone, was_gone);
    if said.shows == Shows::Before && was_gone {
        // Nothing stood before the change: there is no value of its to speak of.
        said.shows = Shows::None;
    }
    said
}

/// What a change to a field of `ty` reads with.
fn by_type(ty: &Ty, named: &str, after: &Value, gone: bool, was_gone: bool) -> Said {
    let mut ty = ty;
    while let Ty::Option(inner) = ty {
        ty = inner;
    }
    match ty {
        Ty::Date if gone => naming(
            "tilolang-did-cleared-date",
            "cleared the { $field } date",
            named,
        ),
        Ty::Date if was_gone => {
            after_naming("tilolang-did-set-date", "set the { $field } date to", named)
        }
        Ty::Time if gone => naming(
            "tilolang-did-cleared-time",
            "cleared the { $field } time",
            named,
        ),
        Ty::Time => after_naming("tilolang-did-set-time", "set the { $field } time to", named),
        Ty::Principal if gone => plain("tilolang-did-unassigned", "unassigned it", Shows::None),
        Ty::Principal => plain("tilolang-did-assigned", "assigned it to", Shows::After),
        Ty::Record(_) | Ty::Union(_) if gone => {
            plain("tilolang-did-taken-out", "moved it out of", Shows::Before)
        }
        Ty::Date | Ty::Record(_) | Ty::Union(_) => {
            plain("tilolang-did-moved", "moved it to", Shows::After)
        }
        // A flag reads as set or cleared: the field's name is what it means ("marked it done"),
        // and there is no value of its to go on with.
        Ty::Bool if flag(after) == Some(true) => {
            naming("tilolang-did-marked", "marked it { $field }", named)
        }
        Ty::Bool if flag(after) == Some(false) => {
            naming("tilolang-did-unmarked", "unmarked it { $field }", named)
        }
        Ty::Bool | Ty::Int | Ty::Float | Ty::Enum(_) if gone => {
            naming("tilolang-did-cleared", "cleared { $field }", named)
        }
        Ty::Bool | Ty::Int | Ty::Float | Ty::Enum(_) => {
            after_naming("tilolang-did-set", "set { $field } to", named)
        }
        _ => naming("tilolang-did-changed", "changed { $field }", named),
    }
}

/// Words that name no value: "unassigned this".
fn plain(key: &'static str, source: &'static str, shows: Shows) -> Said {
    Said {
        key,
        source,
        field: None,
        shows,
    }
}

/// Words that name no value and the field they are of: "changed title".
fn naming(key: &'static str, source: &'static str, named: &str) -> Said {
    Said {
        key,
        source,
        field: Some(named.to_owned()),
        shows: Shows::None,
    }
}

/// Words that speak of the field's new value and name the field: "set priority to".
fn after_naming(key: &'static str, source: &'static str, named: &str) -> Said {
    Said {
        key,
        source,
        field: Some(named.to_owned()),
        shows: Shows::After,
    }
}

/// The flag `shown` holds, where it shows one (`Core::shown`): a flag has no value to go on
/// with, so what it now says is the whole of what changed.
fn flag(shown: &Value) -> Option<bool> {
    match shown {
        Value::Bool(flag) => Some(*flag),
        Value::Tuple(parts) if kind(shown) == "bool" => match parts.get(1) {
            Some(Value::Bool(flag)) => Some(*flag),
            _ => None,
        },
        _ => None,
    }
}

/// The kind of thing a shown value is (`Core::shown`).
fn kind(shown: &Value) -> &str {
    match shown {
        Value::Tuple(parts) => parts.first().and_then(Value::as_text).unwrap_or(""),
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::FieldKind;
    use tilolang::types::TypeKey;

    fn field(name: &str, ty: Ty) -> FieldInfo {
        FieldInfo {
            id: name.to_owned(),
            name: name.to_owned(),
            ty,
            kind: FieldKind::Register,
            default: None,
        }
    }

    fn kind(name: &str) -> TypeKey {
        TypeKey {
            app: "so.tilo.test".to_owned(),
            name: name.to_owned(),
        }
    }

    /// A value as its type shows it (`Core::shown`).
    fn shown(kind: &str, held: Value) -> Value {
        Value::Tuple(vec![Value::Text(kind.to_owned()), held, Value::Opt(None)])
    }

    /// The key a change to `field` reads with, which value it speaks of, and whether it names
    /// the field; the field name it is given is `due`.
    #[track_caller]
    fn reads(f: &FieldInfo, before: &Value, after: &Value) -> (&'static str, &'static str, bool) {
        let said = said(f, "due", before, after);
        (said.key, said.shows.word(), said.field.is_some())
    }

    #[test]
    fn a_day_moved_to_reads_as_moved_and_a_new_one_as_set_and_a_cleared_one_as_cleared() {
        let due = field("due", Ty::Option(Box::new(Ty::Date)));
        let none = shown("none", Value::Unit);
        let later = shown("date", Value::text("2026-11-02"));
        assert_eq!(
            reads(&due, &shown("date", Value::text("2026-11-01")), &later),
            ("tilolang-did-moved", "after", false)
        );
        assert_eq!(
            reads(&due, &none, &later),
            ("tilolang-did-set-date", "after", true)
        );
        assert_eq!(
            reads(&due, &later, &none),
            ("tilolang-did-cleared-date", "", true)
        );
    }

    #[test]
    fn a_person_reads_as_assigned_to_and_unassigned() {
        let assignee = field("assignee", Ty::Option(Box::new(Ty::Principal)));
        let nobody = shown("none", Value::Unit);
        let ben = shown(
            "principal",
            Value::Principal(crate::ids::Principal::new("ben")),
        );
        assert_eq!(
            reads(&assignee, &nobody, &ben),
            ("tilolang-did-assigned", "after", false)
        );
        assert_eq!(
            reads(&assignee, &ben, &nobody),
            ("tilolang-did-unassigned", "", false)
        );
    }

    #[test]
    fn a_resource_reads_as_moved_to_and_out_of_the_one_it_left() {
        let project = field("project", Ty::Option(Box::new(Ty::Record(kind("Project")))));
        let inbox = shown("none", Value::Unit);
        let flat = shown("resource", Value::text("Move flat"));
        assert_eq!(
            reads(&project, &inbox, &flat),
            ("tilolang-did-moved", "after", false)
        );
        assert_eq!(
            reads(&project, &flat, &inbox),
            ("tilolang-did-taken-out", "before", false)
        );
    }

    #[test]
    fn an_enum_reads_as_set_to_or_cleared_and_so_does_another_flag() {
        let priority = field("priority", Ty::Option(Box::new(Ty::Enum(kind("Priority")))));
        let none = shown("none", Value::Unit);
        let high = shown("case", Value::text("High"));
        assert_eq!(
            reads(&priority, &none, &high),
            ("tilolang-did-set", "after", true)
        );
        assert_eq!(
            reads(&priority, &high, &none),
            ("tilolang-did-cleared", "", true)
        );
        // A flag has no value to go on with: it reads as set or cleared, by the field it is
        // of, and never as "set done to" with nothing after it.
        let pinned = field("pinned", Ty::Bool);
        assert_eq!(
            reads(
                &pinned,
                &shown("bool", Value::Bool(false)),
                &shown("bool", Value::Bool(true))
            ),
            ("tilolang-did-marked", "", true)
        );
        assert_eq!(
            reads(
                &pinned,
                &shown("bool", Value::Bool(true)),
                &shown("bool", Value::Bool(false))
            ),
            ("tilolang-did-unmarked", "", true)
        );
    }

    #[test]
    fn a_field_of_no_shape_of_its_own_reads_as_a_change_of_it() {
        let tags = field("tags", Ty::Set(Box::new(Ty::Text)));
        assert_eq!(
            reads(&tags, &Value::List(Vec::new()), &Value::List(Vec::new())),
            ("tilolang-did-changed", "", true)
        );
    }

    #[test]
    fn words_that_speak_of_a_value_that_stood_not_read_of_nothing() {
        // A resource that stood nowhere before cannot be a value "moved this out of".
        let project = field("project", Ty::Option(Box::new(Ty::Record(kind("Project")))));
        let none = shown("none", Value::Unit);
        assert_eq!(
            reads(&project, &none, &none),
            ("tilolang-did-taken-out", "", false)
        );
    }
}
