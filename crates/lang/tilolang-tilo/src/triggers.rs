//! Triggers: a package's declaration that one of its actions runs by itself, every so often or
//! at the time a document's field names, for each of its documents of a type.
//!
//! The host runs a trigger as the app's own invocation of its action ([`crate::authority`]'s
//! `trigger:` actor), with the app's authority and declared effects. Each run is the workspace's
//! once, on the device whose turn it is ([`crate::resources::ResourceStore::take_turn`]), named by
//! the trigger, the document and the instant ([`Due::run`]); the change it makes notes on the
//! document what the trigger last did for it
//! ([`crate::resources::ResourceStore::triggered`]): the period it ran in, or the instant it
//! fired at. A second run of the same period or instant is not made, and a run a device missed
//! while everything was off runs once when one comes up.

use std::collections::BTreeSet;

use serde_json::Value as Json;
use tilolang::calendar::instant;
use tilolang::pred::{CmpOp, Lit, Operand};
use tilolang_runtime::InvokeOutcome;
use tilolang_runtime::meta::{FieldKind, TriggerArgMeta, TriggerMeta, TriggerWhenMeta};
use tilolang_runtime::store::{FieldRead, FieldSpec, FieldValue, Pred, Scalar, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;
use crate::resources::Watch;

/// When the packages' triggers next have something to run, as last worked out: a frame looks
/// for what is due only when the store moved since, a package loaded, or that time came, so a
/// frame that brings none of them reads no index for its triggers.
pub(crate) struct Schedule {
    /// What the store moved since the schedule was last worked out.
    moved: Watch,
    /// The next instant a trigger has something to run, once worked out.
    next: std::cell::Cell<Next>,
    /// The runs found done in the workspace that are still due here: a run that changed nothing
    /// notes nothing on its document, so it is not asked for again while it stays due.
    done: std::cell::RefCell<BTreeSet<String>>,
}

/// How long a run that waits for its turn waits before it is asked for again, in seconds.
const TURN_AGAIN_S: i64 = 5;

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
            done: std::cell::RefCell::default(),
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
    let (ran, waiting) = run(host, schedule, now);
    let next = next(host, now);
    schedule.set(if waiting {
        let again = now.saturating_add(TURN_AGAIN_S);
        Some(next.map_or(again, |next| next.min(again)))
    } else {
        next
    });
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
/// each document of the type it runs for, as the app's own invocation, where this device takes
/// its turn. Returns how many ran, and whether a run waits for its turn. A run that is refused or
/// fails leaves its document as it is, to be tried again the next time.
fn run(host: &Host, schedule: &Schedule, now: i64) -> (usize, bool) {
    let mut ran: usize = 0;
    let mut waiting = false;
    let due = due(host, now);
    schedule
        .done
        .borrow_mut()
        .retain(|run| due.iter().any(|due| due.run() == *run));
    for due in due {
        if host.package(&due.app).is_none() {
            continue;
        }
        let run = due.run();
        if schedule.done.borrow().contains(&run) {
            continue;
        }
        let address = format!("{}/{}#{}", due.app, due.triggering.doc, due.action);
        let inputs = serde_json::json!({ "run": run });
        match host.store().take_turn(&address, &inputs) {
            crate::resources::Turn::Take => {}
            crate::resources::Turn::Done(_) => {
                schedule.done.borrow_mut().insert(run);
                continue;
            }
            crate::resources::Turn::Later => {
                waiting = true;
                continue;
            }
        }
        let by = crate::runtime::Actor::App(due.app.as_str().into());
        // That it ran is noted with what it did, as one change.
        host.store().mark_triggered_in_run(
            &run,
            &due.target,
            &due.triggering.key,
            &due.triggering.value,
        );
        let outcome =
            host.runtime()
                .invoke_action_named(&run, &by, (due.action.as_str(), due.args));
        host.store().drop_run_marks(&run);
        match outcome {
            Ok(InvokeOutcome::Done(answer)) => {
                ran = ran.saturating_add(1);
                host.store()
                    .turn_done(&address, &inputs, answer_json(&answer));
                schedule.done.borrow_mut().insert(run);
            }
            other => {
                // The instant is the run's whether it was carried out or not: a run that failed
                // leaves its document as it is and is not made again for the same instant, which
                // would run it on every frame and never rest. The next instant the document
                // names rings again.
                schedule.done.borrow_mut().insert(run);
                tracing::warn!(
                    app = due.app,
                    trigger = due.name,
                    ?other,
                    "a trigger did not run"
                );
            }
        }
    }
    (ran, waiting)
}

/// The next instant at which a trigger of `host`'s packages has something to run: the start of
/// the next period an `every` trigger covers, or the time a document's field names while that is
/// still to come; `now` itself where something is due already. `None` when no trigger will come
/// due again.
///
/// A due trigger is part of the answer because this and [`run_due`] share one schedule: a window
/// asks when it must wake before the frame that runs what is due, and an instant already come
/// must not read as "never", or the run is skipped until the next change.
///
/// The time a document's field names is the first of the type's when the index keeps its column,
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
                        .resources_of(&trigger.ty)
                        .unwrap_or_default()
                        .into_iter()
                        .next();
                    first.map(|_| {
                        now.div_euclid(*secs)
                            .saturating_add(1)
                            .saturating_mul(*secs)
                    })
                }
                TriggerWhenMeta::Every(_) => None,
                TriggerWhenMeta::At(field) => {
                    if due_at(host, trigger, field, now) {
                        Some(now)
                    } else {
                        later(host, &trigger.ty, field, now)
                            .and_then(|doc| time_of(host, &doc, field).filter(|at| *at > now))
                    }
                }
            };
            if let Some(at) = at {
                next = Some(next.map_or(at, |held| held.min(at)));
            }
        }
    }
    next
}

