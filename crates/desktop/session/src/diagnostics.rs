//! Profiling the session on the device admin's request.
//! The image keeps debuggers and `perf` off every process (`kernel.yama.ptrace_scope = 3`,
//! `kernel.perf_event_paranoid = 3` ), the session included, and that hardening stays. So the
//! session profiles itself with the instruments it already has, for a window the admin asks for
//! through this resource: [`tilo_frame_metrics::profile`] records the loop's frames, phases and
//! threads, and completes itself when the window is over.
//! The resource also reports what the session's heap is doing ([`tilo_compositor::heap_trim`]):
//! whether a trim a close asked for is still owed, how many ran, and whether startup's trim has
//! run.
//! It is published on the session's peer network like the shell and the session's controls, so the action
//! model lists it: `tilo action list session-diagnostics` , an agent through the broker,
//! and the test-control socket all reach the same verbs, and `profile-status` answers the same
//! reading the owner's `profile_status` request does.

use serde_json::{Map, Value, json};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_session_core::surfaces::Published;
use tilo_ui_base::host_operation::ResourceOperationStart;

/// The resource the session's diagnostics are published under.
pub const RESOURCE: &str = "session-diagnostics";

/// The session's diagnostics as a declared type: the profiler's verbs and its reading.
#[derive(tilo_resource::Row)]
#[row(kind = "session.diagnostics", label = "Session diagnostics")]
pub struct Diagnostics;

/// What a person, a script or an agent asks the session's own cost.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.diagnostics")]
#[expect(
    clippy::enum_variant_names,
    reason = "the vocabulary is the profiler's three verbs, and each is named for what it does"
)]
pub enum DiagnosticsEffect {
    /// Profile the session's frames
    ///
    /// Profiles the loop's frames, phases and threads for a few seconds, without a restart.
    #[effect(kind = "outside", returns, label = "session-diagnostics-profile-start")]
    ProfileStart {
        /// How long to profile, 1 to 600 seconds (default 30)
        #[param("How long to profile, 1 to 600 seconds (default 30)")]
        seconds: Option<u64>,
    },
    /// Stop profiling the session
    ///
    /// Ends the recording profile early and completes its files.
    #[effect(kind = "outside", returns, label = "session-diagnostics-profile-stop")]
    ProfileStop,
    /// What the session's profiler and heap are doing
    ///
    /// The profile going now, the last one finished, and the heap trims owed and done.
    #[effect(
        view,
        kind = "undoable",
        returns,
        label = "session-diagnostics-profile-status"
    )]
    ProfileStatus,
}

/// [`RESOURCE`] with its actions.
fn resource() -> ResourceSpec {
    tilo_resource::offer::<DiagnosticsEffect>(tilo_resource::typed_resource::<Diagnostics>(
        RESOURCE,
        crate::i18n::t("session-resource-diagnostics"),
    ))
}

/// What the session knows of its own cost now: the profiler's state and the heap trim's. One
/// reading for every caller that asks.
#[must_use]
pub fn reading() -> Value {
    json!({
        "profile": tilo_frame_metrics::profile::status(),
        "heap_trim": {
            "pending": tilo_compositor::heap_trim::close_trim_pending(),
            "trims": tilo_compositor::heap_trim::trims_completed(),
            "startup_trimmed": tilo_compositor::heap_trim::startup_trimmed(),
        },
    })
}

/// The session's diagnostics as a surface on its peer network: the profiler, its reading, and the heap
/// trim.
pub(crate) struct Served;

