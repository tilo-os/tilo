//! What Remote does beside editing the saved connections, as typed
//! effects, and the live rows it publishes.
//!
//! The saved connections are the [`Remote`](crate::model::Remote) model:
//! their fields are edited at their paths, and adding, deleting and recording
//! a use are its verbs ([`crate::resource`]). Opening a live session is an
//! effect on its connection. The framebuffer, input stream, and credential
//! auth remain app-owned one-off I/O and touch no document; a connection's
//! password or key passphrase lives in this device's secret store. And
//! presentation — which screen is showing, the selected row, the in-progress
//! add/edit draft, scale / fullscreen, the nav rail — is per-peer view state,
//! as is whether a machine answered.

use tilo_actions::ActionSpec;
use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

use crate::model::{Connection, ConnectionId};

/// What Remote does that is no edit of the saved connections: showing them,
/// opening and closing live sessions, typing into one, and the SSH host-key
/// decisions this device keeps for itself.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "remote.effect")]
pub enum RemoteEffect {
    /// Show connections.
    #[effect(view, idempotent)]
    ShowConnections,
    /// New connection.
    #[effect(view)]
    New,
    /// Open connection settings.
    #[effect(view, row)]
    EditConnection {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Connect.
    #[effect(row, job, privileged, kind = "read")]
    Connect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Disconnect session.
    #[effect(row, idempotent)]
    Disconnect {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
    /// Send text to session.
    #[effect(job, privileged)]
    SendText {
        #[param("The saved connection whose session receives it")]
        connection: ConnectionId,
        #[param("Text to deliver to the connected SSH terminal")]
        text: String,
    },
    /// Trust SSH host key.
    #[effect(row, job, privileged)]
    TrustHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
        #[param("The host key observed on first contact")]
        observed: crate::ssh::HostKeyIdentity,
    },
    /// Forget SSH host key.
    #[effect(row, job, destructive)]
    ForgetHostKey {
        #[param("The saved connection's id")]
        connection: ConnectionId,
    },
}

/// Open a connection's settings.
pub(crate) fn open_spec(id: ConnectionId) -> ActionSpec {
    RemoteEffect::EditConnection { connection: id }.bound()
}

/// Open a live session to a connection, when it can start one.
pub(crate) fn connect_spec(connection: &Connection) -> ActionSpec {
    let readiness = crate::session::connection_readiness(connection);
    let mut spec = RemoteEffect::Connect {
        connection: connection.id,
    }
    .bound()
    .with_description(format!(
        "Open a live session to this saved connection. {}",
        readiness.detail(connection.kind)
    ));
    spec.type_mut().contract.tags = vec!["network".into(), "session".into()];
    if readiness.can_connect() {
        spec
    } else {
        spec.unavailable(
            readiness.code(),
            if readiness == crate::session::TransportReadiness::InvalidProfile {
                connection
                    .profile_issue()
                    .unwrap_or_else(|| readiness.detail(connection.kind))
            } else {
                readiness.detail(connection.kind)
            },
        )
    }
}

/// A host-key decision stays on this device.
const HOST_KEY_TAGS: &[&str] = &["ssh", "host-key", "device-private"];

/// `spec`, tagged with what it touches.
fn tagged(mut spec: ActionSpec, tags: &[&str]) -> ActionSpec {
    spec.type_mut().contract.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
    spec
}

/// The open session: disconnect it, and the SSH host-key decision its
/// first contact or a changed key asks for. Text is sent with the
/// `send-text` effect, naming the connection.
pub fn session_row(
    connection: &Connection,
    host_key_issue: Option<&crate::ssh::HostKeyIssue>,
) -> LiveRow {
    let id = connection.id;
    let row = LiveRow::new(
        format!("remote.session:{id}"),
        "remote.session",
        format!("{} session", connection.name),
    )
    .operation(tagged(
        RemoteEffect::Disconnect { connection: id }.bound(),
        &["network", "session"],
    ));
    match host_key_issue {
        Some(crate::ssh::HostKeyIssue::FirstContact { observed }) => row.operation(tagged(
            RemoteEffect::TrustHostKey {
                connection: id,
                observed: observed.clone(),
            }
            .bound()
            .with_description(format!(
                "Explicitly trust {} {} for this device only after out-of-band verification.",
                observed.algorithm, observed.fingerprint
            )),
            HOST_KEY_TAGS,
        )),
        Some(crate::ssh::HostKeyIssue::Changed { .. }) => row.operation(tagged(
            RemoteEffect::ForgetHostKey { connection: id }
                .bound()
                .with_description(
                    "Remove this device's stale SSH host-key pin; reconnect to inspect a replacement key.",
                ),
            HOST_KEY_TAGS,
        )),
        _ => row,
    }
}

/// The effects a saved connection offers beside its verbs: its settings
/// and its connect, merged into the connection's published item by id.
pub fn connection_row(connection: &Connection) -> LiveRow {
    LiveRow::new(
        crate::resource::connection_resource_id(connection.id),
        "remote.connection",
        connection.name.clone(),
    )
    .operation(open_spec(connection.id))
    .operation(connect_spec(connection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, ConnectionKind};
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

    #[test]
    fn connect_is_offered_only_for_connectable_profiles() {
        eprintln!("phase tilo_connect_action_is_available_without_the_ssh_vnc_dev_gate");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = Atom(iroh_base::SecretKey::generate().public().to_string());
            assert!(
                connect_spec(&connection)
                    .contract
                    .availability
                    .is_available()
            );
        }

        eprintln!("phase malformed_tilo_profile_is_not_offered_as_connectable");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = Atom("friendly-name".into());
            let spec = connect_spec(&connection);
            assert!(matches!(
                &spec.contract.availability,
                tilo_actions::Availability::Unavailable { code, reason }
                    if code == "invalid_connection_profile"
                        && reason.contains("exact certified endpoint")
            ));
        }

        eprintln!("phase legacy_rdp_resource_names_the_missing_backend_instead_of_a_generic_gate");
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
            assert!(matches!(
                &connect.contract.availability,
                tilo_actions::Availability::Unavailable { code, reason }
                    if code == "transport_backend_unavailable"
                        && reason.contains("but not RDP")
            ));
            assert!(
                connect
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("but not RDP")),
                "search and agents receive the actionable reason"
            );
        }
    }

    fn session_actions(
        connection: &Connection,
        issue: Option<&crate::ssh::HostKeyIssue>,
    ) -> (String, Vec<ActionSpec>) {
        let row = session_row(connection, issue).into_resource();
        (row.id.as_str().to_owned(), row.actions)
    }

    #[test]
    fn session_effects_declare_permissions_lifecycle_and_trust() {
        eprintln!("phase remote_effects_declare_confirmation_permissions_and_non_repeatability");
        {
            let mut connection = sample(Uuid::new_v4(), "Tilo device");
            connection.kind = ConnectionKind::Tilo;
            connection.auth = AuthMethod::DeviceIdentity;
            connection.host = Atom(iroh_base::SecretKey::generate().public().to_string());
            let connect = connect_spec(&connection);
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
            let trust = session_actions(&sample(Uuid::new_v4(), "Build Box"), Some(&issue))
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
                trust
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("SHA256:example"))
            );
        }
    }
}
