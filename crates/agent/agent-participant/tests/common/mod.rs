//! What the coverage tests share: the catalogue and a generic search for one
//! edit, through the operations a record publishes, in an app the caller
//! knows nothing about.

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test binary uses part of this; a fixture failure is the assertion"
)]

use serde_json::json;
use tilo_actions::door::{ActionDescriptor, InputSpec};
use tilo_actions::{Availability, ConfirmationPolicy, OperationKind, PROMPTED_PARAM, ParamType};
use tilo_agent::surface::Catalogue;
use tilo_agent::tools::{self, Answer, Revealed};
use tilo_agent_participant::{AppCatalogue, PlanModel, StepAction, component_apps};

/// Apps whose state is device-local by design, so no operation changes the
/// shared record: there is nothing for an agent to edit there.
pub const LOCAL_STATE: &[(&str, &str)] = &[
    (
        "so.tilo.camera",
        "the record holds only what this device's camera took and scanned; its preferences are settings",
    ),
    (
        "so.tilo.hex-editor",
        "edits a local file; the record holds nothing until one is opened",
    ),
    (
        "so.tilo.passwords",
        "secrets live in the device's encrypted secret store",
    ),
    (
        "so.tilo.totp",
        "an account's seed lives in the device's secret store; adding one is the device's own",
    ),
];

/// The room the tests' records belong to.
pub const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// The text every edit writes.
pub const WRITTEN: &str = "Written by an agent";

/// Every web app.
pub fn catalogue() -> AppCatalogue {
    AppCatalogue::new(component_apps())
}

/// A record's document as JSON: a page's replica, or the copy an agent's
/// confined record holds.
pub trait Body {
    /// The document, as `AutoSerde` writes it.
    fn body(&self) -> String;
}

fn json(doc: &automerge::AutoCommit) -> String {
    serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
}

impl Body for tilo_agent_participant::Replica {
    fn body(&self) -> String {
        self.read(json).unwrap()
    }
}

impl Body for tilo_agent_participant::ConfinedReplica {
    fn body(&self) -> String {
        self.read(json).unwrap()
    }
}

/// Something an agent can drive: a replica, or a participant's record.
pub trait Driveable {
    /// Every operation the record publishes.
    fn survey(&mut self) -> Vec<ActionDescriptor>;
    /// The record's document, for noticing that it changed.
    fn body(&mut self) -> String;
    fn run(&mut self, action: &StepAction) -> Result<(), String>;
}

/// How an edit was made.
#[derive(Clone, Debug)]
pub struct Edit {
    /// The operation, as the step log names it.
    pub label: String,
    /// The step, replayable.
    pub action: StepAction,
}

/// Arguments a caller with no knowledge of the app would try: a value of
/// each declared type. `None` when a required parameter has a type no
/// generic value fits.
pub fn sample_arguments(
    descriptor: &ActionDescriptor,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut arguments = serde_json::Map::new();
    if descriptor.params.is_empty()
        && let InputSpec::Text { .. } = descriptor.input
    {
        arguments.insert(PROMPTED_PARAM.into(), WRITTEN.into());
    }
    for param in descriptor.params.iter().filter(|param| param.required) {
        let value = if let Some(choice) = param.choices.first() {
            serde_json::Value::from(choice.as_ref())
        } else if let Some(example) = param
            .schema
            .as_ref()
            .and_then(|schema| schema.get("examples"))
            .and_then(serde_json::Value::as_array)
            .and_then(|examples| examples.first())
        {
            // A caller reads constrained input from the published contract,
            // just as a person follows the palette's parameter guidance.
            example.clone()
        } else {
            match param.ty {
                ParamType::Text => WRITTEN.into(),
                ParamType::Integer => 1.into(),
                ParamType::Number => 1.5.into(),
                ParamType::Boolean => true.into(),
                _ => return None,
            }
        };
        arguments.insert(param.name.to_string(), value);
    }
    Some(arguments)
}

/// Available, without a confirmation step, and not an external effect.
pub fn basic(descriptor: &ActionDescriptor) -> bool {
    matches!(descriptor.contract.availability, Availability::Available)
        && matches!(descriptor.contract.confirmation, ConfirmationPolicy::None)
        && descriptor.kind != OperationKind::Effect
}

/// The attempts a caller unfamiliar with the app would make, in order: the
/// record's document operations that need no confirmation, with a value of
/// each declared type.
pub fn candidates(app: &mut impl Driveable) -> Vec<Edit> {
    app.survey()
        .iter()
        .filter(|d| d.kind == OperationKind::Document && basic(d))
        .filter_map(|descriptor| {
            let arguments = sample_arguments(descriptor)?;
            Some(Edit {
                label: format!(
                    "{}#{}",
                    descriptor.address.resource, descriptor.address.action
                ),
                action: StepAction::Invoke {
                    address: descriptor.address.clone(),
                    arguments,
                },
            })
        })
        .collect()
}

