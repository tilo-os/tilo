//! The entries of the person's Passwords, as Passwords on this desktop reads and keeps them
//! ([`person_vault::ENTRIES`], [`person_vault::KEEP_ENTRY`], [`person_vault::FORGET_ENTRY`]).
//!
//! They are the person's own: Passwords' window asks for them as the person using it, and no
//! catalogue offers them. A workspace whose work a server takes keeps them on that server, in the
//! person's vault there, which every device of theirs and their browser pages read: this session
//! asks it as one of the person's devices, on the session it keeps with that server. A workspace
//! no server takes keeps them in this device's own vault, where Passwords on a desktop always
//! kept them.

use tilo_actions::remote::person_vault::{self, Entries, ForgetEntry, KeepEntry};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionFailure, ActionOutput};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_workspace_service::apps::services::HostedWorkspaces as _;
use tilo_workspace_service::workspaces::passwords;

/// The person's Passwords entries, as this session serves them to Passwords.
pub(crate) struct PersonVaultSteps {
    /// The servers of the workspaces placed away from this device. `None` for a session that holds
    /// no workspaces.
    hosting: Option<crate::hosted_workspaces::HostedWorkspaces>,
}

impl PersonVaultSteps {
    pub(crate) fn session(hosting: Option<crate::hosted_workspaces::HostedWorkspaces>) -> Self {
        Self { hosting }
    }
}

/// Step `action`, described for the broker. Only Passwords reaches it, and no catalogue lists it.
fn descriptor(action: &str, kind: tilo_actions::OperationKind) -> tilo_actions::ActionDescriptor {
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(person_vault::RESOURCE, action),
        resource_kind: person_vault::RESOURCE.into(),
        resource_label: "Passwords".into(),
        action_label: action.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::default(),
        kind,
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            },
            ..tilo_actions::ActionContract::view()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

fn failed(message: String) -> ActionFailure {
    ActionFailure {
        code: "refused".to_owned(),
        message,
        details: None,
        retryable: false,
    }
}

/// What the person asked of their entries, read from the step's argument.
enum Ask {
    Entries,
    Keep(KeepEntry),
    Forget(ForgetEntry),
}

impl Ask {
    /// `argument` is the one string the step is handed: the typed object behind its prefix.
    fn of(action: &str, argument: Option<&str>) -> Result<Self, String> {
        let argument = || match InvokeArgument::from_slot(argument) {
            InvokeArgument::Json(object) => serde_json::Value::Object(object),
            _ => serde_json::Value::Null,
        };
        match action {
            person_vault::ENTRIES => Ok(Self::Entries),
            person_vault::KEEP_ENTRY => serde_json::from_value(argument())
                .map(Self::Keep)
                .map_err(|_| "an entry to keep is an entry of Passwords".to_owned()),
            person_vault::FORGET_ENTRY => serde_json::from_value(argument())
                .map(Self::Forget)
                .map_err(|_| "an entry to forget is named by its id".to_owned()),
            _ => Err("no such step of Passwords' entries".to_owned()),
        }
    }

    /// Answer it from this device's own vault.
    fn on_this_device(self) -> Result<Entries, String> {
        let mut store = accounts::SecretStore::open();
        if let Some(error) = store.recovery_error() {
            return Err(format!("Your vault could not be opened: {error}"));
        }
        match self {
            Self::Entries => passwords::entries(&store),
            Self::Keep(keeping) => passwords::keep_entry(&mut store, keeping),
            Self::Forget(forgetting) => passwords::forget_entry(&mut store, forgetting),
        }
    }

    /// The same ask, of the person's vault on the workspace's server.
    fn request(&self, action: &str) -> Result<InvokeRequest, String> {
        let argument = match self {
            Self::Entries => InvokeArgument::None,
            Self::Keep(keeping) => json_argument(keeping)?,
            Self::Forget(forgetting) => json_argument(forgetting)?,
        };
        Ok(InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: person_vault::target(action),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: Some(20_000),
            appearance: None,
        })
    }
}

