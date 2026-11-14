//! A small company's workspace at real size, worked through the API the way
//! an agent moving it in does: a notebook of 1,000 notes, each a page of
//! Markdown, and a tracker of 500 issues, every one made through the record's
//! own action on the relay host. Listing, reading, finding and a write
//! followed by a read each answer in a time that does not grow with how much
//! the records hold, a listing pages through every row with an honest cursor,
//! and a tracker's listing names each kind's actions once.

use super::api::ISSUER;
use super::api_on_relay::{api_on_relay, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use tilo_api::Server;

/// A key's calls of the API.
struct V1<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl V1<'_> {
    async fn send(&self, method: hyper::Method, path: &str, body: &[u8]) -> (u16, Value, usize) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let reply = Box::pin(
            self.api
                .dispatch(&method, path, query, &headers(&pairs), body),
        )
        .await;
        let (status, value) = json_of(reply).await;
        let size = value.to_string().len();
        (status, value, size)
    }

    async fn get(&self, path: &str) -> (u16, Value, usize) {
        self.send(hyper::Method::GET, path, b"").await
    }

    /// `action` of `resource` with `args`, which ran: its result and its
    /// size.
    async fn act(&self, base: &str, resource: &str, action: &str, args: &Value) -> (Value, usize) {
        let (status, reply, size) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
            )
            .await;
        assert_eq!(status, 200, "{resource}#{action} {args}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action} {args}: {reply}");
        (reply["result"].clone(), size)
    }
}

/// A note's Markdown, a few kilobytes like a project's docs.
fn page(index: usize) -> String {
    let mut text = format!("# Design note {index}\n\n");
    for section in 0..6 {
        let _ = write!(
            text,
            "## Part {section}\n\nThe service keeps record {index} beside its peers, and \
             [the guide](https://example.org/{index}/{section}) says why a **retry** is \
             safe: it asks again under the same id.\n\n- one\n- two\n- three\n\n\
             ```sh\necho {index} {section}\n```\n\n"
        );
    }
    text
}

fn quantile(durations: &[Duration], quantile: f64) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

fn count(env: &str, default: usize) -> usize {
    std::env::var(env)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The time `call` took, and what it answered.
async fn timed<T>(call: impl std::future::Future<Output = T>) -> (Duration, T) {
    let began = Instant::now();
    let answer = call.await;
    (began.elapsed(), answer)
}

/// Every row of the listing at `path`, page by page with `limit` rows a
/// page as the cursor each page answers leads on, and the pages it took.
async fn every_row(agent: &V1<'_>, base: &str, path: &str, limit: usize) -> (Vec<Value>, usize) {
    let (mut rows, mut pages) = (Vec::new(), 0usize);
    let mut cursor: Option<String> = None;
    loop {
        let at = cursor
            .as_deref()
            .map(|cursor| format!("&cursor={cursor}"))
            .unwrap_or_default();
        let (status, page, _) = agent
            .get(&format!("{base}/resources?path={path}&limit={limit}{at}"))
            .await;
        assert_eq!(status, 200, "{page}");
        pages = pages.saturating_add(1);
        let got = page["rows"].as_array().cloned().unwrap_or_default();
        let next = page["next_cursor"].as_str().map(str::to_owned);
        assert!(
            got.len() == limit || next.is_none(),
            "a page short of its limit is the last one: {} rows, next {next:?}",
            got.len()
        );
        rows.extend(got);
        match next {
            Some(next) => cursor = Some(next),
            None => return (rows, pages),
        }
        assert!(pages < 10_000, "the cursor leads somewhere");
    }
}

/// How long the calls an agent makes most take in a workspace this size,
/// at most: what an empty workspace answers in, with room for a loaded
/// machine and a debug build.
const CALL_BUDGET: Duration = Duration::from_millis(1500);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "builds a thousand notes and five hundred issues one call at a time, which takes \
            minutes; run it with --ignored, in a release build for its timings"]
