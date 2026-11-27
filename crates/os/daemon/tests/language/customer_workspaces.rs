//! One workspace per customer, through the API as a backend keeps them: a person's key makes
//! workspaces labelled with the backend's own id for each, finds each again by that label, mints
//! a key that reaches one workspace and nothing else, and deletes a customer's workspace when
//! they leave with everything it kept. A customer's agent works in its workspace over MCP and is
//! refused on its neighbour's and on the account.

use serde_json::{Value, json};
use tilo_runtime::RuntimeServices as _;

use super::api_workspaces::ids;
use crate::language::host;

/// Where the account's `resource#action` is run.
fn account(resource: &str, action: &str) -> String {
    format!("/v1/resources/account/{resource}/actions/{action}")
}

/// Where the account's `resource#action` is run, as one name.
fn account_action(action: &str) -> String {
    let (resource, action) = action.split_once('#').expect("resource#action");
    account(resource, action)
}

/// The route of `path` in `workspace`'s resources.
fn workspace_route(workspace: &str, path: &str) -> String {
    format!(
        "/v1/workspaces/{workspace}/resources/{}",
        tilo_api::v1::routing::encode_path(path)
    )
}

/// A workspace made through the account, labelled `labels`.
///
/// The backend asks to wait as long as the API allows: making a workspace writes its records and
/// settles them, which can outlast the default wait on a busy computer, and a backend waiting
/// for its customer's workspace is no reason to answer it `202`.
async fn made(key: &host::Key, name: &str, labels: Value) -> String {
    let body = json!({"name": name, "labels": labels});
    let (status, answer) = key
        .request(
            "POST",
            &account("workspaces", "create"),
            &[("content-type", "application/json"), ("prefer", "wait=60")],
            body.to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 200, "the account makes a workspace: {answer}");
    answer["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a create answers the workspace it made: {answer}"))
        .to_owned()
}

/// The workspaces `key` lists, `labels` narrowing them.
async fn listed(key: &host::Key, labels: Value) -> Vec<String> {
    let (status, answer) = key
        .post(&account("workspaces", "list"), &json!({"labels": labels}))
        .await;
    assert_eq!(status, 200, "the account lists its workspaces: {answer}");
    ids(&answer["result"])
}

/// The key of a customer's agent: a server key of the person's reaching `workspace` alone, as
/// a backend mints one for that customer.
fn customer(host: &host::Host, workspace: &str, label: &str) -> host::Key {
    host.key_reaching(workspace, label)
}

/// The book the site publishes explains the SDK and the MCP tools, and every tool the
/// workspace's agent is offered says what it does.
#[test]
fn the_book_explains_the_sdk_and_the_tools_say_what_they_do() {
    host::run(|host| async move {
        let book = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../docs/tilo-sh/pages.toml"
        ))
        .expect("the tilo.sh book lists its pages");
        assert!(
            book.contains("docs/developers/one-workspace-per-customer.md"),
            "the book carries a workspace per customer"
        );
        assert!(
            book.contains("docs/developers/api.md"),
            "the book carries the API page"
        );

        let session = host
            .daemon()
            .mcp_session_factory(host.workspace())
            .expect("the device serves its workspace's MCP")(None)
        .expect("the session opens");
        let answer = session
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await
            .unwrap();
        let tools = answer["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("the endpoint lists its tools: {answer}"));
        assert!(
            tools.len() > 10,
            "the workspace's tools are listed: {answer}"
        );
        for tool in tools {
            let name = tool["name"].as_str().unwrap_or("?");
            let description = tool["description"].as_str().unwrap_or_default();
            assert!(
                !description.trim().is_empty(),
                "{name} says what it does: {tool}"
            );
        }
    });
}

/// One account call makes a customer's workspace and one mints the key that reaches it: the
/// key names its workspace, its MCP endpoint and the token, and reaches that workspace alone.
#[test]
fn a_workspace_and_its_key_are_minted_with_one_call() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let workspace = made(&key, "Applicant 0", json!({"customer_id": "cus_0"})).await;
        let (status, minted) = key
            .post(&account("keys", "mint"), &json!({"workspace": workspace}))
            .await;
        assert_eq!(status, 200, "the account mints a key: {minted}");
        let minted = &minted["result"];
        assert_eq!(minted["workspace"], workspace.as_str(), "{minted}");
        assert!(
            minted["mcp_url"]
                .as_str()
                .is_some_and(|url| url.ends_with(&format!("/w/{workspace}/mcp"))),
            "{minted}"
        );
        assert!(
            minted["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            "{minted}"
        );
        // The customer's key reaches its own workspace and nothing else.
        assert_eq!(
            listed(&key, json!({"customer_id": "cus_0"})).await,
            [workspace]
        );
    });
}

