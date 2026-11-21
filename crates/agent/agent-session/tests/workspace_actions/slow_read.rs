//! A registered read whose worker takes long to answer holds up nothing else.
//!
//! The feedback projection proves its destination through the record's registered read, mounted
//! in a confined app worker by the thread that serves the workspace. Made slow here ([`Slowed`]),
//! a view of another record of the same workspace answers meanwhile within a small multiple of
//! what it costs with nothing running.
use super::*;
use serde_json::json;
use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery, ViewResourceQuery};
use tilo_agent_session::workspace_actions::slowed::Slowed;
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};
use ui::feedback::{Destination, Draft, Submission};

const TASKS: &str = "so.tilo.tasks";
const CHAT: &str = "so.tilo.chat";
/// How long the slowed read's worker takes at most before it answers.
const SLOW: Duration = Duration::from_secs(4);
/// How many rounds each measure takes the middle one of. A round is one view asked alone, or
/// one view asked while a slowed read goes on.
const ROUNDS: usize = 7;
/// How many times a view's normal cost the one asked while the slow read goes on may take.
const RATIO: u32 = 3;
/// What a view may take beyond that: a shared machine has to find a core for the thread the
/// call goes on in, which costs tens of milliseconds when every core is taken, and the read it
/// waits for is held for seconds.
const SLACK: Duration = Duration::from_millis(500);

/// The middle of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// A view of the resource `(id, kind)` of the record at `route`.
fn view_of(route: &DocumentRoute, (id, kind): (&str, &str)) -> CatalogueQuery {
    CatalogueQuery {
        provider: Some(resource_provider(route)),
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
                provider: Some(resource_provider(route)),
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

/// A chat document with the feedback channel a report goes to, written into the workspace.
async fn feedback_resource(browser: &mut Browser) -> DocumentRoute {
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: browser.cluster_id.clone(),
        document: String::new(),
    };
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        if browser.documents.contains_key(&route) {
            break;
        }
        assert!(Instant::now() < deadline, "the chat record not received");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let source = browser.mount(&route);
    let mut doc = source.read(Clone::clone).unwrap();
    chat::demo::insert_category(&mut doc, "feedback-category", "Feedback").unwrap();
    chat::demo::insert_channel(
        &mut doc,
        "feedback",
        "feedback-category",
        "Feedback",
        chat::model::ChannelKind::Text,
    )
    .unwrap();
    let mutation = collab_types::DocumentMutation::Snapshot(doc.save());
    browser.peer.write(route.clone(), &mutation).unwrap();
    let (collab_types::DocumentMutation::Snapshot(bytes)
    | collab_types::DocumentMutation::Incremental(bytes)) = mutation;
    browser
        .documents
        .entry(route.clone())
        .or_default()
        .push(([0; 32], bytes));
    route
}

/// A report to the feedback channel of `route`, which the projection proves through its read.
fn approved(route: &DocumentRoute) -> Submission {
    let mut preview = Draft {
        text: "The book failed to open".into(),
        context: Vec::new(),
    }
    .preview()
    .unwrap();
    preview.approve();
    preview
        .submission(
            "0123456789abcdef0123456789abcdef".into(),
            Destination {
                workspace: route.cluster_id.clone(),
                provider: resource_provider(route),
                channel: "feedback".into(),
            },
        )
        .unwrap()
}

/// Ask the service for the feedback admission of `submission`: the read of the document's registered
/// destination, with no report attached.
async fn asked(browser: &mut Browser, submission: &Submission) -> Duration {
    use tilo_actions::remote::feedback::{FeedbackDestination, FeedbackQuery};
    let started = Instant::now();
    browser.correlation = browser.correlation.checked_add(1).unwrap();
    let correlation = browser.correlation;
    browser.send(RemoteActionMessage::FeedbackRequest {
        correlation,
        query: FeedbackQuery {
            destination: FeedbackDestination {
                workspace: submission.destination.workspace.clone(),
                provider: submission.destination.provider.clone(),
                channel: submission.destination.channel.clone(),
            },
            submission: None,
            review: None,
        },
    });
    let answer = browser
        .answer("the feedback admission", |message| match message {
            RemoteActionMessage::FeedbackResponse {
                correlation: received,
                result,
            } if *received == correlation => Some(result.clone()),
            _ => None,
        })
        .await;
    assert!(answer.is_ok(), "the feedback read answers: {answer:?}");
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_read_leaves_its_thread_answering() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let catalogue = crate::common::catalogue();
    let tracker = tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS);
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());
    let device = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser_device.cert.device_public_key,
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
    let mut browser = Browser::open_at(&served, &browser_device, &server, &workspace).await;
    for (app, document) in [(TASKS, tracker.as_str()), (CHAT, "")] {
        workspaces
            .prepare_application(&workspace, &server.cert, &catalogue, app, document)
            .await
            .unwrap();
    }
    let chat = feedback_resource(&mut browser).await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let submission = approved(&chat);
    let task_route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let service = std::sync::Arc::new(WorkspaceActionProvider::new(
        actions.clone(),
        WorkspaceId::parse(&workspace).unwrap(),
    ));
    let person = device.in_workspace(&workspace);
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

    // The issue publishes a view, which answers with its rows before anything is made slow.
    let issue = viewable(&service, &person, &task_route).await;
    let here = view_of(&task_route, (&issue.0, &issue.1));
    viewed(&service, &person, here.clone()).await;

    // What a view of the issue costs with nothing running: the middle of a few.
    let mut alone = Vec::new();
    for _ in 0..ROUNDS {
        alone.push(viewed(&service, &person, here.clone()).await);
    }
    let alone = median(alone);

    // Each round, the chat record's read takes long to answer, and the issue's view is asked
    // while it does: it waits for nothing of the chat's.
    let mut beside = Vec::new();
    for _ in 0..ROUNDS {
        let slowed = Slowed::new(&workspace, CHAT, SLOW);
        let sample = async {
            let deadline = tilo_test_env::hang_deadline();
            while slowed.entered() == 0 {
                assert!(Instant::now() < deadline, "the read asks the chat's worker");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let took = viewed(&service, &person, here.clone()).await;
            println!("a view of the issue beside the slow read took {took:?}");
            // Let the slowed read answer, so the round ends and the next starts from a quiet
            // thread.
            drop(slowed);
            took
        };
        let (took, _) = tokio::join!(sample, asked(&mut browser, &submission));
        beside.push(took);
    }
    let beside = median(beside);
    let ratio = beside.as_secs_f64() / alone.as_secs_f64().max(1e-6);
    println!(
        "a view of the issue took {alone:?} with nothing running and {beside:?} while the \
         chat's read was slow: {ratio:.1}x"
    );
    assert!(
        beside <= alone * RATIO + SLACK,
        "a view of the issue took {beside:?} while a slow read of the chat went on, against \
         {alone:?} with nothing running ({ratio:.1}x)"
    );
    drop(served);
}