/// Whether a document of the trigger's type has its time in `field` come at `now` and the
/// trigger last ran at another instant: what [`due`] runs in its next pass.
fn due_at(host: &Host, trigger: &TriggerMeta, field: &str, now: i64) -> bool {
    come(host, &trigger.ty, field, now).into_iter().any(|doc| {
        time_of(host, &doc, field).is_some_and(|at| {
            let mark = at.to_string();
            host.store().triggered(&doc, &trigger.id).as_deref() != Some(mark.as_str())
        })
    })
}

/// The first document of the type `ty` whose time in `field` is after `now`, as the index keeps it.
fn later(host: &Host, ty: &str, field: &str, now: i64) -> Option<ResourceRef> {
    let pred = time_where(field, CmpOp::Gt, now);
    let out = host.store().first(&ty.to_owned(), &pred, None);
    out.ok().flatten()
}

/// Every document of the type `ty` whose time in `field` has come at `now`, as the index keeps it.
fn come(host: &Host, ty: &str, field: &str, now: i64) -> Vec<ResourceRef> {
    let pred = time_where(field, CmpOp::Le, now);
    let out = host.store().select(&ty.to_owned(), &pred, None);
    out.unwrap_or_default()
}

/// The documents of the type `ty` whose time in `field` compares with the instant `now` by `op`:
/// a range of the field's column, which the index answers without reading the documents.
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
    target: ResourceRef,
    triggering: Triggering,
}

/// What a trigger's run is for: the document, the trigger and the period or instant.
struct Triggering {
    doc: String,
    key: String,
    value: String,
}

