//! An issue named by its id through the API an agent calls: an id is matched
//! exactly, before any label, and answered from the resource's items, so a
//! delete and a rename find their issue in one answer however many issues
//! the tracker holds, whether or not the page its part publishes carries it.
//! Paging the Issues listing to find it read one page of twenty-five issues
//! and several of a thousand, so the count is held to none: the server is
//! asked the name, never paged for it. What the replica answering it builds
//! is counted in that replica's own test (`issue_reads`).

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::mcp_note_create::{CATALOGUES, LISTINGS, count_jobs};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tilo_api::Server;
use ui::tilo_resource::Resource as _;

/// What one call asked of the workspace's server: its catalogue requests, and
/// the pages among them that listed a level's items rather than answering a
/// name.
#[derive(Debug, PartialEq, Eq)]
struct Asked {
    catalogues: usize,
    listings: usize,
}

/// Every count [`Asked`] keeps, as they stand now.
fn counts() -> [usize; 2] {
    [&CATALOGUES, &LISTINGS].map(|count| count.load(Ordering::Relaxed))
}

/// A key's calls of the API, as an agent's own `tilo_do`.
struct Key<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl Key<'_> {
    async fn send(&self, method: hyper::Method, path: &str, body: &[u8]) -> (u16, Value) {
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
        json_of(reply).await
    }

    /// `action` of `resource` with `args`, which ran: its result.
    async fn act(&self, base: &str, resource: &str, action: &str, args: &Value) -> Value {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
            )
            .await;
        assert_eq!(status, 200, "{resource}#{action} {args}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action} {args}: {reply}");
        reply["result"].clone()
    }

    /// [`Self::act`] with what it asked of the server counted around it, and
    /// how long it took.
    async fn act_and_count(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        args: &Value,
    ) -> (Value, Asked, Duration) {
        let before = counts();
        let began = Instant::now();
        let value = self.act(base, resource, action, args).await;
        let took = began.elapsed();
        let [catalogues, listings] = counts();
        (
            value,
            Asked {
                catalogues: catalogues.saturating_sub(before[0]),
                listings: listings.saturating_sub(before[1]),
            },
            took,
        )
    }
}

/// A workspace of Ada's through the API: its base path and id.
async fn workspace(key: &Key<'_>, name: &str) -> (String, String) {
    let (status, made) = key
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            json!({ "name": name }).to_string().as_bytes(),
        )
        .await;
    assert_eq!(status, 201, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    (format!("/v1/workspaces/{id}"), id)
}

