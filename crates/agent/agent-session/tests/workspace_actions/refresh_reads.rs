//! What the refresh before every action costs, in reads: the document the
//! request acts on and what a write changed since the last refresh, never
//! every document the workspace holds.
//!
//! The work is counted, not timed: a shared machine's times vary with the
//! load, while the reads a refresh makes are the same wherever it executes. The
//! count is process-wide ([`replica_reads`]), so this test measures a delta
//! around one action; a test binary that carries out several tests at once would
//! see theirs too.

use super::api::ISSUER;
use super::api_agent::{Agent, workspace};
use super::api_on_server::{api_on_server, approved_key};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_agent_session::workspace_actions::replica_reads;

/// The workspace sizes a refresh is counted at: one small enough to show a
/// per-document cost as a handful of reads, one where a per-document cost is
/// hundreds.
const SIZES: [usize; 2] = [10, 1000];

/// The most reads one action may take at either size: the document acted on, what changed since
/// the last refresh, and the documents the worker holds mounted, which is bounded by its own
/// cache and never by the workspace.
const CEILING: u64 = 16;

/// The path of the resource of the tracker's issues: the row whose actions
/// make an issue.
async fn issues(agent: &Agent<'_>, base: &str) -> String {
    let listed = agent
        .get(&format!("{base}/resources?path=tasks&limit=200"))
        .await;
    let rows = listed["rows"].as_array().cloned().unwrap_or_default();
    rows.iter()
        .find(|row| {
            row["actions"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|named| named == "create"))
        })
        .and_then(|row| row["path"].as_str())
        .map_or_else(
            || panic!("the tracker lists the issues it makes: {listed}"),
            str::to_owned,
        )
}

/// An issue to make, one paragraph.
fn new_issue(index: usize) -> Value {
    json!({
        "title": format!("issue-{index}"),
        "body": format!("A paragraph of what issue {index} is about."),
    })
}

/// The reads one action takes, in that agent's workspace of `size` issues: the
/// least of a few, so what a settling workspace sends beside the action does
/// not decide it.
async fn reads_per_action(agent: &Agent<'_>, base: &str, size: usize) -> u64 {
    agent.act(base, "tasks", "open", &json!({})).await;
    let issues = issues(agent, base).await;
    let seed: Vec<Value> = (0..size).map(new_issue).collect();
    if let Some(refused) = agent.many_or_each(base, &issues, "create", &seed).await {
        eprintln!("{issues}#create: no bulk call ({refused}); one at a time");
    }
    // The first action catches up what seeding changed; only the ones after
    // it show what a refresh costs once the workspace is where it is.
    agent.act(base, &issues, "create", &new_issue(size)).await;
    let mut took = Vec::new();
    for index in 0..3 {
        let before = replica_reads();
        agent
            .act(base, &issues, "create", &new_issue(size + 1 + index))
            .await;
        took.push(replica_reads().saturating_sub(before));
    }
    took.into_iter().min().unwrap_or_default()
}

/// A refresh reads the document the action names and what a write changed,
/// so an action at a thousand notes reads what one at ten does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_reads_what_the_action_names_however_full_the_workspace() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Agent {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Refresh").await,
    };
    let mut counted = Vec::new();
    for size in SIZES {
        let (base, _) = workspace(&agent, &format!("Refresh {size}")).await;
        let reads = reads_per_action(&agent, &base, size).await;
        eprintln!("{size} notes: {reads} reads for an action");
        counted.push((size, reads));
    }
    for (size, reads) in &counted {
        assert!(
            *reads <= CEILING,
            "{size} notes: an action read {reads} documents, more than the {CEILING} the \
             document it names and what changed take"
        );
    }
    let (small, large) = (counted[0].1, counted[1].1);
    assert!(
        large <= small.saturating_add(2),
        "a full workspace reads per action what a small one does: {small} reads at {} notes, \
         {large} at {}",
        counted[0].0,
        counted[1].0
    );
}

/// The trackers a listing and an open are counted in: one of a few issues, and
/// one of an imported tracker's thousands, kept in many parts.
const TRACKERS: [usize; 2] = [25, 3_000];

/// What one list of a tracker's only writer reads: the tracker's own document,
/// whose keys the serving device hands it afresh for every action, once to decide it
/// opens and once to merge it, and nothing of the parts it wrote.
const WRITER_READS: u64 = 2;

/// The issues one `create-many` makes, as an import hands them over.
const ISSUE_LIST: usize = 200;

/// An issue as an import brings it: a title and a body of a few paragraphs.
fn issue(index: usize) -> Value {
    let said = "What was seen, what was tried, and what should happen next. ".repeat(8);
    json!({
        "title": format!("Issue {index}"),
        "body": format!("## Issue {index}\n\n{said}"),
        "status": "todo",
    })
}

