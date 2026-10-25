//! The values a step's arguments quote or compute (`{{…}}`,
//! [`crate::values`]), filled in from the room's records just before the
//! step runs.

use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;
use crate::planner::{Step, StepAction};
use crate::values::{self, ViewRef};

impl AgentParticipant {
    /// `arguments` of `step`, about to run at `address` on the record at
    /// `route`, with every value they quote or compute filled in: a value
    /// nothing types that is only arithmetic, `{{…}}`, then arithmetic
    /// written in words where a parameter takes a date or a number (`10
    /// business days after 2026-10-14`, `289 - 50`). The
    /// notes the host wrote beside dates and amounts in a read's answer
    /// ([`values::annotate_json`]) come out of what a step copied from it.
    ///
    /// # Errors
    ///
    /// The reference or expression that could not be filled in, named:
    /// the step fails, and the task's repair plans around it.
    pub(super) fn bind_values(
        &mut self,
        step: &Step,
        route: &DocumentRoute,
        address: &ActionAddress,
        mut arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        arguments.values_mut().for_each(without_notes);
        let quoting = arguments.values().any(quotes);
        if !quoting && !arguments.values().any(has_text) {
            return Ok(arguments);
        }
        let params = self
            .survey(route)
            .into_iter()
            .find(|descriptor| descriptor.address == *address)
            .map(|descriptor| descriptor.params)
            .unwrap_or_default();
        // Before `{{…}}` is filled in, so that what a document holds
        // (`10 - 12`, a range) is copied as it is, not worked out.
        values::fill_untyped(&mut arguments, &params);
        if quoting {
            let mut reads = StepReads {
                participant: self,
                app: &step.app,
                route,
            };
            values::bind(&mut arguments, &params, &mut reads)?;
        }
        values::fill_plain(&mut arguments, &params)?;
        Ok(arguments)
    }

    /// The app of the catalogue `named` names: its id, or the last part of
    /// it (`sheets` for `so.tilo.sheets`).
    fn named_app(&self, named: &str) -> Result<String, String> {
        let ids: Vec<&str> = self
            .catalogue
            .iter()
            .map(|definition| definition.app_id)
            .collect();
        if let Some(id) = ids.iter().find(|id| **id == named) {
            return Ok((*id).to_owned());
        }
        let mut by_name = ids.iter().filter(|id| {
            id.rsplit('.')
                .next()
                .is_some_and(|last| last.eq_ignore_ascii_case(named))
        });
        match (by_name.next(), by_name.next()) {
            (Some(id), None) => Ok((*id).to_owned()),
            _ => Err(format!("no app `{named}`")),
        }
    }
}

/// `value` without the notes the host writes beside dates and amounts.
fn without_notes(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(plain) = values::unannotated(text) {
                *text = plain;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(without_notes),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(without_notes),
        _ => {}
    }
}

/// Whether `value` holds text, which a parameter taking a date or a
/// number may compute.
fn has_text(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(items) => items.iter().any(has_text),
        serde_json::Value::Object(fields) => fields.values().any(has_text),
        _ => false,
    }
}

/// Whether `value` holds a `{{…}}` the host fills in.
fn quotes(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => values::binds(text),
        serde_json::Value::Array(items) => items.iter().any(quotes),
        serde_json::Value::Object(fields) => fields.values().any(quotes),
        _ => false,
    }
}

/// The reads one step's references name, run as a planning model's reads
/// are: on records the agent may open, operations that only read.
struct StepReads<'a> {
    participant: &'a mut AgentParticipant,
    /// The step's app and record: a reference to the same app naming no
    /// record reads the record the step acts on.
    app: &'a str,
    route: &'a DocumentRoute,
}

impl values::Reads for StepReads<'_> {
    fn read(&mut self, view: &ViewRef) -> Result<serde_json::Value, String> {
        let app = self.participant.named_app(&view.app)?;
        let route = match &view.record {
            Some(record) => self.participant.open(&app, Some(record))?,
            None if app == self.app => self.route.clone(),
            None => {
                let finding = Step {
                    app: app.clone(),
                    record: None,
                    action: StepAction::Open,
                    carries: None,
                    serves: None,
                };
                self.participant
                    .existing_record(&finding)?
                    .ok_or_else(|| format!("no {app} document is open to read"))?
            }
        };
        let offered = self.participant.survey(&route);
        let address = operation(&offered, &view.operation)?;
        let arguments = view
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
            .collect();
        let output = self.participant.read_now(&route, &address, &arguments)?;
        Ok(output.value.unwrap_or(serde_json::Value::Null))
    }
}

/// The operation `named` names among `offered`: `resource#action` exactly,
/// else the action of the record's own resource, else the one resource
/// offering it.
fn operation(offered: &[ActionDescriptor], named: &str) -> Result<ActionAddress, String> {
    if let Some(address) = tilo_agent::surface::decode_address(named) {
        return offered
            .iter()
            .find(|descriptor| descriptor.address == address)
            .map(|descriptor| descriptor.address.clone())
            .ok_or_else(|| format!("the document publishes no {named}"));
    }
    let with: Vec<&ActionDescriptor> = offered
        .iter()
        .filter(|descriptor| descriptor.address.action == named)
        .collect();
    let own = with
        .iter()
        .find(|descriptor| !descriptor.address.resource.contains('/'));
    match (own, with.as_slice()) {
        (Some(own), _) | (None, [own]) => Ok(own.address.clone()),
        (None, []) => Err(format!("the document has no read `{named}`")),
        (None, several) => Err(format!(
            "`{named}` is on {}: name it as resource#{named}",
            several
                .iter()
                .take(4)
                .map(|descriptor| descriptor.address.resource.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}
