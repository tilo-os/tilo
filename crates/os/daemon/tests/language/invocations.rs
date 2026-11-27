//! A workspace's invocations, as the daemon's one dispatch reads and follows them: what was
//! called, by whom, how it stands, and what it cost; and the two actions a record offers, to
//! stop it and to run it again.

use serde_json::{Value, json};
use tilo_action_services::workspace_invocations::{self, CANCEL, Invocation, RETRY, UNDO};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvocationReceipt, InvokeRequest,
    ReceiptStatus, ResourceAddress, ResourceCopy,
};

use crate::language::chat::said;
use crate::language::host;

/// Chat's id, as the workspace runs it.
const CHAT: &str = ui::apps::CHAT;
/// Chat's package under the tree's `apps/tl`, which the device installs to run it.
const CHAT_PACKAGE: &str = "chat";
/// Tasks' id, as the workspace runs it.
const TASKS: &str = "so.tilo.tasks";
/// Notes' id, as the workspace runs it.
const NOTES: &str = "so.tilo.notes";

/// The channel `name` made in the workspace's chat by `agent`: the address its posts go to.
async fn made_channel(agent: &host::Agent, name: &str) -> String {
    let answer = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": format!("{CHAT}#new_channel"), "args": {"name": name}}),
        )
        .await;
    let (text, failed) = said(&answer);
    assert!(!failed, "{CHAT}#new_channel: {text}");
    answer["structuredContent"]["resources"]
        .as_array()
        .and_then(|made| made.first())
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the call names the channel it made: {answer}"))
        .to_owned()
}

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
    reads.read(caller, &address).await.map(|copy| match copy {
        ResourceCopy::Reading { value, .. } => value,
        ResourceCopy::Absent => Value::Null,
        other => panic!("the invocations are a reading, not {other:?}"),
    })
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
        let tree = dispatch
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
            tree.rows.iter().any(|row| {
                row.resource.id == workspace_invocations::RESOURCE && row.has_children
            }),
            "the tree stands at its root: {:?}",
            tree.rows
        );
        let page = dispatch
            .query(
                &caller,
                CatalogueQuery {
                    provider: Some("tilo.invocations".into()),
                    parent_resource: Some(workspace_invocations::RESOURCE.to_owned()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the invocations' page")
            .expect("the page is answered");
        assert!(
            page.rows.iter().any(|row| row.resource.id == invocation),
            "the invocation is a row of the tree: {:?}",
            page.rows
        );
    });
}

/// The workspace's invocations are a resource read through the one record API, as every record
/// is: whole, or one by its id, and followed, the next call waking a reader that watches them.
/// Each states what was called, by whom, and how it stands. A reader names the workspace the
/// record is in, and another workspace's are not read here.
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

        // The record states what was called, by whom, and how it stands.
        let listed = collected(dispatch.as_ref(), &caller).await;
        let item = &listed[0];
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(item.calls, std::slice::from_ref(&first));
        assert_eq!(item.by.kind, "agent", "a key's call is the key's: {item:?}");
        assert_eq!(item.by.name, "Visa backend", "{item:?}");
        let action = item.action.as_deref().expect("what was called is named");
        assert!(action.contains('#'), "{action}");
        assert!(
            item.remembered && !item.recorded,
            "no record keeps it across a restart: {item:?}"
        );

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
            .await
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
            reads.read(&caller, &address).await
        };
        assert!(
            matches!(other, Err(ActionCallError::Refused { .. })),
            "another workspace's invocations are not read here: {other:?}"
        );
    });
}

