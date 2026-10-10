//! Fixed session diagnostics, available only to the authenticated local owner.
use serde_json::json;
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource};

use crate::shell::launcher::{
    LaunchSession,
    diagnostics::{FIELDS, Options},
};
use crate::shell::test_control::{Command, Response};

const PROVIDER: &str = "session-diagnostics";

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
    configure.contract.permission = tilo_actions::PermissionRequirement::Admin;
    let mut inspect = ActionSpec::typed_operation(
        "inspect",
        crate::shell::i18n::t("session-diagnostics-inspect"),
        "",
        OperationKind::View,
        "session-diagnostics/v1",
        vec![],
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
    inspect.contract.permission = tilo_actions::PermissionRequirement::Read;
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
    Some(Response::Answer(
        json!({"applies_to":"subsequent_launches","diagnostics":flags.value(),"live_input_delivery":flags.0[0],"current_gpu_renderer":"unchanged"}),
    ))
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
    }
}
