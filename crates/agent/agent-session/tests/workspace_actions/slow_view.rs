//! An app's worker that takes long to answer one workspace's view holds up nothing else.
//!
//! One thread serves the requests of many workspaces, one after another, and a view of a record
//! is read in a confined app worker by that thread. A view whose app's worker takes long to
//! answer (a large note, a worker that hangs) is made slow here ([`Slowed`]): meanwhile a view of
//! another record of the same workspace answers within a small multiple of what it costs with
//! nothing running.
use super::*;
use serde_json::json;
use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery, ViewResourceQuery};
use tilo_agent_session::workspace_actions::slowed::Slowed;
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};

const TASKS: &str = "so.tilo.tasks";
const NOTES: &str = "so.tilo.notes";
/// How long the slowed view's worker takes at most before it answers.
const SLOW: Duration = Duration::from_secs(4);
/// How many rounds each measure takes the middle one of. A round is one view asked alone, or
/// one view asked while a slowed view goes on.
const ROUNDS: usize = 7;
/// How many times a view's normal cost the one asked while the slow view goes on may take.
const RATIO: u32 = 3;
/// What a view may take beyond that: a shared machine has to find a core for the thread its
/// worker call goes on in, which costs tens of milliseconds when every core is taken, and the
/// view it waits for is held for seconds.
const SLACK: Duration = Duration::from_millis(500);

/// The middle of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// A view of the resource `(id, kind)` of the record at `route`.
fn view_of(route: &DocumentRoute, (id, kind): (&str, &str)) -> CatalogueQuery {
    CatalogueQuery {
        provider: Some(record_provider(route)),
        view_resource: Some(ViewResourceQuery {
            id: id.to_owned(),
            kind: kind.to_owned(),
        }),
        max_results: 256,
        ..CatalogueQuery::default()
    }
}

/// One resource `route`'s record publishes, as its listing names it.
async fn viewable(
    service: &WorkspaceActionProvider,
    person: &Caller,
    route: &DocumentRoute,
) -> (String, String) {
    let page = service
        .query(
            person,
            CatalogueQuery {
                provider: Some(record_provider(route)),
                max_results: 256,
                ..CatalogueQuery::default()
            },
        )
        .await
        .expect("the record is listed")
        .expect("the record answers");
    let Some(row) = page.rows.first() else {
        panic!("{} publishes a resource", route.app_id);
    };
    (row.resource.id.clone(), row.resource.kind.clone())
}

/// How long the view `query` takes to answer: its record read in a worker and its views sent.
async fn viewed(
    service: &WorkspaceActionProvider,
    person: &Caller,
    query: CatalogueQuery,
) -> Duration {
    let started = Instant::now();
    let page = service
        .query(person, query)
        .await
        .expect("the view is asked")
        .expect("the view answers");
    let took = started.elapsed();
    assert!(
        !page.revision.is_empty(),
        "the view is built from the record's source"
    );
    took
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_view_leaves_its_thread_answering() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let catalogue = crate::common::catalogue();
    let tracker = tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS);
    let notebook = tilo_server::workspace_bots::workspace_resource(&catalogue, NOTES);
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());
    let device = Caller::authenticated(
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
    let task_route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let note_route = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        document: notebook.clone(),
    };
    let _page = Browser::open_at(&served, &browser, &server, &workspace).await;
    for (app, document) in [(TASKS, &tracker), (NOTES, &notebook)] {
        workspaces
            .prepare_application(&workspace, &server.cert, &catalogue, app, document)
            .await
            .unwrap();
    }
    let service = std::sync::Arc::new(WorkspaceActionProvider::new(
        actions.clone(),
        WorkspaceId::parse(&workspace).unwrap(),
    ));
    let person = device.in_workspace(&workspace);
    invoke_named(
        &service,
        &person,
        &note_route,
        &notes::note_list::chain_resource(),
        "create-page",
        json!({ "title": "Plans", "markdown": "What we do next." })
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("a note is written");
    invoke_named(
        &service,
        &person,
        &task_route,
        &issues,
        "create",
        json!({ "title": "First" }).as_object().unwrap().clone(),
    )
    .await
    .expect("an issue is made");

    // The note and the issue each publish a view, and a view of each answers with its rows
    // before anything is made slow.
    let note = viewable(&service, &person, &note_route).await;
    let issue = viewable(&service, &person, &task_route).await;
    let asked = view_of(&note_route, (&note.0, &note.1));
    let here = view_of(&task_route, (&issue.0, &issue.1));
    viewed(&service, &person, asked.clone()).await;
    viewed(&service, &person, here.clone()).await;

    // What a view of the issue costs with nothing running: the middle of a few.
    let mut alone = Vec::new();
    for _ in 0..ROUNDS {
        alone.push(viewed(&service, &person, here.clone()).await);
    }
    let alone = median(alone);

    // Each round, the note's worker takes long to answer a view, and the issue's view is asked
    // while it does: it waits for nothing of the note's.
    let mut beside = Vec::new();
    for _ in 0..ROUNDS {
        let slowed = Slowed::new(&workspace, NOTES, SLOW);
        let asking = {
            let service = std::sync::Arc::clone(&service);
            let person = person.clone();
            let asked = asked.clone();
            tokio::spawn(async move { viewed(&service, &person, asked).await })
        };
        let deadline = tilo_test_env::hang_deadline();
        while slowed.entered() == 0 {
            assert!(Instant::now() < deadline, "the view asks the note's worker");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let took = viewed(&service, &person, here.clone()).await;
        println!("a view of the issue beside the slow one took {took:?}");
        beside.push(took);
        // Let the slowed view answer, so the next round starts from a quiet thread.
        drop(slowed);
        asking.await.unwrap();
    }
    let beside = median(beside);
    let ratio = beside.as_secs_f64() / alone.as_secs_f64().max(1e-6);
    println!(
        "a view of the issue took {alone:?} with nothing running and {beside:?} while the \
         note's view was slow: {ratio:.1}x"
    );
    assert!(
        beside <= alone * RATIO + SLACK,
        "a view of the issue took {beside:?} while a slow view of the note went on, against \
         {alone:?} with nothing running ({ratio:.1}x)"
    );
    drop(served);
}
