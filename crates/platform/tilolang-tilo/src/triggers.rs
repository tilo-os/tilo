//! Triggers: a package's declaration that one of its actions runs by itself, every so often or
//! at the time a record's field names, for each of its records of a type.
//!
//! The host runs a trigger as the app's own invocation of its action ([`crate::authority`]'s
//! `trigger:` principal), on the workspace's origin, with the app's authority and declared
//! effects, and the change records in the same edit what the trigger last did for the record
//! ([`crate::action::TRIGGERS_KEY`]): the period it ran in, or the instant it fired at. A second
//! run of the same period or instant is refused there, so a trigger runs once across the
//! workspace's devices, and a run a device missed while everything was off runs once when one
//! comes up.

use serde_json::Value as Json;
use tilolang::calendar::instant;
use tilolang_runtime::InvokeOutcome;
use tilolang_runtime::meta::{FieldKind, TriggerArgMeta, TriggerMeta, TriggerWhenMeta};
use tilolang_runtime::store::{FieldSpec, FieldValue, Scalar, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;
use crate::store::Triggering;

/// Run what `host`'s packages declare due at `now` (seconds since the epoch): each trigger, for
/// each record of the type it runs for, as the app's own invocation. Returns how many ran. A
/// run that is refused or fails leaves its record as it is, to be tried again the next time.
pub fn run(host: &Host, now: i64) -> usize {
    let mut ran: usize = 0;
    for due in due(host, now) {
        if host.package(&due.app).is_none() {
            continue;
        }
        let by = crate::authority::trigger_principal(&due.app);
        host.store().set_triggering(Some(due.triggering));
        let outcome = host
            .runtime()
            .invoke_action(&by, (due.action.as_str(), due.args));
        host.store().set_triggering(None);
        match outcome {
            Ok(InvokeOutcome::Done(_)) => ran = ran.saturating_add(1),
            other => tracing::warn!(
                app = due.app,
                trigger = due.name,
                ?other,
                "a trigger did not run"
            ),
        }
    }
    ran
}

/// The next instant strictly after `now` at which a trigger of `host`'s packages has something
/// to run: the start of the next period an `every` trigger covers, or the time a record's field
/// names while that is still to come. `None` when no trigger will come due again.
#[must_use]
pub fn next(host: &Host, now: i64) -> Option<i64> {
    let mut next: Option<i64> = None;
    for package in host.packages() {
        for trigger in &package.meta.triggers {
            for doc in host.store().resources_of(&package.meta.app, &trigger.ty) {
                let at = match &trigger.when {
                    TriggerWhenMeta::Every(secs) if *secs > 0 => Some(
                        now.div_euclid(*secs)
                            .saturating_add(1)
                            .saturating_mul(*secs),
                    ),
                    TriggerWhenMeta::Every(_) => None,
                    TriggerWhenMeta::At(field) => {
                        time_of(host, &doc, &trigger.ty, field).filter(|at| *at > now)
                    }
                };
                if let Some(at) = at {
                    next = Some(next.map_or(at, |held| held.min(at)));
                }
            }
        }
    }
    next
}

/// A trigger's run, as it is about to be made.
struct Due {
    app: String,
    name: String,
    action: String,
    args: Vec<Value>,
    triggering: Triggering,
}

/// What `host`'s packages have due at `now`, each to run once.
fn due(host: &Host, now: i64) -> Vec<Due> {
    let mut out = Vec::new();
    for package in host.packages() {
        for trigger in &package.meta.triggers {
            for doc in host.store().resources_of(&package.meta.app, &trigger.ty) {
                let at = match &trigger.when {
                    TriggerWhenMeta::Every(secs) if *secs > 0 => {
                        now.div_euclid(*secs).saturating_mul(*secs)
                    }
                    TriggerWhenMeta::Every(_) => continue,
                    // A time still to come is left for the wake that reaches it.
                    TriggerWhenMeta::At(field) => match time_of(host, &doc, &trigger.ty, field) {
                        Some(at) if at <= now => at,
                        _ => continue,
                    },
                };
                let value = at.to_string();
                if host.store().triggered(&doc, &trigger.id).as_deref() == Some(value.as_str()) {
                    continue;
                }
                let Some(args) = arguments(host, trigger, &package.meta.app, &doc) else {
                    continue;
                };
                out.push(Due {
                    app: package.meta.app.clone(),
                    name: trigger.name.clone(),
                    action: trigger.action.clone(),
                    args,
                    triggering: Triggering {
                        doc,
                        key: trigger.id.clone(),
                        value,
                    },
                });
            }
        }
    }
    out
}

/// The instant the field `field` holds in the record `doc`: `Time`, or an `Option<Time>` with a
/// time in it.
fn time_of(host: &Host, doc: &str, ty: &str, field: &str) -> Option<i64> {
    let spec = FieldSpec {
        id: field.to_owned(),
        kind: FieldKind::Register,
    };
    let at = ResourceRef::root(doc.to_owned(), ty.to_owned());
    let text = match host.store().read(&at, &spec).ok()? {
        FieldValue::Scalar(Scalar::Text(text) | Scalar::Json(Json::String(text))) => text,
        FieldValue::Scalar(Scalar::Json(Json::Object(held))) => {
            held.get("some").and_then(Json::as_str)?.to_owned()
        }
        _ => return None,
    };
    instant(&text)
}

/// The arguments the trigger passes to its action: the record it runs for, and each root it
/// names.
fn arguments(host: &Host, trigger: &TriggerMeta, app: &str, doc: &str) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for arg in &trigger.args {
        match arg {
            TriggerArgMeta::Subject => {
                out.push(Value::resource(&ResourceRef::root(
                    doc.to_owned(),
                    trigger.ty.clone(),
                )));
            }
            TriggerArgMeta::Root(ty) => {
                let root = host.root_record(app, ty).ok()?;
                out.push(Value::resource(&root));
            }
        }
    }
    Some(out)
}
