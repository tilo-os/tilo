//! Native semantic service consuming the ordinary durable invocation stream.
use super::room_service::ServiceOutcome;
use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_peer_state::desktop_command::{
    self as command, DesktopCommandError, DesktopCommandId, DesktopCommandIdOf as _,
    DesktopCommandReceipt, DesktopRefusal,
};

impl CapnpBrokerState {
    pub(super) fn start_desktop_commands(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.desktop_commands.clone() else {
            return Ok(());
        };
        self.owner_health()?;
        let room = service.room().to_string();
        uuid::Uuid::parse_str(&room).map_err(|e| e.to_string())?;
        let peer_id = self.next_service_peer_id();
        *self
            .desktop_command_peer
            .lock()
            .map_err(|_| "desktop command registry unavailable")? = Some(peer_id);
        let mut actions = vec![desktop_command_action()];
        actions.extend(
            service
                .catalogue()
                .openable()
                .into_iter()
                .filter(|entry| {
                    open_app_resource(&format!("{OPEN_APP_PREFIX}{}", entry.app_id))
                        == Some(entry.app_id)
                })
                .map(|entry| open_app_action(entry.app_id, entry.title)),
        );
        let surface = PublishedSurface {
            peer_id,
            app_id: command::APP_ID.into(),
            route: CrdtRoute::record(command::APP_ID, &room, ""),
            live: true,
            actions,
        };
        self.serve_room(
            surface,
            move |state, envelope| {
                let service = service.clone();
                async move {
                    let mut id = None;
                    match state
                        .execute_desktop_command(peer_id, &envelope, service.as_ref(), &mut id)
                        .await
                    {
                        Ok(receipt) => ServiceOutcome::Succeeded(receipt.to_value()),
                        Err(DesktopCommandError::Rejected(message)) => {
                            ServiceOutcome::Failed(refusal(id.as_ref(), message))
                        }
                        Err(DesktopCommandError::Uncertain(reason)) => {
                            ServiceOutcome::Uncertain(reason)
                        }
                    }
                }
            },
            move |state, _| {
                if let Ok(mut desktop) = state.desktop_command_peer.lock()
                    && *desktop == Some(peer_id)
                {
                    *desktop = None;
                }
            },
        )
    }

    async fn execute_desktop_command(
        &self,
        peer_id: u64,
        envelope: &InvocationEnvelope,
        service: &dyn crate::services::DesktopCommands,
        id: &mut Option<DesktopCommandId>,
    ) -> Result<DesktopCommandReceipt, DesktopCommandError> {
        use DesktopCommandError::{Rejected, Uncertain};
        let staged = self
            .pending_invocations
            .lock()
            .map_err(|_| Uncertain("invocation registry unavailable".into()))?
            .get(&envelope.id)
            .filter(|pending| {
                pending.peer_id == peer_id && pending.update.invocation() == envelope.invocation
            })
            .cloned()
            .ok_or_else(|| Rejected("desktop invocation is no longer admitted".into()))?;
        // Only authority retained by ordinary admission counts: the opaque
        // certified evidence of a network caller. Actor strings or
        // certificates in arguments are never read. The owner rechecks the
        // caller's current rights.
        let key = staged
            .journal_key
            .as_ref()
            .ok_or_else(|| Rejected("desktop invocation has no durable admission".into()))?;
        let evidence = staged.network_evidence.as_ref().ok_or_else(|| {
            Rejected("desktop commands require certified durable caller evidence".into())
        })?;
        let current = self
            .verify_network_action_caller_with_policy_time(evidence, staged.policy_time.as_ref())
            .map_err(Rejected)?;
        let (caller, owner) = (evidence.certificate().clone(), current.owner());
        // The command id: the authenticated caller device and the durable
        // invocation. It identifies the result; it authorizes nothing.
        *id = Some(DesktopCommandId {
            device: caller.device_public_key,
            invocation: envelope.invocation.0,
        });
        let open_app = open_app_resource(&envelope.resource);
        if key.owner != owner
            || key.room != service.room()
            || key.invocation != envelope.invocation
            || !(envelope.resource == command::RESOURCE && envelope.action == command::ACTION
                || open_app.is_some() && envelope.action == OPEN_APP_ACTION)
        {
            return Err(Rejected("desktop invocation binding changed".into()));
        }
        let (command, direct_delivery) = if let Some(app) = open_app {
            if !open_app_argument_is_empty(envelope.argument.as_deref()) {
                return Err(Rejected("opening an application takes no arguments".into()));
            }
            if service.catalogue().find(app).is_none() {
                return Err(Rejected("application cannot be opened here".into()));
            }
            // Placement-independent: the owner places it in its own turn.
            (
                command::BoundCommand {
                    source: command::CommandSource {
                        room: key.room.clone(),
                        heads: Vec::new(),
                    },
                    command: command::DesktopCommand::OpenApp {
                        app: app.to_owned(),
                    },
                },
                false,
            )
        } else {
            let argument: serde_json::Value = serde_json::from_str(
                envelope
                    .argument
                    .as_deref()
                    .ok_or_else(|| Rejected("desktop command arguments missing".into()))?,
            )
            .map_err(|e| Rejected(e.to_string()))?;
            let encoded = argument
                .get("command")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    Rejected("desktop command must preserve exact serialized IDs".into())
                })?;
            if encoded.len() > 32 * 1024 {
                return Err(Rejected("desktop command exceeds bounded size".into()));
            }
            let command: command::BoundCommand =
                serde_json::from_str(encoded).map_err(|e| Rejected(e.to_string()))?;
            let direct_delivery = argument
                .get("committed_desktop_v2")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            (command, direct_delivery)
        };
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: key.room.clone(),
            operation: tilo_peer_state::owner_commit::CommitOperation::DesktopCommand {
                invocation: envelope.invocation.0,
                caller,
                command,
            },
        };
        let receipt = service.execute(request.clone(), direct_delivery).await?;
        receipt.validate_for(&request).map_err(Uncertain)?;
        let id = DesktopCommandId::of(&request)
            .ok_or_else(|| Uncertain("desktop request lost its command id".into()))?;
        // The owner's own receipt (with the windows it opened) when it keeps
        // one; otherwise the bare receipt of the committed generation.
        Ok(service
            .receipt(&request)
            .filter(|kept| kept.command == id.to_string())
            .unwrap_or_else(|| DesktopCommandReceipt::new(&id, receipt.generation, Vec::new())))
    }
}

