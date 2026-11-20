//! A write or a listing waits for parts of a chain the workspace has not sent yet, and the
//! workspace's other requests are answered meanwhile.
//!
//! Every request of a workspace is served on one thread. A server that restarted, or a fresh
//! member, holds only some parts of a tracker's issues, or none, and a write under an older
//! issue waits for that issue's part, as a listing of the issues waits for the chain. What it
//! waits for is held back here ([`HeldBack`]) for as long as the test likes: meanwhile a
//! listing of another app of the same workspace is answered within a small multiple of what it
//! costs when nothing waits, and once the part is let go the write lands with the answer it
//! would have had.
use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery};
use tilo_agent_session::workspace_actions::held_back::HeldBack;
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};

const TASKS: &str = "so.tilo.tasks";
const NOTES: &str = "so.tilo.notes";
/// A part holds 1,000 issues: these fill two, so the oldest issue is not in the newest part.
const ISSUES: usize = 1_200;
const LIST: usize = 200;
/// How many listings each measure takes the middle one of.
const SAMPLES: usize = 7;
/// How many times a listing's normal cost one served while the write waits may take.
const RATIO: u32 = 3;
/// What a listing may take beyond that, for the machine running the test being busy: far less
/// than the wait of a part held on the thread.
const SLACK: Duration = Duration::from_millis(20);
/// How long the write waits for its part before it is let go.
const HELD: Duration = Duration::from_secs(2);

/// The middle of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_waiting_for_its_part_leaves_the_workspace_answering() {
    // The workspace lists the oldest issue's part and has not sent it.
    held_back_beside(
        |workspace| HeldBack::listed(workspace, tasks::issue_list::chain().part(0).as_str()),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_and_a_listing_waiting_for_a_part_leave_the_workspace_answering() {
    // The service was told nothing of the oldest issue's part: the tracker asks for it as its
    // chains are handed over, and waits for it.
    held_back_beside(
        |workspace| HeldBack::unlisted(workspace, tasks::issue_list::chain().part(0).as_str()),
        true,
    )
    .await;
}

/// Restart a server holding a tracker of two parts, hold back what `held_back` names, and
/// measure a listing of the notes while a write under the oldest issue (and, with
/// `list_issues`, a listing of the issues) waits for it.
async fn held_back_beside(held_back: impl FnOnce(&str) -> HeldBack, list_issues: bool) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let catalogue = crate::common::catalogue();
    let tracker = tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS);
    let notebook = tilo_server::workspace_bots::workspace_resource(&catalogue, NOTES);
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    );

    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let person = person.in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let notes = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        document: notebook.clone(),
    };
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    for (app, document) in [(TASKS, &tracker), (NOTES, &notebook)] {
        workspaces
            .prepare_application(&workspace, &server.cert, &catalogue, app, document)
            .await
            .unwrap();
    }
    let mut oldest = None;
    {
        let service =
            WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
        for first in (0..ISSUES).step_by(LIST) {
            let list: Vec<Value> = (first..first + LIST)
                .map(|at| json!({ "title": format!("Issue {at}"), "body": "What it is about." }))
                .collect();
            let answer = invoke_named(
                &service,
                &person,
                &route,
                &issues,
                "create-many",
                json!({ "issues": list }).as_object().unwrap().clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("the workspace takes create-many: {error:?}"));
            let made = answer["value"]["made"]
                .as_array()
                .unwrap_or_else(|| panic!("a create answers what it made: {answer}"));
            oldest.get_or_insert_with(|| made[0]["id"].as_str().unwrap().to_owned());
        }
        invoke_named(
            &service,
            &person,
            &notes,
            &notes::note_list::chain_resource(),
            "create-page",
            json!({ "title": "Plans", "markdown": "What we do next." })
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .expect("a note is written");
    }
    let oldest = oldest.unwrap();
    assert!(
        workspaces
            .resource_document(
                &workspace,
                TASKS,
                tasks::issue_list::chain().part(1).as_str()
            )
            .await
            .unwrap()
            .is_some(),
        "the issues fill two parts of their chain"
    );

    // The server restarts: its workspace actions keep nothing of what they were sent.
    drop(page);
    drop(actions);
    served.shutdown().await;
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let service = std::sync::Arc::new(WorkspaceActionProvider::new(
        actions.clone(),
        WorkspaceId::parse(&workspace).unwrap(),
    ));
    let listing = || async {
        let started = Instant::now();
        let page = service
            .query(
                &person,
                CatalogueQuery {
                    provider: Some(record_provider(&notes)),
                    max_results: 16,
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the notes are listed")
            .expect("the notebook answers");
        assert!(!page.rows.is_empty(), "the notebook lists its note");
        started.elapsed()
    };
    // What a listing costs when nothing waits, the notebook mounted by the first.
    listing().await;
    let mut normal = Vec::new();
    for _ in 0..SAMPLES {
        normal.push(listing().await);
    }
    let normal = median(normal);

    // What the oldest issue is in is held back, and a write under that issue waits for it.
    let held = held_back(&workspace);
    let issues_listed = list_issues.then(|| {
        let service = service.clone();
        let person = person.clone();
        let route = route.clone();
        tokio::spawn(async move {
            service
                .query(
                    &person,
                    CatalogueQuery {
                        provider: Some(record_provider(&route)),
                        max_results: 16,
                        ..CatalogueQuery::default()
                    },
                )
                .await
        })
    });
    let write = tokio::spawn({
        let service = service.clone();
        let person = person.clone();
        let route = route.clone();
        let issues = issues.clone();
        async move {
            let asked = Instant::now();
            let answer = invoke_named(
                &service,
                &person,
                &route,
                &issues,
                "create",
                json!({ "title": "Under the oldest", "parent": oldest })
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await;
            (answer, asked.elapsed())
        }
    });
    let deadline = tilo_test_env::hang_deadline();
    while !held.looked_for() {
        if write.is_finished() {
            let (answer, took) = write.await.unwrap();
            panic!("the write did not wait for the part held back, {took:?}: {answer:?}");
        }
        assert!(
            Instant::now() < deadline,
            "the write looks for the oldest issue's part"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let waiting_since = Instant::now();
    let mut beside = Vec::new();
    for _ in 0..SAMPLES {
        beside.push(listing().await);
    }
    let beside = median(beside);
    assert!(
        !write.is_finished(),
        "the write waits for the part held back"
    );
    if let Some(listed) = &issues_listed {
        assert!(!listed.is_finished(), "the issues' listing waits for them");
    }
    let ratio = beside.as_secs_f64() / normal.as_secs_f64().max(1e-6);
    println!(
        "a listing took {normal:?} with nothing waiting and {beside:?} while a write waited for \
         its part: {ratio:.1}x"
    );
    assert!(
        beside <= normal * RATIO + SLACK,
        "a listing took {beside:?} while a write waited for its part, against {normal:?} with \
         nothing waiting ({ratio:.1}x)"
    );

    // Let go, the part arrives and the write lands with its answer.
    tokio::time::sleep(HELD.saturating_sub(waiting_since.elapsed())).await;
    drop(held);
    let (answer, took) = write.await.unwrap();
    let answer = answer.unwrap_or_else(|error| {
        panic!("the write lands once its part arrives, after {HELD:?}: {error:?}")
    });
    assert!(answer["value"].is_object(), "{answer}");
    println!("the write landed {took:?} after it was asked");
    if let Some(listed) = issues_listed {
        let page = listed
            .await
            .unwrap()
            .expect("the issues are listed")
            .expect("the tracker answers");
        assert!(
            !page.rows.is_empty(),
            "the tracker lists its issues once they arrive"
        );
    }
    drop(served);
}
