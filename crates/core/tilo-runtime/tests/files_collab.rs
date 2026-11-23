//! What a workspace's own host keeps and answers over time: a branch of its Files, a document a
//! remote member wrote sealed and read cold afterwards, a link a host mints under its web entry,
//! and a view asked while a record's read is slow.

use std::time::{Duration, Instant};

use serde_json::json;
use tilo_actions::remote::{ActionProvider as _, CatalogueQuery, ViewResourceQuery};

use crate::files_support::{
    Session, content_at, content_bytes, keeps, kept_when, put, said, tree_if_kept, varied,
};
use crate::host::{self, Host};

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
        assert_eq!(crate::files_support::chunks_of(&small), 1);
        assert!(
            crate::files_support::chunks_of(&large) > 1,
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
            let node = crate::files_support::file_at(&tree, dir, name)
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
            crate::files_support::file_at(&branch_tree, "contracts", "later.txt").is_none(),
            "a file added to the source after the branch is not in the branch",
        );
        assert!(
            crate::files_support::file_at(&branch_tree, "contracts", "signed.pdf").is_some(),
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
            crate::files_support::file_at(tree, "scans", "passport.pdf").is_some()
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

/// Two keys of a workspace: the writer who wrote a sealed Chat document, and the reader who reads
/// it cold after the host starts again.
#[test]
fn remote_encrypted_chat_primary_is_discoverable_after_host_restart() {
    host::run(|host| async move {
        let _ = &host;
        // The flow: a remote writer's Chat document is kept sealed on its exact route; the
        // message's own record is read cold by another member; the host starts again; and the
        // reader's catalogue still finds it, the page's origin naming the restarted host.
        //
        // Three pieces a device's daemon does not give a test: a member's document write over
        // the device link (the fixture's Member sends calls, not documents), the retained bodies
        // a server keeps (`workspace_documents().retained`), and a restart of the daemon whose
        // origin the page would name.
        panic!(
            "a device's daemon gives no member document write, no retained-bodies read, and no \
             restart: an encrypted document cannot be written remotely and read cold after one"
        );
    });
}

/// The workspace's host mints the link to a resource in a record it runs headless: the link names
/// the resource and its range under the web entry the host announces.
#[test]
fn the_server_mints_a_message_link_with_its_range() {
    host::run(|host| async move {
        let _ = &host;
        // A link is minted under the web entry the host announces (`site`, as a kept server's
        // web entry sets it). A device's daemon announces none: `keep.rs` gives a kept server its
        // site from its configuration, and a device that keeps nothing has no page at a host's
        // web entry for a link to open.
        panic!(
            "a device's daemon announces no web entry: there is no site for `files#share-link` \
             (or a message's) to mint a link under"
        );
    });
}

/// A registered read whose worker takes long to answer holds up nothing else: a view of another
/// record of the same workspace answers meanwhile within a small multiple of what it costs with
/// nothing running.
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

        let view = issue_view(&host, &workspace).await;
        // What a view of the issue costs with nothing running: the middle of a few.
        let mut alone = Vec::new();
        for _ in 0..ROUNDS {
            alone.push(viewed(&host, &workspace, view.clone()).await);
        }
        alone.sort_unstable();
        let _alone = alone[alone.len() / 2];

        // Each round, the chat record's read takes long to answer, and the issue's view is asked
        // while it does. Making one record's registered read slow is agent-session's
        // `slowed::Slowed`, a test switch that went with its `WorkspaceActions`; a device's
        // daemon has no production-visible way to hold a worker's read.
        panic!(
            "a device's daemon has no way to make a registered read slow: `slowed::Slowed` went \
             with agent-session's WorkspaceActions, so a view cannot be timed beside one"
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
