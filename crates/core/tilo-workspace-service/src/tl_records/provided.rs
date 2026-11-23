//! The records of the tilolang apps built into this build, served on this device to whoever the
//! device's dispatch admits: a hosted agent's tools, a person's `tilo do`, a view's button.
//!
//! Such an app (tl Chat, Tasks) is no catalogue app and no installed package, so no window of it
//! answers for its record where nobody has it open. Each is registered under the provider a
//! record of it is named by (`record:<app>:`) with the application whose grants decide its
//! actions ([`register`]). The dispatch admits a call against the workspace's policy, asks the
//! person where the action's contract says so, and hands it here; this lists what the record
//! publishes, as the person the caller acts for reads it, and runs the published action through
//! the app's own package: a read answers the core read as that person reads it (a direct
//! conversation's messages only for its people), and a write lands stamped with the caller as its
//! invoker ([`Workspaces::apply_record_action`]).

use std::sync::{Arc, OnceLock};

use serde_json::Map;
use tilo_actions::remote::admitted::settled;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Outcome, Pending,
    RESOURCE_PROVIDER, pending,
};
use tilo_actions::{ActionContext, ActionFailure, ActionInvoker, OperationKind};
use tilo_peer_state::document_frame::DocumentRoute;

use super::{RecordCall, built_in_apps};
use crate::workspaces::Workspaces;
use crate::workspaces::providers::Reach;

/// One built-in app's records, as this device serves them.
pub(crate) struct BuiltInRecords {
    workspaces: Workspaces,
    /// The app, as its package and its records name it.
    app: &'static str,
    /// Where a write waits for the workspace to take it.
    runtime: tokio::runtime::Handle,
}

/// The provider and app names of the built-in apps, made once: a registration keeps them for as
/// long as the process runs.
fn names() -> &'static [(&'static str, &'static str)] {
    static NAMES: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        built_in_apps()
            .into_iter()
            .map(|app| {
                let provider: &'static str =
                    Box::leak(format!("{RESOURCE_PROVIDER}:{app}:").into_boxed_str());
                (provider, &*Box::leak(app.into_boxed_str()))
            })
            .collect()
    })
}

/// Serve the records of every app built into this build on this device from now on, each
/// decided by the grants of its own app.
///
/// # Panics
/// Called outside a reactor: a record's writes wait on the one it was registered on.
pub fn register(workspaces: &Workspaces) {
    let runtime = tokio::runtime::Handle::current();
    let providers = workspaces.providers();
    for (provider, app) in names() {
        providers.provide(
            provider,
            app,
            Reach::Devices,
            Arc::new(BuiltInRecords {
                workspaces: workspaces.clone(),
                app,
                runtime: runtime.clone(),
            }),
        );
    }
}

impl BuiltInRecords {
    /// The app's record of `workspace`.
    fn route(&self, workspace: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: self.app.to_owned(),
            cluster_id: workspace.to_owned(),
            document: String::new(),
        }
    }

    /// What the record publishes, as the person `caller` acts for reads it: an agent's person,
    /// or the person themselves.
    fn rows(&self, caller: &Caller) -> Vec<tilo_actions::remote::CatalogueRow> {
        let (Some(workspace), Some(person)) = (caller.workspace(), person_of(caller)) else {
            return Vec::new();
        };
        self.workspaces
            .record_rows(workspace, &self.route(workspace), &person)
            .unwrap_or_default()
    }

    /// Run `request`, which the dispatch admitted for `caller`, through the app's package.
    fn start(&self, caller: &Caller, request: &InvokeRequest) -> Result<Pending<Outcome>, String> {
        let workspace = caller
            .workspace()
            .ok_or("a record is changed in a workspace")?
            .to_owned();
        let person = person_of(caller).ok_or("a record is read as a person")?;
        let address = &request.target.address;
        let descriptor = self
            .rows(caller)
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| &action.address == address)
            .ok_or_else(|| format!("{} publishes no {address}", self.app))?;
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => return Err("a record's action takes named arguments".into()),
        };
        let invoker =
            ActionInvoker::admitted(caller.acting()).named(caller.agent_name().map(str::to_owned));
        let context: ActionContext = request.context.clone().unwrap_or_default();
        let read = descriptor.kind == OperationKind::View
            || descriptor.contract.kind == tilo_actions::ActionKind::Read;
        let (workspaces, app) = (self.workspaces.clone(), self.app);
        let viewer = super::member(tilo_actions::MemberId::person(person));
        let (done, outcome) = pending();
        self.runtime.spawn(async move {
            let applied = workspaces
                .apply_record_action(
                    &workspace,
                    RecordCall {
                        app,
                        viewer,
                        invoker: &invoker,
                        context: &context,
                        address: &descriptor.address,
                        arguments: &arguments,
                        read,
                    },
                )
                .await;
            done.complete(applied.map_err(|error| {
                Box::new(ActionFailure {
                    code: "record_action_failed".into(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                })
            }));
        });
        Ok(outcome)
    }
}

