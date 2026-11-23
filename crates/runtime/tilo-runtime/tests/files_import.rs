//! A tracker brought into a device's workspace as `tilo migrate` brings one: lists of 200 issues
//! to a `create-many`, their comments as groups, and the tracker opened afterwards. What a list
//! costs does not grow with what the tracker holds already, the last lists land about as fast as
//! the first, and the tracker opens with every issue.

use std::time::Instant;

use serde_json::{Value, json};

use crate::files_support::{Session, json_in, said};
use crate::host::{self, Host};

const TASKS: &str = "so.tilo.tasks";
const ISSUES: usize = 3_000;
const LIST: usize = 200;
const COMMENTS: usize = 2;

/// The issues one `create-many` answered, by id.
fn made(answer: &Value) -> Vec<String> {
    answer["made"]
        .as_array()
        .or_else(|| answer["value"]["made"].as_array())
        .unwrap_or_else(|| panic!("a create answers what it made: {answer}"))
        .iter()
        .map(|made| made["id"].as_str().unwrap().to_owned())
        .collect()
}

fn origin(at: usize) -> Value {
    json!({
        "author": "Grace Hopper",
        "handle": "grace@example.com",
        "source": "Linear",
        "url": format!("https://linear.example/acme/issue/ENG-{at}"),
        "reference": format!("ENG-{at}"),
        "created_ms": 1_756_720_800_000_i64,
        "updated_ms": null,
    })
}

/// One action of the workspace, asked through the key's session: what it answered, and how long it
/// took.
async fn act(
    session: &Session<'_>,
    workspace: &str,
    path: &str,
    args: Value,
) -> (Value, std::time::Duration) {
    let started = Instant::now();
    let (text, failed) = said(
        &session
            .tool(workspace, "tilo_do", &json!({"path": path, "args": args}))
            .await,
    );
    assert!(!failed, "{path}: {text}");
    (json_in(&text), started.elapsed())
}

fn is_issue(index: usize) -> Value {
    json!({
        "title": format!("Issue {index}"),
        "body": format!("What issue {index} is about, as it was written there."),
        "status": "todo",
        "url": format!("https://linear.example/acme/issue/ENG-{index}"),
        "labels": ["Migration"],
        "origin": origin(index),
    })
}

/// A tracker of 3,000 issues and their comments lands at the cost of its first lists: the last
/// list costs about what the second did, and the tracker opens with every issue.
#[test]
fn a_tracker_of_three_thousand_issues_lands_at_the_cost_of_its_first_lists() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Import");
        let session = Session::Key(&key);
        act(&session, &workspace, "tasks#open", json!({})).await;

        let mut lists = Vec::new();
        let mut groups = Vec::new();
        let mut made_count = 0;
        for first in (0..ISSUES).step_by(LIST) {
            let issues: Vec<Value> = (first..first + LIST).map(is_issue).collect();
            let (answer, took) = act(
                &session,
                &workspace,
                "tasks/tasks.issues#create-many",
                json!({ "issues": issues }),
            )
            .await;
            let ids = made(&answer);
            assert_eq!(ids.len(), LIST, "every issue made is answered: {answer}");
            made_count += ids.len();
            lists.push(took);
            let comments: Vec<Value> = ids
                .iter()
                .flat_map(|issue| {
                    (0..COMMENTS).map(move |n| {
                        json!({ "action": "comment", "args": {
                            "issue": issue,
                            "text": format!("Comment {n}, moved in with its author and date."),
                            "origin": origin(n),
                        }})
                    })
                })
                .collect();
            let (_, took) = act(
                &session,
                &workspace,
                "tasks/tasks.issues#group",
                json!({ "actions": comments }),
            )
            .await;
            groups.push(took);
        }
        println!("lists of {LIST} issues took {lists:?}; their comments {groups:?}");
        assert_eq!(made_count, ISSUES);
        // The second list lands in a tracker of 200 issues, the last in one of 2,800: each costs
        // what it makes, not what the tracker already has.
        let (early, late) = (
            lists[1] + groups[1],
            lists[lists.len() - 1] + groups[groups.len() - 1],
        );
        assert!(
            late <= early * 3,
            "the last list and its comments took {late:?}, the second {early:?}",
        );

        // The tracker opens with every issue: each list lands in the parts the tracker keeps them
        // in, and every issue made is in one of them.
        let kept = issues_kept(&host, &workspace).await;
        assert_eq!(kept, ISSUES, "every issue made opens in the tracker");
    });
}