/// A definite refusal as its issuer sees it: the command id (once the caller
/// is known) and a stable reason beside the owner's message.
fn refusal(id: Option<&DesktopCommandId>, message: String) -> tilo_actions::ActionFailure {
    let reason = DesktopRefusal::classify(&message);
    tilo_actions::ActionFailure {
        code: command::REJECTED_CODE.into(),
        details: Some(match id {
            Some(id) => reason.details(id),
            None => serde_json::json!({"reason": reason.as_str()}),
        }),
        message,
        retryable: false,
    }
}

/// Whether an open's wire argument carries nothing: absent (the palette's
/// immediate invoke) or the typed empty argument object every task step
/// sends for a nullary operation. Anything else is refused.
fn open_app_argument_is_empty(argument: Option<&str>) -> bool {
    argument.is_none_or(|argument| {
        argument
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .is_some_and(|value| value.as_object().is_some_and(serde_json::Map::is_empty))
    })
}

/// Action id of the per-application open action.
pub(crate) const OPEN_APP_ACTION: &str = command::OPEN_APP_ACTION;
const OPEN_APP_PREFIX: &str = command::OPEN_APP_RESOURCE_PREFIX;

/// The application a `desktop:app:<id>` resource opens.
pub(crate) fn open_app_resource(resource: &str) -> Option<&str> {
    resource.strip_prefix(OPEN_APP_PREFIX).filter(|app| {
        !app.is_empty() && app.len() <= 128 && app.bytes().all(|b| b.is_ascii_graphic())
    })
}

/// Every desktop command action: Write, answered with the receipt that
/// names its command id and the windows it opened.
fn desktop_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        result: tilo_actions::ResultSpec::Json {
            schema: DesktopCommandReceipt::schema(),
        },
        ..Default::default()
    }
}

fn desktop_command_action() -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: command::RESOURCE.into(),
        action: command::ACTION.into(),
        resource_kind: "desktop".into(),
        resource_label: "Desktop".into(),
        action_label: "Change desktop".into(),
        description: Some("Typed native current-room desktop command".into()),
        operation_kind: tilo_actions::OperationKind::Effect,
        params: vec![
            tilo_actions::ActionParam::required(
                "command",
                tilo_actions::ParamType::Text,
                "Exact source-bound desktop command",
            ),
            tilo_actions::ActionParam::optional(
                "committed_desktop_v2",
                tilo_actions::ParamType::Boolean,
                "Receive the committed desktop immediately; defaults to false",
            ),
        ],
        contract: desktop_contract(),
        view: None,
        input: PublishedInput::None,
    }
}

