//! Programs on a workspace the server hosts: the `tilo` tool and `POST …/programs` run a
//! tilolang program as the key's agent, compiled against the apps the workspace lists, and what
//! the program reads and changes goes through the same action API as every other call.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{OnServer, api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};

/// `POST /v1/workspaces/{workspace}/programs` as `token`'s caller: the status and the typed
/// answer.
async fn programs(
    on_server: &OnServer,
    token: &str,
    workspace: &str,
    program: &str,
) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    json_of(
        on_server
            .api
            .dispatch(
                &hyper::Method::POST,
                &format!("/v1/workspaces/{workspace}/programs"),
                "",
                &headers(&[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ]),
                json!({"program": program}).to_string().as_bytes(),
            )
            .await,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_program_runs_on_a_workspace_the_server_hosts() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Visa backend").await;
    let mut account = Mcp {
        api: &on_server.api,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let created = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Applicant: Jane Doe"}}))
        .await;
    let workspace = created["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {created}"))
        .to_owned();
    let mut session = Mcp {
        api: &on_server.api,
        path: format!("/w/{workspace}/mcp"),
        token: key.clone(),
        session: None,
    };
    let (status, init) = session.initialize().await;
    assert_eq!(status, 200, "{init}");
    let tilo = |program: &str| {
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "tilo", "arguments": {"program": program}}})
    };

    // A program reads the workspace's app: the tracker the app keeps, named through its
    // declarations, which the workspace's host publishes.
    let (status, read) = session.post(tilo("tasks.tracker")).await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["result"]["isError"], false, "{read}");

    // `/programs`, as `tilo run` reaches it, changes the record through the app's own action,
    // as one step.
    let (status, made) = programs(
        &on_server,
        &key,
        &workspace,
        "tasks.create(tasks.issues, title: \"From the hosted task\")",
    )
    .await;
    assert_eq!(status, 200, "{made}");
    assert_eq!(made["status"], "ok", "{made}");
    assert_eq!(made["changed"], 1, "{made}");
    assert_eq!(made["steps"].as_array().map(Vec::len), Some(1), "{made}");

    // The issue is in the workspace's record, read as a person's call reads it.
    let (status, read) = session
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_read", "arguments": {"path": "tasks/Issues"}}}))
        .await;
    assert_eq!(status, 200, "{read}");
    assert!(
        read.to_string().contains("From the hosted task"),
        "the issue the program made is in the workspace: {read}"
    );

    // Another note through the session's own tool: its step is the task's history, so `undo`
    // takes it back.
    let (status, made) = session
        .post(tilo(
            "tasks.create(tasks.issues, title: \"Also from the task\")",
        ))
        .await;
    assert_eq!(status, 200, "{made}");
    assert_eq!(made["result"]["isError"], false, "{made}");
    let (status, history) = session.post(tilo("history task")).await;
    assert_eq!(status, 200, "{history}");
    assert!(
        history.to_string().contains("Also from the task"),
        "the step the program took is the task's history: {history}"
    );
}

/// The lines one REPL entry printed, as one text.
fn said(said: tilolang_cli::repl::Said) -> String {
    match said {
        tilolang_cli::repl::Said::Lines(lines) => lines.join("\n"),
        tilolang_cli::repl::Said::Quit => panic!("the entry ended the REPL"),
    }
}

/// `tilolang repl` as the person, with the key this computer holds of theirs: in process, through
/// the client `tilo` uses. Its entries are one program session of `POST …/programs`: a `let`
/// holds for the entries after it, a bulk change waits for `confirm`, `undo` takes the last step
/// back, a page an entry read is named by its reference in the next, and a second REPL is a
/// session of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_repl_works_in_a_workspace_as_its_person() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    approved_key(&on_server, &ada, &page, "Visa backend").await;
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("laptop.json");
    std::fs::write(
        &key_file,
        serde_json::to_vec(
            &tilo_api::direct::KeyFile::new(
                &on_server.server,
                Some(&on_server.web_entry),
                "Ada's laptop",
                &laptop.key,
                &laptop.cert,
            )
            .reaching_at(on_server.hosted.web.host_info().clone()),
        )
        .unwrap(),
    )
    .unwrap();
    let client = tilo_api::client::Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&key_file),
        json: false,
    })
    .unwrap();
    let made = client
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": "Ada's notes"}),
        )
        .await
        .unwrap();
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    tilolang_cli::repl::style::set(false);

    let mut repl = tilolang_cli::repl::Repl::open(&client, &workspace)
        .await
        .unwrap();
    assert!(
        repl.apps().iter().any(|app| app == "tasks"),
        "the REPL completes the workspace's apps: {:?}",
        repl.apps()
    );

    // A `let` holds for the entries after it.
    said(repl.entry("let title = \"Groceries\"").await);
    let made = said(repl.entry("tasks.create(tasks.issues, title: title)").await);
    assert!(!made.contains("error"), "{made}");

    // A change to many waits for `confirm`, and is then one step.
    let bulk = "for t in [\"Bins\", \"Oat milk\", \"Plants\", \"Library\", \"Haircut\"] { \
                tasks.create(tasks.issues, title: t) }";
    let previewed = said(repl.entry(bulk).await);
    assert!(previewed.contains("preview:"), "{previewed}");
    let applied = said(repl.entry("confirm").await);
    assert!(
        !applied.contains("error") && !applied.contains("preview:"),
        "{applied}"
    );
    assert_eq!(
        said(repl.entry("confirm").await),
        "no preview waits for confirm"
    );

    // `undo` takes the step back whole, and `history task` says so.
    let undone = said(repl.entry("undo").await);
    assert!(undone.contains("undone:"), "{undone}");
    let history = said(repl.entry("history task").await);
    assert!(history.contains("(undone)"), "{history}");

    // A program reads another app's records: the tracker's issues, each shown with a
    // reference a following entry names it by.
    let pages = said(repl.entry("tasks.issues.issues").await);
    // The five the bulk made are undone, so the one issue left is the first the REPL made.
    let groceries = pages
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("the tracker's issues are read: {pages}"))
        .to_owned();
    assert!(
        !groceries.is_empty() && groceries.len() < 8 && groceries != "Groceries",
        "an issue is shown by a short reference: {pages}"
    );
    assert!(
        !pages.contains("Bins") && pages.lines().count() == 1,
        "the undone issues are gone: {pages}"
    );
    let title = said(repl.entry(&format!("{groceries}.title")).await);
    assert_eq!(
        title, "Groceries",
        "a following entry names the page: {pages}"
    );

    // Another REPL is a session of its own: the first one's `let`s and references are not
    // there.
    let mut other = tilolang_cli::repl::Repl::open(&client, &workspace)
        .await
        .unwrap();
    let unknown = said(
        other
            .entry("tasks.create(tasks.issues, title: title)")
            .await,
    );
    assert!(unknown.contains("error"), "{unknown}");
}
