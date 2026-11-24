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
            .ok_or_else(|| format!("{} publishes no {}", self.app, address.encode()))?;
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
        let (workspaces, app, caller) = (self.workspaces.clone(), self.app, caller.clone());
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
                        caller: &caller,
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
    use crate::workspaces::tests::{governor, test_host};
    use tilo_action_policy::{Answer, KindSet, Rule, TreePath};
    use tilo_actions::remote::ReceiptStatus;

    const CHAT: &str = "so.tilo.chat";

    fn records(workspaces: &Workspaces) -> BuiltInRecords {
        BuiltInRecords {
            workspaces: workspaces.clone(),
            app: CHAT,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// The `write_to` tl Chat offers `caller`, asked to tell `who` the words `body`.
    fn write_to(
        records: &BuiltInRecords,
        caller: &Caller,
        who: &tilo_actions::MemberId,
        body: &str,
    ) -> InvokeRequest {
        let offered: Vec<_> = records
            .rows(caller)
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
        let Ok(serde_json::Value::Object(arguments)) =
            serde_json::to_value(serde_json::json!({ "who": who, "body": body }))
        else {
            panic!("arguments are an object");
        };
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: format!("{RESOURCE_PROVIDER}:{CHAT}:"),
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
        }
    }

    /// Run `request` as `caller` through the provider the dispatch hands tl Chat's calls to.
    async fn run(
        workspaces: &Workspaces,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let chat = workspaces
            .providers()
            .get("record:so.tilo.chat:")
            .expect("tl Chat's records are served on this device");
        let receipt = chat
            .answers
            .run(caller, request, Admitted::default())
            .await
            .ok_or("the call was dropped")?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output),
            other => Err(format!("{other:?}")),
        }
    }

    /// The versions of the record's resources `write_to` made, as its history lists them.
    fn written(workspaces: &Workspaces, workspace: &str) -> Vec<serde_json::Value> {
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.to_owned(),
            document: String::new(),
        };
        workspaces
            .record_changes(workspace, &route)
            .unwrap_or_default()
            .into_iter()
            .filter(|version| {
                version["action"]
                    .as_str()
                    .is_some_and(|action| action.contains("write_to"))
            })
            .collect()
    }

    /// Everything `caller` is offered of tl Chat's record, as text to look for words in.
    fn seen(records: &BuiltInRecords, caller: &Caller) -> String {
        serde_json::to_string(&records.rows(caller)).unwrap()
    }

    /// tl Chat's record, which no window holds open on a server, is offered to its person and
    /// written as theirs through the provider the dispatch hands a call to: what an agent's
    /// reply and a person's `tilo do` reach.
    #[tokio::test]
    async fn a_built_in_apps_record_is_offered_and_written_with_no_window_open() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        register(&workspaces);

        let chat = workspaces
            .providers()
            .get("record:so.tilo.chat:")
            .expect("tl Chat's records are served on this device");
        assert_eq!(
            &*chat.app, "so.tilo.chat",
            "decided by tl Chat's own grants"
        );
        assert_eq!(chat.reach, Reach::Devices);

        let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        let request = write_to(
            &records(&workspaces),
            &caller,
            &tilo_actions::MemberId::agent("scout"),
            "The plan is ready",
        );
        let output = run(&workspaces, &caller, request)
            .await
            .expect("the write lands");
        assert!(
            !output.resources.is_empty(),
            "the conversation it wrote in is named: {output:?}"
        );
        server.shutdown().await;
    }

    /// This device writes a record's changes itself, so the record's own authority is what
    /// stops an agent: one its delegation does not give a write is refused, and nothing of it
    /// reaches the store; the same agent with the delegation writes, and the change is stamped
    /// as the agent's, never as the person's whose device this is.
    #[tokio::test]
    async fn an_agent_writes_a_record_only_within_its_delegation_and_as_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        register(&workspaces);
        let person = hex::encode(owner.user_public_key);
        workspaces
            .add_agent(
                &workspace,
                tilo_actions::MemberId::person(person.clone()),
                "scout",
                &person,
            )
            .await
            .expect("the owner adds their agent");
        let records = records(&workspaces);
        let ada = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        let scout = |bound: Vec<Rule>| {
            Caller::delegated_agent("scout", owner.user_public_key, [7; 32])
                .in_workspace(workspace.clone())
                .held_to(bound)
        };

        // Outside its delegation: its bound denies it everything in the workspace.
        let held = scout(vec![Rule::new(
            TreePath::workspace(&workspace).rest(),
            KindSet::ALL,
            Answer::Deny,
        )]);
        let to = tilo_actions::MemberId::person(person.clone());
        let request = write_to(&records, &held, &to, "Sold the house");
        let refused = run(&workspaces, &held, request).await;
        assert!(
            refused.is_err(),
            "an agent outside its delegation writes nothing: {refused:?}"
        );
        assert!(
            written(&workspaces, &workspace).is_empty(),
            "nothing of the refused write reaches the store"
        );
        assert!(
            !seen(&records, &ada).contains("Sold the house"),
            "nobody reads what was refused"
        );

        // Within it, the write lands as the agent's.
        let delegated = scout(Vec::new());
        let request = write_to(&records, &delegated, &to, "The plan is ready");
        run(&workspaces, &delegated, request)
            .await
            .expect("an agent within its delegation writes");
        let versions = written(&workspaces, &workspace);
        assert!(!versions.is_empty(), "the write reaches the store");
        let agent =
            serde_json::to_value(super::super::member(tilo_actions::MemberId::agent("scout")))
                .unwrap();
        let device_person =
            serde_json::to_value(super::super::member(tilo_actions::MemberId::person(person)))
                .unwrap();
        for version in &versions {
            assert_eq!(
                version["who"], agent,
                "the change is the agent's: {version}"
            );
            assert_ne!(
                version["who"], device_person,
                "never the device's person's: {version}"
            );
        }
        server.shutdown().await;
    }

    /// A member reads of tl Chat's record only what they may: a direct conversation between
    /// two others offers them nothing of it, though the device that serves it holds it all.
    #[tokio::test]
    async fn a_member_reads_nothing_of_a_conversation_they_are_not_in() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        register(&workspaces);
        let records = records(&workspaces);

        let cy_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        workspaces
            .add_member(
                &workspace,
                &owner,
                crate::workspaces::AddMemberRequest {
                    request_id: [9; 32],
                    user_public_key: cy_root.verifying_key().to_bytes(),
                    place: crate::workspaces::MemberPlace::Role(tilo_action_policy::Role::Viewer),
                    name: Some("Cy".into()),
                },
            )
            .await
            .expect("the owner adds Cy");

        let ada = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        let request = write_to(
            &records,
            &ada,
            &tilo_actions::MemberId::agent("scout"),
            "The safe's code is 4711",
        );
        run(&workspaces, &ada, request)
            .await
            .expect("Ada writes to her agent");
        assert!(
            seen(&records, &ada).contains("4711"),
            "the conversation's own person reads it"
        );

        let cy = Caller::authenticated(cy_root.verifying_key().to_bytes(), [4; 32])
            .in_workspace(workspace.clone());
        assert!(
            !seen(&records, &cy).contains("4711"),
            "a member outside the conversation reads nothing of it"
        );
        server.shutdown().await;
    }
}