/// The issues the workspace keeps of its tracker, counted over every record of the app.
async fn issues_kept(host: &Host, workspace: &str) -> usize {
    let workspaces = host.daemon().workspaces();
    let records = workspaces.records(workspace).await.unwrap_or_default();
    let mut titled = std::collections::BTreeSet::new();
    for (app, record) in records {
        if app != TASKS || record.is_empty() {
            continue;
        }
        let Some(bytes) = workspaces
            .resource_document(workspace, &app, &record)
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        let document = automerge::AutoCommit::load(&bytes).expect("a tracker part reads");
        collect_titles(&document, &automerge::ROOT, &mut titled);
    }
    titled.len()
}

/// Every `title` of an issue in `object` and what it holds: an issue is a map with a `title`.
fn collect_titles(
    document: &automerge::AutoCommit,
    object: &automerge::ObjId,
    titled: &mut std::collections::BTreeSet<String>,
) {
    use automerge::{ObjType, ReadDoc as _, ScalarValue, Value as Any};
    for key in document.keys(object) {
        let Ok(Some((value, id))) = document.get(object, key.as_str()) else {
            continue;
        };
        match value {
            Any::Object(ObjType::Map) => {
                if let Ok(Some((Any::Scalar(title), _))) = document.get(&id, "title")
                    && let ScalarValue::Str(title) = &*title
                    && title.starts_with("Issue ")
                {
                    titled.insert(title.to_string());
                }
                collect_titles(document, &id, titled);
            }
            Any::Object(_) => collect_titles(document, &id, titled),
            Any::Scalar(_) => {}
        }
    }
}

/// An import into a workspace through the key's session, as `tilo migrate start` sends it: Tasks
/// opened, an area made, issues made 200 to a `create-many` and their comments as groups.
/// Opening Tasks again once the tracker holds every issue costs about what opening it empty did.
#[test]
fn opening_tasks_after_an_import_costs_what_opening_it_empty_did() {
    const ISSUES: usize = 1_600;
    const CALL_CEILING: std::time::Duration = std::time::Duration::from_secs(10);
    const OPEN_AGAIN_FACTOR: u32 = 3;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Import");
        let session = Session::Key(&key);

        let mut took = Vec::new();
        let (_, first_open) = act(&session, &workspace, "tasks#open", json!({})).await;
        took.push(("tasks#open", first_open));
        let (_, area) = act(
            &session,
            &workspace,
            "tasks/tasks.tracker#create-area",
            json!({"name": "Engineering", "kind": "area"}),
        )
        .await;
        took.push(("create-area", area));

        for first in (0..ISSUES).step_by(LIST) {
            let issues: Vec<Value> = (first..first + LIST).map(is_issue).collect();
            let (answer, list_took) = act(
                &session,
                &workspace,
                "tasks/tasks.issues#create-many",
                json!({"issues": issues}),
            )
            .await;
            took.push(("create-many", list_took));
            let comments: Vec<Value> = made(&answer)
                .iter()
                .map(|issue| {
                    json!({"action": "comment", "args": {
                        "issue": issue,
                        "text": "A comment, moved in with its author and date.",
                        "origin": origin(first),
                    }})
                })
                .collect();
            let (_, group_took) = act(
                &session,
                &workspace,
                "tasks/tasks.issues#group",
                json!({"actions": comments}),
            )
            .await;
            took.push(("comments", group_took));
        }

        // The next import opens Tasks again, now that it holds every issue, and then the app it
        // lands people in.
        let (_, again) = act(&session, &workspace, "tasks#open", json!({})).await;
        took.push(("tasks#open again", again));
        let (_, contacts) = act(&session, &workspace, "contacts#open", json!({})).await;
        took.push(("contacts#open", contacts));

        println!("the import's calls took {took:?}");
        let slow: Vec<_> = took
            .iter()
            .filter(|(_, took)| *took > CALL_CEILING)
            .collect();
        assert!(
            slow.is_empty(),
            "calls took longer than {CALL_CEILING:?}: {slow:?} (all: {took:?})",
        );
        assert!(
            again <= first_open * OPEN_AGAIN_FACTOR,
            "opening Tasks with {ISSUES} issues took {again:?}, empty {first_open:?}",
        );
    });
}