/// "Open <App>": the launcher's open, as an ordinary published action. It
/// opens the app in a new window with a new record, exactly as a person
/// opening it from the launcher, so its operations then appear in the
/// catalogue of the host worker that serves the new record.
fn open_app_action(app: &str, title: &str) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: format!("{OPEN_APP_PREFIX}{app}"),
        action: OPEN_APP_ACTION.into(),
        resource_kind: command::OPEN_APP_RESOURCE_KIND.into(),
        resource_label: title.into(),
        action_label: format!("Open {title}"),
        description: Some(format!("Open {title} in a new window with a new document")),
        operation_kind: tilo_actions::OperationKind::Effect,
        params: vec![],
        contract: desktop_contract(),
        view: None,
        input: PublishedInput::None,
    }
}

#[cfg(test)]
mod open_app_tests {
    use super::*;

    /// A task step's typed empty argument object is no argument, as the
    /// palette's immediate invoke is; any real argument is still refused.
    #[test]
    fn an_open_accepts_only_an_absent_or_empty_argument() {
        let typed = |json: &str| format!("{}{json}", tilo_actions::ARGUMENTS_JSON_PREFIX);
        assert!(open_app_argument_is_empty(None));
        assert!(open_app_argument_is_empty(Some(&typed("{}"))));
        assert!(!open_app_argument_is_empty(Some(&typed("{\"x\":1}"))));
        assert!(!open_app_argument_is_empty(Some(&typed("[]"))));
        assert!(!open_app_argument_is_empty(Some("{}")));
        assert!(!open_app_argument_is_empty(Some("")));
    }

    /// Opens and layout commands answer with the receipt their contract
    /// declares: the command id and the windows opened, ids as strings.
    #[test]
    fn a_desktop_receipt_satisfies_the_published_contract() {
        let id = DesktopCommandId {
            device: [3; 32],
            invocation: uuid::Uuid::from_u128(4),
        };
        let receipt = DesktopCommandReceipt::new(
            &id,
            7,
            vec![command::OpenedWindow {
                app_id: "so.tilo.notes".into(),
                record_id: "tab:1".into(),
                strip: 1,
                group: 2,
                tab: u64::MAX,
            }],
        );
        for action in [
            desktop_command_action(),
            open_app_action("so.tilo.notes", "Notes"),
        ] {
            let output = |value| tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            };
            let result = &action.contract.result;
            assert_eq!(result.validate(&output(Some(receipt.to_value()))), Ok(()));
            let bare = DesktopCommandReceipt::new(&id, 7, Vec::new()).to_value();
            assert_eq!(result.validate(&output(Some(bare))), Ok(()));
            assert!(result.validate(&output(None)).is_err());
            let mut numeric = receipt.to_value();
            numeric["opened"][0]["tab"] = serde_json::json!(5);
            assert!(result.validate(&output(Some(numeric))).is_err());
        }
    }

    /// A refusal names the command id once the caller is known, and a
    /// stable reason beside the owner's message.
    #[test]
    fn a_refusal_carries_the_command_id_and_reason() {
        let id = DesktopCommandId {
            device: [3; 32],
            invocation: uuid::Uuid::from_u128(4),
        };
        let failure = refusal(Some(&id), "desktop source revision is stale".into());
        assert_eq!(failure.code, command::REJECTED_CODE);
        assert!(!failure.retryable);
        assert_eq!(failure.message, "desktop source revision is stale");
        assert_eq!(
            failure.details,
            Some(serde_json::json!({"command": id.to_string(), "reason": "stale_source"}))
        );
        let early = refusal(None, "desktop invocation has no durable admission".into());
        assert_eq!(
            early.details,
            Some(serde_json::json!({"reason": "invalid"}))
        );
    }

    #[test]
    fn open_app_resources_name_one_application() {
        assert_eq!(
            open_app_resource("desktop:app:so.tilo.notes"),
            Some("so.tilo.notes")
        );
        assert_eq!(open_app_resource("desktop:app:"), None);
        assert_eq!(open_app_resource("desktop:app:has space"), None);
        assert_eq!(open_app_resource(command::RESOURCE), None);
        let action = open_app_action("so.tilo.notes", "Notes");
        assert_eq!(action.resource, "desktop:app:so.tilo.notes");
        assert_eq!(action.action, OPEN_APP_ACTION);
        assert_eq!(action.action_label, "Open Notes");
        assert!(action.params.is_empty());
        assert_eq!(action.operation_kind, tilo_actions::OperationKind::Effect);
        assert!(matches!(
            action.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ));
    }
}