impl Due {
    /// The run this is, by name: the trigger, the document it runs for and the instant it fires
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
                    .resources_of(&trigger.ty)
                    .unwrap_or_default()
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
                if host.store().triggered(doc, &trigger.id).as_deref() == Some(value.as_str()) {
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
                    target: doc.clone(),
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

/// A trigger's run that lands on a device at an instant still to come: what the device arms its
/// own wakeup with, so the moment comes there whether or not it hears from the device that runs
/// the trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    /// The trigger and the record, as one wakeup: the same while the record's instant moves, so
    /// a moved instant re-arms the wakeup it had.
    pub key: String,
    /// The app whose trigger it is.
    pub app: String,
    /// The instant, in seconds since the epoch.
    pub at: i64,
    /// What the run tells on this device, each notice under the id of the record the run writes
    /// for it ([`tilolang_runtime::notify::record_id`]): the device that runs the trigger writes
    /// that record, and a device that rang it already shows it no more.
    pub told: Vec<(String, tilolang_runtime::notify::Notice)>,
}

/// What `host`'s triggers that name a device (`at r.when on r.device`) land on `device`, the
/// person `person`'s, after `now`: for each record of the trigger's type whose instant is still
/// to come and whose device field names `device` or none, what the trigger's run would tell
/// there, learned by running it ahead as the run it will be, at its instant, and taking it back
/// ([`tilolang_runtime::Runtime::rehearse_named`]). A run that tells this person nothing on this
/// device lands nothing.
#[must_use]
pub fn landing(host: &Host, device: &str, person: &str, now: i64) -> Vec<Landing> {
    let mut out = Vec::new();
    for package in host.packages() {
        for trigger in &package.meta.triggers {
            let (TriggerWhenMeta::At(field), Some(names)) = (&trigger.when, &trigger.device) else {
                continue;
            };
            let pred = time_where(field, CmpOp::Gt, now);
            let records = host
                .store()
                .select(&trigger.ty, &pred, None)
                .unwrap_or_default();
            for doc in &records {
                let Some(at) = time_of(host, doc, field).filter(|at| *at > now) else {
                    continue;
                };
                let named = text_of(host, doc, names);
                if !named.is_empty() && !named.eq_ignore_ascii_case(device) {
                    continue;
                }
                let value = at.to_string();
                if host.store().triggered(doc, &trigger.id).as_deref() == Some(value.as_str()) {
                    continue;
                }
                let Some(args) = arguments(host, trigger, &package.meta.app, doc) else {
                    continue;
                };
                let due = Due {
                    app: package.meta.app.clone(),
                    name: trigger.name.clone(),
                    action: trigger.action.clone(),
                    args,
                    target: doc.clone(),
                    triggering: Triggering {
                        doc: doc.id_string(),
                        key: trigger.id.clone(),
                        value,
                    },
                };
                let by = crate::runtime::Actor::App(due.app.as_str().into());
                let told = match host.runtime().rehearse_named(
                    &due.run(),
                    &by,
                    (due.action.as_str(), due.args.clone()),
                    &crate::host::iso_utc(at.cast_unsigned()),
                ) {
                    Ok(told) => told,
                    Err(error) => {
                        tracing::warn!(
                            app = due.app,
                            trigger = due.name,
                            %error,
                            "a trigger's moment could not be learned ahead; it is told when it runs"
                        );
                        continue;
                    }
                };
                let told: Vec<_> = told
                    .into_iter()
                    .filter(|(_, notice)| {
                        notice
                            .to
                            .member_id()
                            .and_then(|member| member.person_id())
                            .is_some_and(|told| told.eq_ignore_ascii_case(person))
                            && (notice.device.is_empty()
                                || notice.device.eq_ignore_ascii_case(device))
                    })
                    .collect();
                if told.is_empty() {
                    continue;
                }
                out.push(Landing {
                    key: format!("{}:{}", due.triggering.key, due.triggering.doc),
                    app: due.app,
                    at,
                    told,
                });
            }
        }
    }
    out
}

/// The text the field `field` holds in the document `at`; empty where it holds none.
fn text_of(host: &Host, at: &ResourceRef, field: &str) -> String {
    let spec = FieldSpec {
        id: field.to_owned(),
        kind: FieldKind::Register,
    };
    match host.store().read(at, &spec) {
        Ok(FieldValue::Scalar(Scalar::Text(text) | Scalar::Json(Json::String(text)))) => text,
        _ => String::new(),
    }
}

/// The instant the field `field` holds in the document `at`: `Time`, or an `Option<Time>` with a
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

/// The arguments the trigger passes to its action: the document it runs for, and each root it
/// names.
fn arguments(
    host: &Host,
    trigger: &TriggerMeta,
    app: &str,
    target: &ResourceRef,
) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for arg in &trigger.args {
        match arg {
            TriggerArgMeta::Target => out.push(Value::resource(target)),
            TriggerArgMeta::Root(ty) => {
                let root = host.root_resource(app, ty).ok()?;
                out.push(Value::resource(&root));
            }
        }
    }
    Some(out)
}

/// What a once-only action that ran answered, as every device that asks for it after is given it.
pub(crate) fn answer_json(answer: &Value) -> Json {
    tilolang_runtime::value::to_json(answer).unwrap_or_else(|error| {
        tracing::warn!(%error, "a once-only action answered what JSON cannot hold; kept as nothing");
        Json::Null
    })
}