/// One of the import's lists: how long it took, and the bytes the process moved meanwhile.
#[derive(Clone, Copy, Debug)]
struct List {
    took: std::time::Duration,
    moved: u64,
}

/// One of the person's calls: how many of the import's lists had landed when it began and when it
/// answered, and how long it took.
#[derive(Clone, Copy, Debug)]
struct Call {
    kind: &'static str,
    began: usize,
    answered: usize,
    took: std::time::Duration,
}

fn listed(first: usize) -> Value {
    let issues: Vec<Value> = (first..first + LIST)
        .map(|at| {
            json!({
                "title": format!("ENG-{at} Speed up the list"),
                "body": format!("## ENG-{at}\n\nWhat the issue says there, with a step or two.\n"),
                "status": "todo",
                "labels": ["Migration"],
                "origin": origin(at),
            })
        })
        .collect();
    json!({ "issues": issues })
}

/// The bytes of history this process read and wrote so far.
fn bytes_moved() -> u64 {
    let work = tilo_peer_state::work::now();
    work.bytes_read.saturating_add(work.bytes_written)
}

/// A tracker brought in through the key's session while the person goes on working beside it:
/// listing the tracker and making an issue of their own. Each list moves about what the first
/// did, into a tracker ten times as big, and a listing beside the import waits for the list it
/// began beside, never for one list per page.
#[test]
fn an_import_keeps_its_pace_while_the_person_works_beside_it() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const LISTS: usize = 10;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Import");
        let person = host.key("Person");
        let session = Session::Key(&key);
        act(&session, &workspace, "tasks#open", json!({})).await;

        let landed = AtomicUsize::new(0);
        let done = AtomicBool::new(false);
        let importer = async {
            let mut lists = Vec::with_capacity(LISTS);
            for list in 0..LISTS {
                let (before, began) = (bytes_moved(), Instant::now());
                let (answer, _) = act(
                    &session,
                    &workspace,
                    "tasks/tasks.issues#create-many",
                    listed(list * LIST),
                )
                .await;
                lists.push(List {
                    took: began.elapsed(),
                    moved: bytes_moved().saturating_sub(before),
                });
                assert_eq!(made(&answer).len(), LIST, "every issue of a list is made");
                landed.store(list + 1, Ordering::Release);
            }
            done.store(true, Ordering::Release);
            lists
        };
        let beside = async {
            let person = Session::Key(&person);
            let mut calls: Vec<Call> = Vec::new();
            let mut made = 0;
            while !done.load(Ordering::Acquire) {
                let (began, started) = (landed.load(Ordering::Acquire), Instant::now());
                let (text, failed) = said(
                    &person
                        .tool(&workspace, "tilo_ls", &json!({"path": "tasks"}))
                        .await,
                );
                assert!(!failed, "the person's listing answers: {text}");
                calls.push(Call {
                    kind: "ls",
                    began,
                    answered: landed.load(Ordering::Acquire),
                    took: started.elapsed(),
                });
                let (began, started) = (landed.load(Ordering::Acquire), Instant::now());
                act(
                    &person,
                    &workspace,
                    "tasks/tasks.issues#create",
                    json!({ "title": format!("The person's own {made}") }),
                )
                .await;
                calls.push(Call {
                    kind: "create",
                    began,
                    answered: landed.load(Ordering::Acquire),
                    took: started.elapsed(),
                });
                made += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            calls
        };
        let (lists, calls) = tokio::join!(importer, beside);
        for list in &lists {
            println!(
                "a list of {LIST} issues took {:?} and moved {} bytes",
                list.took, list.moved
            );
        }
        for call in &calls {
            println!(
                "the person's {} took {:?} while lists {}-{} landed",
                call.kind, call.took, call.began, call.answered
            );
        }
        assert!(
            calls.len() >= 4,
            "the person's calls ran beside the import: {calls:?}",
        );
        let half = lists.len() / 2;
        let moved = |lists: &[List]| {
            let mut samples: Vec<u64> = lists.iter().map(|list| list.moved).collect();
            samples.sort_unstable();
            samples[samples.len() / 2]
        };
        let (first, last) = (moved(&lists[..half]), moved(&lists[half..]));
        assert!(
            last.saturating_mul(4) <= first.saturating_mul(5),
            "the import's lists moved {last} bytes late, {first} early",
        );
        let mut landed_meanwhile: Vec<u64> = calls
            .iter()
            .filter(|call| call.kind == "ls" && call.began < LISTS)
            .map(|call| u64::try_from(call.answered.saturating_sub(call.began)).unwrap())
            .collect();
        landed_meanwhile.sort_unstable();
        let meanwhile = landed_meanwhile
            .get(landed_meanwhile.len() / 2)
            .copied()
            .unwrap_or_default();
        assert!(
            meanwhile <= 2,
            "{meanwhile} of the import's lists landed while a listing beside it read, in the \
             middle: {landed_meanwhile:?}",
        );
    });
}