/// The person `caller` acts for: the person an agent works for, or the person calling.
fn person_of(caller: &Caller) -> Option<String> {
    let acting = caller.acting();
    acting
        .acted_for_person()
        .or_else(|| acting.person_id())
        .map(str::to_owned)
}

impl ActionProvider for BuiltInRecords {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // A page of what the record publishes, as the person the caller acts for reads it: the
        // rows the query admits from its cursor on, never every row of the record at once.
        let page = match (caller.workspace(), person_of(caller)) {
            (Some(workspace), Some(person)) => {
                self.workspaces
                    .record_page(workspace, &self.route(workspace), &person, &query)
            }
            _ => None,
        };
        Pending::ready(Ok(page.unwrap_or_else(|| CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        })))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        _admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use crate::workspaces::actions::tests::governed;
    use crate::workspaces::tests::{governor, test_host};

    /// tl Chat's record, which no window holds open on a server, is offered to its person and
    /// written as theirs through the provider the dispatch hands a call to: what an agent's
    /// reply and a person's `tilo do` reach.
    #[tokio::test]
    async fn a_built_in_apps_record_is_offered_and_written_with_no_window_open() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let (workspace, _, _) = governed(&workspaces).await;
        register(&workspaces);

        let chat = workspaces
            .providers()
            .get("record:so.tilo.chat:")
            .expect("tl Chat's records are served on this device");
        assert_eq!(chat.app, "so.tilo.chat", "decided by tl Chat's own grants");
        assert_eq!(chat.reach, Reach::Devices);

        let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        let records = BuiltInRecords {
            workspaces: workspaces.clone(),
            app: "so.tilo.chat",
            runtime: tokio::runtime::Handle::current(),
        };
        let offered: Vec<_> = records
            .rows(&caller)
            .into_iter()
            .filter_map(|row| row.action)
            .collect();
        let write_to = offered
            .iter()
            .find(|action| action.address.action == "write_to")
            .unwrap_or_else(|| {
                panic!(
                    "Chat's actions are offered: {:?}",
                    offered.iter().map(|a| &a.address).collect::<Vec<_>>()
                )
            });

        let Ok(serde_json::Value::Object(arguments)) = serde_json::to_value(serde_json::json!({
            "who": tilo_actions::MemberId::agent("scout"),
            "body": "The plan is ready",
        })) else {
            panic!("arguments are an object");
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: chat.provider.to_owned(),
                address: write_to.address.clone(),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = chat
            .answers
            .run(&caller, request, Admitted::default())
            .await
            .expect("the write is answered")
            .expect("the write is taken");
        let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the write lands: {:?}", receipt.status);
        };
        assert!(
            !output.resources.is_empty(),
            "the conversation it wrote in is named: {output:?}"
        );
        server.shutdown().await;
    }
}
