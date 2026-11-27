//! What a workspace's own host keeps and answers over time: a branch of its Files, a document a
//! remote member wrote sealed and read cold afterwards, a link a host mints under its web entry,
//! and a view asked while a provider's call is slow.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{
    ActionProvider as _, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    ViewResourceQuery,
};
use tilo_actions::{ActionAddress, ActionOutput};

use crate::language::files_support::{
    Session, content_at, content_bytes, keeps, kept_when, put, said, tree_if_kept, varied,
};
use crate::language::host::{self, Host};

/// Where a branch is asked for: the account's `workspaces#branch`.
const BRANCH: &str = "/v1/resources/account/workspaces/actions/branch";

/// A branch of a workspace holds every file its tree names, under the same path, readable from the
/// branch's own content alone; from then on each tree goes its own way.
#[test]
fn a_branch_holds_the_workspaces_files_and_each_goes_its_own_way() {
    host::run(|host| async move {
        let source = host
            .daemon()
            .workspaces()
            .create("Contracts")
            .await
            .unwrap()
            .workspace;
        let key = host.key("Backend");
        let session = Session::Key(&key);

        // A file at the root, and a folder with two: one of one chunk, one larger than a chunk,
        // which the workspace keeps as many.
        let readme = b"Every contract of the customer, one folder a year.\n".repeat(200);
        let small = varied(20_000, 1);
        let large = varied(700_000, 2);
        assert_eq!(crate::language::files_support::chunks_of(&small), 1);
        assert!(
            crate::language::files_support::chunks_of(&large) > 1,
            "the large file is kept as chunks",
        );
        let files: [(&str, &str, &[u8]); 3] = [
            ("", "README.txt", &readme),
            ("contracts", "terms.txt", &small),
            ("contracts", "signed.pdf", &large),
        ];
        for (dir, name, bytes) in files {
            put_file(&session, &source, dir, name, bytes).await;
        }
        // The host keeps every one of them whole before the branch.
        let mut source_hashes = std::collections::BTreeMap::new();
        for (dir, name, bytes) in files {
            source_hashes.insert(
                (dir, name),
                keeps(&session, &host, &source, (dir, name), bytes).await,
            );
        }

        let (status, branched) = key
            .post(
                BRANCH,
                &json!({"workspace": source, "name": "Contracts: Acme"}),
            )
            .await;
        assert_eq!(status, 200, "{branched}");
        let branch = branched["result"]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();

        // Every file is in the branch's tree under the same path, and the branch's own content
        // holds every byte of it.
        let tree = tree_if_kept(&host, &branch)
            .await
            .expect("the branch keeps a tree");
        let mut branch_hashes = std::collections::BTreeMap::new();
        for (dir, name, bytes) in files {
            let node = crate::language::files_support::file_at(&tree, dir, name)
                .unwrap_or_else(|| panic!("the branch's tree lacks {dir}/{name}"));
            let hash = node.hash.as_deref().expect("the file names its content");
            assert_ne!(
                hash,
                source_hashes[&(dir, name)],
                "the branch seals {dir}/{name} under its own keys",
            );
            assert_eq!(node.size, bytes.len() as u64);
            assert_eq!(
                content_bytes(&session, &branch, &format!("tilo.content:{hash}"))
                    .await
                    .as_slice(),
                bytes,
                "the branch's content holds {dir}/{name} whole",
            );
            branch_hashes.insert((dir, name), hash.to_owned());
        }

        // A file changed in the branch is changed there alone, and the reverse.
        let in_branch = b"Acme's own terms.\n".repeat(300);
        put_file(&session, &branch, "contracts", "terms.txt", &in_branch).await;
        let edited = keeps(
            &session,
            &host,
            &branch,
            ("contracts", "terms.txt"),
            &in_branch,
        )
        .await;
        let in_source = b"The template's next terms.\n".repeat(300);
        put_file(&session, &source, "", "README.txt", &in_source).await;
        keeps(&session, &host, &source, ("", "README.txt"), &in_source).await;
        // A file added to the source after the branch is the source's alone.
        let later = varied(9_000, 3);
        put_file(&session, &source, "contracts", "later.txt", &later).await;
        keeps(&session, &host, &source, ("contracts", "later.txt"), &later).await;
        tokio::time::sleep(Duration::from_secs(2)).await;

        let (source_tree, branch_tree) = (
            tree_if_kept(&host, &source).await.expect("a tree"),
            tree_if_kept(&host, &branch).await.expect("a tree"),
        );
        assert_eq!(
            content_at(&source_tree, "contracts", "terms.txt"),
            source_hashes.remove(&("contracts", "terms.txt")),
            "the branch's edit stays in the branch",
        );
        assert_eq!(
            content_at(&branch_tree, "", "README.txt"),
            branch_hashes.remove(&("", "README.txt")),
            "the source's edit stays in the source",
        );
        assert_eq!(
            content_at(&branch_tree, "contracts", "terms.txt"),
            Some(edited)
        );
        assert!(
            crate::language::files_support::file_at(&branch_tree, "contracts", "later.txt")
                .is_none(),
            "a file added to the source after the branch is not in the branch",
        );
        assert!(
            crate::language::files_support::file_at(&branch_tree, "contracts", "signed.pdf")
                .is_some(),
            "the branch keeps what it took",
        );
    });
}

