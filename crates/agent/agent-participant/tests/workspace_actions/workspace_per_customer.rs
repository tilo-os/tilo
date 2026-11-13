//! A backend keeps a workspace per customer through the API server on the
//! relay, as tilo.sh runs them, labelled with its own id for the customer,
//! and mints a key per workspace for that customer's agent. The agent, over
//! MCP with only its own key, works in its own workspace; no tool and no
//! `/v1` route takes its key into another customer's workspace, or the
//! account.
use super::api::{ISSUER, Mcp};
use super::api_on_relay::{OnRelay, api_on_relay, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use tilo_api::Server;

/// A server key of a new person's on `on_relay`, approved on a page of theirs.
async fn server_key(on_relay: &OnRelay, name: &str) -> String {
    let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
    let page = enrol(
        &root,
        &format!("{name}'s browser"),
        DeviceCapabilities::member(),
    );
    approved_key(on_relay, &root, &page, "Visa backend").await
}

/// `method path?query` with `body` as `token`: the status and JSON answer.
async fn rest(
    api: &Arc<Server>,
    token: &str,
    method: hyper::Method,
    path: &str,
    body: &[u8],
    extra: &[(&'static str, &str)],
) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    let mut pairs = vec![
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    pairs.extend_from_slice(extra);
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    json_of(
        api.dispatch(&method, path, query, &headers(&pairs), body)
            .await,
    )
    .await
}

/// An MCP session of `token` at `path`.
fn mcp<'a>(api: &'a Arc<Server>, path: &str, token: &str) -> Mcp<'a> {
    Mcp {
        api,
        path: path.to_owned(),
        token: token.to_owned(),
        session: None,
    }
}

/// `tool` with `arguments`, typed: the status and the reply.
async fn call(session: &mut Mcp<'_>, tool: &str, arguments: Value) -> (u16, Value) {
    session
        .post(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                     "params": {"name": tool, "arguments": arguments,
                                "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await
}

/// The ids `token` lists among its workspaces.
async fn listed(api: &Arc<Server>, token: &str, query: &str) -> Vec<String> {
    let (status, list) = rest(
        api,
        token,
        hyper::Method::GET,
        &format!("/v1/workspaces{query}"),
        b"",
        &[],
    )
    .await;
    assert_eq!(status, 200, "{list}");
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

/// What `path` with `args` answered once it finished: a pending answer is
/// followed with `tilo_status`, as an agent and the SDKs follow it.
async fn done(session: &mut Mcp<'_>, path: &str, args: Value) -> Value {
    let mut answer = session.tilo_do(json!({"path": path, "args": args})).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let typed = &answer["result"]["structuredContent"];
        if typed["status"] != "pending" {
            assert_eq!(typed["status"], "ok", "{path} ran: {answer}");
            return typed["result"].clone();
        }
        assert!(Instant::now() < deadline, "{path} never finished: {answer}");
        let invocation = typed["invocation_id"].clone();
        tokio::time::sleep(Duration::from_millis(200)).await;
        answer = call(session, "tilo_status", json!({"invocation_id": invocation}))
            .await
            .1;
    }
}

/// `field` of a tool's answer: typed, or in the JSON its text carries.
fn answered(reply: &Value, field: &str) -> Value {
    let result = &reply["result"];
    if let Some(typed) = result["structuredContent"].get(field) {
        return typed.clone();
    }
    result["content"][0]["text"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get(field).cloned())
        .unwrap_or(Value::Null)
}

/// `token`'s agent, over MCP at its workspace's URL, writes a page titled
/// `title` there and puts a file in: the page's path and the file's reference.
async fn work(api: &Arc<Server>, mcp_url: &str, token: &str, title: &str) -> (String, String) {
    let path = mcp_url.strip_prefix(ISSUER).unwrap();
    let mut agent = mcp(api, path, token);
    let began = Instant::now();
    let (status, opened) = agent.initialize().await;
    assert_eq!(status, 200, "{opened}");
    let entered = began.elapsed();
    let (status, apps) = call(&mut agent, "tilo_ls", json!({})).await;
    eprintln!(
        "{title}: entered in {entered:?}, first catalogue in {:?}",
        began.elapsed().saturating_sub(entered)
    );
    assert_eq!(status, 200, "{apps}");
    assert_eq!(apps["result"]["isError"], false, "{apps}");
    done(
        &mut agent,
        "notes#create-page",
        json!({"title": title, "markdown": "- passport\n"}),
    )
    .await;
    let (_, notes) = call(&mut agent, "tilo_ls", json!({"path": "notes"})).await;
    let page = notes["result"]["structuredContent"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.to_string().contains(title))
        .unwrap_or_else(|| panic!("its page is listed: {notes}"))["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, begun) = call(&mut agent, "tilo_put_begin", json!({"size": 9})).await;
    let upload = answered(&begun, "upload");
    assert!(!upload.is_null(), "a put begins: {begun}");
    let (_, appended) = call(
        &mut agent,
        "tilo_put_append",
        json!({"upload": upload, "offset": 0, "chunk": "cGFzc3BvcnQK"}),
    )
    .await;
    assert_eq!(appended["result"]["isError"], false, "{appended}");
    let (_, finished) = call(&mut agent, "tilo_put_finish", json!({"upload": upload})).await;
    let content = answered(&finished, "content")
        .as_str()
        .unwrap_or_else(|| panic!("a put finishes: {finished}"))
        .to_owned();
    (page, content)
}

/// What a tool is asked with, naming `other`'s workspace and what is in it:
/// its page, its file, a path in it.
fn arguments_against(tool: &str, other: &str, page: &str, content: &str) -> Value {
    let mut arguments = match tool {
        "tilo_ls" => json!({"path": "notes"}),
        "tilo_read" | "tilo_link" | "tilo_watch" => json!({"path": page}),
        "tilo_do" => json!({"path": format!("{page}#rename"), "args": {"title": "taken"}}),
        "tilo_find" => json!({"query": "Checklist"}),
        "tilo_get" => json!({"content": content}),
        "tilo_put_begin" => json!({"size": 4}),
        "tilo_put_append" => json!({"upload": content, "offset": 0, "chunk": "eA=="}),
        "tilo_put_finish" | "tilo_status" | "tilo_cancel" => {
            json!({"upload": content, "invocation_id": content})
        }
        _ => json!({}),
    };
    arguments["workspace"] = json!(other);
    arguments
}

/// A `/v1` route's template with `other`'s workspace and what is in it
/// filled in.
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the route templates name their parameters in braces"
)]
fn route_against(template: &str, other: &str, page: &str, content: &str, grant: &str) -> String {
    template
        .replace("{workspace}", other)
        .replace("{path}", page)
        .replace("{action}", "rename")
        .replace("{run}", "00000000-0000-4000-8000-000000000001")
        .replace("{content}", content)
        .replace("{webhook}", "00000000-0000-4000-8000-000000000002")
        .replace("{grant}", grant)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "every tool and every route, against one other workspace"
)]
async fn a_workspace_key_reaches_no_other_workspace_through_any_tool_or_route() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let key = server_key(&on_relay, "Ada").await;
    let make = |name: &'static str| {
        let (api, key) = (Arc::clone(&api), key.clone());
        async move {
            let (status, made) = rest(
                &api,
                &key,
                hyper::Method::POST,
                "/v1/workspaces",
                json!({"name": name, "labels": {"customer_id": name}})
                    .to_string()
                    .as_bytes(),
                &[],
            )
            .await;
            assert_eq!(status, 201, "{made}");
            let workspace = made["id"].as_str().unwrap().to_owned();
            let (status, minted) = rest(
                &api,
                &key,
                hyper::Method::POST,
                "/v1/keys",
                json!({"workspace": workspace}).to_string().as_bytes(),
                &[],
            )
            .await;
            assert_eq!(status, 201, "{minted}");
            (workspace, minted)
        }
    };
    let (a, a_key) = make("cus_a").await;
    let (b, b_key) = make("cus_b").await;
    let a_token = a_key["token"].as_str().unwrap().to_owned();
    let b_token = b_key["token"].as_str().unwrap().to_owned();
    let b_grant = b_key["grant"].as_str().unwrap().to_owned();
    // B's agent writes a page there no one else may read.
    let secret = "Checklist of cus_b, sealed";
    let (b_page, b_content) =
        work(&api, b_key["mcp_url"].as_str().unwrap(), &b_token, secret).await;
    work(
        &api,
        a_key["mcp_url"].as_str().unwrap(),
        &a_token,
        "Checklist of cus_a",
    )
    .await;
    // What only B holds: its page's words. (A call may echo back what it
    // was asked with, B's id or file reference among it; that is no leak.)
    let leaks = |answer: &Value| answer.to_string().contains(secret);

    // B's MCP URL turns A's key away before any session opens.
    let mut at_b = mcp(&api, &format!("/w/{b}/mcp"), &a_token);
    let (status, refused) = at_b.initialize().await;
    assert_eq!(status, 403, "{refused}");
    assert!(at_b.session.is_none());

    // Every tool A's sessions list, naming B: at A's own URL and at the
    // server's, where a call names its workspace.
    for path in [format!("/w/{a}/mcp"), "/mcp".to_owned()] {
        let mut session = mcp(&api, &path, &a_token);
        assert_eq!(session.initialize().await.0, 200, "{path}");
        // The server's own session lists the workspace tools once it served
        // a workspace: A's.
        let _ = call(&mut session, "tilo_ls", json!({"workspace": a})).await;
        let (_, tools) = session
            .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;
        let names: Vec<String> = tools["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} lists its tools: {tools}"))
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(names.iter().any(|name| name == "tilo_do"), "{names:?}");
        for tool in &names {
            let arguments = arguments_against(tool, &b, &b_page, &b_content);
            let (_, answer) = call(&mut session, tool, arguments.clone()).await;
            assert!(
                !leaks(&answer),
                "{tool} {arguments} at {path} answered B's: {answer}"
            );
        }
        // The account is beyond it: no listing of B, no deleting or minting there.
        for (action, args) in [
            ("account/workspaces#list", json!({})),
            ("account/workspaces#delete", json!({"workspace": b})),
            ("account/keys#mint", json!({"workspace": b})),
            ("account/keys#revoke", json!({"grant": b_grant})),
            ("account/workspaces#create", json!({"name": "cus_c"})),
        ] {
            let answer = session.tilo_do(json!({"path": action, "args": args})).await;
            assert_ne!(
                answer["result"]["structuredContent"]["status"], "ok",
                "{action} at {path} ran for A's key: {answer}"
            );
        }
    }

    // Every `/v1` route, as A's key, naming B or what is B's.
    for operation in tilo_api::v1::operations::OPERATIONS.iter() {
        let route = route_against(operation.path, &b, &b_page, &b_content, &b_grant);
        let body = match operation.id {
            "createWorkspace" => json!({"name": "cus_c"}),
            "mintKey" => json!({"workspace": b}),
            "runProgram" => json!({"program": "notes"}),
            "invitePerson" => json!({"role": "editor"}),
            "createWebhook" => json!({"url": "https://example.com/hook"}),
            _ => json!({"title": "taken"}),
        };
        let (status, answer) = rest(
            &api,
            &a_token,
            operation.method.clone(),
            &route,
            body.to_string().as_bytes(),
            &[],
        )
        .await;
        assert!(
            !leaks(&answer),
            "{} {route} answered B's: {answer}",
            operation.id
        );
        if operation.path.contains("{workspace}") {
            assert_eq!(status, 403, "{} {route}: {answer}", operation.id);
            assert_eq!(
                answer["error"]["code"], "beyond_scope",
                "{} {route}",
                operation.id
            );
        } else if !matches!(operation.id, "getApi" | "listWorkspaces" | "listKeys") {
            assert!(
                status >= 400,
                "{} {route} ran for A's key: {status} {answer}",
                operation.id
            );
        }
    }
    // A's key lists A alone, and only its own key.
    assert_eq!(listed(&api, &a_token, "").await, std::slice::from_ref(&a));

    // B is as its agent left it: its page, its file, its key.
    let mut b_agent = mcp(&api, &format!("/w/{b}/mcp"), &b_token);
    assert_eq!(b_agent.initialize().await.0, 200);
    let (_, notes) = call(&mut b_agent, "tilo_ls", json!({"path": "notes"})).await;
    assert!(notes.to_string().contains(secret), "{notes}");
    assert!(!notes.to_string().contains("taken"), "{notes}");
    assert_eq!(listed(&api, &key, "?label[customer_id]=cus_b").await, [b]);
}