/// The documents one call read, as the daemon counts them.
///
/// The daemon keeps no such count: agent-session's `replica_reads`, which counted the documents a
/// refresh made, went with its `WorkspaceActions`, and `tilo_peer_state::work` counts change
/// frames and bytes, not documents. A port of the refresh tests needs the device's dispatch to
/// count the documents one call reads.
fn documents_read() -> u64 {
    panic!(
        "the daemon counts no documents read for a call: the counter lived in agent-session's \
         deleted WorkspaceActions (`replica_reads`)"
    )
}

/// An issue to make, one paragraph.
fn new_issue(index: usize) -> Value {
    json!({
        "title": format!("issue-{index}"),
        "body": format!("A paragraph of what issue {index} is about."),
    })
}

/// The reads one action takes: the least of a few.
async fn reads_per_action(session: &Session<'_>, workspace: &str, size: usize) -> u64 {
    act(session, workspace, "tasks#open", json!({})).await;
    let seed: Vec<Value> = (0..size).map(new_issue).collect();
    act(
        session,
        workspace,
        "tasks/tasks.issues#create-many",
        json!({ "issues": seed }),
    )
    .await;
    act(
        session,
        workspace,
        "tasks/tasks.issues#create",
        new_issue(size),
    )
    .await;
    let mut took = Vec::new();
    for index in 0..3 {
        let before = documents_read();
        act(
            session,
            workspace,
            "tasks/tasks.issues#create",
            new_issue(size + 1 + index),
        )
        .await;
        took.push(documents_read().saturating_sub(before));
    }
    took.into_iter().min().unwrap_or_default()
}

/// The most reads one action may take at either size: the document acted on and what changed since
/// the last refresh.
const CEILING: u64 = 16;

/// A refresh reads the document the action names and what a write changed, so an action at a
/// thousand notes reads what one at ten does.
#[test]
fn a_refresh_reads_what_the_action_names_however_full_the_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Refresh");
        let session = Session::Key(&key);
        let mut counted = Vec::new();
        for size in [10_usize, 1000] {
            let reads = reads_per_action(&session, &workspace, size).await;
            eprintln!("{size} notes: {reads} reads for an action");
            counted.push((size, reads));
        }
        for (size, reads) in &counted {
            assert!(
                *reads <= CEILING,
                "{size} notes: an action read {reads} documents, more than the {CEILING} the \
                 document it names and what changed take",
            );
        }
        let (small, large) = (counted[0].1, counted[1].1);
        assert!(
            large <= small.saturating_add(2),
            "a full workspace reads per action what a small one does: {small} reads at {} notes, \
             {large} at {}",
            counted[0].0,
            counted[1].0,
        );
    });
}

/// The trackers a listing and an open are counted in: one of a few issues, and one of an imported
/// tracker's thousands, kept in many parts.
const TRACKERS: [usize; 2] = [25, 3_000];
/// What one list of a tracker's only writer reads.
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

