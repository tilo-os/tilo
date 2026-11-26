//! What Remote does beside editing the saved connections, as typed effects, and the live
//! rows it publishes. Opening a session is an effect on its connection; the framebuffer,
//! input stream and credential auth touch no document, and a credential lives in this
//! device's secret store. What screen shows, the selection and the draft are per-peer.

use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::{self, AbsolutePath, Effect, RowBindings};

use crate::model::{Connection, ConnectionId, ConnectionKind};
use crate::resource::CATALOG;
use ui::i18n::ArgValue;

/// A saved connection, as Remote publishes it: a row of the `remote.connection`
/// type, its effects bound to the connection's id.
#[derive(tilo_resource::Row)]
#[row(
    kind = "remote.connection",
    label = "Connection",
    effects = RemoteEffect
)]
pub struct ConnectionRow;

/// The open session, as Remote publishes it: a row of the `remote.session`
/// type, its effects bound to the connection and, on a first contact, to the
/// host key it observed.
#[derive(tilo_resource::Row)]
#[row(kind = "remote.session", label = "Session", effects = RemoteEffect)]
pub struct SessionRow;

/// What Remote does beyond editing the saved connections: showing them, opening and closing
/// sessions, typing into one, and this device's SSH host-key decisions.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "remote.effect")]
pub enum RemoteEffect {
    #[effect(view, idempotent)]
    ShowConnections,
    #[effect(view, label = "remote-new-connection")]
    New,
    #[effect(view, row, on = ConnectionRow)]
    EditConnection {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    #[effect(
        row,
        job,
        privileged,
        kind = "read",
        label = "remote-connect-confirm",
        on = ConnectionRow,
        tag = "network",
        tag = "session"
    )]
    Connect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    #[effect(kind = "outside")]
    #[effect(row, idempotent, label = "remote-disconnect", on = SessionRow, tag = "network", tag = "session")]
    Disconnect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    #[effect(kind = "outside")]
    #[effect(job, privileged)]
    SendText {
        #[param("The saved connection whose session receives it")]
        connection: ConnectionId,
        #[param("Text to deliver to the connected SSH terminal")]
        text: String,
    },
    #[effect(kind = "outside")]
    #[effect(
        row,
        job,
        privileged,
        on = SessionRow,
        tag = "network",
        tag = "session",
        tag = "files"
    )]
    PutFile {
        #[param("The saved connection whose SSH session receives it")]
        connection: ConnectionId,
        #[param("Absolute path of the file on this device to send")]
        local_path: AbsolutePath,
        #[param("Where to write it on the remote machine")]
        remote_path: String,
    },
    #[effect(kind = "outside")]
    #[effect(
        row,
        job,
        privileged,
        on = SessionRow,
        tag = "network",
        tag = "session",
        tag = "files"
    )]
    FetchFile {
        #[param("The saved connection whose SSH session reads it")]
        connection: ConnectionId,
        #[param("The file on the remote machine to read")]
        remote_path: String,
        #[param("Absolute path to write it to on this device")]
        local_path: AbsolutePath,
    },
    /// Trust SSH host key.
    #[effect(kind = "outside")]
    #[effect(
        row,
        job,
        privileged,
        on = SessionRow,
        tag = "ssh",
        tag = "host-key",
        tag = "device-private"
    )]
    TrustHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
        #[param("The host key observed on first contact")]
        observed: crate::ssh::HostKeyIdentity,
    },
    /// Forget SSH host key.
    #[effect(kind = "outside")]
    #[effect(
        row,
        job,
        destructive,
        on = SessionRow,
        tag = "ssh",
        tag = "host-key",
        tag = "device-private"
    )]
    ForgetHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
}

/// What a saved connection's `connect` says of itself on the row `id`: what
/// opening it would do, and why it cannot be opened when this device's
/// profile or transport says so.
pub(crate) fn connect_status(connection: &Connection, id: &str) -> EffectStatus {
    let readiness = crate::session::connection_readiness(connection);
    let description = CATALOG.t_args(
        "remote-action-open-session",
        &[(
            "detail",
            ArgValue::Text(CATALOG.t(readiness.detail_message(connection.kind))),
        )],
    );
    if readiness.can_connect() {
        return EffectStatus::described(RemoteEffect::CONNECT, description).on(id);
    }
    EffectStatus::unavailable(
        RemoteEffect::CONNECT,
        readiness.code(),
        if readiness == crate::session::TransportReadiness::InvalidProfile {
            CATALOG.t(connection
                .profile_issue()
                .unwrap_or_else(|| readiness.detail_message(connection.kind)))
        } else {
            CATALOG.t(readiness.detail_message(connection.kind))
        },
    )
    .on(id)
    .with_description(description)
}