/// A retry runs the same action again as its caller, under a new invocation id, and the new
/// record says what it runs again; a stranger may not run it again, stop it or take it back.
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
        // A caller neither its own, nor the person its agent works for, nor a writer of its
        // origin, is refused in words a person reads.
        let stranger = Caller::local_person(host.person())
            .in_workspace(host.workspace())
            .in_new_execution();
        for action in [RETRY, CANCEL, UNDO] {
            let asked = invoked(
                dispatch.as_ref(),
                &stranger,
                record_action(&original, action),
            );
            let refused = rejected(asked.await.expect("the record answers"));
            assert!(
                matches!(&refused, ActionCallError::Refused { reason }
                    if reason.starts_with("only the caller who made this invocation")),
                "a stranger is refused in words: {refused:?}"
            );
        }

        let answered = completed(
            invoked(dispatch.as_ref(), &caller, record_action(&original, RETRY))
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
            invoked(dispatch.as_ref(), &writer, record_action(&original, RETRY))
                .await
                .expect("a writer runs it again"),
        );
        assert_eq!(answered["retries"], original.as_str());

        // A writer may stop it too: the record admits them, and what the provider then answers
        // about a call that already ended is the provider's own refusal, not this rule's.
        let refused = rejected(
            invoked(dispatch.as_ref(), &writer, record_action(&original, CANCEL))
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
        let answered = agent
            .tool("tilo_do", &json!({"path": "devices#info"}))
            .await;
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
            invoked(dispatch.as_ref(), &person, record_action(&original, RETRY))
                .await
                .expect("the agent's person runs it again"),
        );
        assert_eq!(answered["retries"], original.as_str());
    });
}

/// An agent's run that wrote a message is taken back whole by its record's `undo`: the write
/// goes, the record says what it changed was taken back, and asking again is refused in words a
/// person reads. Another agent session's two messages go together through the `undo` of its
/// group, as the activity's Undo run asks.
#[test]
fn an_agents_run_is_taken_back_whole_through_its_record() {
    use crate::language::chat::{chat_holds, chat_stands_with, opens};

    host::run_with(&[CHAT_PACKAGE], |host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let agent = host.agent();
        // The channel the run posts into is the test's, as Chat starts empty.
        let channel = made_channel(&agent, "general").await;

        // One invocation of the chat's send writes the message.
        let run = tilo_actions::InvocationId::new().0.to_string();
        let posted = "bulk one";
        let (sent, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": format!("{CHAT}/{channel}#send"),
                            "args": {"body": posted},
                            "invocation_id": run}),
                )
                .await,
        );
        assert!(!failed, "the run writes its message: {sent}");
        assert!(
            chat_holds(host.as_ref(), posted)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the run wrote {posted:?}"
        );
        assert_eq!(
            chat_stands_with(host.as_ref(), posted)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            Some(true),
            "the record stands holding {posted:?}"
        );

        let dispatch = host.dispatch();
        let person = host.caller();
        // The person its agent works for takes the run back whole.
        let undone = completed(
            invoked(dispatch.as_ref(), &person, record_action(&run, UNDO))
                .await
                .expect("the person takes it back"),
        );
        assert!(undone.is_null(), "the undo answers nothing of its own");
        // The record stands without the write: what it holds now, not the history the write was
        // taken back out of, which keeps its bytes.
        assert_eq!(
            chat_stands_with(host.as_ref(), posted)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            Some(false),
            "the run's write {posted:?} is gone"
        );
        let taken = one(dispatch.as_ref(), &person, &run).await;
        assert_eq!(taken["status"], "undone", "{taken}");

        // Asking again says what stands, in words.
        let again = rejected(
            invoked(dispatch.as_ref(), &person, record_action(&run, UNDO))
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

        let session = host.agent();
        let group_of =
            async |call: &str| one(dispatch.as_ref(), &person, call).await["group"]["id"].clone();
        let mut groups = vec![group_of(&run).await];
        for body in ["bulk two", "bulk three"] {
            let call = tilo_actions::InvocationId::new().0.to_string();
            let args = json!({"path": format!("{CHAT}/{channel}#send"), "args": {"body": body}, "invocation_id": call});
            let (sent, failed) = said(&session.tool("tilo_do", &args).await);
            assert!(!failed, "the session writes {body:?}: {sent}");
            groups.push(group_of(&call).await);
        }
        assert!(
            groups[1].is_string() && groups[1] == groups[2] && groups[0] != groups[1],
            "{groups:?}"
        );
        let group = groups[1].as_str().unwrap_or_default();
        let undo = || record_action(group, UNDO);
        completed(
            invoked(dispatch.as_ref(), &person, undo())
                .await
                .expect("the person takes the session back"),
        );
        for body in ["bulk two", "bulk three"] {
            let stands = chat_stands_with(host.as_ref(), body)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(stands, Some(false), "the session's {body:?} is gone");
        }
        let again = rejected(
            invoked(dispatch.as_ref(), &person, undo())
                .await
                .expect("the record answers"),
        );
        assert!(
            matches!(&again, ActionCallError::Refused { reason } if reason.contains("already taken back")),
            "{again:?}"
        );
    });
}