/// Put `bytes` into `workspace`'s Files at `dir/name`, as the API puts a file.
async fn put_file(session: &Session<'_>, workspace: &str, dir: &str, name: &str, bytes: &[u8]) {
    let content = put(session, workspace, bytes).await;
    let (text, failed) = said(
        &session
            .tool(
                workspace,
                "tilo_do",
                &json!({"path": "files#open",
                    "args": {"content": content, "name": name, "dir": dir}}),
            )
            .await,
    );
    assert!(!failed, "files#open {dir}/{name}: {text}");
}

/// A file whose host is still gathering one chunk of it makes no branch until it is here; asked
/// again under the same key once it arrives, the branch is made and holds the file whole.
#[test]
fn a_file_its_host_is_still_gathering_makes_no_branch_until_it_is_here() {
    host::run(|host| async move {
        let source = host
            .daemon()
            .workspaces()
            .create("Scans")
            .await
            .unwrap()
            .workspace;
        let key = host.key("Backend");
        let session = Session::Key(&key);
        let scan = varied(700_000, 4);
        put_file(&session, &source, "scans", "passport.pdf", &scan).await;
        keeps(&session, &host, &source, ("scans", "passport.pdf"), &scan).await;
        kept_when(&host, &source, |tree| {
            crate::language::files_support::file_at(tree, "scans", "passport.pdf").is_some()
        })
        .await;

        // The tree names the file, but one of its chunks has not reached the host yet, as when a
        // device that wrote it is still sending it. Taking the chunk back is a content store's
        // `release`, which a device's daemon does not hand a test: the workspace's content is
        // read through the dispatch that opens it for a member, never as the store holds it.
        panic!(
            "a device's daemon hands no content store to a test: the chunk of a file cannot be \
             released to stand in for one still on its way, and the branch's `branch_incomplete` \
             refusal cannot be asked"
        );
    });
}