/// What the session row `id`'s effects say of themselves right now: files
/// move over an SSH shell only, and a host-key decision says what it decides
/// on while the key it decides on is the one this session observed.
pub(crate) fn session_status(
    connection: &Connection,
    host_key_issue: Option<&crate::ssh::HostKeyIssue>,
    id: &str,
) -> Vec<EffectStatus> {
    let mut statuses = Vec::new();
    if connection.kind == ConnectionKind::Ssh {
        statuses.push(
            EffectStatus::described(
                RemoteEffect::PUT_FILE,
                CATALOG.t("remote-describe-put-file"),
            )
            .on(id),
        );
        statuses.push(
            EffectStatus::described(
                RemoteEffect::FETCH_FILE,
                CATALOG.t("remote-describe-fetch-file"),
            )
            .on(id),
        );
    }
    match host_key_issue {
        Some(crate::ssh::HostKeyIssue::FirstContact { observed }) => statuses.push(
            EffectStatus::described(
                RemoteEffect::TRUST_HOST_KEY,
                CATALOG.t_args(
                    "remote-action-trust-host-key",
                    &[
                        ("algorithm", ArgValue::Text(observed.algorithm.clone())),
                        ("fingerprint", ArgValue::Text(observed.fingerprint.clone())),
                    ],
                ),
            )
            .on(id),
        ),
        Some(crate::ssh::HostKeyIssue::Changed { .. }) => statuses.push(
            EffectStatus::described(
                RemoteEffect::FORGET_HOST_KEY,
                CATALOG.t("remote-describe-forget-host-key"),
            )
            .on(id),
        ),
        _ => {}
    }
    statuses
}

/// The open session: disconnect it, move a file over an SSH shell, and the
/// host-key decision a first contact or a changed key asks for.
pub fn session_row(
    connection: &Connection,
    host_key_issue: Option<&crate::ssh::HostKeyIssue>,
) -> LiveRow {
    let connection_id = serde_json::Value::String(connection.id.to_string());
    let mut bindings = RowBindings::new().bind(
        RemoteEffect::DISCONNECT,
        [("connection", connection_id.clone())],
    );
    if connection.kind == ConnectionKind::Ssh {
        bindings = bindings
            .bind(
                RemoteEffect::PUT_FILE,
                [("connection", connection_id.clone())],
            )
            .bind(RemoteEffect::FETCH_FILE, [("connection", connection_id)]);
    }
    match host_key_issue {
        Some(crate::ssh::HostKeyIssue::FirstContact { observed }) => {
            bindings = bindings.bind(
                RemoteEffect::TRUST_HOST_KEY,
                [
                    (
                        "connection",
                        serde_json::Value::String(connection.id.to_string()),
                    ),
                    (
                        "observed",
                        serde_json::to_value(observed).unwrap_or(serde_json::Value::Null),
                    ),
                ],
            );
        }
        Some(crate::ssh::HostKeyIssue::Changed { .. }) => {
            bindings = bindings.bind(
                RemoteEffect::FORGET_HOST_KEY,
                [(
                    "connection",
                    serde_json::Value::String(connection.id.to_string()),
                )],
            );
        }
        _ => {}
    }
    LiveRow::typed_with::<SessionRow>(
        format!("remote.session:{}", connection.id),
        format!("{} session", connection.name),
        &bindings,
    )
}