fn json_argument(value: &impl serde::Serialize) -> Result<InvokeArgument, String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(map)) => Ok(InvokeArgument::Json(map)),
        _ => Err("The entry could not be sent".to_owned()),
    }
}

fn output(entries: &Entries) -> HostStepOutcome {
    serde_json::to_value(entries)
        .map(|value| ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|error| Box::new(failed(error.to_string())))
}

/// What the workspace's server answered, as this step's outcome.
fn answered(
    receipt: Option<Result<tilo_actions::remote::InvocationReceipt, ActionCallError>>,
) -> HostStepOutcome {
    match receipt {
        Some(Ok(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output),
            ReceiptStatus::Failed { error } => Err(Box::new(error)),
            ReceiptStatus::Rejected { error } => Err(Box::new(failed(error.to_string()))),
            _ => Err(Box::new(failed(
                "The workspace's server did not finish keeping your passwords".to_owned(),
            ))),
        },
        Some(Err(error)) => Err(Box::new(failed(error.to_string()))),
        None => Err(Box::new(failed(
            "The workspace's server did not answer about your passwords".to_owned(),
        ))),
    }
}

impl HostStepServices for PersonVaultSteps {
    fn provider(&self) -> &'static str {
        person_vault::PROVIDER
    }

    /// Passwords' window reaches them, as the person using it.
    fn app_id(&self) -> &'static str {
        tilo_app_id::apps::PASSWORDS
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        use tilo_actions::OperationKind::{Effect, View};
        vec![
            descriptor(person_vault::ENTRIES, View),
            descriptor(person_vault::KEEP_ENTRY, Effect),
            descriptor(person_vault::FORGET_ENTRY, Effect),
        ]
    }

    /// No catalogue offers them: a secret of the vault comes back to its own person alone.
    fn listed(&self) -> bool {
        false
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.actor.direct_person().is_none() {
            return Err("only the person reads and keeps their passwords".into());
        }
        let action = request.address.action.as_str();
        let ask = Ask::of(action, request.argument)?;
        if let Some(server) = self
            .hosting
            .as_ref()
            .and_then(|hosting| hosting.host_actions(request.workspace))
        {
            let asked = server.invoke(&Caller::local_person("tilo"), ask.request(action)?);
            let (done, outcome) = tilo_actions::remote::pending();
            asked.then(move |receipt| {
                done.complete(answered(receipt));
            });
            return Ok(outcome);
        }
        // The device's vault is a file and the keyring that seals it: read off the broker's
        // thread.
        let (done, outcome) = tilo_actions::remote::pending();
        std::thread::spawn(move || {
            done.complete(
                ask.on_this_device()
                    .map_err(|why| Box::new(failed(why)))
                    .and_then(|entries| output(&entries)),
            );
        });
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::{Ask, InvokeArgument, person_vault};

    /// The broker hands a step its argument as one string, the typed object behind its prefix.
    #[test]
    fn an_entry_to_keep_reads_from_the_string_the_broker_hands_over() {
        let mut object = serde_json::Map::new();
        object.insert("entry".into(), serde_json::json!({ "id": "a" }));
        let handed = InvokeArgument::Json(object).to_slot();
        assert!(matches!(
            Ask::of(person_vault::KEEP_ENTRY, handed.as_deref()),
            Ok(Ask::Keep(_))
        ));
        let mut object = serde_json::Map::new();
        object.insert("id".into(), serde_json::json!("a"));
        let handed = InvokeArgument::Json(object).to_slot();
        assert!(matches!(
            Ask::of(person_vault::FORGET_ENTRY, handed.as_deref()),
            Ok(Ask::Forget(_))
        ));
        assert!(Ask::of(person_vault::KEEP_ENTRY, None).is_err());
    }
}
