//! Profiling the session on the device admin's request.
//!
//! The image keeps debuggers and `perf` off every process
//! (`kernel.yama.ptrace_scope = 3`, `kernel.perf_event_paranoid = 3`), the
//! session included, and that hardening stays. So the session profiles itself
//! with the instruments it already has, for a window the admin asks for
//! through this resource: [`tilo_frame_metrics::profile`] records the loop's
//! frames, phases and threads, and completes itself when the window is over.
//!
//! The resource also reads what the session's heap is doing
//! ([`tilo_compositor::heap_trim`]): whether a trim a close asked for is still
//! owed, how many ran, and whether startup's trim has run. The diagnostics UI
//! and a person's own scripts read the same resource, so there is one path to
//! what the session knows about its own cost.

use serde_json::{Map, Value, json};
use tilo_actions::{
    ActionAddress, ActionOutput, ActionParam, ActionSpec, DoorError, EncodedAction, OperationKind,
    ParamType, ResourceSpec,
};
use tilo_service::{Service, Started};

/// The resource the session's diagnostics are published under.
pub const RESOURCE: &str = "session-diagnostics";

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = "session.diagnostics";

/// [`RESOURCE`] with its actions.
fn resource() -> ResourceSpec {
    let result = tilo_actions::ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    let mut start = ActionSpec::typed_operation(
        "profile-start",
        "Profile the session's frames",
        "Records the loop's frames, phases and threads for a few seconds, without a restart.",
        OperationKind::Effect,
        KIND,
        vec![ActionParam::optional_owned(
            "seconds",
            ParamType::Integer,
            "How long to record, 1 to 600 seconds (default 30)",
        )],
        |_| EncodedAction {
            type_id: KIND.into(),
            payload: b"profile-start".to_vec(),
        },
    );
    start.type_mut().contract.result = result.clone();
    let mut stop = ActionSpec::typed_operation(
        "profile-stop",
        "Stop profiling the session",
        "Ends the recording profile early and completes its files.",
        OperationKind::Effect,
        KIND,
        Vec::new(),
        |_| EncodedAction {
            type_id: KIND.into(),
            payload: b"profile-stop".to_vec(),
        },
    );
    stop.type_mut().contract.result = result;
    ResourceSpec::new(RESOURCE, KIND, "Session diagnostics")
        .with_action(start)
        .with_action(stop)
}

/// The session's diagnostics as one resource: the profiler and the heap trim.
pub(crate) struct Served;

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            json!({
                "profile": tilo_frame_metrics::profile::status(),
                "heap_trim": {
                    "pending": tilo_compositor::heap_trim::close_trim_pending(),
                    "trims": tilo_compositor::heap_trim::trims_completed(),
                    "startup_trimmed": tilo_compositor::heap_trim::startup_trimmed(),
                },
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let profile = match action {
            "profile-start" => profile_start(arguments)?,
            "profile-stop" => tilo_frame_metrics::profile::stop(),
            other => {
                return Err(DoorError::Unknown(ActionAddress::new(resource, other)));
            }
        };
        Ok(Started::Done(ActionOutput {
            value: Some(json!({ "profile": profile })),
            resources: Vec::new(),
        }))
    }
}

/// Start recording a profile of the session for `seconds`, into the session's
/// profile directory.
fn profile_start(arguments: &Map<String, Value>) -> Result<Value, DoorError> {
    use tilo_frame_metrics::profile;
    let seconds = match arguments.get("seconds") {
        None => profile::DEFAULT_SECONDS,
        Some(value) => value
            .as_u64()
            .filter(|seconds| (1..=profile::MAX_SECONDS).contains(seconds))
            .ok_or_else(|| {
                DoorError::InvalidArguments(format!("`seconds` is 1 to {}", profile::MAX_SECONDS))
            })?,
    };
    let root = profile::default_root().ok_or_else(|| DoorError::Unavailable {
        code: "no-state-dir".into(),
        reason: "there is no state directory to keep the profile in".into(),
    })?;
    profile::start(&root, std::time::Duration::from_secs(seconds)).map_err(|error| match error {
        profile::StartError::AlreadyRecording(path) => DoorError::Unavailable {
            code: "already-recording".into(),
            reason: format!("a profile is already recording into {}", path.display()),
        },
        profile::StartError::Io(error) => DoorError::Failed(format!(
            "the profile directory could not be created: {error}"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resource_offers_the_profiler_and_reads_what_the_session_knows_of_its_cost() {
        let served = Served;
        let resources = served.resources();
        assert_eq!(resources.len(), 1);
        let resource = &resources[0];
        assert_eq!(resource.id.0, RESOURCE);
        let actions: Vec<&str> = resource
            .actions
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        assert_eq!(actions, ["profile-start", "profile-stop"]);
        // The reading: the profiler's state and the heap trim's, whether or
        // not a trim ever ran in this process.
        let reading = served.read(RESOURCE).expect("the resource's reading");
        assert_eq!(reading["profile"]["state"], "idle");
        assert!(reading["heap_trim"]["pending"].is_boolean());
        assert!(reading["heap_trim"]["trims"].is_u64());
        assert!(reading["heap_trim"]["startup_trimmed"].is_boolean());
        assert!(served.read("session:devices").is_none());
    }

    #[test]
    fn a_profiles_length_is_one_to_ten_minutes() {
        for seconds in [json!(0), json!(601), json!("ten"), json!(-1)] {
            let arguments = json!({ "seconds": seconds });
            assert!(
                matches!(
                    profile_start(arguments.as_object().unwrap()),
                    Err(DoorError::InvalidArguments(_))
                ),
                "{seconds}"
            );
        }
        assert!(matches!(
            Served.invoke(RESOURCE, "profile-elsewhere", &Map::new()),
            Err(DoorError::Unknown(_))
        ));
    }
}