/// Every customer's workspace at once, each labelled with the backend's own id for them; the
/// batch lists whole, each id finds its one workspace, and a label nobody has finds none.
#[test]
fn many_customer_workspaces_are_made_with_one_call_each() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let mut workspaces = Vec::new();
        for customer in 0..8 {
            let label = format!("cus_{customer}");
            let workspace = made(
                &key,
                &format!("Applicant {customer}"),
                json!({"customer_id": label, "batch": "flow"}),
            )
            .await;
            workspaces.push((label, workspace));
        }
        let batch = listed(&key, json!({"batch": "flow"})).await;
        assert_eq!(batch.len(), 8, "{batch:?}");
        for (label, workspace) in &workspaces {
            assert_eq!(
                listed(&key, json!({"customer_id": label})).await,
                std::slice::from_ref(workspace)
            );
        }
        assert!(
            listed(&key, json!({"customer_id": "cus_x"}))
                .await
                .is_empty()
        );

        // Asked again under the same key, a create answers the workspace made.
        let (status, again) = key
            .post(
                &account("workspaces", "create"),
                &json!({"name": "Applicant 0",
                        "labels": {"customer_id": "cus_0", "batch": "flow"}}),
            )
            .await;
        assert!((200..300).contains(&status), "{again}");
        assert_eq!(
            again["result"]["workspace"]["id"],
            workspaces[0].1.as_str(),
            "{again}"
        );
    });
}

/// A branch of a customer's workspace is a workspace of the person's own: made under its own
/// id and labels, listed beside its source, and served at once.
#[test]
fn a_branch_of_a_customers_workspace_is_the_persons_own() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let source = made(&key, "Applicant 0", json!({"customer_id": "cus_0"})).await;
        let (status, branched) = key
            .post(
                &account("workspaces", "branch"),
                &json!({"workspace": source,
                        "name": "Applicant 0: interview",
                        "labels": {"customer_id": "cus_0-branch"}}),
            )
            .await;
        assert_eq!(status, 200, "the account branches it: {branched}");
        let branch = branched["result"]["workspace"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a branch answers the workspace it made: {branched}"))
            .to_owned();
        assert_ne!(branch, source);
        assert_eq!(
            listed(&key, json!({"customer_id": "cus_0-branch"})).await,
            std::slice::from_ref(&branch)
        );
        let (status, read) = key.get(&format!("/v1/workspaces/{branch}/resources")).await;
        assert_eq!(status, 200, "the branch is served at once: {read}");
    });
}

/// A few files go in, each by the address of its own bytes, and read back through the
/// workspace: the byte routes `tilo files put` sends a folder through.
#[test]
fn a_customers_files_are_put_and_read_with_its_workspace() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let workspace = made(&key, "Applicant 0", json!({"customer_id": "cus_0"})).await;
        for text in ["passport scan\n", "cover letter\n"] {
            let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
            let (status, kept) = key
                .request(
                    "PUT",
                    &format!("/v1/workspaces/{workspace}/contents/{hash}"),
                    &[("content-type", "application/octet-stream")],
                    text.as_bytes(),
                )
                .await;
            assert!((200..300).contains(&status), "{kept}");
            let kept = kept["content"]
                .as_str()
                .unwrap_or_else(|| panic!("the put answers its content: {kept}"));
            let reference = kept
                .strip_prefix("tilo.content:")
                .unwrap_or_else(|| panic!("the put answers a content reference: {kept}"));
            let (status, read) = key
                .bytes(&format!("/v1/workspaces/{workspace}/contents/{reference}"))
                .await;
            assert_eq!(
                status,
                200,
                "{reference}: {}",
                String::from_utf8_lossy(&read)
            );
            assert_eq!(read, text.as_bytes(), "the bytes come back");
        }
    });
}

/// The customer's agent, an MCP client outside Tilo, works in its workspace: an issue written
/// through an action, then read back where the app holds it. No window shows the app on the
/// agent's connection, so the record is what it reads, not a view a person at a window sees.
#[test]
fn an_outside_agent_runs_an_action_and_reads_a_view() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let workspace = made(&key, "Applicant 0", json!({"customer_id": "cus_0"})).await;
        let customer = customer(&host, &workspace, "cus_0's agent");

        let wrote = customer
            .tool(
                "tilo_do",
                &json!({"path": "so.tilo.tasks#new_task",
                        "args": {"title": "Checklist of cus_0", "notes": "- passport\n"}}),
            )
            .await;
        assert_eq!(
            wrote["isError"], false,
            "the agent writes an issue: {wrote}"
        );
        let read = customer
            .tool("tilo_read", &json!({"path": "so.tilo.tasks"}))
            .await;
        assert_eq!(read["isError"], false, "{read}");
        assert!(
            read.to_string().contains("Checklist of cus_0"),
            "the record holds the issue the agent wrote: {read}"
        );
    });
}

/// A key scoped to one workspace is turned away at a neighbour's routes, before any of it is
/// read, and the account is beyond it.
#[test]
fn a_key_is_refused_on_a_neighbouring_workspace_and_the_account() {
    host::run(|host| async move {
        let neighbour = uuid::Uuid::new_v4().to_string();
        let customer = customer(&host, host.workspace(), "cus_a's agent");

        // A route naming another workspace is beyond its scope.
        for path in [
            workspace_route(&neighbour, "tasks"),
            workspace_route(&neighbour, "workspace/members"),
        ] {
            let (status, beyond) = customer.get(&path).await;
            assert_eq!(status, 403, "{path}: {beyond}");
            assert_eq!(beyond["error"]["code"], "beyond_scope", "{beyond}");
        }
        // The account is beyond it too: nothing is made, minted or revoked for it.
        for (action, arguments) in [
            ("workspaces#create", json!({"name": "cus_c"})),
            ("keys#mint", json!({"workspace": neighbour})),
            ("keys#revoke", json!({"grant": neighbour})),
        ] {
            let (status, refused) = customer.post(&account_action(action), &arguments).await;
            assert!(
                status >= 400,
                "{action} ran for a customer's key: {refused}"
            );
        }
    });
}

