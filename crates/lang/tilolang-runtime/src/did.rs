//! What history says a change reads with, worked out from the change itself: the type the field
//! declares, and what it held before and after.
//!
//! One place decides it, so the index store and the Am store read a change the same way.
//!
//! The words are the platform's: [`crate::words::Words`] resolves each key to the reader's
//! language, and the source words beside the key are what a host with no catalogue reads, as
//! the words an action declares for itself (`did:`) are. The words name the field they are of
//! (`$field`), in the case a sentence goes on with ("set priority to").

use crate::registry::{FieldInfo, TypeInfo};
use crate::value::Value;
use tilolang::types::Ty;

/// The arguments a change of `action` recorded (`args`, one per parameter in order) that the
/// words it declares for what it did read ([`tilolang::meta::ActionMeta::did_reads`]), by the
/// parameter's name, each as a plain value: a flag, a number, text, or the name of an enum's
/// case. What a history entry says the change with, and the only arguments it keeps: an
/// argument the words do not read is never shown beside them.
#[must_use]
pub fn did_args(
    package: &tilolang::meta::PackageMeta,
    action: &tilolang::meta::ActionMeta,
    args: &serde_json::Value,
) -> Vec<(String, serde_json::Value)> {
    let reads = action.did_reads();
    let Some(given) = args.as_array() else {
        return Vec::new();
    };
    action
        .params
        .iter()
        .zip(given)
        .filter(|(param, _)| reads.contains(&param.name))
        .filter_map(|(param, value)| {
            let plain = match value {
                serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_) => value.clone(),
                // An enum's case is said by the name its declaration gives it, which the
                // words pick by: the name a change kept beside the case's id, else the name of
                // that id.
                serde_json::Value::Object(fields) => {
                    if let Some(name) = fields.get("case").and_then(serde_json::Value::as_str) {
                        return Some((param.name.clone(), name.into()));
                    }
                    let case = fields.get("variant")?.as_str()?;
                    let name = package
                        .enums
                        .iter()
                        .flat_map(|en| &en.variants)
                        .find(|variant| variant.id == case)
                        .map_or(case, |variant| variant.name.as_str());
                    serde_json::Value::String(name.to_owned())
                }
                _ => return None,
            };
            Some((param.name.clone(), plain))
        })
        .collect()
}

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

/// What a write of the field `field` of a resource of `info` reads with when the field is the one
/// its rows, menus and links show the resource by ([`TypeInfo::name_field`]): it renamed the resource,
/// whatever the field's type. One rule for the sentence history says and the word a write leaves
/// where it landed.
#[must_use]
pub fn renamed(info: &TypeInfo, field: &str) -> Option<Said> {
    info.name_field()
        .filter(|named| named.id == field)
        .map(|_| plain("tilolang-did-renamed", "renamed", Shows::None))
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
        Ty::Actor if gone => plain("tilolang-did-unassigned", "unassigned it", Shows::None),
        Ty::Actor => plain("tilolang-did-assigned", "assigned it to", Shows::After),
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
