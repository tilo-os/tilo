//! A workspace's invocations, as the daemon's one dispatch reads and follows them: what was
//! called, by whom, how it stands, and what it cost; and the two actions a record offers, to
//! stop it and to run it again.

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::workspace_invocations::{self, Invocation, InvocationStatus};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvocationReceipt, InvokeRequest,
    ReceiptStatus, ResourceAddress, ResourceCopy,
};

use crate::host;

/// What `/v1` answered for the action at `path`, and the invocation id it named.
async fn ran(key: &host::Key, path: &str) -> String {
    let (status, answered) = key.post(path, &json!({})).await;
    assert_eq!(status, 200, "{answered}");
    answered["invocation"]
        .as_str()
        .expect("the answer names its invocation")
        .to_owned()
}

/// The reading of the record `record` of the workspace's invocations, through `provider`'s
/// dispatch as `caller`.
async fn reading(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    record: &str,
) -> Result<Value, ActionCallError> {
    let reads = provider
        .resource_reads()
        .expect("the dispatch reads records");
    let address = ResourceAddress {
        workspace: caller.workspace().unwrap_or_default().to_owned(),
        app: workspace_invocations::PROVIDER.to_owned(),
        record: record.to_owned(),
    };
    let caller = caller.clone();
    let read = tokio::task::spawn_blocking(move || reads.read(&caller, &address))
        .await
        .expect("the read ends")
        .map(|copy| match copy {
            ResourceCopy::Reading { value, .. } => value,
            ResourceCopy::Absent => Value::Null,
            other => panic!("the invocations are a reading, not {other:?}"),
        });
    read
}

/// Every invocation the reading of the workspace's record names, newest first.
async fn collected(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
) -> Vec<Invocation> {
    let whole = reading(provider, caller, "")
        .await
        .expect("the person reads the workspace's invocations");
    serde_json::from_value(whole["items"].clone()).expect("the items read as invocations")
}

/// One invocation as the reading of its record names it.
async fn one(provider: &(dyn ActionProvider + Send + Sync), caller: &Caller, id: &str) -> Value {
    reading(provider, caller, id)
        .await
        .expect("the record of the invocation is read")
}

/// Run `request` through `provider`'s dispatch as `caller` until it answers.
async fn invoked(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    request: InvokeRequest,
) -> Result<InvocationReceipt, ActionCallError> {
    tilo_test_env::within("the dispatch answers", provider.invoke(caller, request))
        .await
        .expect("the dispatch answers")
}

/// The output a completed receipt carries.
fn completed(receipt: InvocationReceipt) -> Value {
    match receipt.status {
        ReceiptStatus::Completed { output } => output.value.unwrap_or(Value::Null),
        other => panic!("the invocation completed, not {other:?}"),
    }
}

/// The refusal a rejected receipt carries.
fn rejected(receipt: InvocationReceipt) -> ActionCallError {
    match receipt.status {
        ReceiptStatus::Rejected { error } => error,
        other => panic!("the invocation was refused, not {other:?}"),
    }
}

/// The request of one of a record's actions: stopping it or running it again.
fn record_action(record: &str, action: &str) -> InvokeRequest {
    host::call(
        workspace_invocations::PROVIDER,
        ActionAddress::new(record.to_owned(), action),
        &json!({}),
    )
}

/// The call a test makes: an invocation of the device's own `info` as `provider` publishes it,
/// from `from` when it names the resource it is made from.
fn info(provider: &str, address: &ActionAddress, from: Option<&str>) -> InvokeRequest {
    let arguments = match from {
        Some(from) => json!({
            "$from": {"ref": from, "ty": "chat.workspace.channels"},
        }),
        None => json!({}),
    };
    host::call(provider, address.clone(), &arguments)
}

