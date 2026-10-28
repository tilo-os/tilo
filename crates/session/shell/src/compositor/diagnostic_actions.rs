//! Fixed session diagnostics, available only to the authenticated local owner.
use serde_json::json;
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource};

use crate::shell::launcher::{
    LaunchSession,
    diagnostics::{FIELDS, Options},
};
use crate::shell::test_control::{Command, Response};
use tilo_lock::MutexRecover as _;

pub(crate) const PROVIDER: &str = "session-diagnostics";

pub(super) fn owner_required(command: &Command) -> Option<Response> {
    matches!(command, Command::ActionsInvoke { provider, .. } if provider == PROVIDER).then(|| {
        Response::Err {
            code: "not_permitted".into(),
            message: crate::shell::i18n::t("session-diagnostics-owner-required"),
        }
    })
}

fn resources() -> Vec<Resource> {
    let mut configure = ActionSpec::typed_operation(
        "configure",
        crate::shell::i18n::t("session-diagnostics-configure"),
        "",
        OperationKind::Effect,
        "session-diagnostics/v1",
        FIELDS
            .iter()
            .map(|(field, _)| {
                ActionParam::required_owned(
                    *field,
                    ParamType::Boolean,
                    crate::shell::i18n::t(&format!("session-diagnostics-{field}")),
                )
            })
            .collect(),
        |_| EncodedAction {
            type_id: "session-diagnostics/v1".into(),
            payload: b"configure".to_vec(),
        },
    );
    configure.type_mut().contract.kind = tilo_actions::ActionKind::Outside;
    let mut inspect = ActionSpec::typed_operation(
        "inspect",
        crate::shell::i18n::t("session-diagnostics-inspect"),
        "",
        OperationKind::View,
        "session-diagnostics/v1",
        vec![ActionParam::optional_owned(
            "app_id",
            ParamType::Text,
            crate::shell::i18n::t("session-diagnostics-app-filter"),
        )],
        |_| EncodedAction {
            type_id: "session-diagnostics/v1".into(),
            payload: b"inspect".to_vec(),
        },
    );
    let result = tilo_actions::ResultSpec::Json {
        schema: json!({"type":"object"}),
    };
    let mut profile_start = ActionSpec::typed_operation(
        "profile-start",
        crate::shell::i18n::t("session-diagnostics-profile-start"),
        "",
        OperationKind::Effect,
        "session-diagnostics/v1",
        vec![ActionParam::optional_owned(
            "seconds",
            ParamType::Integer,
            crate::shell::i18n::t("session-diagnostics-profile-seconds"),
        )],
        |_| EncodedAction {
            type_id: "session-diagnostics/v1".into(),
            payload: b"profile-start".to_vec(),
        },
    );
    let mut profile_stop = ActionSpec::typed_operation(
        "profile-stop",
        crate::shell::i18n::t("session-diagnostics-profile-stop"),
        "",
        OperationKind::Effect,
        "session-diagnostics/v1",
        Vec::new(),
        |_| EncodedAction {
            type_id: "session-diagnostics/v1".into(),
            payload: b"profile-stop".to_vec(),
        },
    );
    configure.type_mut().contract.result = result.clone();
    profile_start.type_mut().contract.result = result.clone();
    profile_stop.type_mut().contract.result = result.clone();
    inspect.type_mut().contract.result = result;
    inspect.type_mut().contract.kind = tilo_actions::ActionKind::Read;
    vec![
        Resource::new(
            PROVIDER,
            "session-diagnostics/v1",
            crate::shell::i18n::t("session-diagnostics-title"),
        )
        .with_action(configure)
        .with_action(inspect)
        .with_action(profile_start)
        .with_action(profile_stop),
    ]
}