/// Every `/v1` route and every tool a customer's key reaches turns it away from another
/// workspace: no route names one it does not keep.
#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the routes' templates name their parameters in braces"
)]
fn a_workspace_key_reaches_no_other_workspace_through_any_tool_or_route() {
    host::run(|host| async move {
        let neighbour = uuid::Uuid::new_v4().to_string();
        let customer = customer(&host, host.workspace(), "cus_a's agent");

        for operation in tilo_api::v1::operations::OPERATIONS.iter() {
            let path = operation
                .path
                .replace("{workspace}", &neighbour)
                .replace("{path}", "tasks")
                .replace("{action}", "rename")
                .replace("{run}", "00000000-0000-4000-8000-000000000001")
                .replace("{content}", "0000")
                .replace("{grant}", "0000");
            let body = json!({"title": "planted"}).to_string();
            let (status, answer) = customer
                .request(operation.method.as_str(), &path, &[], body.as_bytes())
                .await;
            if operation.path.contains("{workspace}") {
                assert_eq!(status, 403, "{} {path}: {answer}", operation.id);
                assert_eq!(answer["error"]["code"], "beyond_scope", "{answer}");
            } else if !matches!(operation.id, "getApi" | "listAccountResources") {
                assert!(
                    status >= 400,
                    "{} {path} ran for a customer's key: {status} {answer}",
                    operation.id
                );
            }
        }

        // The tools that name a workspace are turned away from another one.
        for tool in ["tilo_ls", "tilo_read", "tilo_find", "tilo_do", "tilo_view"] {
            let answer = customer
                .tool(
                    tool,
                    &json!({"workspace": neighbour, "path": "so.tilo.tasks", "query": "planted"}),
                )
                .await;
            assert!(
                answer["isError"].as_bool() == Some(true) || answer.get("error").is_some(),
                "{tool} reached another workspace: {answer}"
            );
        }
    });
}

/// A server key deletes a customer's workspace when they leave: it leaves every listing, the
/// key minted for it is revoked with it, and deleting it again finds no such workspace.
#[test]
fn a_server_key_deletes_a_customers_workspace_and_everything_it_kept_goes() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let workspace = made(&key, "Applicant: Jane Doe", json!({})).await;
        let kept = made(&key, "Applicant: John Roe", json!({})).await;
        let (status, minted) = key
            .post(&account("keys", "mint"), &json!({"workspace": workspace}))
            .await;
        assert_eq!(status, 200, "{minted}");

        let (status, deleted) = key
            .post(
                &account("workspaces", "delete"),
                &json!({"workspace": workspace}),
            )
            .await;
        assert_eq!(status, 200, "the backend deletes it: {deleted}");
        assert_eq!(
            deleted["result"]["deleted"],
            workspace.as_str(),
            "{deleted}"
        );
        assert_eq!(
            deleted["result"]["revoked_keys"],
            json!([minted["result"]["grant"]]),
            "the key minted for it is revoked with it: {deleted}"
        );
        let remaining = listed(&key, json!({})).await;
        assert!(!remaining.contains(&workspace), "{remaining:?}");
        assert!(remaining.contains(&kept), "{remaining:?}");

        // Deleting it again finds no such workspace.
        let (status, again) = key
            .post(
                &account("workspaces", "delete"),
                &json!({"workspace": workspace}),
            )
            .await;
        assert_ne!(status, 200, "{again}");
        assert_eq!(again["error"]["code"], "not_found", "{again}");
    });
}

/// Neither a customer's key nor a member of the workspace deletes it: what only its owner's key
/// may do is refused, and the workspace stays with the key that reads it.
#[test]
fn neither_a_minted_key_nor_a_member_deletes_a_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let customer = customer(&host, &workspace, "cus_a's agent");

        // A key minted for it reaches no account, so it deletes nothing.
        let (status, refused) = customer
            .post(
                &account("workspaces", "delete"),
                &json!({"workspace": workspace}),
            )
            .await;
        assert!(status >= 400, "a customer's key deletes nothing: {refused}");

        // A key the workspace has no certificate for deletes nothing either: it is refused
        // before any of the workspace's governance answers it.
        let visitor = host.visitor(&workspace).await;
        let refused = visitor
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "remove-member"),
                &json!({"person": "00"}),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(tilo_actions::remote::ActionCallError::NotPermitted)
            ),
            "a visitor deletes nothing: {refused:?}"
        );

        // The workspace stays, and the customer's key still reads it.
        assert_eq!(host.daemon().workspace().expect("the workspace"), workspace);
        let (status, read) = customer
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{read}");
    });
}
