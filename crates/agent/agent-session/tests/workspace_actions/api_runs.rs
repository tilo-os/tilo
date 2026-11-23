//! A workspace's runs, listed through the API on the server as tilo.sh
//! hosts it (`invocations#list`, the resource `invocations` beside the
//! workspace's apps, over REST and MCP alike): two keys made for the workspace and the
//! person's own key each run something there, and each of those keys, which
//! all may read every record there, reads all three, newest first, each with who ran it and how it
//! ended. A command waiting for the person reads as waiting until they allow
//! it, then as ended; a run taken back reads as undone, and one that someone's
//! later change stands on says so. Pages hold `limit` runs and read on from
//! their cursor; a key for another workspace is refused as it is on the
//! neighbouring routes. After the server restarts, every run that changed a
//! record is still listed, by the same step, from what the records keep.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{api_on_server_shelled, approved_key, headers, json_of};
use super::api_unattended::{Key, answer, backend, page_in, waiting, workspace};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// `key` runs `action` on `path` in `workspace` with `args`, as a step of the
/// run its client calls `title`: its answer.
async fn in_run(
    key: &Key<'_>,
    workspace: &str,
    (path, action): (&str, &str),
    args: &Value,
    title: &str,
) -> (u16, Value) {
    let bearer = format!("Bearer {}", key.token);
    let run = title.to_lowercase().replace(' ', "-");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
        ("tilo-invocation", run.as_str()),
        ("tilo-invocation-title", title),
    ];
    let route = format!("/v1/workspaces/{workspace}/resources/{path}/actions/{action}");
    let reply = Box::pin(key.api.dispatch(
        &hyper::Method::POST,
        &route,
        "",
        &headers(&pairs),
        args.to_string().as_bytes(),
    ))
    .await;
    Box::pin(json_of(reply)).await
}

/// `key` writes `text` into the workspace's letter, in a run of its own
/// called `title`: the id of the run's last call, as the action answered it.
async fn write_letter(key: &Key<'_>, workspace: &str, text: &str, title: &str) -> String {
    let (status, opened) = in_run(
        key,
        workspace,
        ("word-processor", "open"),
        &json!({}),
        title,
    )
    .await;
    assert_eq!(status, 200, "{opened}");
    let (status, written) = in_run(
        key,
        workspace,
        ("word-processor", "append-paragraph"),
        &json!({"text": text}),
        title,
    )
    .await;
    assert_eq!(status, 200, "{written}");
    written["invocation"].as_str().unwrap().to_owned()
}

/// Where `invocations#list` is run.
fn listing(workspace: &str) -> String {
    format!("/v1/workspaces/{workspace}/resources/invocations/actions/list")
}

/// The page of `workspace`'s runs `asked` asks for (`limit`, `cursor`), as `key`
/// reads it.
async fn invocations(key: &Key<'_>, workspace: &str, asked: &Value) -> Value {
    let (status, answered) = key.post(&listing(workspace), asked).await;
    assert_eq!(status, 200, "{answered}");
    answered["result"].clone()
}

/// The run titled `title` in `listed`.
fn titled<'a>(listed: &'a Value, title: &str) -> &'a Value {
    listed["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|run| run["title"] == title)
        .unwrap_or_else(|| panic!("a run titled {title:?} is listed: {listed}"))
}

/// The run named `id` in `listed`.
fn with_id<'a>(listed: &'a Value, id: &str) -> &'a Value {
    listed["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|run| run["id"] == id)
        .unwrap_or_else(|| panic!("run {id} is listed: {listed}"))
}

const LETTERS: [(&str, &str); 3] = [
    ("Jane Doe's agent", "Agent's letter"),
    ("Reviewer's agent", "Reviewer's note"),
    ("Visa backend", "Backend's postscript"),
];

/// An invocation is taken back by a key other than the one that ran it, on
/// a connection of its own, by the id the invocation's answer named; after the server
/// restarted, when no call of an invocation is remembered, by the step the listing
/// names it by and by the id its answer named, as history keeps both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invocation_is_taken_back_after_a_reconnect_and_a_restart() {
    Box::pin(taken_back_after_a_restart()).await;
}