#[expect(
    clippy::too_many_lines,
    reason = "one move of a company's notes and issues, then every call timed"
)]
async fn a_company_sized_workspace_answers_at_the_speed_of_an_empty_one() {
    let notes = count("TILO_SCALE_NOTES", 1000);
    let issues = count("TILO_SCALE_ISSUES", 500);
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = V1 {
        api: &api,
        token: approved_key(&on_relay, &ada, &browser, "Mover").await,
    };
    let (status, made, _) = agent
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({"name": "Company"}).to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let base = format!("/v1/workspaces/{}", made["id"].as_str().unwrap());

    // The notebook, filled one note after another, as an agent moving a
    // company's docs in does.
    agent.act(&base, "notes", "open", &json!({})).await;
    let mut writing = Vec::with_capacity(notes);
    for index in 0..notes {
        let (took, _) = timed(agent.act(
            &base,
            "notes/Notebook",
            "create-page",
            &json!({"title": format!("note-{index}"), "markdown": page(index)}),
        ))
        .await;
        writing.push(took);
        if index % 100 == 99 {
            eprintln!(
                "{} notes: last hundred p50 {:?}, max {:?}",
                index + 1,
                quantile(&writing[index - 99..], 0.5),
                quantile(&writing[index - 99..], 1.0)
            );
        }
    }

    // Listed a page at a time, every note is reachable, and the cursor says
    // whether more follow.
    let (list, (status, listed, list_bytes)) =
        timed(agent.get(&format!("{base}/resources?path=notes/Notebook&limit=40"))).await;
    eprintln!("list notes: {list:?}, {list_bytes} bytes");
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed["rows"].as_array().map(Vec::len),
        Some(40),
        "{listed}"
    );
    let next = listed["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("more notes follow the first page: {listed}"));
    let (_, second, _) = agent
        .get(&format!(
            "{base}/resources?path=notes/Notebook&limit=40&cursor={next}"
        ))
        .await;
    assert_ne!(
        second["rows"][0]["path"], listed["rows"][0]["path"],
        "the next page starts where the first ended"
    );
    let began = Instant::now();
    let (rows, pages) = every_row(&agent, &base, "notes/Notebook", 200).await;
    let walked = began.elapsed();
    let names: std::collections::BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row["name"].as_str().map(str::to_owned))
        .collect();
    eprintln!(
        "every note listed: {} rows in {pages} pages, {walked:?}",
        rows.len()
    );
    assert_eq!(rows.len(), notes, "every note, once: {}", names.len());
    assert_eq!(names.len(), notes, "no note twice");

    // Read, found, and changed then read again, each in the time an empty
    // notebook answers in.
    let (read, (_, read_bytes)) =
        timed(agent.act(&base, "notes/Notebook", "read", &json!({}))).await;
    eprintln!("read notebook: {read:?}, {read_bytes} bytes");
    let (find, (found, _)) = timed(agent.act(
        &base,
        "notes/Notebook",
        "find-page",
        &json!({"title": format!("note-{}", notes / 2)}),
    ))
    .await;
    eprintln!("find a note: {find:?}");
    assert!(!found.is_null(), "the note is found");
    let (write, _) = timed(agent.act(
        &base,
        "notes/Notebook",
        "new-note",
        &json!({"title": "after the move"}),
    ))
    .await;
    let (after, (_, relisted, _)) =
        timed(agent.get(&format!("{base}/resources?path=notes/Notebook&limit=40"))).await;
    eprintln!("write a note: {write:?}, then list: {after:?}");
    assert_eq!(
        relisted["rows"].as_array().map(Vec::len),
        Some(40),
        "{relisted}"
    );
    for (call, took) in [
        ("listing notes", list),
        ("reading the notebook", read),
        ("finding a note", find),
        ("writing a note", write),
        ("listing after a write", after),
    ] {
        assert!(took < CALL_BUDGET, "{call} at {notes} notes took {took:?}");
    }
    let (first, last) = (
        quantile(&writing[1..=100.min(notes - 1)], 0.5),
        quantile(&writing[notes.saturating_sub(100)..], 0.5),
    );
    assert!(
        last <= first * 3 + Duration::from_millis(100),
        "the {notes}th note costs what the first ones did: p50 {first:?} at first, {last:?} at last"
    );

    // The tracker, an issue at a time.
    agent.act(&base, "tasks", "open", &json!({})).await;
    let mut acting = Vec::with_capacity(issues);
    for index in 0..issues {
        let (took, _) = timed(agent.act(
            &base,
            "tasks/Tasks",
            "create",
            &json!({"title": format!("Issue {index}"), "body": page(index)}),
        ))
        .await;
        acting.push(took);
        if index % 100 == 99 {
            eprintln!(
                "{} issues: last hundred p50 {:?}, max {:?}",
                index + 1,
                quantile(&acting[index - 99..], 0.5),
                quantile(&acting[index - 99..], 1.0)
            );
        }
    }
    let (first, last) = (
        quantile(&acting[1..=100.min(issues - 1)], 0.5),
        quantile(&acting[issues.saturating_sub(100)..], 0.5),
    );
    assert!(
        last <= first * 3 + Duration::from_millis(100),
        "the {issues}th issue costs what the first ones did: p50 {first:?} at first, {last:?} at last"
    );

    // Its listing names each kind's actions once, and its rows only by id.
    let (list, (status, listed, list_bytes)) =
        timed(agent.get(&format!("{base}/resources?path=tasks/Tasks&limit=40"))).await;
    eprintln!("list issues: {list:?}, {list_bytes} bytes");
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["rows"][0]["actions"][0].is_string(),
        "a row names its actions: {}",
        listed["rows"][0]
    );
    assert!(
        listed["kinds"]
            .as_object()
            .is_some_and(|kinds| !kinds.is_empty()),
        "the listing describes them once: {}",
        listed["kinds"]
    );
    assert!(
        list_bytes < 64 * 1024,
        "a page of 40 issues is not their actions forty times: {list_bytes} bytes"
    );
    let (task_rows, _) = every_row(&agent, &base, "tasks/Tasks", 200).await;
    assert!(
        task_rows.len() >= issues,
        "every issue is listed: {}",
        task_rows.len()
    );

    // Its read is a page, and the cursor leads through every issue.
    let (read, (status, tracker, read_bytes)) =
        timed(agent.get(&format!("{base}/resources/tasks/Tasks"))).await;
    eprintln!("read the tracker: {read:?}, {read_bytes} bytes");
    assert_eq!(status, 200, "{tracker}");
    assert!(
        read_bytes < 256 * 1024,
        "a read of {issues} issues is a page of them: {read_bytes} bytes"
    );
    let mut seen = 0usize;
    let mut page_read = tracker;
    loop {
        seen = seen.saturating_add(
            page_read["children"].as_array().map_or(0, Vec::len)
                + page_read["fields"]["issues"].as_array().map_or(0, Vec::len),
        );
        let Some(cursor) = page_read["cursor"].as_str().map(str::to_owned) else {
            break;
        };
        let (status, next, _) = agent
            .get(&format!("{base}/resources/tasks/Tasks?cursor={cursor}"))
            .await;
        assert_eq!(status, 200, "{next}");
        page_read = next;
    }
    assert!(
        seen >= issues,
        "every issue is read, a page at a time: {seen}"
    );
    let (act, _) = timed(agent.act(
        &base,
        "tasks/Tasks",
        "create",
        &json!({"title": "After the move"}),
    ))
    .await;
    let (reread, _) = timed(agent.get(&format!("{base}/resources/tasks/Tasks"))).await;
    eprintln!("write an issue: {act:?}, then read: {reread:?}");
    for (call, took) in [
        ("listing issues", list),
        ("reading the tracker", read),
        ("writing an issue", act),
        ("reading after a write", reread),
    ] {
        assert!(
            took < CALL_BUDGET,
            "{call} at {issues} issues took {took:?}"
        );
    }
}
