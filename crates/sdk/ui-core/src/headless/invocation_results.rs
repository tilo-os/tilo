//! Opt-in retention of the actual semantic door outcomes, never its input secrets.
use super::FrameHarness;
use crate::widget::ResourceOperationStart;
use serde::Serialize;
use std::io::{self, Write};
use tilo_actions::{ActionAddress, ActionOutput, DoorError};

const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Completed { output: ActionOutput },
    Accepted,
    Refused { code: String, message: String },
}
#[derive(Serialize)]
struct Entry {
    address: ActionAddress,
    #[serde(flatten)]
    outcome: Outcome,
}
#[derive(Serialize)]
pub(super) struct InvocationResults {
    schema_version: u8,
    author: String,
    entries: Vec<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_error: Option<String>,
    #[serde(skip)]
    remaining: usize,
}
impl InvocationResults {
    fn new(author: String) -> Self {
        Self {
            schema_version: 1,
            author,
            entries: Vec::new(),
            retention_error: None,
            remaining: MAX_BYTES.saturating_sub(1024),
        }
    }
    fn record(
        &mut self,
        address: &ActionAddress,
        result: &Result<ResourceOperationStart, DoorError>,
    ) -> Result<(), String> {
        if let Some(error) = &self.retention_error {
            return Err(error.clone());
        }
        let message = result.as_ref().err().map(ToString::to_string);
        let borrowed = BorrowedEntry {
            address,
            outcome: match result {
                Ok(ResourceOperationStart::Completed(output)) => {
                    BorrowedOutcome::Completed { output }
                }
                Ok(ResourceOperationStart::Accepted) => BorrowedOutcome::Accepted,
                Err(error) => BorrowedOutcome::Refused {
                    code: error.code(),
                    message: message.as_deref().unwrap_or_default(),
                },
            },
        };
        let mut counter = Limited {
            remaining: self.remaining,
        };
        if self.entries.len() >= MAX_ENTRIES
            || serde_json::to_writer(&mut counter, &borrowed).is_err()
        {
            let message = format!(
                "invocation result retention limit reached after action {}; its actual result is not retained",
                self.entries.len().saturating_add(1)
            );
            self.retention_error = Some(message.clone());
            return Err(message);
        }
        let outcome = match result {
            Ok(ResourceOperationStart::Completed(output)) => Outcome::Completed {
                output: output.clone(),
            },
            Ok(ResourceOperationStart::Accepted) => Outcome::Accepted,
            Err(error) => Outcome::Refused {
                code: error.code().into(),
                message: error.to_string(),
            },
        };
        let entry = Entry {
            address: address.clone(),
            outcome,
        };
        self.remaining = counter.remaining.saturating_sub(1);
        self.entries.push(entry);
        Ok(())
    }
}
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum BorrowedOutcome<'a> {
    Completed { output: &'a ActionOutput },
    Accepted,
    Refused { code: &'a str, message: &'a str },
}
#[derive(Serialize)]
struct BorrowedEntry<'a> {
    address: &'a ActionAddress,
    #[serde(flatten)]
    outcome: BorrowedOutcome<'a>,
}