/// A remote writer's Chat document reaches the host as the member's own write, kept as it was
/// signed on its exact route, and is read cold after the host starts again.
#[test]
fn remote_encrypted_chat_primary_is_discoverable_after_host_restart() {
    const CHAT: &str = "so.tilo.chat";
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        // Another person's computer joins and keeps the workspace as a member copy. Its Chat
        // instance writes there, and what it writes reaches this host over the device link.
        let second = host::Second::joined(
            &host,
            "Bo",
            "editor",
            &[crate::language::hosted_turns::CHAT_PACKAGE],
            &[],
        )
        .await;
        // The workspace lists them among its people, by the root the invitation named.
        let (_, people) = host
            .daemon()
            .workspaces()
            .people_of(&workspace)
            .await
            .expect("the host keeps the workspace");
        let person: [u8; 32] = hex::decode(&second.person)
            .expect("the person is named by their root")
            .try_into()
            .expect("a root is 32 bytes");
        assert!(
            people.values().any(|user| *user == person),
            "the second person is one of the workspace's members: {people:?}"
        );
        let made = second.do_in("so.tilo.chat#new_channel", &["name=Shared"]);
        assert!(
            made.contains("so.tilo.chat/Chat#new_channel"),
            "the other computer's write answers the channel it made: {made}"
        );
        assert!(
            crate::language::chat::chat_holds_soon(&host, "Shared").await,
            "the member's write reaches the host's record"
        );

        // The host keeps the member's own signed write, on Chat's route, beside its own bodies.
        let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
        let own_device = host.daemon().workspaces().device_cert().device_public_key;
        let retained = host
            .daemon()
            .workspaces()
            .documents()
            .retained(id)
            .await
            .expect("the host keeps the bodies its members send");
        assert!(
            retained.iter().any(|body| body.author != own_device),
            "the member's write is a body the host kept, signed by the member's device"
        );

        // The host starts again, and the record reads cold: what the member wrote is still here.
        host.restart().await;
        let routes = host
            .daemon()
            .workspaces()
            .document_routes(&workspace)
            .await
            .expect("the restarted host lists its records");
        let (_, record) = routes
            .iter()
            .find(|(app, _)| app == CHAT)
            .unwrap_or_else(|| panic!("the restarted host keeps Chat's record: {routes:?}"));
        let document = host
            .daemon()
            .workspaces()
            .resource_document(&workspace, CHAT, record)
            .await
            .expect("the record reads")
            .expect("the restarted host holds it");
        assert!(
            document
                .windows(b"Shared".len())
                .any(|window| window == b"Shared"),
            "the member's write is in the record the host reads cold after its restart"
        );
    });
}

/// The provider whose calls take long: each one holds the thread it is asked on until the test
/// lets it go or [`SLOW`] passes, as a provider doing a large import, a long effect or one that
/// hangs does.
const SLOW_PROVIDER: &str = "so.tilo.slow";
/// How long a slow call holds its thread at most before it answers.
const SLOW: Duration = Duration::from_secs(4);
/// How many times a view's normal cost the one asked while a slow call goes on may take.
const RATIO: u32 = 3;
/// What a view may take beyond that: a shared machine has to find a core for the thread the
/// call goes on in, which costs tens of milliseconds when every core is taken, and the slow
/// call holds its own for seconds.
const SLACK: Duration = Duration::from_millis(500);

/// A provider registered on the device whose every call holds its thread: it says when one
/// starts, and answers once `let_go` is dropped or [`SLOW`] passes.
fn slow_provider(
    started: Arc<tokio::sync::Notify>,
    let_go: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
) -> Arc<TestProvider> {
    Arc::new(
        TestProvider::new()
            .with_catalogue(|caller, query| {
                let row = CatalogueRow::for_action(
                    SLOW_PROVIDER,
                    SLOW_PROVIDER,
                    tilo_actions::ActionDescriptor {
                        did: None,
                        state: tilo_actions::ActionState::default(),
                        semantic_identity: None,
                        address: ActionAddress::new("slow", "import"),
                        resource_kind: "slow".into(),
                        resource_label: "Slow".into(),
                        action_label: "Import".into(),
                        keywords: Vec::new(),
                        icon: None,
                        description: None,
                        locus: tilo_actions::ExecutionLocus::ClientLocal,
                        kind: tilo_actions::OperationKind::Effect,
                        contract: tilo_actions::ActionContract::default(),
                        input: tilo_actions::InputSpec::None,
                        params: Vec::new(),
                        view: None,
                    },
                );
                let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
                if query.provider.is_none() {
                    page.providers.push(CatalogueProvider {
                        provider: SLOW_PROVIDER.into(),
                        app_id: SLOW_PROVIDER.into(),
                        workspace: caller.workspace().unwrap_or_default().to_owned(),
                        document_id: String::new(),
                        browsable: true,
                    });
                }
                Ok(page)
            })
            .completing(move |_| {
                started.notify_one();
                let held = let_go.lock().unwrap().take();
                if let Some(held) = held {
                    // Either the test lets go (its sender dropped) or the bound passes.
                    let _ = held.recv_timeout(SLOW);
                }
                ActionOutput::default()
            }),
    )
}