/// What one tracker costs in reads once `size` issues are in it.
async fn tracker_reads(session: &Session<'_>, workspace: &str, size: usize) -> (u64, u64, u64) {
    act(session, workspace, "tasks#open", json!({})).await;
    let issues: Vec<Value> = (0..size).map(issue).collect();
    for list in issues.chunks(ISSUE_LIST) {
        act(
            session,
            workspace,
            "tasks/tasks.issues#create-many",
            json!({ "issues": list }),
        )
        .await;
    }
    let mut wrote = Vec::new();
    for index in 0..3 {
        let before = documents_read();
        act(
            session,
            workspace,
            "tasks/tasks.issues#create-many",
            json!({ "issues": [issue(size + index)] }),
        )
        .await;
        wrote.push(documents_read().saturating_sub(before));
    }
    let before = documents_read();
    session
        .tool(workspace, "tilo_ls", &json!({"path": "tasks"}))
        .await;
    let listed = documents_read().saturating_sub(before);
    let before = documents_read();
    act(session, workspace, "tasks#open", json!({})).await;
    let opened = documents_read().saturating_sub(before);
    (wrote.into_iter().min().unwrap_or_default(), listed, opened)
}

/// An import's lists land without reading back the parts the import wrote, and the tracker lists
/// and opens from its head and its own record: what each reads at three thousand issues is what it
/// reads at twenty-five.
#[test]
fn a_tracker_lists_and_opens_at_any_size_and_its_writer_reads_nothing_back() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Importer");
        let session = Session::Key(&key);
        let mut seen = Vec::new();
        for size in TRACKERS {
            let (wrote, listed, opened) = tracker_reads(&session, &workspace, size).await;
            eprintln!("{size} issues: a list read {wrote}, listing {listed}, opening {opened}");
            seen.push((size, wrote, listed, opened));
        }
        for (size, wrote, listed, opened) in &seen {
            assert!(
                *wrote <= WRITER_READS,
                "{size} issues: a list of the only writer read {wrote} documents, more than the \
                 {WRITER_READS} of the record it acts on",
            );
            assert!(
                *listed <= CEILING && *opened <= CEILING,
                "{size} issues: listing read {listed} records and opening {opened}, more than \
                 {CEILING}",
            );
        }
        let ((_, _, small_list, small_open), (_, _, large_list, large_open)) = (seen[0], seen[1]);
        assert!(
            large_list <= small_list.saturating_add(1)
                && large_open <= small_open.saturating_add(1),
            "a tracker of {} issues lists in {large_list} reads and opens in {large_open}, one of \
             {} in {small_list} and {small_open}",
            TRACKERS[1],
            TRACKERS[0],
        );
    });
}

/// An issue as `tilo migrate` brings one from a tracker: a body of a few paragraphs.
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

/// The lists of an import read no more after each part it starts than after its first: the parts
/// are the workspace's own writes, made for the agent that sends the lists, and none is read back
/// as it lands.
#[test]
fn an_imports_lists_read_nothing_back_of_the_parts_it_starts() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Importer");
        let session = Session::Key(&key);
        act(&session, &workspace, "tasks#open", json!({})).await;
        let mut reads = Vec::new();
        for list in 0..8 {
            let issues: Vec<Value> = (list * ISSUE_LIST..(list + 1) * ISSUE_LIST)
                .map(imported)
                .collect();
            let before = documents_read();
            act(
                &session,
                &workspace,
                "tasks/tasks.issues#create-many",
                json!({ "issues": issues }),
            )
            .await;
            reads.push(documents_read().saturating_sub(before));
        }
        eprintln!("each list of the import read {reads:?}");
        let after_first = reads[1];
        let after_later: Vec<u64> = reads.iter().skip(3).step_by(2).copied().collect();
        assert!(
            after_later.iter().all(|read| *read <= after_first),
            "the lists of an import read {reads:?}: those after a part was started read it back",
        );
    });
}

/// A workspace updated from a build that named each app's surface by the hash of its `surface.toml`
/// still lists, over the API, the issues that build wrote.
#[test]
fn an_update_lists_what_a_build_that_named_surfaces_by_their_file_wrote() {
    host::run(|host| async move {
        let _ = &host;
        // The update is a server's: it decides the surfaces its changes are read with from the
        // content store an earlier build kept them in (`tilo_server::surfaces::decide_receipt`),
        // restarts on the state the earlier one kept, and lists what that build wrote. A device's
        // daemon starts no server: it has no content store of surfaces and no restart in which to
        // decide them again.
        panic!(
            "a device's daemon starts no server and restarts not: there is no surface decision \
             from a content store (`decide_receipt`) and no restart on the state an earlier build \
             kept"
        );
    });
}