/// The target the workspace publishes the device's own `info` under, as a walk of its
/// catalogue finds it: what a `/v1` path resolves to before it runs.
async fn devices_info(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
) -> (String, ActionAddress) {
    let root = provider
        .query(caller, CatalogueQuery::default())
        .await
        .expect("the catalogue")
        .expect("the catalogue is answered");
    let mut pages = Vec::new();
    for published in &root.providers {
        let top = provider
            .query(
                caller,
                CatalogueQuery {
                    provider: Some(published.provider.clone()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("a provider's page")
            .expect("the page is answered");
        pages.push(top);
    }
    for page in &pages {
        for row in &page.rows {
            if let Some(action) = &row.action
                && action.address.action == "info"
                && action.address.resource == "devices"
            {
                return (row.provider.clone(), action.address.clone());
            }
        }
    }
    for published in &root.providers {
        let children = provider
            .query(
                caller,
                CatalogueQuery {
                    provider: Some(published.provider.clone()),
                    parent_resource: Some("devices".to_owned()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("a device's page")
            .expect("the page is answered");
        for row in children.rows {
            if let Some(action) = &row.action
                && action.address.action == "info"
            {
                return (row.provider.clone(), action.address.clone());
            }
        }
    }
    panic!(
        "the workspace publishes no devices#info among {:?}",
        root.providers
    );
}

/// One workspace's invocations state what was called, by whom, and how it stands, after the call
/// ended.
#[test]
fn a_reading_states_what_was_called_by_whom_and_how_it_stands() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let invocation = ran(
            &key,
            &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
        )
        .await;

        let listed = collected(host.dispatch().as_ref(), &host.caller()).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        let item = &listed[0];
        assert_eq!(item.id.as_deref(), Some(invocation.as_str()));
        assert_eq!(item.calls, [invocation.clone()]);
        assert_eq!(item.status, InvocationStatus::Ended, "{item:?}");
        assert_eq!(item.by.kind, "agent", "a key's call is the key's: {item:?}");
        assert_eq!(item.by.name, "Visa backend", "{item:?}");
        let action = item.action.as_deref().expect("what was called is named");
        assert!(action.contains('#'), "{action}");
        assert!(item.remembered, "{item:?}");
        assert!(
            !item.recorded,
            "no record keeps it across a restart: {item:?}"
        );
    });
}

/// A caller the workspace's grants do not admit reads nothing.
#[test]
fn a_caller_the_workspace_does_not_admit_is_refused() {
    host::run(|host| async move {
        let stranger = Caller::local_person("someone-else").in_workspace(host.workspace());
        let refused = reading(host.dispatch().as_ref(), &stranger, "").await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    });
}

/// The workspace's invocations are published beside its apps, as rows a walk of the workspace
/// finds.
#[test]
fn the_workspace_publishes_its_invocations_beside_its_apps() {
    host::run(|host| async move {
        let dispatch = host.dispatch();
        let caller = host.caller();
        let root = dispatch
            .query(&caller, CatalogueQuery::default())
            .await
            .expect("the workspace's catalogue")
            .expect("the catalogue is answered");
        assert!(
            root.providers
                .iter()
                .any(|provider| provider.app_id == "invocations"),
            "a walk of the workspace finds its invocations: {:?}",
            root.providers
        );

        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let invocation = ran(
            &key,
            &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
        )
        .await;
        let page = dispatch
            .query(
                &caller,
                CatalogueQuery {
                    provider: Some("tilo.invocations".into()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the invocations' tree")
            .expect("the tree is answered");
        assert!(
            page.rows.iter().any(|row| row.resource.id == invocation),
            "the invocation is a row of the tree: {:?}",
            page.rows
        );
    });
}

/// The workspace's invocations are a resource read through the one record API, as every record
/// is: whole, or one by its id, and followed, the next call waking a reader that watches them. A
/// reader names the workspace the record is in, and another workspace's are not read here.
#[test]
fn the_workspaces_invocations_are_read_and_followed_through_the_record_api() {
    use tilo_actions::remote::ResourceWake;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let path = format!("/v1/workspaces/{workspace}/resources/devices/actions/info");
        let first = ran(&key, &path).await;
        let dispatch = host.dispatch();
        let caller = host.caller();

        let whole = reading(dispatch.as_ref(), &caller, "")
            .await
            .expect("the workspace's invocations are read");
        assert_eq!(whole["total"], 1, "{whole}");
        assert_eq!(whole["items"][0]["id"], first.as_str(), "{whole}");
        let one = reading(dispatch.as_ref(), &caller, &first)
            .await
            .expect("one invocation is read by its id");
        assert_eq!(one["id"], first.as_str(), "{one}");
        assert_eq!(one["status"], "ended", "{one}");
        let unknown = reading(
            dispatch.as_ref(),
            &caller,
            "00000000-0000-0000-0000-000000000000",
        )
        .await
        .expect("an unknown record answers");
        assert!(unknown.is_null(), "no such call reads as none: {unknown}");

        // A reader that follows them is woken by the next call, and reads it there.
        let reads = dispatch
            .resource_reads()
            .expect("the dispatch reads records");
        let address = ResourceAddress {
            workspace: workspace.clone(),
            app: workspace_invocations::PROVIDER.to_owned(),
            record: String::new(),
        };
        let (wake, woken) = ResourceWake::new();
        reads
            .watch(&caller, &address, &wake)
            .expect("the invocations can be followed");
        let second = ran(&key, &path).await;
        tilo_test_env::within("the next call wakes the reader", woken)
            .await
            .expect("the reader is woken");
        let whole = reading(dispatch.as_ref(), &caller, "")
            .await
            .expect("the workspace's invocations are read");
        assert_eq!(whole["total"], 2, "{whole}");
        assert_eq!(
            whole["items"][0]["id"],
            second.as_str(),
            "newest first: {whole}"
        );

        let elsewhere = reading(dispatch.as_ref(), &caller, "").await;
        assert!(
            elsewhere.is_ok(),
            "the caller's own workspace is read: {elsewhere:?}"
        );
        let other = {
            let reads = dispatch
                .resource_reads()
                .expect("the dispatch reads records");
            let caller = caller.clone();
            let address = ResourceAddress {
                workspace: "elsewhere".to_owned(),
                app: workspace_invocations::PROVIDER.to_owned(),
                record: String::new(),
            };
            tokio::task::spawn_blocking(move || reads.read(&caller, &address))
                .await
                .expect("the read ends")
        };
        assert!(
            matches!(other, Err(ActionCallError::Refused { .. })),
            "another workspace's invocations are not read here: {other:?}"
        );
    });
}

/// A retry runs the same action again as its caller, under a new invocation id, and the new
/// record says what it runs again.
#[test]
fn a_retry_runs_the_action_again_as_its_caller_under_a_new_id() {
    host::run(|host| async move {
        let dispatch = host.dispatch();
        let caller = host.caller();
        let (provider, address) = devices_info(dispatch.as_ref(), &caller).await;
        let started = invoked(dispatch.as_ref(), &caller, info(&provider, &address, None))
            .await
            .expect("the device answers");
        let original = started.invocation.0.to_string();

        let answered = completed(
            invoked(
                dispatch.as_ref(),
                &caller,
                record_action(&original, workspace_invocations::RETRY),
            )
            .await
            .expect("its caller runs it again"),
        );
        let fresh = answered["invocation"]
            .as_str()
            .expect("the new invocation")
            .to_owned();
        assert_ne!(fresh, original, "a retry is a new invocation");
        assert_eq!(answered["retries"], original.as_str());
        assert_eq!(
            answered["address"],
            format!("tilo.invocations/{fresh}").as_str(),
            "the answer names the new record's address"
        );

        let again = one(dispatch.as_ref(), &caller, &fresh).await;
        assert_eq!(again["retries"], original.as_str(), "{again}");
        assert_eq!(again["status"], "ended", "{again}");
        let listed = collected(dispatch.as_ref(), &caller).await;
        assert!(
            listed
                .iter()
                .any(|invocation| invocation.id.as_deref() == Some(fresh.as_str())),
            "the retry is listed beside what it ran again: {listed:?}"
        );
    });
}

/// A person who may write the resource an invocation was made from may run it again and stop
/// it, though another execution of theirs made neither call.
#[test]
fn a_caller_who_may_write_the_origin_may_run_an_invocation_again_and_stop_it() {
    host::run(|host| async move {
        let dispatch = host.dispatch();
        let caller = host.caller();
        let (provider, address) = devices_info(dispatch.as_ref(), &caller).await;
        let started = invoked(
            dispatch.as_ref(),
            &caller,
            info(&provider, &address, Some("chat.workspace.channels:c1")),
        )
        .await
        .expect("the device answers");
        let original = started.invocation.0.to_string();
        let recorded = one(dispatch.as_ref(), &caller, &original).await;
        assert_eq!(
            recorded["origin"],
            json!({"kind": "chat.workspace.channels", "id": "c1"}),
            "the record says what it was made from: {recorded}"
        );

        let writer = caller.clone().in_new_execution();
        assert!(
            !writer.continues(&caller),
            "another execution of the person is not its caller"
        );
        let answered = completed(
            invoked(
                dispatch.as_ref(),
                &writer,
                record_action(&original, workspace_invocations::RETRY),
            )
            .await
            .expect("a writer runs it again"),
        );
        assert_eq!(answered["retries"], original.as_str());

        // A writer may stop it too: the record admits them, and what the provider then answers
        // about a call that already ended is the provider's own refusal, not this rule's.
        let refused = rejected(
            invoked(
                dispatch.as_ref(),
                &writer,
                record_action(&original, workspace_invocations::CANCEL),
            )
            .await
            .expect("a writer stops it"),
        );
        assert!(
            !matches!(refused, ActionCallError::Refused { .. }),
            "a writer of the origin is admitted: {refused:?}"
        );
    });
}

/// The person an agent works for may run the agent's invocation again: it made no such call and
/// the record names no origin to write.
#[test]
fn the_person_an_agent_works_for_may_run_its_invocation_again() {
    host::run(|host| async move {
        let agent = host.agent();
        let answered = agent.tool("tilo_do", &json!({"path": "devices#info"})).await;
        assert!(
            !answered["isError"].as_bool().unwrap_or(false),
            "{answered}"
        );

        let dispatch = host.dispatch();
        let caller = host.caller();
        let listed = collected(dispatch.as_ref(), &caller).await;
        let run = listed
            .iter()
            .find(|invocation| invocation.by.kind == "agent")
            .expect("the agent's call is listed");
        let original = run.id.clone().expect("the agent's invocation id");
        assert!(
            run.origin.is_none(),
            "the agent's call was made from no resource: {run:?}"
        );

        let person = caller.clone().in_new_execution();
        assert!(!person.continues(&caller));
        let answered = completed(
            invoked(
                dispatch.as_ref(),
                &person,
                record_action(&original, workspace_invocations::RETRY),
            )
            .await
            .expect("the agent's person runs it again"),
        );
        assert_eq!(answered["retries"], original.as_str());
    });
}

/// A caller that is neither the invocation's own, nor the person its agent works for, nor a
/// writer of its origin, is refused in words a person reads.
#[test]
fn a_caller_neither_its_own_nor_a_writer_of_its_origin_is_refused() {
    host::run(|host| async move {
        let dispatch = host.dispatch();
        let caller = host.caller();
        let (provider, address) = devices_info(dispatch.as_ref(), &caller).await;
        let started = invoked(dispatch.as_ref(), &caller, info(&provider, &address, None))
            .await
            .expect("the device answers");
        let original = started.invocation.0.to_string();

        let stranger = Caller::local_person(host.person())
            .in_workspace(host.workspace())
            .in_new_execution();
        for action in [workspace_invocations::RETRY, workspace_invocations::CANCEL] {
            let refused = rejected(
                invoked(
                    dispatch.as_ref(),
                    &stranger,
                    record_action(&original, action),
                )
                .await
                .expect("the record answers"),
            );
            let ActionCallError::Refused { reason } = &refused else {
                panic!("a stranger is refused in words, not {refused:?}");
            };
            assert!(
                reason.starts_with("only the caller who made this invocation"),
                "{reason}"
            );
        }
    });
}

/// An agent's run that made two writes is taken back whole by its record's `undo`: both writes
/// go, the record says what it changed was taken back, a caller who may not manage the run is
/// refused, and asking again is refused in words a person reads.
#[test]
fn an_agents_run_is_taken_back_whole_through_its_record() {
    use crate::chat::{chat_holds, opens, said};

    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let agent = host.agent();
        // The channel the chat opened with is listed before the agent posts into it.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let (listed, failed) = said(&agent.tool("tilo_ls", &json!({"path": "chat"})).await);
            if !failed && listed.contains("c_chat") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the chat is never listed: {listed}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // One invocation of the chat's group makes two writes.
        let run = tilo_actions::InvocationId::new().0.to_string();
        let posts = [
            json!({"action": "post", "args": {"channel": "c_chat", "text": "bulk one"}}),
            json!({"action": "post", "args": {"channel": "c_chat", "text": "bulk two"}}),
        ];
        let (grouped, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#group",
                            "args": {"actions": posts},
                            "invocation_id": run}),
                )
                .await,
        );
        assert!(!failed, "the batch runs as one invocation: {grouped}");
        for text in ["bulk one", "bulk two"] {
            assert!(
                chat_holds(host.as_ref(), text)
                    .await
                    .unwrap_or_else(|error| panic!("{error}")),
                "the run wrote {text:?}"
            );
        }

        let dispatch = host.dispatch();
        let person = host.caller();
        // A stranger, neither the caller nor the person its agent works for, may not take it back.
        let stranger = Caller::local_person(host.person())
            .in_workspace(host.workspace())
            .in_new_execution();
        let refused = rejected(
            invoked(
                dispatch.as_ref(),
                &stranger,
                record_action(&run, workspace_invocations::UNDO),
            )
            .await
            .expect("the record answers"),
        );
        let ActionCallError::Refused { reason } = &refused else {
            panic!("a stranger is refused in words, not {refused:?}");
        };
        assert!(
            reason.starts_with("only the caller who made this invocation"),
            "{reason}"
        );
        for text in ["bulk one", "bulk two"] {
            assert!(
                chat_holds(host.as_ref(), text)
                    .await
                    .unwrap_or_else(|error| panic!("{error}")),
                "a refusal wrote nothing: {text:?}"
            );
        }

        // The person its agent works for takes the run back whole.
        let undone = completed(
            invoked(
                dispatch.as_ref(),
                &person,
                record_action(&run, workspace_invocations::UNDO),
            )
            .await
            .expect("the person takes it back"),
        );
        assert!(undone.is_null(), "the undo answers nothing of its own");
        for text in ["bulk one", "bulk two"] {
            assert!(
                !chat_holds(host.as_ref(), text)
                    .await
                    .unwrap_or_else(|error| panic!("{error}")),
                "the run's write {text:?} is gone"
            );
        }
        let taken = one(dispatch.as_ref(), &person, &run).await;
        assert_eq!(taken["status"], "undone", "{taken}");

        // Asking again says what stands, in words.
        let again = rejected(
            invoked(
                dispatch.as_ref(),
                &person,
                record_action(&run, workspace_invocations::UNDO),
            )
            .await
            .expect("the record answers"),
        );
        let ActionCallError::Refused { reason } = &again else {
            panic!("a second undo is refused in words, not {again:?}");
        };
        assert!(
            reason.contains("already taken back"),
            "a person reads what stands: {reason}"
        );
    });
}
