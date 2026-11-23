//! A workspace's invocations, as the daemon's one dispatch lists them: what was called, by
//! whom, and how it stands, read by a caller whose grants decide which of them they see.

use serde_json::{Value, json};
use tilo_actions::remote::workspace_invocations::{
    Invocation, InvocationStatus, Invocations, target,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
};

use crate::host;

/// The request that asks a workspace's dispatch for a page of its invocations, with `arguments`.
fn listing(arguments: Value) -> InvokeRequest {
    let target = target();
    host::call(&target.provider, target.address, &arguments)
}

/// The page of invocations `arguments` asks for, through `provider`'s dispatch as `caller`.
async fn listed(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    arguments: Value,
) -> Result<Invocations, ActionCallError> {
    let answered = provider.invoke(caller, listing(arguments));
    let receipt = tilo_test_env::within("the dispatch answers the listing", answered)
        .await
        .expect("the dispatch answers");
    match receipt?.status {
        ReceiptStatus::Completed { output } => Ok(serde_json::from_value(
            output.value.expect("the page says what it lists"),
        )
        .expect("the page reads as one")),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("a listing is answered whole, not {other:?}"),
    }
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

/// Every id a listing names, newest first.
fn ids(listed: &Invocations) -> Vec<String> {
    listed
        .items
        .iter()
        .filter_map(|invocation| invocation.id.clone())
        .collect()
}

/// One workspace's invocations state what was called, by whom, and how it stands, after the call
/// ended.
#[test]
fn a_listing_states_what_was_called_by_whom_and_how_it_stands() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let invocation = ran(
            &key,
            &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
        )
        .await;

        let page = listed(host.dispatch().as_ref(), &host.caller(), json!({}))
            .await
            .expect("the person reads the workspace's invocations");
        assert_eq!(page.total, 1, "{page:?}");
        let item: &Invocation = &page.items[0];
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

/// A listing pages newest first, and reads on from the cursor its page ended with.
#[test]
fn a_listing_reads_on_from_its_cursor() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let mut called = Vec::new();
        for _ in 0..5 {
            called.push(
                ran(
                    &key,
                    &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
                )
                .await,
            );
        }
        let dispatch = host.dispatch();
        let caller = host.caller();
        let whole = listed(dispatch.as_ref(), &caller, json!({"limit": 50}))
            .await
            .expect("the workspace lists its invocations");
        assert_eq!(whole.total, 5, "{whole:?}");
        assert_eq!(ids(&whole).len(), 5);

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..3 {
            let asked = match &cursor {
                Some(cursor) => json!({"limit": 2, "cursor": cursor}),
                None => json!({"limit": 2}),
            };
            let listed = listed(dispatch.as_ref(), &caller, asked)
                .await
                .expect("a page is answered");
            assert_eq!(listed.total, 5, "each page says how many there are");
            seen.extend(ids(&listed));
            cursor = listed.next_cursor;
            if page < 2 {
                assert!(cursor.is_some(), "a page before the last names the next");
            }
        }
        assert_eq!(cursor, None, "the last page names no next");
        seen.sort();
        let mut whole = ids(&whole);
        whole.sort();
        assert_eq!(seen, whole, "every invocation once, across the pages");
    });
}

/// A listing of one member's invocations leaves another's out: an agent reads its own runs, and
/// the person's.
#[test]
fn a_listing_of_one_member_leaves_anothers_out() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        ran(
            &key,
            &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
        )
        .await;
        let agent = host.agent();
        let answered = agent.tool("tilo_do", json!({"path": "devices#info"})).await;
        assert!(
            !answered["isError"].as_bool().unwrap_or(false),
            "{answered}"
        );

        let dispatch = host.dispatch();
        let caller = host.caller();
        let whole = listed(dispatch.as_ref(), &caller, json!({"limit": 50}))
            .await
            .expect("the workspace lists its invocations");
        let agent_run = whole
            .items
            .iter()
            .find(|invocation| invocation.by.kind == "agent")
            .expect("the agent's call is listed");
        let agent_id = agent_run.by.id.clone().expect("the agent's member id");

        let mine = listed(dispatch.as_ref(), &caller, json!({"by": agent_id}))
            .await
            .expect("the agent's own runs");
        assert_eq!(mine.total, 1, "{mine:?}");
        assert_eq!(mine.items[0].id, agent_run.id);
        let nobody = listed(
            dispatch.as_ref(),
            &caller,
            json!({"by": "nobody-is-this-member"}),
        )
        .await
        .expect("an empty listing is still a listing");
        assert_eq!(nobody.total, 0, "{nobody:?}");
    });
}

/// A caller the workspace's grants do not admit is refused.
#[test]
fn a_caller_the_workspace_does_not_admit_is_refused() {
    host::run(|host| async move {
        let stranger = Caller::local_person("someone-else").in_workspace(host.workspace());
        let refused = listed(host.dispatch().as_ref(), &stranger, json!({})).await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    });
}

/// The workspace's invocations are published beside its apps, and read as a tree.
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
    use tilo_actions::remote::workspace_invocations::PROVIDER;
    use tilo_actions::remote::{ResourceAddress, ResourceCopy, ResourceWake};
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let path = format!("/v1/workspaces/{workspace}/resources/devices/actions/info");
        let first = ran(&key, &path).await;
        let reads = host
            .dispatch()
            .resource_reads()
            .expect("the dispatch reads records");
        let caller = host.caller();
        let address = |workspace: &str, record: &str| ResourceAddress {
            workspace: workspace.to_owned(),
            app: PROVIDER.to_owned(),
            record: record.to_owned(),
        };
        let read = {
            let (reads, caller) = (reads.clone(), caller.clone());
            move |address: ResourceAddress| {
                let (reads, caller) = (reads.clone(), caller.clone());
                tokio::task::spawn_blocking(move || reads.read(&caller, &address))
            }
        };
        let reading = |copy: ResourceCopy| match copy {
            ResourceCopy::Reading { value, .. } => value,
            other => panic!("the invocations are a reading, not {other:?}"),
        };

        let whole = reading(read(address(&workspace, "")).await.unwrap().unwrap());
        assert_eq!(whole["total"], 1, "{whole}");
        assert_eq!(whole["items"][0]["id"], first.as_str(), "{whole}");
        let one = reading(read(address(&workspace, &first)).await.unwrap().unwrap());
        assert_eq!(one["id"], first.as_str(), "one invocation by its id: {one}");
        assert_eq!(one["status"], "ended", "{one}");
        let unknown = read(address(&workspace, "00000000-0000-0000-0000-000000000000"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unknown, ResourceCopy::Absent, "no such call is absent");

        // A reader that follows them is woken by the next call, and reads it there.
        let (wake, woken) = ResourceWake::new();
        reads
            .watch(&caller, &address(&workspace, ""), &wake)
            .expect("the invocations can be followed");
        let second = ran(&key, &path).await;
        tilo_test_env::within("the next call wakes the reader", woken)
            .await
            .expect("the reader is woken");
        let whole = reading(read(address(&workspace, "")).await.unwrap().unwrap());
        assert_eq!(whole["total"], 2, "{whole}");
        assert_eq!(whole["items"][0]["id"], second.as_str(), "newest first: {whole}");

        let elsewhere = read(address("elsewhere", "")).await.unwrap();
        assert!(
            matches!(elsewhere, Err(ActionCallError::Refused { .. })),
            "another workspace's invocations are not read here: {elsewhere:?}"
        );
    });
}
