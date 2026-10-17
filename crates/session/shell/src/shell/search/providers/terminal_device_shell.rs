//! Owner operations on a live Terminal's existing authenticated lifecycle channel.
//! Host selection still uses the ordinary ush consent flow. Input is never agent input.

use crate::shell::overlay_host::is_active_workspace_owner;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{
    ActionReceipt, InvocationDisposition, ResourceKind, SearchContext, SearchProvider, SearchResult,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use ui::floating_surface::{DeviceShellOperation, FloatingSurfaceHostMessage};

pub const PROVIDER_ID: &str = "terminal-device-shell";
type Reply = Rc<RefCell<Option<Result<serde_json::Value, String>>>>;
#[derive(Default)]
struct Connections {
    writers: BTreeMap<u64, mpsc::Sender<Vec<u8>>>,
    pending: BTreeMap<(u64, u64), Reply>,
    next: u64,
}
#[derive(Clone)]
pub struct TerminalDeviceShellProvider {
    connections: Rc<RefCell<Connections>>,
    ranker: Rc<ui::search::ListIndex>,
}
impl Default for TerminalDeviceShellProvider {
    fn default() -> Self {
        Self {
            connections: Rc::new(RefCell::new(Connections::default())),
            ranker: Rc::new(ui::search::ListIndex::default()),
        }
    }
}
impl TerminalDeviceShellProvider {
    pub fn connect(&self, connection: u64, writer: mpsc::Sender<Vec<u8>>) {
        self.connections
            .borrow_mut()
            .writers
            .insert(connection, writer);
    }
    pub fn disconnect(&self, connection: u64) {
        let mut connections = self.connections.borrow_mut();
        connections.writers.remove(&connection);
        connections.pending.retain(|&(owner, _), reply| {
            if owner != connection {
                return true;
            }
            *reply.borrow_mut() = Some(Err("This Terminal session has closed".into()));
            false
        });
    }
    /// Only replies on the originating launcher-authenticated Terminal connection settle a request.
    pub fn complete(
        &self,
        connection: u64,
        request: u64,
        result: Result<serde_json::Value, String>,
    ) {
        let reply = self
            .connections
            .borrow_mut()
            .pending
            .remove(&(connection, request));
        if let Some(reply) = reply {
            *reply.borrow_mut() = Some(
                if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 65536) {
                    Err("Terminal response exceeded its limit".into())
                } else {
                    result
                },
            );
        }
    }
    fn row(&self, connection: u64, operation: &'static str) -> SearchResult {
        let provider = self.clone();
        let mut params = Vec::new();
        if matches!(operation, "request" | "run") {
            params.push(ActionParam::required_owned(
                "confirm",
                ParamType::Boolean,
                crate::shell::i18n::t("terminal-device-shell-confirm-param"),
            ));
        }
        if matches!(operation, "run" | "read") {
            params.push(ActionParam::required_owned(
                "session",
                ParamType::Text,
                crate::shell::i18n::t("terminal-device-shell-session-param"),
            ));
        }
        if operation == "run" {
            params.push(ActionParam::required_owned(
                "command",
                ParamType::Text,
                crate::shell::i18n::t("terminal-device-shell-command-param"),
            ));
        }
        SearchResult {
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: if matches!(operation, "inspect" | "read") {
                tilo_actions::OperationKind::View
            } else {
                tilo_actions::OperationKind::Effect
            },
            provider_id: PROVIDER_ID.into(),
            id: format!("{operation}@{connection}"),
            title: crate::shell::i18n::t(match operation {
                "request" => "terminal-request-device-shell",
                "inspect" => "terminal-inspect-device-shell",
                "run" => "terminal-run-device-shell",
                _ => "terminal-read-device-shell",
            }),
            subtitle: Some(format!("Terminal {connection}")),
            badge: Some(self.display_name()),
            score: 0,
            params,
            contract: Some(tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                risk: if matches!(operation, "request" | "run") {
                    tilo_actions::RiskLevel::Destructive
                } else {
                    tilo_actions::RiskLevel::None
                },
                confirmation: if matches!(operation, "request" | "run") {
                    tilo_actions::ConfirmationPolicy::Confirm
                } else {
                    tilo_actions::ConfirmationPolicy::None
                },
                retry: tilo_actions::RetryPolicy::AtMostOnce,
                ..tilo_actions::ActionContract::default()
            }),
            on_open: Rc::new(|_, _, _, _| {
                Err(ActionError::Refused(
                    "Use the declared device shell operation".into(),
                ))
            }),
            on_invoke: None,
            on_begin_invoke: Some(Rc::new(move |_, _, chrome, args| {
                if !is_active_workspace_owner(&chrome.borrow()) {
                    return Err(ActionError::Explained(crate::shell::i18n::t(
                        "terminal-device-shell-owner-required",
                    )));
                }
                let request = operation_from_args(operation, args)?;
                provider.begin(connection, request)
            })),
        }
    }

    fn begin(
        &self,
        connection: u64,
        operation: DeviceShellOperation,
    ) -> Result<InvocationDisposition, ActionError> {
        let (request, reply) = {
            let mut connections = self.connections.borrow_mut();
            if connections.pending.len() >= 32 {
                return Err(ActionError::Refused(
                    "Device shell operations are busy".into(),
                ));
            }
            let writer = connections
                .writers
                .get(&connection)
                .cloned()
                .ok_or_else(|| ActionError::Refused("This Terminal session has closed".into()))?;
            let request = connections.next.checked_add(1).ok_or_else(|| {
                ActionError::Refused("Device shell operations are unavailable".into())
            })?;
            connections.next = request;
            let reply = Rc::new(RefCell::new(None));
            let mut frame = serde_json::to_vec(&FloatingSurfaceHostMessage::DeviceShellOperation {
                request,
                operation,
            })
            .map_err(|_| ActionError::Refused("Could not request the device shell".into()))?;
            frame.push(b'\n');
            writer
                .send(frame)
                .map_err(|_| ActionError::Refused("This Terminal session has closed".into()))?;
            connections
                .pending
                .insert((connection, request), reply.clone());
            (request, reply)
        };
        let provider = self.clone();
        let started = Instant::now();
        let invocation = tilo_actions::InvocationId::new();
        Ok(InvocationDisposition::Pending(Rc::new(move || {
            if let Some(result) = reply.borrow().clone() {
                return result
                    .map(|value| {
                        Some(ActionReceipt {
                            broker_invocation_id: None,
                            update: tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: tilo_actions::ActionOutput {
                                    value: Some(value),
                                    resources: Vec::new(),
                                },
                            },
                        })
                    })
                    .map_err(ActionError::Refused);
            }
            if started.elapsed() >= Duration::from_secs(10) {
                provider
                    .connections
                    .borrow_mut()
                    .pending
                    .remove(&(connection, request));
                return Err(ActionError::Refused(
                    "Terminal did not acknowledge this operation".into(),
                ));
            }
            Ok(None)
        })))
    }
}
fn operation_from_args(
    operation: &str,
    args: &crate::shell::search::action_api::ActionArgs,
) -> Result<DeviceShellOperation, ActionError> {
    let session = || {
        args.text("session")
            .map(str::to_owned)
            .filter(|value| uuid::Uuid::parse_str(value).is_ok())
            .ok_or_else(|| ActionError::Refused("Choose the exact current Terminal session".into()))
    };
    let request = match operation {
        "request" => DeviceShellOperation::Select,
        "inspect" => DeviceShellOperation::Inspect,
        "run" => DeviceShellOperation::Run {
            session: session()?,
            command: args
                .text("command")
                .filter(|command| command.len() <= 8192)
                .ok_or_else(|| ActionError::Refused("Provide one bounded command".into()))?
                .into(),
        },
        _ => DeviceShellOperation::Read {
            session: session()?,
        },
    };
    Ok(request)
}