/// Every issue the workspace's Tasks keeps, by the id a path names it by and
/// its title, as the parts hold them now.
async fn issues_held(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Vec<(String, String)> {
    let mut held = Vec::new();
    for index in 0..16 {
        let part = tasks::part::chain().part(index).as_str().to_owned();
        let Ok(Some(bytes)) = workspaces
            .resource_document(workspace, "so.tilo.tasks", &part)
            .await
        else {
            break;
        };
        let part =
            tasks::part::IssuePart::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
        held.extend(
            part.issues
                .iter()
                .map(|(id, issue)| (id.as_str().to_owned(), issue.title.clone())),
        );
    }
    held
}

/// [`issues_held`] as the parts held them until the issues are `done` or
/// `wait` passed: the server keeps each part a moment after the write that
/// made it, and an action's answer a moment before what it changed reaches
/// them.
async fn issues(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    wait: Duration,
    done: impl Fn(&[(String, String)]) -> bool,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + wait;
    loop {
        let held = issues_held(workspaces, workspace).await;
        if done(&held) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `size` issues filed through the API as an agent files them: in bulk.
async fn file_issues(key: &Key<'_>, base: &str, size: usize) {
    key.act(base, "tasks", "open", &json!({})).await;
    for start in (0..size).step_by(tasks::model::MAX_MANY) {
        let actions: Vec<Value> = (start..(start + tasks::model::MAX_MANY).min(size))
            .map(|index| json!({"action": "create", "args": {"title": format!("Issue {index}")}}))
            .collect();
        key.act(base, "tasks/Issues", "group", &json!({"actions": actions}))
            .await;
    }
}

/// `target` renamed and then deleted by its id alone, as an agent names it,
/// with what each asked of the server: the tracker of `size` issues keeps
/// it renamed, then not at all, and its other issues as they were.
async fn rename_and_delete(
    (key, workspaces): (&Key<'_>, &tilo_server::workspaces::Workspaces),
    (base, id): (&str, &str),
    size: usize,
    target: &str,
) -> (Asked, Asked) {
    let path = format!("tasks/Issues/{target}");
    let (_renamed, rename, took) = key
        .act_and_count(base, &path, "rename", &json!({"title": "Renamed by id"}))
        .await;
    eprintln!("{size} issues: rename {rename:?} in {took:?}");
    let held = issues(workspaces, id, Duration::from_secs(10), |held| {
        held.iter()
            .any(|(id, title)| id == target && title == "Renamed by id")
    })
    .await;
    assert_eq!(
        held.iter()
            .find(|(id, _)| id == target)
            .map(|(_, title)| title.as_str()),
        Some("Renamed by id"),
        "the issue is renamed: {held:?}"
    );
    let (_deleted, delete, took) = key.act_and_count(base, &path, "delete", &json!({})).await;
    eprintln!("{size} issues: delete {delete:?} in {took:?}");
    let held = issues(workspaces, id, Duration::from_secs(10), |held| {
        !held.iter().any(|(id, _)| id == target)
    })
    .await;
    assert!(
        !held.iter().any(|(id, _)| id == target),
        "the issue is gone, and its neighbours stay: {held:?}"
    );
    (rename, delete)
}

/// A delete and a rename by id at twenty-five issues and at a thousand ask
/// the server the same and read no page of the Issues listing; an issue past
/// the page its part publishes is renamed and deleted the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_issue_named_by_its_id_is_found_in_one_answer_however_many_there_are() {
    count_jobs();
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = Key {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Issues").await,
    };
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    let mut asked = Vec::new();
    for size in [25, 1000] {
        let (base, id) = workspace(&key, &format!("Issues {size}")).await;
        file_issues(&key, &base, size).await;
        let held = issues(&workspaces, &id, Duration::from_secs(60), |held| {
            held.len() == size
        })
        .await;
        assert_eq!(
            held.len(),
            size,
            "the tracker holds every issue it was sent"
        );
        // An issue the page its part publishes carries: one of the first
        // the tracker holds.
        let on_page = held[100.min(size - 1)].0.clone();
        asked.push(rename_and_delete((&key, &workspaces), (&base, &id), size, &on_page).await);
        if size > tasks::part::PART_ISSUES {
            // One its part holds past the page it publishes of them, which
            // the chain names but no published tree carries.
            let past = held[tasks::part::PART_ISSUES - 100].0.clone();
            const {
                assert!(
                    tasks::part::PART_ISSUES - 100 >= ui::tilo_resource::items::ITEM_PAGE,
                    "the issue stands past its part's published page"
                );
            }
            let (rename, delete) =
                rename_and_delete((&key, &workspaces), (&base, &id), size, &past).await;
            assert_eq!(
                (&rename, &delete),
                (&asked[0].0, &asked[0].1),
                "an issue past its part's page costs what one on it does"
            );
        }
        let held = issues_held(&workspaces, &id).await;
        assert_eq!(
            held.len(),
            size - 1 - usize::from(size > tasks::part::PART_ISSUES),
            "only the issues deleted went"
        );
    }
    assert_eq!(
        asked[1], asked[0],
        "a rename and a delete by id ask as much at a thousand issues as at twenty-five"
    );
    for (rename, delete) in &asked {
        assert_eq!(
            (rename.listings, delete.listings),
            (0, 0),
            "the server is asked the id, never paged for it: {asked:?}"
        );
    }
}
