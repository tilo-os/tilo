//! An app's worker that takes long to answer one workspace's write holds up nothing else.
//!
//! One thread serves the requests of many workspaces. A write whose app's worker takes long to
//! answer (a large import, a long effect, a worker that hangs) is made slow here ([`Slowed`]):
//! meanwhile a listing of another workspace served by the same thread, and a listing of
//! another app of the same workspace, each answer within a small multiple of what they cost
//! with nothing running. A second write of the same workspace waits for the first, and both
//! land with the answers they would have had.
use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery};
use tilo_agent_session::workspace_actions::slowed::{Slowed, served_together};
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};

const TASKS: &str = "so.tilo.tasks";
const NOTES: &str = "so.tilo.notes";
/// How many rounds each measure takes the middle of: in each, a listing with nothing running,
/// then a slowed write, then the first listing asked while it runs.
const ROUNDS: usize = 5;
/// How many times a listing's normal cost one served while the write goes on may take.
const RATIO: u32 = 3;
/// What a listing may take beyond that: the thread waits the app call's grace (20 ms) before
/// the call goes on off it, and a machine running the whole suite stretches that wait and the
/// listing behind it. Far less than the slow write, which holds the thread for seconds.
const SLACK: Duration = Duration::from_millis(100);
/// How long the write's worker takes at most before it answers.
const SLOW: Duration = Duration::from_secs(4);
/// How many workspaces are made at most to find one served with the first.
const TRIES: usize = 40;

/// The middle of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// How long a listing of the notes at `notes` takes, as `person`.
async fn listed(
    service: &WorkspaceActionProvider,
    person: &Caller,
    notes: &DocumentRoute,
) -> Duration {
    let started = Instant::now();
    let page = service
        .query(
            person,
            CatalogueQuery {
                provider: Some(resource_provider(notes)),
                max_results: 16,
                ..CatalogueQuery::default()
            },
        )
        .await
        .expect("the notes are listed")
        .expect("the notebook answers");
    assert!(!page.rows.is_empty(), "the notebook lists its note");
    started.elapsed()
}

/// Wait until the tracker's worker has started on the slowed write `write`.
async fn started<T>(slowed: &Slowed, write: &tokio::task::JoinHandle<T>) {
    let deadline = tilo_test_env::hang_deadline();
    while slowed.entered() == 0 {
        assert!(
            !write.is_finished(),
            "the write is answered without its worker"
        );
        assert!(
            Instant::now() < deadline,
            "the write asks the tracker's worker"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_write_leaves_its_thread_answering() {
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
    let busy = workspaces.create("Team").await.unwrap().workspace;
    let mut other = None;
    for at in 0..TRIES {
        let made = workspaces
            .create(&format!("Other {at}"))
            .await
            .unwrap()
            .workspace;
        if served_together(&busy, &made) {
            other = Some(made);
            break;
        }
    }
    let other = other.expect("a workspace served on the same thread as the first");
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: busy.clone(),
        document: tracker.clone(),
    };
    let notes_of = |workspace: &str| DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.to_owned(),
        document: notebook.clone(),
    };
    let (busy_notes, other_notes) = (notes_of(&busy), notes_of(&other));
    let _pages = (
        Browser::open_at(&served, &browser, &server, &busy).await,
        Browser::open_at(&served, &browser, &server, &other).await,
    );
    for (workspace, app, document) in [
        (&busy, TASKS, &tracker),
        (&busy, NOTES, &notebook),
        (&other, NOTES, &notebook),
    ] {
        workspaces
            .prepare_application(workspace, &server.cert, &catalogue, app, document)
            .await
            .unwrap();
    }
    let service_of = |workspace: &str| {
        std::sync::Arc::new(WorkspaceActionProvider::new(
            actions.clone(),
            WorkspaceId::parse(workspace).unwrap(),
        ))
    };
    let (busy_service, other_service) = (service_of(&busy), service_of(&other));
    let (busy_person, other_person) = (
        device.clone().in_workspace(&busy),
        device.in_workspace(&other),
    );
    for (service, person, notes) in [
        (&busy_service, &busy_person, &busy_notes),
        (&other_service, &other_person, &other_notes),
    ] {
        invoke_named(
            service,
            person,
            notes,
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
    let create = |title: &str| {
        let service = busy_service.clone();
        let person = busy_person.clone();
        let route = route.clone();
        let issues = issues.clone();
        let arguments = json!({ "title": title }).as_object().unwrap().clone();
        tokio::spawn(async move {
            let answer =
                invoke_named(&service, &person, &route, &issues, "create", arguments).await;
            (answer, Instant::now())
        })
    };
    // The tracker is mounted by a first write, and the notebooks by a first listing each.
    let (first, _) = create("First").await.unwrap();
    first.expect("a first issue is made");
    listed(&busy_service, &busy_person, &busy_notes).await;
    listed(&other_service, &other_person, &other_notes).await;

    // Each round measures a listing with nothing running, then the first one asked while the
    // tracker's worker takes long to answer a write. The middle of the rounds is compared: a
    // machine running many other checks shifts both the same way, where the slowest of a few
    // against the middle of another few marked an uneven moment as the write holding the thread.
    for (what, service, person, notes) in [
        (
            "another workspace",
            &other_service,
            &other_person,
            &other_notes,
        ),
        (
            "the same workspace",
            &busy_service,
            &busy_person,
            &busy_notes,
        ),
    ] {
        let mut normal = Vec::new();
        let mut beside = Vec::new();
        for round in 0..ROUNDS {
            normal.push(listed(service, person, notes).await);
            // The tracker's worker takes long to answer a write.
            let slowed = Slowed::new(&busy, TASKS, SLOW);
            let write = create(&format!("Slow {round}"));
            started(&slowed, &write).await;
            beside.push(listed(service, person, notes).await);
            drop(slowed);
            let (answer, _) = write.await.unwrap();
            answer.unwrap_or_else(|error| panic!("the slow write lands: {error:?}"));
        }
        let (normal, beside) = (median(normal), median(beside));
        let ratio = beside.as_secs_f64() / normal.as_secs_f64().max(1e-6);
        println!(
            "the first listing of {what} took {normal:?} with nothing running and {beside:?} \
             while a slow write ran: {ratio:.1}x"
        );
        assert!(
            beside <= normal * RATIO + SLACK,
            "the first listing of {what} took {beside:?} while a slow write ran, against \
             {normal:?} with nothing running ({ratio:.1}x)"
        );
    }

    // A second write of the same workspace waits for the first.
    let slowed = Slowed::new(&busy, TASKS, SLOW);
    let asked = Instant::now();
    let slow = create("Slow");
    started(&slowed, &slow).await;
    let next = create("Next");
    // Given the time a fast write takes, the second one has not landed: it waits for the first.
    while asked.elapsed() < Duration::from_millis(200) {
        assert!(!next.is_finished(), "the second write waits for the first");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        asked.elapsed() < SLOW,
        "the second write was checked while the first was slowed"
    );
    assert!(!slow.is_finished(), "the slow write goes on");

    // Let go, both writes land with their answers, in the order they were asked.
    drop(slowed);
    let (slow, slow_at) = slow.await.unwrap();
    let (next, next_at) = next.await.unwrap();
    for (what, answer) in [("slow", slow), ("next", next)] {
        let answer: Value =
            answer.unwrap_or_else(|error| panic!("the {what} write lands: {error:?}"));
        assert!(answer["value"].is_object(), "{answer}");
    }
    assert!(slow_at <= next_at, "the second write lands after the first");
    drop(served);
}
