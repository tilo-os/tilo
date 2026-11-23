//! A card gives a reader nothing they may not read. An agent shows its person a document, live,
//! as a card in its reply. Another member of the workspace reads that document while the
//! workspace lets them. Once the workspace denies them reading there, the card still stands and
//! opens nothing to them: what it names is refused them as any read of it is, and its person
//! reads on.
use super::*;
use ui::permissions::policy::{ActionKind, Answer, Grant, Grantee, KindSet, TreePath};

/// Admit `device`'s person to `workspace` as an editor, by an invitation its server issues, as
/// another member joins.
async fn admit(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    device: &crate::support::Device,
) {
    crate::workspace_actions::resource_support::admit_with(
        workspaces,
        workspaces.governor(),
        workspace,
        device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
}

/// An agent shows its person the document it wrote, live. Another member reads that document
/// while the workspace lets them. The workspace then denies that member reading in the word
/// processor: the card stands as it stood, and what it names is refused them, while its person
/// reads on. A card gives nobody a reading the workspace does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_card_opens_nothing_to_a_member_the_workspace_denies_reading() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_browser).await;
    let ada = caller_of(&root, &browser, &workspace);
    let bo = caller_of(&bo_root, &bo_browser, &workspace);

    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Write a poem in the word processor and show me the document.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(served, workspace, writes_then_shows(&asked));
    let shown = cards(&reply(&workspaces, &workspace, &root).await);
    assert_eq!(shown.len(), 1, "one card: {shown:?}");
    let document = shown[0].1.clone().unwrap_or_default();

    // A member of the workspace reads its documents: the card's too.
    let read = read_by(&actions, &workspace, &bo, &document)
        .await
        .expect("a member reads the document while the workspace lets them");
    assert!(
        read.contains("The page is shared, the cursor waits."),
        "{read}"
    );

    // The workspace denies that member reading in the word processor.
    let here: &str = &workspace;
    let deny = Grant::new(
        "bo-reads-no-documents",
        Grantee::person(hex::encode(bo_root.verifying_key().to_bytes())),
        TreePath::workspace(here).app(WRITER),
        KindSet::of(ActionKind::Read),
        Answer::Deny,
        Grantee::person(hex::encode(root.verifying_key().to_bytes())),
    );
    let governor = workspaces.governor().clone();
    workspaces
        .put_workspace_grants(&workspace, &governor, [0x5a; 32], vec![deny])
        .await
        .expect("the workspace takes its governor's grant");
    let deadline = tilo_test_env::hang_deadline();
    let refused = loop {
        match read_by(&actions, &workspace, &bo, &document).await {
            Err(refused) => break refused,
            Ok(read) => assert!(
                Instant::now() < deadline,
                "the member still reads the document the workspace denies them: {read}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(!refused.is_empty(), "they are told why");

    // The card stands as it stood, and gives them nothing: its person reads on.
    let still = cards(&reply(&workspaces, &workspace, &root).await);
    assert_eq!(still, shown, "the card names the same document");
    let again = read_by(&actions, &workspace, &bo, &document)
        .await
        .expect_err("what a card names is refused a member who may not read it");
    assert_eq!(again, refused, "as every read of it is refused them");
    let theirs = read_by(&actions, &workspace, &ada, &document)
        .await
        .expect("its person reads on");
    assert!(
        theirs.contains("The page is shared, the cursor waits."),
        "{theirs}"
    );
    page.peer.close();
}