/// What `agent`'s `tilo_do` of `path` with `args` answered once it ended, under the invocation
/// `run` when it names one; a refusal fails the test, saying why.
async fn done(agent: &host::Agent, path: &str, args: Value, run: Option<&str>) -> Value {
    let mut call = json!({"path": path, "args": args, "wait_ms": 120_000});
    if let Some(run) = run {
        call["invocation_id"] = run.into();
    }
    let answer = agent.tool_structured("tilo_do", &call).await;
    let (text, failed) = said(&answer);
    assert!(!failed, "{path}: {text}");
    answer["structuredContent"].clone()
}

/// The record `id` of the kind `kind` as the person reads it in the workspace's store, once
/// `settled` is true of it; as it reads when it never does.
async fn stored(
    host: &host::Host,
    kind: &str,
    id: &str,
    settled: impl Fn(Option<&collab_types::store::Indexed>) -> bool,
) -> Option<collab_types::store::Indexed> {
    use collab_types::store::wire::{Command, Reply};
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        let read = host
            .daemon()
            .workspaces()
            .read_store_as(host.workspace(), host.person(), &Command::Get { kind, id })
            .await;
        let Ok(Reply::Resource(now)) = read else {
            panic!("the person's read answers the record: {read:?}");
        };
        if settled(now.as_ref()) || std::time::Instant::now() >= deadline {
            return now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// The status of the task `id` as the person reads it in the workspace's store, once it
/// reads `want` (any status with `None`); the last one read when it never comes to `want`.
async fn status(host: &host::Host, id: &str, want: Option<&str>) -> String {
    let of = |task: &collab_types::store::Indexed| format!("{:?}", task.scalar("status"));
    let reads =
        |task: Option<&_>| task.is_some_and(|task| want.is_none_or(|w| of(task).contains(w)));
    stored(host, "tl.so.tilo.tasks.Task", id, reads)
        .await
        .as_ref()
        .map(of)
        .unwrap_or_default()
}

/// A run is taken back around another member's later change: an agent's run closes a task and
/// with it both its subtasks, the person moves one subtask on after it, and the run's `undo`
/// leaves the person's move and puts the other subtask back as it was. Taking back a run that is
/// taken back already is refused in words.
#[test]
fn a_run_is_taken_back_around_a_members_later_change() {
    host::run_with(&["tasks"], |host| async move {
        host::start_app(host.as_ref(), TASKS)
            .await
            .unwrap_or_else(|refused| panic!("Tasks starts: {refused:?}"));
        let agent = host.agent();
        let made = |answer: Value| {
            answer["made"][0]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("the call names what it made: {answer}"))
                .to_owned()
        };
        let plan = made(
            done(
                &agent,
                &format!("{TASKS}#new_task"),
                json!({"title": "Plan"}),
                None,
            )
            .await,
        );
        let add_subtask = format!("{TASKS}/{plan}#add_subtask");
        let subtask =
            |title: &'static str| done(&agent, &add_subtask, json!({"title": title}), None);
        let (moved, untouched) = (made(subtask("Budget").await), made(subtask("Hiring").await));
        let before = status(host.as_ref(), &untouched, None).await;

        let run = tilo_actions::InvocationId::new().0.to_string();
        done(
            &agent,
            &format!("{TASKS}/{plan}#set_status"),
            json!({"status": "done"}),
            Some(&run),
        )
        .await;
        let closed = status(host.as_ref(), &untouched, Some("done")).await;
        assert!(closed.contains("done"), "the run closed it: {closed}");

        let dispatch = host.dispatch();
        let person = host.caller();
        // The person's own move, by the action Tasks publishes on the subtask: on the resource it
        // publishes the subtask as, `tl:<app>:<id>`.
        let set_status = host::app_action(
            host.as_ref(),
            TASKS,
            &format!("tl:{TASKS}:{moved}"),
            "set_status",
        );
        host::output(host.as_ref(), set_status, json!({"status": "in_review"}))
            .await
            .expect("the person moves the subtask on");
        let moved_on = status(host.as_ref(), &moved, Some("in_review")).await;
        assert!(
            moved_on.contains("in_review"),
            "the person's move lands: {moved_on}"
        );
        let undo = || record_action(&run, UNDO);
        completed(
            invoked(dispatch.as_ref(), &person, undo())
                .await
                .expect("the person takes the run back"),
        );
        assert_eq!(
            status(host.as_ref(), &untouched, Some(&before)).await,
            before,
            "the run's change of the other subtask is taken back"
        );
        let kept = status(host.as_ref(), &moved, None).await;
        assert!(
            kept.contains("in_review"),
            "the person's later move stays: {kept}"
        );
        assert_eq!(
            one(dispatch.as_ref(), &person, &run).await["status"],
            "undone"
        );

        let again = rejected(
            invoked(dispatch.as_ref(), &person, undo())
                .await
                .expect("the record answers"),
        );
        assert!(
            matches!(&again, ActionCallError::Refused { reason } if reason.contains("already taken back")),
            "a second undo says what stands: {again:?}"
        );
    });
}

/// An agent's run in one app that writes another app's record is taken back whole: a task's
/// `write_note` makes the note in Notes as a step of the run, the run's `undo` takes back what it
/// changed in both apps, and the record reads undone.
#[test]
fn a_task_step_in_two_apps_is_taken_back_as_one() {
    host::run_with(&["tasks", "notes"], |host| async move {
        for app in [TASKS, NOTES] {
            host::start_app(host.as_ref(), app)
                .await
                .unwrap_or_else(|refused| panic!("{app} starts: {refused:?}"));
        }
        let agent = host.agent();
        let new_task = format!("{TASKS}#new_task");
        let task = done(&agent, &new_task, json!({"title": "Plan"}), None).await;
        let task = task["result"]["id"]
            .as_str()
            .expect("the call names the task it made");
        let run = tilo_actions::InvocationId::new().0.to_string();
        let path = format!("{TASKS}/{task}#write_note");
        let note = done(&agent, &path, json!({}), Some(&run)).await;
        let note = note["result"]["id"]
            .as_str()
            .expect("the run answers the note it wrote")
            .to_owned();
        let kind = "tl.so.tilo.notes.Note";
        let made = stored(host.as_ref(), kind, &note, |n| n.is_some()).await;
        assert!(made.is_some(), "Notes keeps the run's note");

        let (dispatch, person) = (host.dispatch(), host.caller());
        let undo = record_action(&run, UNDO);
        completed(
            invoked(dispatch.as_ref(), &person, undo)
                .await
                .expect("the run is taken back"),
        );
        let left = stored(host.as_ref(), kind, &note, |n| n.is_none()).await;
        let taken = one(dispatch.as_ref(), &person, &run).await;
        assert!(
            left.is_none(),
            "the run's note in Notes is taken back: {taken}"
        );
        assert_eq!(taken["status"], "undone", "{taken}");
    });
}
