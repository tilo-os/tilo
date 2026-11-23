//! A share gives a reader nothing they may not read. An agent shows its person a document,
//! live, with a card that offers to work in it. Another member of the workspace reads that
//! document while the workspace lets them. Once the workspace denies them reading there, the
//! share still stands live and opens nothing to them: what it names is refused them as any
//! read of it is, and its person reads on.
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
        workspaces.device_cert(),
        workspace,
        device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
}

/// What the word processor's document `document` answers `caller` who reads it, through the
/// server's action service: its text, or the words it is refused with.
async fn read_by(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &tilo_actions::remote::Caller,
    document: &str,
) -> Result<String, String> {
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(actions.clone(), id);
    let route = DocumentRoute {
        app_id: WRITER.into(),
        cluster_id: workspace.into(),
        document: document.into(),
    };
    tilo_workspace_service::agents::invoke_named(
        &service,
        caller,
        &route,
        "word-processor.document",
        "read-text",
        serde_json::Map::new(),
    )
    .await
    .map(|read| read.to_string())
}

/// An agent shows its person the document it wrote, live. Another member reads that document
/// while the workspace lets them. The workspace then denies that member reading in the word
/// processor: the share stands as it stood, live, and what it names is refused them, while
/// its person reads on. A share gives nobody a reading the workspace does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_share_opens_nothing_to_a_member_the_workspace_denies_reading() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_browser).await;
    let caller = |root: &SigningKey, device: &crate::support::Device| {
        tilo_actions::remote::Caller::authenticated(
            root.verifying_key().to_bytes(),
            device.cert.device_public_key,
        )
        .in_workspace(&workspace)
    };
    let ada = caller(&root, &browser);
    let bo = caller(&bo_root, &bo_browser);

    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Write a poem in the word processor and show me the document.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(served, workspace, writes_then_shows(&asked));
    let part = conversation_part(&workspaces, &workspace).await;
    let shared = shares(&part);
    assert_eq!(shared.len(), 1, "one share: {shared:?}");
    let document = shared[0].at[DOCUMENT]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // A member of the workspace reads its documents: the share's too.
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
    let governor = workspaces.device_cert().clone();
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

    // The share stands as it stood, and gives them nothing: its person reads on.
    let part = conversation_part(&workspaces, &workspace).await;
    let still = shares(&part);
    assert_eq!(still.len(), 1, "{still:?}");
    assert!(
        still[0].share.open && !still[0].share.ended,
        "live: {still:?}"
    );
    assert_eq!(format!("{:?}", still[0].share.access), "Edit");
    assert_eq!(
        still[0].at[DOCUMENT].as_str(),
        Some(document.as_str()),
        "and names the same document"
    );
    let again = read_by(&actions, &workspace, &bo, &document)
        .await
        .expect_err("what a live share names is refused a member who may not read it");
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