/// What `asked` read of the replica, beside what it answered.
async fn counted<T>(asked: impl std::future::Future<Output = T>) -> (T, u64) {
    let before = replica_reads();
    let answered = asked.await;
    (answered, replica_reads().saturating_sub(before))
}

/// What one tracker costs in reads once `size` issues are in it: the least
/// one more list of its writer read, and what listing the app and opening it
/// read after.
async fn tracker_reads(agent: &Agent<'_>, size: usize) -> (u64, u64, u64) {
    let (base, _) = workspace(agent, &format!("Tracker {size}")).await;
    agent.act(&base, "tasks", "open", &json!({})).await;
    let issues: Vec<Value> = (0..size).map(issue).collect();
    for list in issues.chunks(ISSUE_LIST) {
        agent
            .act(
                &base,
                "tasks/Issues",
                "create-many",
                &json!({ "issues": list }),
            )
            .await;
    }
    let mut wrote = Vec::new();
    for index in 0..3 {
        let list = json!({ "issues": [issue(size + index)] });
        let (_, reads) = counted(agent.act(&base, "tasks/Issues", "create-many", &list)).await;
        wrote.push(reads);
    }
    let (_, listed) = counted(agent.get(&format!("{base}/resources?path=tasks"))).await;
    let (_, opened) = counted(agent.act(&base, "tasks", "open", &json!({}))).await;
    (wrote.into_iter().min().unwrap_or_default(), listed, opened)
}

/// An import's lists land without reading back the parts the import wrote, and
/// the tracker lists and opens from its head and its own record: what each
/// reads at three thousand issues is what it reads at twenty-five.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tracker_lists_and_opens_at_any_size_and_its_writer_reads_nothing_back() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Agent {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Importer").await,
    };
    let mut seen = Vec::new();
    for size in TRACKERS {
        let (wrote, listed, opened) = tracker_reads(&agent, size).await;
        eprintln!("{size} issues: a list read {wrote}, listing {listed}, opening {opened}");
        seen.push((size, wrote, listed, opened));
    }
    for (size, wrote, listed, opened) in &seen {
        assert!(
            *wrote <= WRITER_READS,
            "{size} issues: a list of the only writer read {wrote} documents, more than the \
             {WRITER_READS} of the record it acts on"
        );
        assert!(
            *listed <= CEILING && *opened <= CEILING,
            "{size} issues: listing read {listed} records and opening {opened}, more than {CEILING}"
        );
    }
    let ((_, _, small_list, small_open), (_, _, large_list, large_open)) = (seen[0], seen[1]);
    assert!(
        large_list <= small_list.saturating_add(1) && large_open <= small_open.saturating_add(1),
        "a tracker of {} issues lists in {large_list} reads and opens in {large_open}, one of {} \
         in {small_list} and {small_open}",
        TRACKERS[1],
        TRACKERS[0]
    );
}

/// An issue as `tilo migrate` brings one from a tracker: a body of a few paragraphs, so a
/// part of the tracker fills within a couple of lists.
fn imported(index: usize) -> Value {
    let said = "The page should load in a second whatever the tracker holds, and a click should \
        answer within a frame. This paragraph stands in for what a real issue says: context, \
        steps to reproduce, what was expected and what happened instead. ";
    json!({
        "title": format!("ENG-{index} Speed up the list"),
        "body": format!("## ENG-{index}\n\n{}", said.repeat(2 + index % 7)),
        "status": "todo",
    })
}

/// The lists of an import read no more after each part it starts than after its first: the
/// parts are the service's own writes, made for the agent that sends the lists, and none is read
/// back as it lands. Each part landed as another's write instead (it is signed by the agent's
/// session, and its keys reached the service only after it), was read out whole and taken up
/// again by the agent's replica, which had made it: the list after every part an import started
/// waited most of a second on a hosted workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_imports_lists_read_nothing_back_of_the_parts_it_starts() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let agent = Agent {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Importer").await,
    };
    let (base, _) = workspace(&agent, "Import").await;
    agent.act(&base, "tasks", "open", &json!({})).await;
    let mut reads = Vec::new();
    for list in 0..8 {
        let issues: Vec<Value> = (list * ISSUE_LIST..(list + 1) * ISSUE_LIST)
            .map(imported)
            .collect();
        let list = json!({ "issues": issues });
        let (_, read) = counted(agent.act(&base, "tasks/Issues", "create-many", &list)).await;
        reads.push(read);
    }
    eprintln!("each list of the import read {reads:?}");
    // A part holds two of these lists: every other list starts one, the first list the
    // tracker's first, and those between come right after a part was started.
    let after_first = reads[1];
    let after_later: Vec<u64> = reads.iter().skip(3).step_by(2).copied().collect();
    assert!(
        after_later.iter().all(|read| *read <= after_first),
        "the lists of an import read {reads:?}: those after a part was started read it back"
    );
}