struct Limited {
    remaining: usize,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("result limit exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl FrameHarness {
    /// Retain outcomes from subsequent explicit scenario invocations. Inputs and
    /// confirmation phrases are excluded; returned data is retained only on request.
    pub fn retain_invocation_results(&mut self) {
        self.invalidate_local();
        self.invocation_results = Some(InvocationResults::new(self.author.0.clone()));
    }
    /// Serialize retained actual Completed/Accepted/Refused outcomes in invocation order.
    /// Accepted means only admission, never successful completion of a background job.
    ///
    /// # Errors
    /// Fails when recording was not enabled or serialization fails.
    pub fn invocation_results_json(&self) -> Result<String, String> {
        let results = self
            .invocation_results
            .as_ref()
            .ok_or("invocation result retention is disabled")?;
        serde_json::to_writer(
            &mut Limited {
                remaining: MAX_BYTES,
            },
            results,
        )
        .map_err(|error| format!("retained invocation envelope exceeds its limit: {error}"))?;
        serde_json::to_string(results).map_err(|error| error.to_string())
    }
    pub(super) fn retain_invocation_outcome(
        &mut self,
        address: &ActionAddress,
        result: &Result<ResourceOperationStart, DoorError>,
    ) -> Result<(), String> {
        if let Some(results) = &mut self.invocation_results {
            results.record(address, result)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{Action, Invocation};
    use serde_json::{Value, json};

    fn frame() -> FrameHarness {
        FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Text::new("results-fixture")),
            (640, 480),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        )
    }
    fn invoke(address: ActionAddress, arguments: Value, expected: Option<&str>) -> Action {
        Action::Invoke(Invocation {
            resource: address.resource,
            action: address.action,
            args: arguments.as_object().unwrap().clone(),
            confirm: false,
            confirmation_phrase: Some("private confirmation".into()),
            expect_error: expected.map(str::to_owned),
            bind: None,
        })
    }
    fn unknown() -> Action {
        invoke(
            ActionAddress::new("missing", "action"),
            json!({"secret":"private arguments"}),
            Some("unknown_action"),
        )
    }
    #[test]
    fn expected_refusals_retain_actor_and_address_without_input_secrets() {
        let mut frame = frame();
        assert!(frame.invocation_results_json().is_err());
        frame.retain_invocation_results();
        frame.perform(&unknown()).unwrap();
        let raw = frame.invocation_results_json().unwrap();
        let result: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(result["author"], "fixture");
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["entries"][0]["address"]["resource"], "missing");
        assert_eq!(result["entries"][0]["status"], "refused");
        assert_eq!(result["entries"][0]["code"], "unknown_action");
        assert!(!raw.contains("private arguments"));
        assert!(!raw.contains("private confirmation"));
    }
    #[test]
    fn real_background_admission_is_not_relabelled_completed() {
        let owner = crate::application_test_support::ProbeHost::new();
        let (component, observed) =
            crate::widget::control_result_probe::component(&owner, 17, true);
        let mut frame = FrameHarness::from_component(
            component,
            (640, 480),
            crate::application_test_support::runner_inputs(),
        )
        .unwrap();
        let address = tilo_actions::door::survey(&frame.resources())
            .into_iter()
            .find(|action| action.address.action == "run")
            .expect("real component publishes its action")
            .address;
        frame.retain_invocation_results();
        frame.perform(&invoke(address, json!({}), None)).unwrap();
        assert_eq!(observed.calls.get(), 1);
        let result: Value =
            serde_json::from_str(&frame.invocation_results_json().unwrap()).unwrap();
        assert_eq!(result["entries"][0]["status"], "accepted");
        assert!(result["entries"][0].get("output").is_none());
    }
    #[test]
    fn retention_byte_failure_preserves_prefix_and_does_not_hide_executed_search() {
        let mut frame = frame();
        frame.retain_invocation_results();
        frame.perform(&unknown()).unwrap();
        // Exercise exhausted recording through the real semantic operation,
        // rather than fabricate a refused outcome after the operation ran.
        frame.invocation_results.as_mut().unwrap().remaining = 0;
        assert!(
            frame
                .perform(&invoke(crate::search::search_address(), json!({}), None))
                .is_err()
        );
        assert!(frame.action_palette.is_some(), "the operation really ran");
        // A caller may ignore the first error. It cannot silently resume the
        // journal after an omitted invocation and turn the log into a prefix.
        frame.invocation_results.as_mut().unwrap().remaining = MAX_BYTES;
        assert!(frame.perform(&unknown()).is_err());
        let result: Value =
            serde_json::from_str(&frame.invocation_results_json().unwrap()).unwrap();
        assert_eq!(result["entries"].as_array().unwrap().len(), 1);
        assert!(
            result["retention_error"]
                .as_str()
                .unwrap()
                .contains("action 2")
        );
    }
    #[test]
    fn entry_limit_fails_the_next_scenario_step_with_a_retained_prefix() {
        let mut frame = frame();
        frame.retain_invocation_results();
        for _ in 0..MAX_ENTRIES {
            frame.perform(&unknown()).unwrap();
        }
        assert!(frame.perform(&unknown()).is_err());
        let result: Value =
            serde_json::from_str(&frame.invocation_results_json().unwrap()).unwrap();
        assert_eq!(result["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        assert!(result["retention_error"].is_string());
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn failed_real_startup_control_keeps_an_empty_explicit_invocation_envelope() {
        let owner = crate::application_test_support::ProbeHost::new();
        let (component, observed) =
            crate::widget::control_result_probe::component(&owner, 18, false);
        let mut frame = FrameHarness::from_component(
            component,
            (640, 480),
            crate::application_test_support::runner_inputs(),
        )
        .unwrap();
        observed.enqueue("missing");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("results.json");
        let spec = crate::headless::Spec {
            results: Some(path.to_str().unwrap().to_owned()),
            ..Default::default()
        };
        let failure = crate::headless::drive_spec_actions(&spec, &mut frame, |_| {});
        assert!(
            failure
                .unwrap()
                .contains("startup control invocation failed")
        );
        assert_eq!(observed.calls.get(), 0);
        crate::headless::write_spec_outputs(&spec, &mut frame).unwrap();
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(result["entries"], serde_json::json!([]));
    }
}