async fn taken_back_after_a_restart() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let (ada, token) = backend(&on_server).await;
    let person = Key {
        api: &on_server.api,
        token,
    };
    let ws = workspace(&person, "Applicant: Jane Doe").await;
    let later = workspace(&person, "Applicant: John Doe").await;
    let third = workspace(&person, "Applicant: Max Doe").await;
    let agent = Key {
        api: &on_server.api,
        token: person.mint(&ws, "Jane Doe's agent").await,
    };
    let other = Key {
        api: &on_server.api,
        token: person.mint(&later, "John Doe's agent").await,
    };
    let another = Key {
        api: &on_server.api,
        token: person.mint(&third, "Max Doe's agent").await,
    };
    let letter = write_letter(&agent, &ws, "Dear Visa Officer,", "Cover letter").await;
    write_letter(&other, &later, "Yours faithfully", "Closing").await;
    let postscript = write_letter(&another, &third, "P.S. Enclosed", "Postscript").await;

    let (status, undone) = person
        .post(
            &format!("/v1/workspaces/{ws}/invocations/{letter}/undo"),
            &json!({}),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    let listed = invocations(&person, &ws, &json!({})).await;
    let taken = titled(&listed, "Cover letter");
    assert_eq!(taken["status"], "undone", "{listed}");
    assert_eq!(taken["undone"], taken["changes"], "{taken}");

    drop((agent, other, another, person));
    let on_server = on_server.restart(false).await;
    let page = enrol(&ada, "Ada's browser again", DeviceCapabilities::member());
    let person = Key {
        api: &on_server.api,
        token: Box::pin(approved_key(&on_server, &ada, &page, "Visa backend")).await,
    };
    let listed = invocations(&person, &third, &json!({})).await;
    let postscript_run = titled(&listed, "Postscript");
    assert_eq!(postscript_run["remembered"], false, "{postscript_run}");
    let (status, undone) = person
        .post(
            &format!("/v1/workspaces/{third}/invocations/{postscript}/undo"),
            &json!({}),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    let listed = invocations(&person, &third, &json!({})).await;
    let taken = titled(&listed, "Postscript");
    assert_eq!(taken["status"], "undone", "{listed}");
    assert_eq!(taken["undone"], taken["changes"], "{taken}");

    let listed = invocations(&person, &later, &json!({})).await;
    let closing = titled(&listed, "Closing");
    assert_eq!(closing["remembered"], false, "{closing}");
    let step = closing["step"].as_str().unwrap();
    let (status, undone) = person
        .post(
            &format!("/v1/workspaces/{later}/invocations/{step}/undo"),
            &json!({}),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    let listed = invocations(&person, &later, &json!({})).await;
    let closing = titled(&listed, "Closing");
    assert_eq!(closing["status"], "undone", "{listed}");
    assert_eq!(closing["undone"], closing["changes"], "{closing}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keys_reading_a_workspace_list_its_runs_newest_first_also_after_a_restart() {
    Box::pin(every_run_listed()).await;
}

async fn every_run_listed() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, token) = backend(&on_server).await;
    let person = Key { api: &api, token };
    let ws = workspace(&person, "Applicant: Jane Doe").await;
    let agent = Key {
        api: &api,
        token: person.mint(&ws, "Jane Doe's agent").await,
    };
    let reviewer = Key {
        api: &api,
        token: person.mint(&ws, "Reviewer's agent").await,
    };
    let elsewhere = workspace(&person, "Another applicant").await;
    let others = Key {
        api: &api,
        token: person.mint(&elsewhere, "Other's agent").await,
    };

    // Two keys made for the workspace and the person's own key each write
    // into its letter, one after the other, in runs of their own.
    let mut ids = Vec::new();
    for ((_, title), key) in LETTERS.iter().zip([&agent, &reviewer, &person]) {
        ids.push(write_letter(key, &ws, &format!("A line of the {title}"), title).await);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Each key, reading every record there, lists all three, newest first,
    // each with who ran it and how it ended.
    for reader in [&agent, &reviewer, &person] {
        let listed = invocations(reader, &ws, &json!({})).await;
        let order: Vec<&str> = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|run| run["title"].as_str())
            .collect();
        assert_eq!(
            order,
            ["Backend's postscript", "Reviewer's note", "Agent's letter"],
            "newest first: {listed}"
        );
        for (((name, title), id), _) in LETTERS.iter().zip(&ids).zip(0..) {
            let run = titled(&listed, title);
            assert_eq!(run["id"], id.as_str(), "{run}");
            assert!(
                run["by"]["name"].as_str().unwrap().starts_with(name),
                "{run}"
            );
            assert_eq!(run["by"]["kind"], "agent", "{run}");
            assert_eq!(run["status"], "ended", "{run}");
            assert!(
                run["calls"].as_array().unwrap().contains(&json!(id)),
                "its calls are its own: {run}"
            );
            assert!(run["changes"].as_u64().unwrap() >= 1, "{run}");
            assert_eq!(run["records"][0]["app"], "word-processor", "{run}");
            assert!(
                run["ended_ms"].as_i64() >= run["started_ms"].as_i64(),
                "{run}"
            );
            assert_eq!(
                (&run["recorded"], &run["remembered"]),
                (&json!(true), &json!(true))
            );
        }
        assert_eq!(
            listed["total"].as_u64().unwrap() as usize,
            listed["items"].as_array().unwrap().len(),
            "the first page holds every run: {listed}"
        );
        titled(&listed, "Agent's letter");
    }
    // The agent's letter stands under the others' lines: it says so.
    let listed = invocations(&agent, &ws, &json!({})).await;
    let first = titled(&listed, "Agent's letter");
    assert_eq!(first["undoable"], false, "{first}");
    assert!(
        first["undo_blocked"]
            .as_str()
            .unwrap()
            .contains("word-processor"),
        "{first}"
    );
    assert_eq!(titled(&listed, "Backend's postscript")["undoable"], true);

    // A command the person must allow reads as waiting, then as ended.
    let mut ada_at_page = page_in(&on_server, &ada, &ws).await;
    let (status, ran) = agent.run(&ws, "echo listed-while-waiting").await;
    let command = waiting(status, &ran).unwrap_or_else(|| panic!("asked: {status} {ran}"));
    let listed = invocations(&reviewer, &ws, &json!({})).await;
    let waits = with_id(&listed, &command);
    assert_eq!(waits["status"], "waiting", "{waits}");
    assert!(
        waits["by"]["name"]
            .as_str()
            .unwrap()
            .starts_with("Jane Doe's agent"),
        "{waits}"
    );
    assert_eq!(waits["recorded"], false, "it changed nothing yet: {waits}");
    assert!(listed["waiting"].as_u64().unwrap() >= 1, "{listed}");
    answer(&mut ada_at_page, &command, "approve").await;
    let (status, followed) = agent.follow(&ws, &command).await;
    assert_eq!(status, 200, "{followed}");
    let listed = invocations(&reviewer, &ws, &json!({})).await;
    let ended = with_id(&listed, &command);
    assert_eq!(ended["status"], "ended", "{ended}");
    assert!(ended["ended_ms"].is_i64(), "{ended}");
    assert_eq!(listed["waiting"], 0, "{listed}");
    if ended["recorded"] != true {
        let (_, history) = agent
            .post(
                &format!("/v1/workspaces/{ws}/resources/terminal/actions/history"),
                &json!({"last": 20}),
            )
            .await;
        panic!("the command's run is in the terminal's history: {ended} {history}");
    }
    ada_at_page.peer.close();

    // The person's key takes its run back: it reads as undone.
    let backends = &ids[2];
    let (status, undone) = person
        .post(
            &format!("/v1/workspaces/{ws}/invocations/{backends}/undo"),
            &json!({}),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    let listed = invocations(&agent, &ws, &json!({})).await;
    let taken = titled(&listed, "Backend's postscript");
    assert_eq!(taken["status"], "undone", "{taken}");
    assert_eq!(taken["undone"], taken["changes"], "{taken}");
    assert_eq!(taken["undoable"], false, "{taken}");

    // A page holds `limit` runs, the totals the whole, and the next page
    // reads on from its cursor.
    let total = listed["total"].as_u64().unwrap();
    assert!(
        total >= 5,
        "three letters, the command and the undo: {listed}"
    );
    let first = invocations(&agent, &ws, &json!({"limit": 2})).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2, "{first}");
    assert_eq!(first["total"], total, "{first}");
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = invocations(&agent, &ws, &json!({"limit": 2, "cursor": cursor})).await;
    assert_eq!(second["items"].as_array().unwrap().len(), 2, "{second}");
    let seen = |page: &Value| -> Vec<Value> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["started_ms"].clone())
            .collect()
    };
    let (first_seen, second_seen) = (seen(&first), seen(&second));
    assert!(
        first_seen.last().unwrap().as_i64() >= second_seen.first().unwrap().as_i64(),
        "the second page reads on, older: {first} {second}"
    );
    assert_eq!(listed["items"][2], second["items"][0], "{listed} {second}");
    let (status, refused) = agent.post(&listing(&ws), &json!({"limit": "many"})).await;
    assert_eq!(status, 400, "{refused}");

    // A key for another workspace is refused as on the routes beside it.
    let (status, refused) = others.post(&listing(&ws), &json!({})).await;
    let (beside_status, beside_refused) = others
        .get(&format!("/v1/workspaces/{ws}/invocations/{}", ids[0]))
        .await;
    let (members_status, members_refused) = others
        .post(
            &format!("/v1/workspaces/{ws}/resources/workspace/members/actions/list"),
            &json!({}),
        )
        .await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (beside_status, &beside_refused["error"]["code"]),
        "{refused} {beside_refused}"
    );
    assert_eq!(
        (status, &refused["error"]["code"]),
        (members_status, &members_refused["error"]["code"]),
        "{refused} {members_refused}"
    );
    assert_eq!(status, 403, "{refused}");

    // Over MCP, the agent's own key finds the listing beside the workspace's apps,
    // pages it as any other resource and runs the same action.
    let (status, minted) = person
        .post(
            "/v1/resources/account/keys/actions/mint",
            &json!({"workspace": ws, "name": "Listing agent"}),
        )
        .await;
    assert_eq!(status, 200, "{minted}");
    let minted = &minted["result"];
    let mut mcp = Mcp {
        api: &api,
        path: minted["mcp_url"]
            .as_str()
            .unwrap()
            .strip_prefix(ISSUER)
            .unwrap()
            .to_owned(),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    let (status, initialized) = mcp.initialize().await;
    assert_eq!(status, 200, "{initialized}");
    let (status, root) = mcp
        .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {"path": ""}}}))
        .await;
    assert_eq!(status, 200, "{root}");
    let root_said = root["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        root_said
            .lines()
            .any(|line| line.starts_with("invocations")),
        "the workspace's root lists its invocations: {root}"
    );
    let (status, paged) = mcp
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {"path": "invocations", "limit": 1}}}))
        .await;
    assert_eq!(status, 200, "{paged}");
    let paged_said = paged["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        paged_said.contains("cursor"),
        "one invocation to a page, and where the next starts: {paged}"
    );
    let (status, read) = mcp
        .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                     "params": {"name": "tilo_do", "arguments": {"path": "invocations#list"},
                                "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await;
    assert_eq!(status, 200, "{read}");
    let over_mcp = &read["result"]["structuredContent"]["result"];
    assert_eq!(over_mcp["total"], total, "{read}");
    titled(over_mcp, "Agent's letter");

    // After the server restarts, every run that changed a record is still
    // listed, by the same step, though no call of it is remembered.
    let steps: Vec<(String, Value)> = LETTERS
        .iter()
        .map(|(_, title)| ((*title).to_owned(), titled(&listed, title)["step"].clone()))
        .collect();
    let command_step = with_id(&listed, &command)["step"].clone();
    drop((agent, reviewer, others, person, mcp));
    drop(api);
    let on_server = on_server.restart(false).await;
    let page = enrol(&ada, "Ada's browser again", DeviceCapabilities::member());
    let person = Key {
        api: &on_server.api,
        token: Box::pin(approved_key(&on_server, &ada, &page, "Visa backend")).await,
    };
    let listed = invocations(&person, &ws, &json!({})).await;
    for (title, step) in &steps {
        let run = titled(&listed, title);
        assert!(step.is_string(), "{title} had a step");
        assert_eq!(&run["step"], step, "{run}");
        assert_eq!(run["recorded"], true, "{run}");
        assert_eq!(run["remembered"], false, "{run}");
        assert!(
            run.get("id").is_none(),
            "no call of it is remembered: {run}"
        );
    }
    assert_eq!(titled(&listed, "Backend's postscript")["status"], "undone");
    assert_eq!(titled(&listed, "Agent's letter")["status"], "ended");
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["step"] == command_step && run["recorded"] == true),
        "the command the person allowed is still listed: {listed}"
    );
}