/// The middle of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// A call of a provider the device registered that takes long to answer holds up nothing else: a
/// view of a record of the same workspace answers meanwhile within a small multiple of what it
/// costs with nothing running.
#[test]
fn a_slow_read_leaves_its_thread_answering() {
    const ROUNDS: usize = 7;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Viewer");
        let session = Session::Key(&key);
        // An issue is made, and the record it is kept in publishes a view of it.
        let (text, failed) = said(
            &session
                .tool(
                    &workspace,
                    "tilo_do",
                    &json!({"path": "so.tilo.tasks#open", "args": {}}),
                )
                .await,
        );
        assert!(!failed, "{text}");
        let (text, failed) = said(
            &session
                .tool(
                    &workspace,
                    "tilo_do",
                    &json!({"path": "so.tilo.tasks#new_task",
                        "args": {"title": "First"}}),
                )
                .await,
        );
        assert!(!failed, "{text}");

        let started = Arc::new(tokio::sync::Notify::new());
        let let_go = Arc::new(Mutex::new(None));
        host.daemon().provide(
            SLOW_PROVIDER,
            slow_provider(Arc::clone(&started), Arc::clone(&let_go)) as _,
        );

        let view = issue_view(&host, &workspace).await;
        viewed(&host, &workspace, view.clone()).await;
        // What a view of the issue costs with nothing running: the middle of a few.
        let mut alone = Vec::new();
        for _ in 0..ROUNDS {
            alone.push(viewed(&host, &workspace, view.clone()).await);
        }
        let alone = median(alone);

        // Each round, a key's call of the slow provider holds its thread, and the issue's view
        // is asked while it does: it waits for nothing of the slow call's.
        let mut beside = Vec::new();
        for _ in 0..ROUNDS {
            let (release, held) = std::sync::mpsc::channel::<()>();
            *let_go.lock().unwrap() = Some(held);
            let slow_key = host.key("Importer");
            let in_workspace = workspace.clone();
            let call = tokio::spawn(async move {
                said(
                    &slow_key
                        .tool(
                            "tilo_do",
                            &json!({"workspace": in_workspace, "path": "slow#import",
                                "args": {}}),
                        )
                        .await,
                )
            });
            tokio::time::timeout(SLOW, started.notified())
                .await
                .expect("the key's call reaches the slow provider");
            let took = viewed(&host, &workspace, view.clone()).await;
            println!("a view of the issue beside the slow call took {took:?}");
            // Let the slow call answer, so the round ends and the next starts from a quiet
            // thread.
            drop(release);
            let (text, failed) = call.await.expect("the slow call ends");
            assert!(!failed, "the slow call is answered: {text}");
            beside.push(took);
        }
        let beside = median(beside);
        let ratio = beside.as_secs_f64() / alone.as_secs_f64().max(1e-6);
        println!(
            "a view of the issue took {alone:?} with nothing running and {beside:?} while a \
             provider's call was slow: {ratio:.1}x"
        );
        assert!(
            beside <= alone * RATIO + SLACK,
            "a view of the issue took {beside:?} while a provider's slow call went on, against \
             {alone:?} with nothing running ({ratio:.1}x)"
        );
    });
}

/// One resource the workspace's record publishes, as its listing names it.
async fn issue_view(host: &Host, workspace: &str) -> CatalogueQuery {
    let page = host
        .dispatch()
        .query(
            &host.caller(),
            CatalogueQuery {
                provider: Some(tilo_actions::remote::workspace_provider(workspace)),
                max_results: 256,
                ..CatalogueQuery::default()
            },
        )
        .await
        .expect("the record is listed")
        .expect("the record answers");
    let row = page
        .rows
        .first()
        .expect("the workspace publishes a resource");
    CatalogueQuery {
        provider: Some(tilo_actions::remote::workspace_provider(workspace)),
        view_resource: Some(ViewResourceQuery {
            id: row.resource.id.clone(),
            kind: row.resource.kind.clone(),
        }),
        max_results: 256,
        ..CatalogueQuery::default()
    }
}

/// How long the view `query` takes to answer: its record read in a worker and its views sent.
async fn viewed(host: &Host, workspace: &str, query: CatalogueQuery) -> Duration {
    let _ = workspace;
    let started = Instant::now();
    let page = host
        .dispatch()
        .query(&host.caller(), query)
        .await
        .expect("the view is asked")
        .expect("the view answers");
    let took = started.elapsed();
    assert!(
        !page.revision.is_empty(),
        "the view is built from the record's source",
    );
    took
}
