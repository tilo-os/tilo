//! A stand-in for the person's vault on their workspace's server, for tests: it answers the
//! requests Passwords sends as the server does, from entries it keeps in memory, and serves
//! every window and model made on this thread while it lives.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ui::lock::MutexRecover as _;
use ui::tilo_actions::ActionOutput;
use ui::tilo_actions::remote::person_vault::{self, Entries, ForgetEntry, KeepEntry};
use ui::tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeArgument, Pending,
    ReceiptStatus, TestProvider,
};

/// The entries the stand-in keeps, by id.
pub(crate) type Kept = Arc<Mutex<BTreeMap<String, serde_json::Value>>>;

pub(crate) struct ServedVault {
    pub(crate) kept: Kept,
    _served: ui::RemoteActionsGuard,
}

/// Serve the vault on this thread until the answer drops.
pub(crate) fn served() -> ServedVault {
    let kept = Kept::default();
    let answering = Arc::clone(&kept);
    let provider = TestProvider::new().answering(move |_, request| {
        let argument = match &request.argument {
            InvokeArgument::Json(map) => serde_json::Value::Object(map.clone()),
            _ => serde_json::Value::Null,
        };
        let answered = answer(&answering, &request.target.address.action, argument);
        Pending::ready(answered.map(|value| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    ..Default::default()
                },
            },
        }))
    });
    let provider: Arc<dyn ActionProvider + Send + Sync> = Arc::new(provider);
    ServedVault {
        kept,
        _served: ui::RemoteActions::enter(provider, Caller::local_person("ada")),
    }
}

fn answer(
    kept: &Kept,
    action: &str,
    argument: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let invalid = || ActionCallError::InvalidRequest {
        reason: "not a request of the vault".into(),
    };
    let mut entries = kept.lock_recover();
    match action {
        person_vault::ENTRIES => {}
        person_vault::KEEP_ENTRY => {
            let keeping: KeepEntry = serde_json::from_value(argument).map_err(|_| invalid())?;
            let id = keeping.entry["id"].as_str().ok_or_else(invalid)?.to_owned();
            entries.insert(id, keeping.entry);
        }
        person_vault::FORGET_ENTRY => {
            let forgetting: ForgetEntry =
                serde_json::from_value(argument).map_err(|_| invalid())?;
            entries.remove(&forgetting.id);
        }
        _ => return Err(ActionCallError::UnknownAction),
    }
    serde_json::to_value(Entries {
        entries: entries.values().cloned().collect(),
    })
    .map_err(|_| invalid())
}
