//! Triggers: a package's declaration that one of its actions runs by itself, every so often or
//! at the time a record's field names, for each of its records of a type.
//!
//! The host runs a trigger as the app's own invocation of its action ([`crate::authority`]'s
//! `trigger:` principal), on the workspace's origin, with the app's authority and declared
//! effects, and the change records in the same edit what the trigger last did for the record
//! ([`crate::action::TRIGGERS_KEY`]): the period it ran in, or the instant it fired at. A second
//! run of the same period or instant is refused there, so a trigger runs once across the
//! workspace's devices, and a run a device missed while everything was off runs once when one
//! comes up. The run it is is named by the trigger, the record and the instant ([`Due::run`]),
//! so two devices that each come to run that instant before they have met — a workspace whose
//! origin moved — write one notice and queue one effect between them.

use serde_json::Value as Json;
use tilolang::calendar::instant;
use tilolang::pred::{CmpOp, Lit, Operand};
use tilolang_runtime::InvokeOutcome;
use tilolang_runtime::meta::{FieldKind, TriggerArgMeta, TriggerMeta, TriggerWhenMeta};
use tilolang_runtime::store::{FieldRead, FieldSpec, FieldValue, Pred, Scalar, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;
use crate::store::{Triggering, Watch};

/// When the packages' triggers next have something to run, as last worked out: a frame looks
/// for what is due only when the store moved since, a package loaded, or that time came, so a
/// frame that brings none of them reads no index for its triggers.
pub(crate) struct Schedule {
    /// What the store moved since the schedule was last worked out.
    moved: Watch,
    /// The next instant a trigger has something to run, once worked out.
    next: std::cell::Cell<Next>,
}

/// The next instant a trigger has something to run, as a [`Schedule`] holds it.
#[derive(Clone, Copy)]
enum Next {
    /// Not worked out since something moved.
    Unknown,
    /// No trigger will come due again.
    Never,
    At(i64),
}

impl Schedule {
    pub(crate) fn new(moved: Watch) -> Self {
        Self {
            moved,
            next: std::cell::Cell::new(Next::Unknown),
        }
    }

    /// Work the schedule out again: what the packages declare changed.
    pub(crate) fn forget(&self) {
        self.next.set(Next::Unknown);
    }

    /// The next instant worked out, while the store has not moved since.
    fn known(&self) -> Next {
        if !self.moved.take().is_empty() {
            self.next.set(Next::Unknown);
        }
        self.next.get()
    }

    fn set(&self, at: Option<i64>) {
        self.next.set(at.map_or(Next::Never, Next::At));
    }
}

/// [`run`], when something may be due at `now`: the schedule was not worked out since the store
/// last moved, or its next instant came.
pub(crate) fn run_due(host: &Host, schedule: &Schedule, now: i64) -> usize {
    match schedule.known() {
        Next::Never => return 0,
        Next::At(at) if at > now => return 0,
        Next::At(_) | Next::Unknown => {}
    }
    let ran = run(host, now);
    schedule.set(next(host, now));
    ran
}

/// [`next`], as the schedule last worked it out while nothing moved since.
pub(crate) fn next_due(host: &Host, schedule: &Schedule, now: i64) -> Option<i64> {
    match schedule.known() {
        Next::Never => None,
        Next::At(at) => Some(at),
        Next::Unknown => {
            let at = next(host, now);
            schedule.set(at);
            at
        }
    }
}

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
        host.store().set_triggering(Some(due.triggering.clone()));
        let run = due.run();
        let outcome =
            host.runtime()
                .invoke_action_named(&run, &by, (due.action.as_str(), due.args));
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
///
/// The time a record's field names is the first of the type's when the index keeps its column,
/// so a trigger over a large collection costs the pages up to its next instant, not the
/// collection.
#[must_use]
pub fn next(host: &Host, now: i64) -> Option<i64> {
    let mut next: Option<i64> = None;
    for package in host.packages() {
        for trigger in &package.meta.triggers {
            let at = match &trigger.when {
                TriggerWhenMeta::Every(secs) if *secs > 0 => {
                    let first = host
                        .store()
                        .resources_of(&package.meta.app, &trigger.ty)
                        .into_iter()
                        .next();
                    first.map(|_| {
                        now.div_euclid(*secs)
                            .saturating_add(1)
                            .saturating_mul(*secs)
                    })
                }
                TriggerWhenMeta::Every(_) => None,
                TriggerWhenMeta::At(field) => later(host, &trigger.ty, field, now)
                    .and_then(|doc| time_of(host, &doc, field).filter(|at| *at > now)),
            };
            if let Some(at) = at {
                next = Some(next.map_or(at, |held| held.min(at)));
            }
        }
    }
    next
}

/// The first record of the type `ty` whose time in `field` is after `now`, as the index keeps it.
fn later(host: &Host, ty: &str, field: &str, now: i64) -> Option<ResourceRef> {
    let pred = time_where(field, CmpOp::Gt, now);
    let out = host.store().first(&ty.to_owned(), &pred, None);
    out.ok().flatten()
}

/// Every record of the type `ty` whose time in `field` has come at `now`, as the index keeps it.
fn come(host: &Host, ty: &str, field: &str, now: i64) -> Vec<ResourceRef> {
    let pred = time_where(field, CmpOp::Le, now);
    let out = host.store().select(&ty.to_owned(), &pred, None);
    out.unwrap_or_default()
}

/// The records of the type `ty` whose time in `field` compares with the instant `now` by `op`:
/// a range of the field's column, which the index answers without reading the records.
fn time_where(field: &str, op: CmpOp, now: i64) -> Pred {
    Pred::Cmp(
        op,
        Operand::Field(FieldRead {
            spec: FieldSpec {
                id: field.to_owned(),
                kind: FieldKind::Register,
            },
            absent: Lit::Absent,
        }),
        Operand::Lit(Lit::Time(crate::host::iso_utc(now.cast_unsigned()))),
    )
}

/// A trigger's run, as it is about to be made.
struct Due {
    app: String,
    name: String,
    action: String,
    args: Vec<Value>,
    triggering: Triggering,
}

impl Due {
    /// The run this is, by name: the trigger, the record it runs for and the instant it fires
    /// at. A device that comes to run the same one runs the same run, so the notice it tells
    /// and the effects it queues are the ones the device that ran it first made, held once.
    fn run(&self) -> String {
        format!(
            "trigger:{}:{}:{}",
            self.triggering.key, self.triggering.doc, self.triggering.value
        )
    }
}

/// What `host`'s packages have due at `now`, each to run once.
fn due(host: &Host, now: i64) -> Vec<Due> {
    let mut out = Vec::new();
    for package in host.packages() {
        for trigger in &package.meta.triggers {
            let records: Vec<ResourceRef> = match &trigger.when {
                TriggerWhenMeta::Every(secs) if *secs > 0 => host
                    .store()
                    .resources_of(&package.meta.app, &trigger.ty)
                    .into_iter()
                    .map(|doc| ResourceRef::root(doc, trigger.ty.clone()))
                    .collect(),
                TriggerWhenMeta::Every(_) => continue,
                TriggerWhenMeta::At(field) => come(host, &trigger.ty, field, now),
            };
            for doc in &records {
                let at = match &trigger.when {
                    TriggerWhenMeta::Every(secs) if *secs > 0 => {
                        now.div_euclid(*secs).saturating_mul(*secs)
                    }
                    TriggerWhenMeta::Every(_) => continue,
                    // A time still to come is left for the wake that reaches it.
                    TriggerWhenMeta::At(field) => match time_of(host, doc, field) {
                        Some(at) if at <= now => at,
                        _ => continue,
                    },
                };
                let value = at.to_string();
                let id = doc.id_string();
                if host.store().triggered(&id, &trigger.id).as_deref() == Some(value.as_str()) {
                    continue;
                }
                let Some(args) = arguments(host, trigger, &package.meta.app, doc) else {
                    continue;
                };
                out.push(Due {
                    app: package.meta.app.clone(),
                    name: trigger.name.clone(),
                    action: trigger.action.clone(),
                    args,
                    triggering: Triggering {
                        doc: id,
                        key: trigger.id.clone(),
                        value,
                    },
                });
            }
        }
    }
    out
}

/// The instant the field `field` holds in the record `at`: `Time`, or an `Option<Time>` with a
/// time in it.
fn time_of(host: &Host, at: &ResourceRef, field: &str) -> Option<i64> {
    let spec = FieldSpec {
        id: field.to_owned(),
        kind: FieldKind::Register,
    };
    let text = match host.store().read(at, &spec).ok()? {
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
fn arguments(
    host: &Host,
    trigger: &TriggerMeta,
    app: &str,
    subject: &ResourceRef,
) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for arg in &trigger.args {
        match arg {
            TriggerArgMeta::Subject => out.push(Value::resource(subject)),
            TriggerArgMeta::Root(ty) => {
                let root = host.root_record(app, ty).ok()?;
                out.push(Value::resource(&root));
            }
        }
    }
    Some(out)
}
