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

use tilo_lock::MutexRecover as _;

use serde_json::Map;
use tilo_actions::remote::admitted::settled;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Outcome, Pending,
    RESOURCE_PROVIDER, pending,
};
use tilo_actions::{ActionContext, ActionFailure, ActionInvoker, OperationKind};

use super::{RecordCall, built_in, built_in_apps, member, runs_elsewhere, with_tl_record};
use tilo_workspace_service::workspaces::DocumentRoute;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::Reach;

/// One built-in app's records, as this device serves them.
#[derive(Clone)]
pub(crate) struct BuiltInRecords {
    workspaces: Workspaces,
    /// The app, as its package and its records name it.
    app: &'static str,
    /// Where a write waits for the workspace to take it.
    runtime: tokio::runtime::Handle,
    /// The workspaces whose record of the app has its root, as opening the app makes it.
    rooted: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
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
                rooted: Arc::default(),
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
        let address = request.target.address.clone();
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => return Err("a record's action takes named arguments".into()),
        };
        let invoker =
            ActionInvoker::admitted(caller.acting()).named(caller.agent_name().map(str::to_owned));
        let context: ActionContext = request.context.clone().unwrap_or_default();
        let (records, caller) = (self.clone(), caller.clone());
        let (done, outcome) = pending();
        self.runtime.spawn(async move {
            let applied = records
                .apply(
                    &workspace,
                    &person,
                    &caller,
                    (&address, &arguments),
                    (&invoker, &context),
                )
                .await;
            done.complete(applied.map_err(|message| {
                Box::new(ActionFailure {
                    code: "record_action_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                })
            }));
        });
        Ok(outcome)
    }

    /// Run the action at `address` with `arguments` as `caller`, who acts for `person`, once the
    /// record has its root: what the record publishes is decided by the app's package.
    async fn apply(
        &self,
        workspace: &str,
        person: &str,
        caller: &Caller,
        (address, arguments): (
            &tilo_actions::ActionAddress,
            &Map<String, serde_json::Value>,
        ),
        (invoker, context): (&ActionInvoker, &ActionContext),
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.rooted(workspace, person).await?;
        let descriptor = self
            .rows(caller)
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| &action.address == address)
            .ok_or_else(|| format!("{} publishes no {}", self.app, address.encode()))?;
        let read = descriptor.kind == OperationKind::View
            || descriptor.contract.kind == tilo_actions::ActionKind::Read;
        self.workspaces
            .apply_record_action(
                workspace,
                RecordCall {
                    app: self.app,
                    viewer: member(tilo_actions::MemberId::person(person.to_owned())),
                    invoker,
                    caller,
                    context,
                    address: &descriptor.address,
                    arguments,
                    read,
                },
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Make the app's root in `workspace`'s record where it has none yet, as a window of the app
    /// makes it when it opens: what the app's actions on its whole (Chat's `write_to` on its
    /// conversations) are invoked on, on a device where nobody opened the app.
    ///
    /// # Errors
    /// The record could not be opened or written.
    async fn rooted(&self, workspace: &str, person: &str) -> Result<(), String> {
        if self.rooted.lock_recover().contains(workspace) {
            return Ok(());
        }
        let packages = built_in(self.app)?.ok_or_else(|| runs_elsewhere(self.app))?;
        let app = self.app;
        // A record no device wrote yet has no channel to seal its first write for: this device
        // founds it, as a window opening the record would. One it may not found is founded
        // elsewhere, and the write says so if it still has none.
        if let Some(Err(error)) = self
            .workspaces
            .own_route_keys(&self.route(workspace), true)
            .await
        {
            tracing::debug!(%error, app, "the record's channel is not founded here");
        }
        let viewer = member(tilo_actions::MemberId::person(person.to_owned()));
        with_tl_record(
            &self.workspaces,
            workspace,
            &packages,
            (viewer, person),
            move |opened| {
                opened
                    .host()
                    .root_of(app)
                    .map(drop)
                    .map_err(|error| format!("{app}'s root is not made: {error}"))
            },
        )
        .await?;
        self.rooted.lock_recover().insert(workspace.to_owned());
        Ok(())
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
        // rows the query admits from its cursor on, never every row of the record at once. The
        // record has its root first, so what the app offers on its whole is offered.
        let nothing = || CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        };
        let (Some(workspace), Some(person)) =
            (caller.workspace().map(str::to_owned), person_of(caller))
        else {
            return Pending::ready(Ok(nothing()));
        };
        let records = self.clone();
        let (done, page) = pending();
        self.runtime.spawn(async move {
            if let Err(error) = records.rooted(&workspace, &person).await {
                tracing::warn!(app = records.app, %workspace, %error, "a built-in app's record has no root");
            }
            let page = records.workspaces.record_page(
                &workspace,
                &records.route(&workspace),
                &person,
                &query,
            );
            done.complete(Ok(page.unwrap_or_else(nothing)));
        });
        page
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

/// Calling a built-in app's actions the way a person's `tilo do` or an agent's turn does, for
/// the tests of what reads the record afterwards.
pub mod calls {
    #![allow(clippy::expect_used, reason = "a fixture failing is its test failing")]

    use super::{
        Admitted, Arc, BuiltInRecords, Caller, InvokeArgument, InvokeRequest, RESOURCE_PROVIDER,
        Workspaces, person_of,
    };

    /// Run the first `action` `app`'s record offers `caller`, with `arguments`, through the
    /// provider the dispatch hands the app's calls to: what it answered, or why not.
    ///
    /// # Panics
    /// The record offers `caller` no such action, or the app's records are not served here.
    pub async fn call(
        workspaces: &Workspaces,
        caller: &Caller,
        app: &'static str,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let records = BuiltInRecords {
            workspaces: workspaces.clone(),
            app,
            runtime: tokio::runtime::Handle::current(),
            rooted: Arc::default(),
        };
        let workspace = caller.workspace().expect("a caller in a workspace");
        let person = person_of(caller).expect("a caller acting as a person");
        records.rooted(workspace, &person).await?;
        let offered: Vec<_> = records
            .rows(caller)
            .into_iter()
            .filter_map(|row| row.action)
            .collect();
        let address = offered
            .iter()
            .find(|offered| offered.address.action == action)
            .map(|offered| offered.address.clone())
            .unwrap_or_else(|| {
                panic!(
                    "{app} offers {action}: {:?}",
                    offered.iter().map(|o| &o.address).collect::<Vec<_>>()
                )
            });
        let serde_json::Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: format!("{RESOURCE_PROVIDER}:{app}:"),
                address,
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
        let served = workspaces
            .providers()
            .get(&format!("{RESOURCE_PROVIDER}:{app}:"))
            .expect("the app's records are served on this device");
        let receipt = served
            .answers
            .run(caller, request, Admitted::default())
            .await
            .ok_or("the call was dropped")?
            .map_err(|error| error.to_string())?;
        match receipt.status {
            tilo_actions::remote::ReceiptStatus::Completed { output } => Ok(output),
            other => Err(format!("{other:?}")),
        }
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
    use tilo_action_policy::{Answer, KindSet, TreePath};
    use tilo_actions::remote::ReceiptStatus;
    use tilo_workspace_service::workspaces::fixtures::{governor, test_host};

    const CHAT: &str = "so.tilo.chat";

    /// Chat's surface, declared as the device that runs the app names it, so a write to its
    /// record is judged by the types this build carries.
    fn declare_chat(workspaces: &Workspaces) {
        let packages = built_in(CHAT).unwrap().expect("Chat is built in");
        workspaces.declare_package_surface(
            CHAT,
            &packages.surface(CHAT).expect("Chat declares its surface"),
        );
    }

    fn records(workspaces: &Workspaces) -> BuiltInRecords {
        BuiltInRecords {
            workspaces: workspaces.clone(),
            app: CHAT,
            runtime: tokio::runtime::Handle::current(),
            rooted: Arc::default(),
        }
    }

    /// The `write_to` tl Chat offers `caller`, asked to tell `who` the words `body`.
    async fn write_to(
        records: &BuiltInRecords,
        caller: &Caller,
        who: &tilo_actions::MemberId,
        body: &str,
    ) -> InvokeRequest {
        // What a caller is offered is read once the record has its root, as a query reads it.
        records
            .rooted(caller.workspace().unwrap(), &person_of(caller).unwrap())
            .await
            .expect("Chat's root is made");
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

    /// The versions of the record's resources `write_to` made that `person` may read, as its
    /// history lists them.
    fn written(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<serde_json::Value> {
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.to_owned(),
            document: String::new(),
        };
        // History keeps the id Chat's declaration gives each action that wrote, which a rename
        // leaves alone: `write_to` writes through the actions it runs, finding the conversation
        // with the person and sending to it.
        let chat = built_in(CHAT).unwrap().expect("Chat is built in");
        workspaces
            .record_changes(workspace, &route, person)
            .unwrap_or_default()
            .into_iter()
            .filter(|version| {
                version["action"]
                    .as_str()
                    .and_then(|id| chat.action_name(id))
                    .is_some_and(|name| {
                        ["write_to", "message_person", "send"].contains(&name.as_str())
                    })
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
        workspaces.provide_tilolang(crate::host());
        declare_chat(&workspaces);
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
        )
        .await;
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
        workspaces.provide_tilolang(crate::host());
        declare_chat(&workspaces);
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        register(&workspaces);
        let person = hex::encode(owner.user_public_key);
        let records = records(&workspaces);
        let ada = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        // The workspace's roster bound it, so its certificate's delegation reaches it.
        let scout = || {
            Caller::delegated_agent("scout", owner.user_public_key, [7; 32])
                .in_workspace(workspace.clone())
                .on_roster(1, owner.user_public_key)
        };

        // Chat's record is in use before the workspace narrows its agent, its channel's keys
        // made as a workspace's are when it starts.
        records
            .rooted(&workspace, &person)
            .await
            .expect("Chat's root is made");

        // Outside its delegation: the workspace denies it everything.
        tilo_workspace_service::workspaces::fixtures::record_grants(
            &workspaces,
            &workspace,
            &[tilo_action_policy::Grant::new(
                "scout-denied",
                tilo_action_policy::Grantee::agent("scout"),
                TreePath::workspace(&workspace).rest(),
                KindSet::ALL,
                Answer::Deny,
                tilo_action_policy::Grantee::person(person.clone()),
            )],
        )
        .await;
        let held = scout();
        let to = tilo_actions::MemberId::person(person.clone());
        let request = write_to(&records, &held, &to, "Sold the house").await;
        let refused = run(&workspaces, &held, request).await;
        assert!(
            refused.is_err(),
            "an agent outside its delegation writes nothing: {refused:?}"
        );
        assert!(
            written(&workspaces, &workspace, &person).is_empty(),
            "nothing of the refused write reaches the store"
        );
        assert!(
            !seen(&records, &ada).contains("Sold the house"),
            "nobody reads what was refused"
        );

        // Within it, once the workspace takes its deny back and grants the agent its person's
        // undoable writes, the write lands as the agent's.
        tilo_workspace_service::workspaces::fixtures::revoke_grants(
            &workspaces,
            &workspace,
            &[tilo_action_policy::GrantId::new("scout-denied")],
        )
        .await;
        tilo_workspace_service::workspaces::fixtures::record_grants(
            &workspaces,
            &workspace,
            &[tilo_action_policy::Grant::new(
                "scout-delegated",
                tilo_action_policy::Grantee::agent("scout"),
                TreePath::workspace(&workspace),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                tilo_action_policy::Grantee::person(person.clone()),
            )],
        )
        .await;
        let delegated = scout();
        let request = write_to(&records, &delegated, &to, "The plan is ready").await;
        run(&workspaces, &delegated, request)
            .await
            .expect("an agent within its delegation writes");
        let versions = written(&workspaces, &workspace, &person);
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.clone(),
            document: String::new(),
        };
        assert!(
            !versions.is_empty(),
            "the write reaches the store; its history is {:?}",
            workspaces
                .record_changes(&workspace, &route, &person)
                .unwrap_or_default()
                .iter()
                .map(|version| (version["action"].clone(), version["who"].clone()))
                .collect::<Vec<_>>()
        );
        let agent = serde_json::to_value(member(tilo_actions::MemberId::agent("scout"))).unwrap();
        let device_person =
            serde_json::to_value(member(tilo_actions::MemberId::person(person))).unwrap();
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
        workspaces.provide_tilolang(crate::host());
        declare_chat(&workspaces);
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        register(&workspaces);
        let records = records(&workspaces);

        let cy_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        workspaces
            .add_member(
                &workspace,
                &owner,
                tilo_workspace_service::workspaces::AddMemberRequest {
                    request_id: [9; 32],
                    user_public_key: cy_root.verifying_key().to_bytes(),
                    place: tilo_workspace_service::workspaces::MemberPlace::Role(
                        tilo_action_policy::Role::Viewer,
                    ),
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
        )
        .await;
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
        let cy_person = hex::encode(cy_root.verifying_key().to_bytes());
        let found = workspaces
            .record_entries(&workspace, CHAT, None, &cy_person)
            .expect("Cy's search reads the record");
        assert!(
            !format!("{found:?}").contains("4711"),
            "search finds nothing of it for a member outside the conversation"
        );
        let ada_person = hex::encode(owner.user_public_key);
        let found = workspaces
            .record_entries(&workspace, CHAT, None, &ada_person)
            .expect("Ada's search reads the record");
        assert!(
            format!("{found:?}").contains("4711"),
            "search finds it for the conversation's own person"
        );
        // Every level of the record's pages as `person` reads them, from the top down.
        let page = |person: &str| {
            let route = records.route(&workspace);
            let mut parents = vec![None::<String>];
            let mut shown = String::new();
            let mut walked = std::collections::BTreeSet::new();
            while let Some(parent) = parents.pop() {
                let mut query = tilo_actions::remote::CatalogueQuery {
                    parent_resource: parent,
                    ..tilo_actions::remote::CatalogueQuery::default()
                };
                loop {
                    let page = workspaces
                        .record_page(&workspace, &route, person, &query)
                        .expect("the record's page reads");
                    shown.push_str(&serde_json::to_string(&page.rows).unwrap());
                    for row in page.rows.iter().filter(|row| row.has_children) {
                        if walked.insert(row.resource.id.clone()) {
                            parents.push(Some(row.resource.id.clone()));
                        }
                    }
                    if page.next_cursor.is_empty() {
                        break;
                    }
                    query.cursor = page.next_cursor;
                }
            }
            shown
        };
        assert!(
            !page(&cy_person).contains("4711"),
            "a page of the record shows nothing of it to a member outside the conversation"
        );
        assert!(
            page(&ada_person).contains("4711"),
            "a page of the record shows it to the conversation's own person"
        );
        server.shutdown().await;
    }

    /// Search over Tasks, read with no window of it open, finds a workspace's task for its
    /// members and nothing for a person who is not one: the record reads as the person searching,
    /// never as the person whose device serves it.
    #[tokio::test]
    async fn search_finds_nothing_of_another_apps_record_for_a_person_outside_the_workspace() {
        use tilolang_tilo::runtime::Value;
        const TASKS: &str = "so.tilo.tasks";
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        workspaces.provide_tilolang(crate::host());
        declare_chat(&workspaces);
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let ada_person = hex::encode(owner.user_public_key);
        let tasks = built_in(TASKS).unwrap().expect("Tasks is built in");
        // The record's channel is founded here, as a window opening it first would.
        let tasks_records = BuiltInRecords {
            workspaces: workspaces.clone(),
            app: TASKS,
            runtime: tokio::runtime::Handle::current(),
            rooted: Arc::default(),
        };
        tasks_records
            .rooted(&workspace, &ada_person)
            .await
            .expect("Tasks' record has its root");
        let ada = member(tilo_actions::MemberId::person(ada_person.clone()));
        let who = ada.clone();
        with_tl_record(
            &workspaces,
            &workspace,
            &tasks,
            (ada, &ada_person),
            move |opened| {
                opened.open_root()?;
                opened
                    .invoke(
                        &who,
                        (TASKS, "new_tasks"),
                        vec![
                            Value::List(vec![Value::Text("Quokka census".to_owned())]),
                            Value::Opt(None),
                        ],
                        None,
                    )
                    .map(drop)
            },
        )
        .await
        .expect("Ada writes a task");

        let found = |person: &str| {
            let found = workspaces
                .record_entries(&workspace, TASKS, None, person)
                .expect("search reads Tasks' record");
            format!("{found:?}")
        };
        assert!(
            found(&ada_person).contains("Quokka census"),
            "search finds the task for a member"
        );
        let stranger = hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[5; 32])
                .verifying_key()
                .to_bytes(),
        );
        assert!(
            !found(&stranger).contains("Quokka census"),
            "search finds nothing of it for a person outside the workspace"
        );

        // `new_tasks` targets no resource, and the caller's grants still decide it: a viewer of
        // the workspace adds no task.
        let cy_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        workspaces
            .add_member(
                &workspace,
                &owner,
                tilo_workspace_service::workspaces::AddMemberRequest {
                    request_id: [9; 32],
                    user_public_key: cy_root.verifying_key().to_bytes(),
                    place: tilo_workspace_service::workspaces::MemberPlace::Role(
                        tilo_action_policy::Role::Viewer,
                    ),
                    name: Some("Cy".into()),
                },
            )
            .await
            .expect("the owner adds Cy");
        let ada_caller = Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_workspace(workspace.clone());
        let new_tasks = tasks_records
            .rows(&ada_caller)
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| action.address.action == "new_tasks")
            .expect("Tasks offers new_tasks");
        let cy = Caller::authenticated(cy_root.verifying_key().to_bytes(), [4; 32])
            .in_workspace(workspace.clone());
        let receipt = tasks_records
            .run(
                &cy,
                InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: format!("{RESOURCE_PROVIDER}:{TASKS}:"),
                        address: new_tasks.address,
                    },
                    argument: InvokeArgument::Json(
                        serde_json::json!({ "titles": ["Wombat survey"] })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                    signed: None,
                },
                Admitted::default(),
            )
            .await
            .expect("the call is answered");
        assert!(
            !matches!(
                &receipt,
                Ok(receipt) if matches!(receipt.status, ReceiptStatus::Completed { .. })
            ),
            "a viewer adds no task: {receipt:?}"
        );
        assert!(
            !found(&ada_person).contains("Wombat survey"),
            "nothing of the refused call reaches the record"
        );
        server.shutdown().await;
    }
}
