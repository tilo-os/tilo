//! What history says a change reads with, worked out from the change itself: the role and type
//! the field declares, and what it held before and after.
//!
//! One place decides it, so the index store and the Am store read a change the same way, as
//! [`crate::derived`] keeps derived keys for both. A field's role says what it is of its type
//! (`(done)`, `(title)`, `(time of: due)`), which reads better than its type alone: a flag that
//! is a task's done reads "completed this", where another flag reads "set pinned to".
//!
//! The words are the platform's: [`crate::words::Words`] resolves each key to the reader's
//! language, and the source words beside the key are what a host with no catalogue reads, as
//! the words an action declares for itself (`did:`) are. The words name the field they are of
//! (`$field`), in the case a sentence goes on with ("set priority to").

use crate::registry::FieldInfo;
use crate::value::Value;
use tilolang::types::{FieldRole, Ty};

/// Which of a change's two values its words speak of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    /// What the field held before: "moved this out of" a project it left.
    Before,
    /// What it holds after: "moved this to" the day it is due.
    After,
    /// Neither: "completed this".
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

/// What a change to `field` reads with, `named` being the field's name in a sentence (a time of
/// day reads with the day it is of: "set the due time to").
#[must_use]
pub fn said(field: &FieldInfo, named: &str, before: &Value, after: &Value) -> Said {
    let gone = kind(after) == "none";
    let was_gone = kind(before) == "none";
    let mut said = match &field.role {
        Some(FieldRole::Title) => plain("tilolang-did-renamed", "renamed this to", Shows::After),
        Some(FieldRole::Body) => naming("tilolang-did-edited", "edited the { $field }", named),
        Some(FieldRole::Done) => {
            if flag(after) {
                plain("tilolang-did-completed", "completed this", Shows::None)
            } else {
                plain("tilolang-did-reopened", "reopened this", Shows::None)
            }
        }
        Some(FieldRole::Parent) => {
            if gone {
                plain(
                    "tilolang-did-moved-top",
                    "moved this to the top level",
                    Shows::None,
                )
            } else {
                plain("tilolang-did-moved-under", "moved this under", Shows::After)
            }
        }
        Some(FieldRole::TimeOf { .. }) => {
            if gone {
                naming(
                    "tilolang-did-cleared-time",
                    "cleared the { $field } time",
                    named,
                )
            } else {
                after_naming("tilolang-did-set-time", "set the { $field } time to", named)
            }
        }
        // Repeats, reminders and what a resource was before read by their type.
        _ => by_type(&field.ty, named, gone, was_gone),
    };
    if said.shows == Shows::Before && was_gone {
        // Nothing stood before the change: there is no value of its to speak of.
        said.shows = Shows::None;
    }
    said
}

