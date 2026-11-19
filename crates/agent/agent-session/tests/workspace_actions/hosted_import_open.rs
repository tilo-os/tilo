//! An import into a hosted workspace through the API, as `tilo migrate start`
//! sends it: Tasks opened (`tasks#open`), an area made, issues made 200 to a
//! `create-many` and their comments as groups. Opening Tasks again once the
//! tracker holds every issue costs about what opening it empty did: the
//! issues an import made since the tracker's view was last laid out are
//! taken into its list in one pass, not one pass per issue. That open lists
//! the tracker on the workspace's one action thread, so while it took
//! seconds, every other call to the workspace waited behind it, past the
//! 15 s an agent's call waits for an answer.

use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_api::Server;

/// How many issues the import brings, in lists of [`LIST`].
const ISSUES: usize = 1_600;
const LIST: usize = 200;

/// The most one call of the import may take, on a loaded machine and
/// unoptimized: far under the 15 s an agent's call waits for an answer.
const CALL_CEILING: Duration = Duration::from_secs(10);

/// The most opening Tasks after the import may cost, as a multiple of
/// opening it empty: a list read issue by issue made it a cube of the issues
/// the import made, seconds where the first open takes under one.
const OPEN_AGAIN_FACTOR: u32 = 3;

struct Key<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl Key<'_> {
    fn headers(&self) -> hyper::HeaderMap {
        let bearer = format!("Bearer {}", self.token);
        headers(&[
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
            ("prefer", "wait=60"),
        ])
    }

    /// `resource#action` with `args` through the API, as `tilo migrate`'s
    /// hosted workspace calls it: its result and how long it took.
    async fn act(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        args: &Value,
    ) -> (Value, Duration) {
        let path = format!(
            "{base}/resources/{}/actions/{action}",
            tilo_api::client::encode_path(resource)
        );
        let began = Instant::now();
        let reply = Box::pin(self.api.dispatch(
            &hyper::Method::POST,
            &path,
            "",
            &self.headers(),
            args.to_string().as_bytes(),
        ))
        .await;
        let (status, reply) = json_of(reply).await;
        let took = began.elapsed();
        assert_eq!(status, 200, "{resource}#{action}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action}: {reply}");
        (reply["result"].clone(), took)
    }
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

/// The ids a `create-many` answered.
fn made(result: &Value) -> Vec<String> {
    result["made"]
        .as_array()
        .or_else(|| result["value"]["made"].as_array())
        .unwrap_or_else(|| panic!("a create answers what it made: {result}"))
        .iter()
        .map(|made| made["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_tasks_after_an_import_costs_what_opening_it_empty_did() {
    let on_server = api_on_server(super::api::ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = Key {
        api: &api,
        token: approved_key(&on_server, &ada, &browser, "Import").await,
    };
    let (status, workspace) = json_of(
        Box::pin(api.dispatch(
            &hyper::Method::POST,
            "/v1/workspaces",
            "",
            &key.headers(),
            json!({"name": "Company"}).to_string().as_bytes(),
        ))
        .await,
    )
    .await;
    assert_eq!(status, 201, "{workspace}");
    let base = format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap());

    let mut took = Vec::new();
    let (_, first_open) = key.act(&base, "tasks", "open", &json!({})).await;
    took.push(("tasks#open".to_owned(), first_open));
    let (_, area) = key
        .act(
            &base,
            "tasks/tasks.tracker",
            "create-area",
            &json!({"name": "Engineering", "kind": "area"}),
        )
        .await;
    took.push(("create-area".to_owned(), area));

    for first in (0..ISSUES).step_by(LIST) {
        let list: Vec<Value> = (first..first + LIST)
            .map(|at| {
                json!({
                    "title": format!("Issue {at}"),
                    "body": format!("What issue {at} is about, as it was written there."),
                    "status": "todo",
                    "url": format!("https://linear.example/acme/issue/ENG-{at}"),
                    "labels": ["Migration"],
                    "origin": origin(at),
                })
            })
            .collect();
        let (answer, list_took) = key
            .act(
                &base,
                "tasks/tasks.issues",
                "create-many",
                &json!({"issues": list}),
            )
            .await;
        took.push((format!("create-many at {first}"), list_took));
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
        let (_, group_took) = key
            .act(
                &base,
                "tasks/tasks.issues",
                "group",
                &json!({"actions": comments}),
            )
            .await;
        took.push((format!("comments at {first}"), group_took));
    }

    // The next import opens Tasks again, now that it holds every issue, and
    // then the app it lands mail in.
    let (_, again) = key.act(&base, "tasks", "open", &json!({})).await;
    took.push(("tasks#open again".to_owned(), again));
    let (_, mail) = key.act(&base, "mail", "open", &json!({})).await;
    took.push(("mail#open".to_owned(), mail));

    println!("the import's calls took {took:?}");
    let slow: Vec<_> = took
        .iter()
        .filter(|(_, took)| *took > CALL_CEILING)
        .collect();
    assert!(
        slow.is_empty(),
        "calls took longer than {CALL_CEILING:?}: {slow:?} (all: {took:?})"
    );
    assert!(
        again <= first_open * OPEN_AGAIN_FACTOR,
        "opening Tasks with {ISSUES} issues took {again:?}, empty {first_open:?}"
    );
}