impl Published for Served {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn revision(&self) -> Option<u64> {
        // What it offers never changes; what it answers does, and is read when asked.
        Some(0)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        if address.resource != RESOURCE {
            return Err(ApiError::Unknown(address.clone()));
        }
        let answered = match address.action.as_str() {
            "profile-start" => profile_start(arguments)?,
            "profile-stop" => json!({ "profile": tilo_frame_metrics::profile::stop() }),
            "profile-status" => reading(),
            _ => return Err(ApiError::Unknown(address.clone())),
        };
        Ok(ResourceOperationStart::Completed(ActionOutput {
            value: Some(answered),
            resources: Vec::new(),
        }))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Start recording a profile of the session for `seconds` , into the session's profile directory.
fn profile_start(arguments: &Map<String, Value>) -> Result<Value, ApiError> {
    use tilo_frame_metrics::profile;
    let seconds = match arguments.get("seconds") {
        None => profile::DEFAULT_SECONDS,
        Some(value) => value
            .as_u64()
            .filter(|seconds| (1..=profile::MAX_SECONDS).contains(seconds))
            .ok_or_else(|| {
                ApiError::InvalidArguments(format!("`seconds` is 1 to {}", profile::MAX_SECONDS))
            })?,
    };
    let root = profile::default_root().ok_or_else(|| ApiError::Unavailable {
        code: "no-state-dir".into(),
        reason: "there is no state directory to keep the profile in".into(),
    })?;
    let profile = profile::start(&root, std::time::Duration::from_secs(seconds)).map_err(
        |error| match error {
            profile::StartError::AlreadyRecording(path) => ApiError::Unavailable {
                code: "already-recording".into(),
                reason: format!("a profile is already recording into {}", path.display()),
            },
            profile::StartError::Io(error) => ApiError::Failed(format!(
                "the profile directory could not be created: {error}"
            )),
        },
    )?;
    Ok(json!({ "profile": profile }))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_diagnostics_resource_offers_a_bounded_profiler;

        phase the_resource_offers_the_profiler_and_reports_what_the_session_knows_of_its_cost;
        {
            let mut served = Served;
            let resources = served.resources();
            assert_eq!(resources.len(), 1);
            let resource = &resources[0];
            assert_eq!(resource.id.0, RESOURCE);
            let actions: Vec<&str> = resource
                .actions
                .iter()
                .map(|spec| spec.id.as_str())
                .collect();
            assert_eq!(actions, ["profile-start", "profile-stop", "profile-status"]);
            // The reading: the profiler's state and the heap trim's, whether or not a trim ever ran in
            // this process.
            let reading = reading();
            assert_eq!(reading["profile"]["state"], "idle");
            assert!(reading["heap_trim"]["pending"].is_boolean());
            assert!(reading["heap_trim"]["trims"].is_u64());
            assert!(reading["heap_trim"]["startup_trimmed"].is_boolean());
            let status = served
                .invoke(
                    &ActionAddress::new(RESOURCE, "profile-status"),
                    &Map::new(),
                    &mut tilo_session_core::surfaces::Look::none(),
                )
                .expect("the reading");
            let ResourceOperationStart::Completed(output) = status else {
                panic!("a reading answers at once");
            };
            assert_eq!(output.value.expect("a value")["profile"]["state"], "idle");
        }

        phase a_profiles_length_is_one_to_ten_minutes;
        {
            for seconds in [json!(0), json!(601), json!("ten"), json!(-1)] {
                let arguments = json!({ "seconds": seconds });
                assert!(
                    matches!(
                        profile_start(arguments.as_object().unwrap()),
                        Err(ApiError::InvalidArguments(_))
                    ),
                    "{seconds}"
                );
            }
            assert!(matches!(
                Served.invoke(
                    &ActionAddress::new(RESOURCE, "profile-elsewhere"),
                    &Map::new(),
                    &mut tilo_session_core::surfaces::Look::none()
                ),
                Err(ApiError::Unknown(_))
            ));
            assert!(matches!(
                Served.invoke(
                    &ActionAddress::new("session:devices", "profile-stop"),
                    &Map::new(),
                    &mut tilo_session_core::surfaces::Look::none()
                ),
                Err(ApiError::Unknown(_))
            ));
        }
    }
}