/// Add the diagnostics rows to a listing's last page, so a caller following
/// `next_cursor` sees them once.
pub(super) fn append_catalogue(response: &mut Response, query: &str) {
    let Response::Answer(answer) = response else {
        return;
    };
    if answer
        .get("next_cursor")
        .is_some_and(|cursor| !cursor.is_null())
    {
        return;
    }
    let rows = console_control::enumerate(PROVIDER, &resources(), query);
    let Some(actions) = answer
        .get_mut("actions")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let added = rows
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .map_or(0, |rows| {
            actions.extend(rows.iter().cloned());
            rows.len()
        });
    let count = actions.len();
    answer["count"] = json!(count);
    if let Some(total) = answer.get("total").and_then(serde_json::Value::as_u64) {
        answer["total"] = json!(total.saturating_add(u64::try_from(added).unwrap_or(u64::MAX)));
    }
}

/// Native control calls this only after admin peer authentication, without
/// Agent scope, while unlocked. No environment or privilege is stored in CRDT.
pub(super) fn dispatch(command: &Command) -> Option<Response> {
    // Answered at once, so there is nothing for `wait_ms` to wait for.
    let Command::ActionsInvoke {
        provider,
        id,
        args,
        device: None,
        wait_ms: _,
    } = command
    else {
        return None;
    };
    if provider != PROVIDER {
        return None;
    }
    let admitted = match console_control::admit(PROVIDER, &resources(), provider, id, args) {
        Ok(admitted) => admitted,
        Err(error) => {
            return Some(Response::Err {
                code: error.code.into(),
                message: error.message,
            });
        }
    };
    match admitted.encoded.payload.as_slice() {
        b"profile-start" => return Some(profile_start(args)),
        b"profile-stop" => {
            return Some(Response::Answer(
                json!({ "profile": super::metrics::profile::stop() }),
            ));
        }
        _ => {}
    }
    LaunchSession::with(|session| {
        if admitted.encoded.payload == b"configure" {
            let flags = FIELDS.map(|(field, _)| {
                args.get(field)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            });
            if flags[3] && !flags[2] {
                return Some(Response::Err {
                    code: "invalid_arguments".into(),
                    message: crate::shell::i18n::t(
                        "session-diagnostics-dispatch-requires-timestamps",
                    ),
                });
            }
            session.configure_diagnostics(Options(flags));
        }
        let flags = session.diagnostic_options();
        let mut answer = serde_json::Map::from_iter([
            ("applies_to".into(), json!("subsequent_launches")),
            ("diagnostics".into(), flags.value()),
            ("live_input_delivery".into(), json!(flags.0[0])),
            ("current_gpu_renderer".into(), json!("unchanged")),
        ]);
        if admitted.encoded.payload == b"inspect" {
            let mut history = session.launch_history.lock_recover().value();
            if let Some(app) = args.get("app_id").and_then(serde_json::Value::as_str)
                && let Some(attempts) = history
                    .get_mut("attempts")
                    .and_then(serde_json::Value::as_array_mut)
            {
                attempts.retain(|attempt| {
                    attempt
                        .get("package_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(app)
                });
            }
            answer.insert("launch_history".into(), history);
            answer.insert("profile".into(), super::metrics::profile::status());
        }
        Some(Response::Answer(serde_json::Value::Object(answer)))
    })
}

/// Start profiling the compositor for `seconds` (default
/// [`profile::DEFAULT_SECONDS`]) into the session's profile directory.
fn profile_start(args: &serde_json::Map<String, serde_json::Value>) -> Response {
    use super::metrics::profile;
    let Some(root) = profile::default_root() else {
        return Response::Err {
            code: "unavailable".into(),
            message: crate::shell::i18n::t("session-diagnostics-profile-no-state-dir"),
        };
    };
    profile_start_in(&root, args)
}

fn profile_start_in(
    root: &std::path::Path,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Response {
    use super::metrics::profile;
    let seconds = match args.get("seconds").map(serde_json::Value::as_u64) {
        None => profile::DEFAULT_SECONDS,
        Some(Some(seconds)) if (1..=profile::MAX_SECONDS).contains(&seconds) => seconds,
        Some(_) => {
            return Response::Err {
                code: "invalid_arguments".into(),
                message: crate::shell::i18n::t_args(
                    "session-diagnostics-profile-seconds-range",
                    &[("max", profile::MAX_SECONDS.to_string().into())],
                ),
            };
        }
    };
    match profile::start(root, std::time::Duration::from_secs(seconds)) {
        Ok(status) => Response::Answer(json!({ "profile": status })),
        Err(profile::StartError::AlreadyRecording(path)) => Response::Err {
            code: "action_failed".into(),
            message: crate::shell::i18n::t_args(
                "session-diagnostics-profile-already-recording",
                &[("path", path.display().to_string().into())],
            ),
        },
        Err(profile::StartError::Io(error)) => Response::Err {
            code: "action_failed".into(),
            message: crate::shell::i18n::t_args(
                "session-diagnostics-profile-not-created",
                &[("error", error.to_string().into())],
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(id: &str, args: &serde_json::Value) -> Command {
        Command::ActionsInvoke {
            provider: PROVIDER.into(),
            id: id.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            device: None,
            wait_ms: None,
        }
    }

    #[test]
    fn profiling_is_owner_only_on_every_other_connection() {
        for id in [
            "profile-start@session-diagnostics",
            "profile-stop@session-diagnostics",
        ] {
            assert!(
                matches!(owner_required(&invoke(id, &json!({}))), Some(Response::Err { code, .. }) if code == "not_permitted"),
                "{id}"
            );
        }
    }

    /// The admin starts a profile, the loop records, the admin stops it: the
    /// directory holds the frame rows in the frame trace's columns, every
    /// loop-timing phase, each thread's CPU and a summary written last.
    #[test]
    fn a_profile_the_owner_starts_and_stops_is_complete_on_disk() {
        use crate::compositor::metrics::{CSV_HEADER, FrameRecorder, profile};
        use std::time::{Duration, Instant};

        // Off by default: the loop's recorder records nothing.
        let mut recorder = FrameRecorder::disabled();
        recorder.begin_tick(Instant::now());
        recorder.end_tick();
        assert!(!recorder.is_enabled() && !profile::active());
        assert!(recorder.samples().is_empty());

        let root = tempfile::tempdir().expect("tempdir");
        for seconds in [json!(0), json!(profile::MAX_SECONDS + 1), json!("ten")] {
            assert!(
                matches!(profile_start_in(root.path(), json!({ "seconds": seconds }).as_object().unwrap()), Response::Err { code, .. } if code == "invalid_arguments"),
                "{seconds}"
            );
        }
        assert!(!profile::active(), "a refused length records nothing");
        let Response::Answer(started) =
            profile_start_in(root.path(), json!({"seconds": 60}).as_object().unwrap())
        else {
            panic!("the owner starts a profile");
        };
        assert_eq!(started["profile"]["state"], "recording");
        let dir = std::path::PathBuf::from(started["profile"]["path"].as_str().unwrap());
        assert!(dir.starts_with(root.path()));
        assert!(
            matches!(profile_start_in(root.path(), &serde_json::Map::new()), Response::Err { code, .. } if code == "action_failed"),
            "one profile at a time"
        );

        // The loop, unchanged: a recorder with no environment begins
        // recording at its next tick.
        recorder.set_target_period(Duration::from_micros(11_111));
        for _ in 0..3 {
            recorder.begin_tick(Instant::now());
            profile::record_phase("widget-pump", Duration::from_micros(250), None);
            profile::record_phase("legacy-inbound", Duration::from_micros(40), Some(2));
            recorder.mark_pump(Duration::from_micros(300));
            recorder.end_tick();
        }
        assert!(recorder.is_enabled());
        assert!(
            recorder.samples().is_empty(),
            "a profile keeps its own rows, not the environment's summary"
        );

        let Some(Response::Answer(stopped)) =
            dispatch(&invoke("profile-stop@session-diagnostics", &json!({})))
        else {
            panic!("the owner stops the profile");
        };
        assert_eq!(stopped["profile"]["state"], "idle");
        assert_eq!(stopped["profile"]["last"]["ticks"], 3);
        assert!(!profile::active());
        recorder.begin_tick(Instant::now());
        recorder.end_tick();
        assert!(!recorder.is_enabled(), "recording stops with the profile");

        let summary_path = dir.join("summary.json");
        tilo_test_env::wait_until("the profile to complete", || summary_path.exists());
        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let mut rows = frames.lines();
        assert_eq!(rows.next(), Some(CSV_HEADER));
        assert_eq!(rows.count(), 3);
        let phases = std::fs::read_to_string(dir.join("phases.csv")).unwrap();
        assert_eq!(phases.lines().next(), Some("seq,phase,elapsed_us,items"));
        assert_eq!(
            phases
                .lines()
                .filter(|l| l.contains(",widget-pump,250,"))
                .count(),
            3
        );
        assert!(phases.contains(",legacy-inbound,40,2"));
        let threads = std::fs::read_to_string(dir.join("threads.csv")).unwrap();
        assert_eq!(threads.lines().next(), Some("t_us,tid,thread,cpu_us"));
        let summary: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        assert_eq!(summary["frames"]["ticks"], 3);
        assert_eq!(summary["phases"]["widget-pump"]["count"], 3);
        assert_eq!(summary["phases"]["widget-pump"]["total_us"], 750);
        assert!(
            !summary["threads"].as_array().unwrap().is_empty(),
            "the sampler saw this process's threads"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("frames.csv"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "a profile names apps; it is private");
        }
        let Some(Response::Answer(inspected)) =
            dispatch(&invoke("inspect@session-diagnostics", &json!({})))
        else {
            panic!("inspect");
        };
        assert_eq!(inspected["profile"]["last"]["path"], json!(dir));
    }

    #[test]
    fn session_diagnostics_strict_admission_readback_and_session_isolation() {
        LaunchSession::with(|session| {
            assert!(session.diagnostics.lock().unwrap().is_none());
            let mut command = Command::ActionsInvoke {
                provider: PROVIDER.into(), id: "configure@session-diagnostics".into(),
                args: json!({"presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false}).as_object().unwrap().clone(), device: None, wait_ms: None,
            };
            assert!(
                matches!(dispatch(&command), Some(Response::Answer(value)) if value["diagnostics"]["presentation"] == true)
            );
            let options = session.diagnostic_options();
            if let Command::ActionsInvoke { args, .. } = &mut command {
                args.insert(
                    "environment".into(),
                    json!({"TILO_NATIVE_SCENARIO":"private"}),
                );
            }
            assert!(
                matches!(dispatch(&command), Some(Response::Err { code, .. }) if code == "invalid_arguments")
            );
            assert_eq!(session.diagnostic_options(), options);
            if let Command::ActionsInvoke { args, .. } = &mut command {
                args.remove("environment");
                args.insert("gpu_timestamps".into(), json!(false));
                args.insert("gpu_dispatch_timestamps".into(), json!(true));
            }
            assert!(
                matches!(dispatch(&command), Some(Response::Err { code, .. }) if code == "invalid_arguments")
            );
            assert_eq!(session.diagnostic_options(), options);
            assert!(
                LaunchSession::default()
                    .diagnostics
                    .lock()
                    .unwrap()
                    .is_none()
            );
            let read = Command::ActionsInvoke {
                provider: PROVIDER.into(),
                id: "inspect@session-diagnostics".into(),
                args: serde_json::Map::new(),
                device: None,
                wait_ms: None,
            };
            assert!(
                matches!(dispatch(&read), Some(Response::Answer(value)) if value["applies_to"] == "subsequent_launches" && value["diagnostics"] == options.value())
            );
            crate::shell::launcher::set_pending_placement(
                "dev.tilo.diagnostic-app",
                crate::shell::tab_binding::Placement::IntoTab(77),
            );
            let Some(Response::Answer(value)) = dispatch(&read) else {
                panic!("owner inspect receipt");
            };
            assert_eq!(value["launch_history"]["attempts"][0]["saved_tab"], 77);
            assert_eq!(
                value["launch_history"]["stderr_policy"],
                "fully_redacted_fixed_reason_only"
            );
            assert!(
                value["launch_history"]["attempts"][0]["requested_epoch_ns"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
            crate::shell::launcher::clear_pending_placements();
        });
    }
}