/// Try [`candidates`] until one changes the record, surveying again after
/// every attempt: an edit can change what the record publishes. Returns the
/// edit and every attempt before it.
pub fn find_edit(app: &mut impl Driveable, limit: usize) -> (Option<Edit>, Vec<String>) {
    let mut tried = std::collections::HashSet::new();
    let mut refused = Vec::new();
    for _ in 0..limit {
        let Some(edit) = candidates(app)
            .into_iter()
            .find(|edit| !tried.contains(&edit.label))
        else {
            break;
        };
        tried.insert(edit.label.clone());
        let before = app.body();
        match app.run(&edit.action) {
            Ok(()) if app.body() != before => return (Some(edit), refused),
            Ok(()) => refused.push(format!("{}: no change", edit.label)),
            Err(error) => refused.push(format!("{}: {error}", edit.label)),
        }
    }
    (None, refused)
}

/// Chooses the offer sharing the most words with the task (its app,
/// resource and action) and fills its text parameters with the task's
/// quoted phrase. Deterministic, and bound by the same rule as a model: it
/// reveals before it plans.
pub struct KeywordModel;

/// Scratchpad's note field edit: the body replaced by `text`, as the door
/// takes it (`field-edit` with the edit as JSON).
pub fn scratchpad_body(
    text: &str,
) -> (
    tilo_actions::door::ActionAddress,
    serde_json::Map<String, serde_json::Value>,
) {
    let edit = scratchpad::Note::paths().body.replace(text.to_owned());
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        ui::tilo_resource::EDIT_PARAM.into(),
        serde_json::to_value(&edit).expect("a field edit is JSON"),
    );
    (
        tilo_actions::door::ActionAddress::new("scratchpad.note", ui::tilo_resource::FIELD_EDIT),
        arguments,
    )
}

impl PlanModel for KeywordModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        let task = task.to_lowercase();
        let words: Vec<&str> = task
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.len() > 2)
            .collect();
        let quoted = task.split('"').nth(1).unwrap_or_default().to_owned();
        let score = |offer: &&tilo_agent::surface::Offer| {
            let d = &offer.descriptor;
            let text = format!(
                "{} {} {} {} {}",
                offer.app, d.resource_label, d.address.resource, d.action_label, d.address.action
            )
            .to_lowercase();
            let own: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).collect();
            let verb = usize::from(words.contains(&d.address.action.to_lowercase().as_str()));
            words
                .iter()
                .filter(|word| own.contains(word))
                .count()
                .saturating_add(verb.saturating_mul(2))
        };
        let offer = catalogue
            .offers
            .iter()
            .filter(|offer| score(offer) > 0)
            .min_by_key(|offer| std::cmp::Reverse(score(offer)))
            .ok_or("nothing offered fits")?;
        let mut revealed = Revealed::new();
        tools::answer(
            tools::LIST_OPERATIONS,
            &json!({ "resource": offer.address().resource }),
            catalogue,
            &mut revealed,
        );
        let mut arguments = serde_json::Map::new();
        for param in offer.descriptor.params.iter().filter(|p| p.required) {
            arguments.insert(param.name.to_string(), quoted.clone().into());
        }
        tools::read_plan(
            &json!({"steps": [{"operation": offer.id(), "arguments": arguments}], "why": "keyword"}),
            &revealed,
            most_steps,
        )
    }
}

/// The document bodies among what a host retains, as `(readable, sealed)`:
/// a readable one opens without any content key, a sealed one needs a key
/// the host does not have. Governance and other messages are neither.
pub fn retained_documents(bodies: impl IntoIterator<Item = ([u8; 32], Vec<u8>)>) -> (usize, usize) {
    let mut ciphers = tilo_browser_peer::document::Ciphers::new();
    let (mut readable, mut sealed) = (0_usize, 0_usize);
    for (author, body) in bodies {
        match tilo_browser_peer::document::open_frame(&body, author, &mut ciphers) {
            Ok(Some(_)) => readable = readable.saturating_add(1),
            Ok(None) => {}
            Err(_) => sealed = sealed.saturating_add(1),
        }
    }
    (readable, sealed)
}

/// A PDF of one page per line in `pages`, set in Helvetica.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a fixture's object numbers and offsets are small"
)]
pub fn pdf(pages: &[&str]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            (0..pages.len())
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" "),
            pages.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    for (i, line) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        let stream = format!("BT /F1 12 Tf 72 720 Td ({line}) Tj ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}