/// What a change to a field of `ty` reads with, when its role says nothing more.
fn by_type(ty: &Ty, named: &str, gone: bool, was_gone: bool) -> Said {
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
        Ty::Principal if gone => plain("tilolang-did-unassigned", "unassigned this", Shows::None),
        Ty::Principal => plain("tilolang-did-assigned", "assigned this to", Shows::After),
        Ty::Record(_) | Ty::Union(_) if gone => {
            plain("tilolang-did-taken-out", "moved this out of", Shows::Before)
        }
        Ty::Date | Ty::Record(_) | Ty::Union(_) => {
            plain("tilolang-did-moved", "moved this to", Shows::After)
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

/// Words that name no value: "completed this".
fn plain(key: &'static str, source: &'static str, shows: Shows) -> Said {
    Said {
        key,
        source,
        field: None,
        shows,
    }
}

/// Words that name no value and the field they are of: "edited the notes".
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

/// The kind of thing a shown value is (`Core::shown`).
fn kind(shown: &Value) -> &str {
    match shown {
        Value::Tuple(parts) => parts.first().and_then(Value::as_text).unwrap_or(""),
        _ => "",
    }
}

/// Whether a shown value is the flag true.
fn flag(shown: &Value) -> bool {
    matches!(shown, Value::Tuple(parts) if matches!(parts.get(1), Some(Value::Bool(true))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::FieldKind;
    use tilolang::types::TypeKey;

    fn field(name: &str, ty: Ty, role: Option<FieldRole>) -> FieldInfo {
        FieldInfo {
            id: name.to_owned(),
            name: name.to_owned(),
            ty,
            kind: FieldKind::Register,
            personal: false,
            role,
            default: None,
            derived: None,
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
    fn a_title_reads_as_a_rename_and_speaks_of_the_new_name() {
        let title = field("title", Ty::Text, Some(FieldRole::Title));
        assert_eq!(
            reads(
                &title,
                &shown("text", Value::text("Milk")),
                &shown("text", Value::text("Oat milk"))
            ),
            ("tilolang-did-renamed", "after", false)
        );
    }

    #[test]
    fn a_body_reads_as_an_edit_of_it_and_speaks_of_no_value() {
        let body = field("notes", Ty::RichText, Some(FieldRole::Body));
        assert_eq!(
            reads(
                &body,
                &shown("rich", Value::Unit),
                &shown("rich", Value::Unit)
            ),
            ("tilolang-did-edited", "", true)
        );
    }

    #[test]
    fn a_done_flag_reads_as_completed_or_reopened() {
        let done = field("done", Ty::Bool, Some(FieldRole::Done));
        assert_eq!(
            reads(
                &done,
                &shown("bool", Value::Bool(false)),
                &shown("bool", Value::Bool(true))
            ),
            ("tilolang-did-completed", "", false)
        );
        assert_eq!(
            reads(
                &done,
                &shown("bool", Value::Bool(true)),
                &shown("bool", Value::Bool(false))
            ),
            ("tilolang-did-reopened", "", false)
        );
    }

    #[test]
    fn a_day_moved_to_reads_as_moved_and_a_new_one_as_set_and_a_cleared_one_as_cleared() {
        let due = field("due", Ty::Option(Box::new(Ty::Date)), None);
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
    fn a_time_of_day_reads_with_the_day_it_belongs_to() {
        let at = field(
            "at",
            Ty::Option(Box::new(Ty::Time)),
            Some(FieldRole::TimeOf {
                date: "due".to_owned(),
            }),
        );
        let none = shown("none", Value::Unit);
        let at_nine = shown("time", Value::text("2026-11-02T09:00:00Z"));
        assert_eq!(
            reads(&at, &none, &at_nine),
            ("tilolang-did-set-time", "after", true)
        );
        assert_eq!(
            reads(&at, &at_nine, &none),
            ("tilolang-did-cleared-time", "", true)
        );
    }

    #[test]
    fn a_person_reads_as_assigned_to_and_unassigned() {
        let assignee = field("assignee", Ty::Option(Box::new(Ty::Principal)), None);
        let nobody = shown("none", Value::Unit);
        let ben = shown(
            "principal",
            Value::Principal(crate::ids::Principal("ben".to_owned())),
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
        let project = field(
            "project",
            Ty::Option(Box::new(Ty::Record(kind("Project")))),
            None,
        );
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
    fn a_parent_reads_as_moved_under_or_to_the_top_level() {
        let parent = field(
            "parent",
            Ty::Option(Box::new(Ty::Record(kind("Task")))),
            Some(FieldRole::Parent),
        );
        let top = shown("none", Value::Unit);
        let above = shown("resource", Value::text("Write the post"));
        assert_eq!(
            reads(&parent, &top, &above),
            ("tilolang-did-moved-under", "after", false)
        );
        assert_eq!(
            reads(&parent, &above, &top),
            ("tilolang-did-moved-top", "", false)
        );
    }

    #[test]
    fn an_enum_reads_as_set_to_or_cleared_and_so_does_another_flag() {
        let priority = field(
            "priority",
            Ty::Option(Box::new(Ty::Enum(kind("Priority")))),
            None,
        );
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
        let pinned = field("pinned", Ty::Bool, None);
        assert_eq!(
            reads(
                &pinned,
                &shown("bool", Value::Bool(false)),
                &shown("bool", Value::Bool(true))
            ),
            ("tilolang-did-set", "after", true)
        );
    }

    #[test]
    fn a_field_of_no_role_of_its_own_reads_as_a_change_of_it() {
        let tags = field("tags", Ty::Set(Box::new(Ty::Text)), None);
        assert_eq!(
            reads(&tags, &Value::List(Vec::new()), &Value::List(Vec::new())),
            ("tilolang-did-changed", "", true)
        );
    }

    #[test]
    fn words_that_speak_of_a_value_that_stood_not_read_of_nothing() {
        // A resource that stood nowhere before cannot be a value "moved this out of".
        let project = field(
            "project",
            Ty::Option(Box::new(Ty::Record(kind("Project")))),
            None,
        );
        let none = shown("none", Value::Unit);
        assert_eq!(
            reads(&project, &none, &none),
            ("tilolang-did-taken-out", "", false)
        );
    }
}
