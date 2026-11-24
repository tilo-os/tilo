//! The notices an app raises in its record, and what a person does with them: the documents a
//! device with no desktop session shows, the mark that says one was seen, and the action one of
//! them offers, all read and written through the record's own package.

use std::sync::Arc;

use tilo_action_policy::WorkspacePolicy;
use tilo_workspace_service::tl_records::{Package, Raised, Record};
use tilolang_tilo::action::EDIT_FLAG_ACTION;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::tilolang::{
    NOTICE_ACTIONS, NOTICE_BODY, NOTICE_DEVICE, NOTICE_OPENS, NOTICE_SEEN, NOTICE_TITLE, NOTICE_TO,
    NOTICE_TYPE,
};

use crate::records::{Opened, member};

/// `record` opened through `package` as `person` reads it: the host a notice is read and answered
/// on. `package` is the app's as this device has it, `None` for the one this build carries.
///
/// # Errors
/// The app is neither built in nor installed here, or its package does not load.
pub(crate) fn open(
    record: Record,
    policy: Option<Arc<WorkspacePolicy>>,
    person: &str,
    app: &str,
    package: Option<Package>,
) -> Result<Opened, String> {
    let viewer = member(tilo_actions::MemberId::person(person.to_owned()));
    crate::records::opened_with(record, policy, viewer, app, package)
}

/// The notices `opened`, an app's record, holds for `person` that they have not seen: the
/// documents a device of theirs shows.
///
/// # Errors
/// The record's notices cannot be read.
pub(crate) fn raised(opened: &Opened, person: &str) -> Result<Vec<Raised>, String> {
    let runtime = opened.host().runtime();
    let mut out = Vec::new();
    for doc in opened
        .host()
        .store()
        .resources_of(NOTICE_TYPE)
        .map_err(|error| error.to_string())?
    {
        let at = ResourceRef::root(doc.clone(), NOTICE_TYPE.to_owned());
        let to = match runtime.read(&at, NOTICE_TO) {
            Ok(Value::Actor(actor)) => actor
                .member_id()
                .and_then(|member| member.person_id())
                .map(str::to_owned),
            _ => None,
        };
        if to.as_deref() != Some(person) {
            continue;
        }
        if matches!(runtime.read(&at, NOTICE_SEEN), Ok(Value::Bool(true))) {
            continue;
        }
        let text = |field: &str| match runtime.read(&at, field) {
            Ok(Value::Text(text)) => text,
            _ => String::new(),
        };
        let names: Vec<String> = match runtime.read(&at, NOTICE_ACTIONS) {
            Ok(Value::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Text(name) => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        out.push(Raised {
            actions: labelled(opened, &at, &names),
            device: text(NOTICE_DEVICE),
            title: text(NOTICE_TITLE),
            body: text(NOTICE_BODY),
            doc,
        });
    }
    Ok(out)
}

/// The actions `names` the notice `at` offers, each with the words the app that keeps what it
/// opens labels it by: one the app does not declare keeps the name it was offered by.
fn labelled(opened: &Opened, at: &ResourceRef, names: &[String]) -> Vec<(String, String)> {
    let package = match opened.host().runtime().read(at, NOTICE_OPENS) {
        Ok(Value::Ref(reference)) => tilolang_tilo::schema::lookup(&reference.target.ty)
            .and_then(|schema| opened.host().package(&schema.app)),
        _ => None,
    };
    names
        .iter()
        .map(|name| {
            let label = package
                .as_ref()
                .and_then(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.name == *name)
                })
                .map_or_else(String::new, |action| action.label.clone());
            let label = if label.is_empty() {
                name.clone()
            } else {
                label
            };
            (name.clone(), label)
        })
        .collect()
}

/// `person` saw the notice `doc` — they dismissed it, chose on it, or its time there ran out — so
/// it is marked seen in the app's record and no device of theirs raises it again.
///
/// # Errors
/// The record refused the write.
pub(crate) fn seen(opened: &Opened, person: &str, doc: &str) -> Result<(), String> {
    let at = ResourceRef::root(doc.to_owned(), NOTICE_TYPE.to_owned());
    let by = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
    opened
        .host()
        .store()
        .set_register(
            &at,
            NOTICE_SEEN,
            tilolang_tilo::runtime::store::Scalar::Bool(true),
            EDIT_FLAG_ACTION,
            &by,
        )
        .map_err(|error| error.to_string())
}

/// Run `action`, one of the app's actions by the name the notice `doc` offered it by, on what the
/// notice opens, as `person`.
///
/// # Errors
/// The notice opens nothing, the app does not declare the action, or the action was refused.
pub(crate) fn choose(opened: &Opened, person: &str, doc: &str, action: &str) -> Result<(), String> {
    let at = ResourceRef::root(doc.to_owned(), NOTICE_TYPE.to_owned());
    let opens = match opened.host().runtime().read(&at, NOTICE_OPENS) {
        Ok(Value::Ref(reference)) => ResourceRef::clone(&reference.target),
        other => return Err(format!("the notice opens nothing it can run: {other:?}")),
    };
    let schema = tilolang_tilo::schema::lookup(&opens.ty)
        .ok_or_else(|| format!("no app keeps `{}`", opens.ty))?;
    let package = opened
        .host()
        .package(&schema.app)
        .ok_or_else(|| format!("{} is not loaded", schema.app))?;
    let named = package
        .meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .ok_or_else(|| format!("{} declares no action `{action}`", schema.app))?;
    let by = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
    opened
        .host()
        .runtime()
        .invoke_action(&by, (named.id.as_str(), vec![Value::resource(&opens)]))
        .map(drop)
        .map_err(|error| error.to_string())
}
