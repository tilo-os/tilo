//! The first page of a tracker's issues after its server restarts waits for the parts that page
//! shows, not for every part of the tracker: its app's worker takes in each part it holds none
//! of again, which costs about a second a part, and a first page of forty issues waited for all
//! of them, minutes for a tracker of thousands. The page answered while the others are still
//! being taken in is the page the tracker lists once they are all in.


use super::api_on_server::{api_on_server, approved_key};
use super::tracker_listing::{V1, every_page};
use super::*;
use serde_json::{Value, json};

/// An issue's description long enough that a part of the tracker's chain holds a few dozen
/// issues, as a moved-in tracker's do, so a tracker of many parts is made in a test's time.
fn long_description(at: usize) -> String {
    format!(
        "What issue {at} is about, as it was written there. {}",
        "The service keeps its work beside its peers, and reads it back. ".repeat(380)
    )
}

/// The issues one call makes when each is long: a call stays under what one action carries.
const LONG_LIST: usize = 20;

/// The first page's rows, by name, in order.
fn names(page: &Value) -> Vec<String> {
    page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["name"].as_str().map(str::to_owned))
        .collect()
}

/// A tracker of `size` long issues, its server restarted, and its first page listed after the
/// restart: how long that took, and whether it was the page listed before the restart, and the
/// first page of the tracker listed whole after it.
async fn first_page_after_a_restart(size: usize) -> Duration {
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;
    let (base, before) = {
        let agent = V1 {
            api: &on_server.api,
            token: token.clone(),
        };
        let (status, workspace) = agent
            .send(
                hyper::Method::POST,
                "/v1/workspaces",
                json!({"name": "Company"}).to_string().as_bytes(),
            )
            .await;
        assert_eq!(status, 201, "{workspace}");
        let base = format!("/v1/workspaces/{}", workspace["id"].as_str().unwrap());
        agent.act(&base, "tasks", "open", &json!({})).await;
        agent.act(&base, "contacts", "open", &json!({})).await;
        for start in (0..size).step_by(LONG_LIST) {
            let list: Vec<Value> = (start..(start + LONG_LIST).min(size))
                .map(|at| {
                    json!({
                        "key": format!("k{at}"),
                        "title": format!("Issue {at}"),
                        "body": long_description(at),
                        "status": "todo",
                    })
                })
                .collect();
            agent
                .act(
                    &base,
                    "tasks/tasks.issues",
                    "create-many",
                    &json!({"issues": list}),
                )
                .await;
        }
        let (status, page) = agent
            .get(&format!("{base}/resources?path=tasks/Issues&limit=40"))
            .await;
        assert_eq!(status, 200, "{page}");
        (base, names(&page))
    };
    assert_eq!(before.len(), 40, "a full first page before the restart");
    let on_server = on_server.restart(false).await;
    let agent = V1 {
        api: &on_server.api,
        token,
    };
    // The first call after the restart joins the workspace, whatever it lists.
    let (status, page) = agent
        .get(&format!("{base}/resources?path=contacts&limit=40"))
        .await;
    assert_eq!(status, 200, "{page}");
    let began = Instant::now();
    let (status, page) = agent
        .get(&format!("{base}/resources?path=tasks/Issues&limit=40"))
        .await;
    let took = began.elapsed();
    assert_eq!(status, 200, "{page}");
    let first = names(&page);
    eprintln!("{size} issues: the first page after a restart took {took:?}");
    assert_eq!(
        first, before,
        "the first page while the tracker's parts are taken in is its first page"
    );
    // Once every part is in, the tracker lists whole, and starts with the same page.
    let (rows, _) = every_page(&agent, &base, 40).await;
    let whole: Vec<String> = rows
        .iter()
        .filter_map(|row| row["name"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(whole.len(), size, "every issue is listed after the restart");
    assert_eq!(
        whole.get(..40),
        Some(before.as_slice()),
        "the tracker listed whole starts with the page answered while its parts arrived"
    );
    took
}

/// A tracker's first page after its server restarts costs about the same at thirty parts as
/// at three, and is the tracker's first page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trackers_first_page_after_a_restart_waits_only_for_the_parts_it_shows() {
    let small = Box::pin(first_page_after_a_restart(3 * LONG_LIST)).await;
    let large = Box::pin(first_page_after_a_restart(30 * LONG_LIST)).await;
    eprintln!("the first page after a restart: {small:?} at 60 issues, {large:?} at 600");
    assert!(
        large <= small * 3 + Duration::from_secs(3),
        "the first page after a restart took {large:?} at 600 issues, {small:?} at 60"
    );
}
