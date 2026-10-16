//! Fixed session diagnostics, available only to the authenticated local owner.
use serde_json::json;
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource};

use crate::shell::launcher::{
    LaunchSession,
    diagnostics::{FIELDS, Options},
};
use crate::shell::test_control::{Command, Response};
use tilo_lock::MutexRecover as _;

const PROVIDER: &str = "session-diagnostics";

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
    configure.contract.kind = tilo_actions::ActionKind::Outside;
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
    configure.contract.result = result.clone();
    inspect.contract.result = result;
    inspect.contract.kind = tilo_actions::ActionKind::Read;
    vec![
        Resource::new(
            PROVIDER,
            "session-diagnostics/v1",
            crate::shell::i18n::t("session-diagnostics-title"),
        )
        .with_action(configure)
        .with_action(inspect),
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
    let session = LaunchSession::current();
    if admitted.encoded.payload == b"configure" {
        let flags = FIELDS.map(|(field, _)| {
            args.get(field)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        });
        if flags[3] && !flags[2] {
            return Some(Response::Err {
                code: "invalid_arguments".into(),
                message: crate::shell::i18n::t("session-diagnostics-dispatch-requires-timestamps"),
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
    }
    Some(Response::Answer(serde_json::Value::Object(answer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_diagnostics_strict_admission_readback_and_session_isolation() {
        let session = LaunchSession::current();
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
    }
}