impl SearchProvider for TerminalDeviceShellProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Device shell"
    }
    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-device-shell")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) || !is_active_workspace_owner(ctx.chrome) {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for &connection in self.connections.borrow().writers.keys() {
            for operation in ["request", "inspect", "run", "read"] {
                rows.push(self.row(connection, operation));
            }
        }
        let fields: Vec<_> = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (row.id.clone(), ui::search::FieldWeight::IDENTITY),
                    (PROVIDER_ID.into(), ui::search::FieldWeight::IDENTITY),
                    (
                        row.subtitle.clone().unwrap_or_default(),
                        ui::search::FieldWeight::SUBTITLE,
                    ),
                ]
            })
            .collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), rows, &fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ArgValue, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use crate::shell::state::WorkspaceTree;
    use automerge::{AutoCommit, ROOT};

    #[test]
    fn confirmed_owner_operations_wait_for_exact_connection_ack_and_fail_on_close() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        chrome.borrow_mut().test_only_owner_override = Some(true);
        let provider = TerminalDeviceShellProvider::default();
        let (writer, replies) = mpsc::channel();
        provider.connect(9, writer);
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(provider.clone()));
        let address = ResourceRef::new(PROVIDER_ID, "request@9");
        let catalog = ActionCatalog::build_exact(
            &registry,
            &address,
            SearchScope::All,
            None,
            &chrome.borrow(),
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        );
        let action = catalog.select(&address).unwrap();
        let mut doc = AutoCommit::new();
        assert!(
            action
                .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                .is_err()
        );
        assert!(replies.try_recv().is_err());
        let InvocationDisposition::Pending(poll) = action
            .invoke(
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT,
                &chrome,
                [("confirm".into(), ArgValue::Boolean(true))],
            )
            .unwrap()
        else {
            panic!("operation requires actual Terminal acknowledgement");
        };
        let frame = replies.recv().unwrap();
        let FloatingSurfaceHostMessage::DeviceShellOperation { request, operation } =
            serde_json::from_slice(&frame).unwrap()
        else {
            panic!("wrong owner wire message");
        };
        assert_eq!(operation, DeviceShellOperation::Select);
        assert!(poll().unwrap().is_none());
        provider.complete(8, request, Ok(serde_json::json!({"lifecycle":"open"})));
        assert!(
            poll().unwrap().is_none(),
            "another connection cannot settle it"
        );
        provider.complete(
            9,
            request,
            Ok(serde_json::json!({"lifecycle":"awaiting_allow"})),
        );
        assert!(poll().unwrap().is_some());
        assert!(
            chrome.borrow().pending_grants.is_empty(),
            "selection must not itself grant permission"
        );
        let InvocationDisposition::Pending(poll) =
            provider.begin(9, DeviceShellOperation::Inspect).unwrap()
        else {
            panic!("pending reply");
        };
        provider.disconnect(9);
        assert!(poll().is_err());
        assert!(provider.begin(9, DeviceShellOperation::Inspect).is_err());
    }

    #[test]
    fn unknown_duplicate_and_oversized_responses_cannot_overwrite_or_exhaust_owner_slots() {
        let provider = TerminalDeviceShellProvider::default();
        let (writer, replies) = mpsc::channel();
        provider.connect(1, writer);
        let InvocationDisposition::Pending(poll) =
            provider.begin(1, DeviceShellOperation::Inspect).unwrap()
        else {
            panic!("pending");
        };
        let FloatingSurfaceHostMessage::DeviceShellOperation { request, .. } =
            serde_json::from_slice(&replies.recv().unwrap()).unwrap()
        else {
            panic!("wire");
        };
        for id in 100..1000 {
            provider.complete(1, id, Ok(serde_json::Value::Null));
        }
        assert_eq!(provider.connections.borrow().pending.len(), 1);
        provider.complete(1, request, Err("actual failure".into()));
        provider.complete(1, request, Ok(serde_json::Value::Null));
        assert!(poll().unwrap_err().to_string().contains("actual failure"));
        let InvocationDisposition::Pending(poll) =
            provider.begin(1, DeviceShellOperation::Inspect).unwrap()
        else {
            panic!("pending");
        };
        let FloatingSurfaceHostMessage::DeviceShellOperation { request, .. } =
            serde_json::from_slice(&replies.recv().unwrap()).unwrap()
        else {
            panic!("wire");
        };
        provider.complete(1, request, Ok(serde_json::json!("x".repeat(65537))));
        assert!(poll().is_err());
        for _ in 0..32 {
            provider.begin(1, DeviceShellOperation::Inspect).unwrap();
        }
        assert!(provider.begin(1, DeviceShellOperation::Inspect).is_err());
        provider.disconnect(1);
        assert!(provider.connections.borrow().pending.is_empty());
    }
}