/// The effects a saved connection offers beside its verbs: its settings and its connect.
pub fn connection_row(connection: &Connection) -> LiveRow {
    let connection_id = serde_json::Value::String(connection.id.to_string());
    let bindings = RowBindings::new()
        .bind(
            RemoteEffect::EDIT_CONNECTION,
            [("connection", connection_id.clone())],
        )
        .bind(RemoteEffect::CONNECT, [("connection", connection_id)]);
    LiveRow::typed_with::<ConnectionRow>(
        crate::resource::connection_resource_id(connection.id),
        connection.name.clone(),
        &bindings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, ConnectionKind};
    use tilo_actions::ActionSpec;
    use tilo_actions::{
        ActionKind, ConfirmationPolicy, ParamType, ResultSpec, RetryPolicy, RiskLevel,
    };
    use ui::tilo_resource::Atom;
    use uuid::Uuid;

    pub(crate) fn sample(id: ConnectionId, name: &str) -> Connection {
        Connection {
            id,
            name: name.into(),
            host: Atom("10.0.0.9".into()),
            port: 22,
            user: Atom("admin".into()),
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            last_used_ms: None,
            group: Atom("lab".into()),
            favorite: false,
        }
    }

    tilo_test_env::scenario! {
        fn connections_and_sessions_publish_the_actions_they_can_take;

        phase connect_is_offered_only_for_connectable_profiles;
        {
            eprintln!("phase tilo_connect_action_is_available_without_the_ssh_vnc_dev_check");
            {
                let mut connection = sample(Uuid::new_v4(), "Tilo device");
                connection.kind = ConnectionKind::Tilo;
                connection.auth = AuthMethod::DeviceIdentity;
                connection.host = Atom(iroh_base::SecretKey::generate().public().to_string());
                let status = connect_status(&connection, "remote/connections/tilo");
                assert!(
                    status.availability().is_none(),
                    "a Tilo endpoint this device can reach says nothing against connecting"
                );
                assert!(status.description().is_some(), "and says what it would do");
            }

            eprintln!("phase malformed_tilo_profile_is_not_offered_as_connectable");
            {
                let mut connection = sample(Uuid::new_v4(), "Tilo device");
                connection.kind = ConnectionKind::Tilo;
                connection.auth = AuthMethod::DeviceIdentity;
                connection.host = Atom("friendly-name".into());
                let status = connect_status(&connection, "remote/connections/tilo");
                assert!(matches!(
                    status.availability(),
                    Some(tilo_actions::Availability::Unavailable { code, reason })
                        if code == "invalid_connection_profile"
                            && reason.contains("exact endpoint id")
                ));
            }

            eprintln!("phase legacy_rdp_resource_names_the_missing_backend_instead_of_a_generic_check");
            {
                let mut connection = sample(Uuid::new_v4(), "Old Windows host");
                connection.kind = ConnectionKind::Rdp;
                connection.auth = AuthMethod::Password;
                let row = connection_row(&connection).into_resource();
                let connect = row
                    .actions
                    .iter()
                    .find(|action| action.id == "connect")
                    .expect("RDP profile stays manageable");
                assert!(
                    connect
                        .contract
                        .tags
                        .iter()
                        .any(|tag| tag == "session"),
                    "a connection row's connect is a session"
                );
                let status = connect_status(&connection, row.id.as_str());
                assert!(matches!(
                    status.availability(),
                    Some(tilo_actions::Availability::Unavailable { code, reason })
                        if code == "transport_backend_unavailable"
                            && reason.contains("but not RDP")
                ));
                assert!(
                    status
                        .description()
                        .is_some_and(|description| description.contains("but not RDP")),
                    "search and agents receive the actionable reason"
                );
            }
        }

        phase ssh_sessions_offer_file_transfer_in_both_directions;
        {
            let (_row, actions) = session_actions(&sample(Uuid::new_v4(), "Build Box"), None);
            for id in ["put-file", "fetch-file"] {
                let action = actions
                    .iter()
                    .find(|action| action.id == id)
                    .unwrap_or_else(|| panic!("an SSH session offers {id}"));
                assert_eq!(action.contract.risk, RiskLevel::Privileged, "{id}");
                assert_eq!(action.contract.result, ResultSpec::Job, "{id}");
                for path in ["local_path", "remote_path"] {
                    assert!(
                        action
                            .params()
                            .iter()
                            .any(|param| param.name == path && param.ty == ParamType::Text),
                        "{id} asks for {path}"
                    );
                }
            }
            assert!(
                session_statuses(&sample(Uuid::new_v4(), "Build Box"), None)
                    .iter()
                    .filter(|status| matches!(status.id(), "put-file" | "fetch-file"))
                    .all(|status| status.availability().is_none()),
                "an SSH session moves files both ways"
            );
            // A relative local path is refused: it would land wherever Remote runs.
            let arguments = serde_json::Map::from_iter([
                ("connection".to_owned(), serde_json::json!(Uuid::new_v4())),
                ("remote_path".to_owned(), serde_json::json!("/srv/log.txt")),
                ("local_path".to_owned(), serde_json::json!("log.txt")),
            ]);
            let refused = RemoteEffect::decode("fetch-file", &arguments).unwrap_err();
            assert!(
                matches!(
                    &refused,
                    ui::tilo_resource::EffectDecodeError::Argument { name, source }
                        if name == "local_path" && source.to_string().contains("absolute")
                ),
                "{refused}"
            );
            let mut desktop = sample(Uuid::new_v4(), "Desktop");
            desktop.kind = ConnectionKind::Vnc;
            let row = session_row(&desktop, None).into_resource();
            assert!(
                !row.actions.iter().any(|action| action.id == "put-file"),
                "a framebuffer session has no shell to move a file through"
            );
        }

        phase session_effects_declare_permissions_lifecycle_and_trust;
        {
            eprintln!("phase remote_effects_declare_confirmation_permissions_and_non_repeatability");
            {
                let mut connection = sample(Uuid::new_v4(), "Tilo device");
                connection.kind = ConnectionKind::Tilo;
                connection.auth = AuthMethod::DeviceIdentity;
                connection.host = Atom(iroh_base::SecretKey::generate().public().to_string());
                let row = connection_row(&connection).into_resource();
                let connect = row
                    .actions
                    .iter()
                    .find(|action| action.id == "connect")
                    .expect("a connection offers connecting");
                assert_eq!(connect.contract.kind, ActionKind::Read);
                assert_eq!(connect.contract.risk, RiskLevel::Privileged);
                assert_eq!(connect.contract.confirmation, ConfirmationPolicy::Confirm);
                assert_eq!(connect.contract.retry, RetryPolicy::AtMostOnce);
                assert_eq!(connect.contract.result, ResultSpec::Job);

                let input = RemoteEffect::specs()
                    .into_iter()
                    .find(|action| action.id == "send-text")
                    .expect("typed input action");
                assert_eq!(input.contract.kind, ActionKind::Outside);
                assert_eq!(input.contract.risk, RiskLevel::Privileged);
                assert_eq!(input.contract.confirmation, ConfirmationPolicy::Confirm);
                assert_eq!(input.contract.retry, RetryPolicy::AtMostOnce);
                assert_eq!(input.contract.result, ResultSpec::Job);
            }

            eprintln!("phase active_session_exports_lifecycle_and_typed_input");
            {
                let id = Uuid::new_v4();
                let (row, actions) = session_actions(&sample(id, "Build Box"), None);
                assert_eq!(row, format!("remote.session:{id}"));
                let disconnect = actions.iter().find(|a| a.id == "disconnect").unwrap();
                assert_eq!(disconnect.contract.retry, RetryPolicy::Idempotent);
                let input = RemoteEffect::specs()
                    .into_iter()
                    .find(|a| a.id == "send-text")
                    .unwrap();
                assert!(
                    input
                        .params()
                        .iter()
                        .any(|param| param.name == "text" && param.ty == ParamType::Text)
                );
            }

            eprintln!("phase first_contact_exports_an_explicit_device_private_trust_action");
            {
                let issue = crate::ssh::HostKeyIssue::FirstContact {
                    observed: crate::ssh::HostKeyIdentity {
                        algorithm: "ssh-ed25519".into(),
                        fingerprint: "SHA256:example".into(),
                    },
                };
                let connection = sample(Uuid::new_v4(), "Build Box");
                let trust = session_actions(&connection, Some(&issue))
                    .1
                    .into_iter()
                    .find(|action| action.id == "trust-host-key")
                    .expect("first contact publishes a trust action");
                assert_eq!(trust.contract.confirmation, ConfirmationPolicy::Confirm);
                assert_eq!(trust.contract.risk, RiskLevel::Privileged);
                assert!(
                    trust
                        .contract
                        .tags
                        .iter()
                        .any(|tag| tag == "device-private")
                );
                assert!(
                    trust.params().is_empty(),
                    "the key this first contact observed is the row's own value"
                );
                let status = session_statuses(&connection, Some(&issue))
                    .into_iter()
                    .find(|status| status.id() == "trust-host-key")
                    .expect("the trust action says what it trusts");
                assert!(
                    status
                        .description()
                        .is_some_and(|description| description.contains("SHA256:example"))
                );
            }
        }
    }

    fn session_actions(
        connection: &Connection,
        issue: Option<&crate::ssh::HostKeyIssue>,
    ) -> (String, Vec<ActionSpec>) {
        let row = session_row(connection, issue).into_resource();
        (row.id.as_str().to_owned(), row.actions)
    }

    fn session_statuses(
        connection: &Connection,
        issue: Option<&crate::ssh::HostKeyIssue>,
    ) -> Vec<EffectStatus> {
        session_status(
            connection,
            issue,
            &format!("remote.session:{}", connection.id),
        )
    }
}
