//! An agent reads the person's Files window: the folders the workspace's tree holds, and an
//! empty workspace as the empty folder the person sees — never a skeleton of a tree on its
//! way. The window's own document says which folder it shows; the tree is a document of its
//! own, which the window opens beside it.
use super::agent_mcp::tool;
use super::*;
use serde_json::json;
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

/// A folder the person keeps in the workspace.
const FOLDER: &str = "Launch plan";

/// The app whose window an agent reads.
const FILES: &str = fs_protocol::tree::FILES_APP;

/// The lines the Files window of `server`'s workspace shows an agent, asked as the person's
/// own agent asks it.
async fn view(server: &McpServer<SessionActions>) -> String {
    let (lines, failed) = tool(server, "tilo_view", json!({"path": "file-explorer"})).await;
    assert!(!failed, "{lines}");
    lines
}

/// The person's Files founds the workspace's tree with the folder `name` in it, and publishes
/// it as their page does: the tree's own document and the chunk the folder went into.
fn person_keeps_a_folder(page: &Browser, name: &str, author: &str) {
    person_keeps_a_folder_from(page, name, (1_000, author));
}

/// [`person_keeps_a_folder`], the folder last changed at `changed_ms` since the Unix epoch.
fn person_keeps_a_folder_from(page: &Browser, name: &str, (changed_ms, author): (u64, &str)) {
    let owners = ui::resource::ResourceOwners::portable(
        ui::portable_storage::PortableStorage::new("files-window-view", "page-device")
            .expect("a page keeps its documents"),
    );
    let resources = ui::resource::workspace::WorkspaceResources::in_workspace(
        owners,
        WORKSPACE.into(),
        MemberId::person("ada"),
    );
    let open = |document: &str| {
        ui::framework::WorkspaceResource::<fs_protocol::tree::FilesTree>::open_at(
            &resources,
            FILES,
            document,
            (FILES, "Files"),
            None,
            None,
        )
        .expect("the window opens the tree's resources")
    };
    let chunk = open(&fs_protocol::tree::chunk_document_id(1));
    let index = open(&fs_protocol::tree::document_id());
    let mut tree = fs_protocol::tree::Tree::default();
    let mut chunk_doc = chunk.read(Clone::clone).expect("the chunk reads");
    fs_protocol::tree::place(
        (&mut chunk_doc, 1),
        &mut tree,
        fs_protocol::tree::ROOT_ID,
        name,
        fs_protocol::tree::Placing::Folder,
        (changed_ms, author),
    )
    .expect("the folder is placed");
    chunk
        .record()
        .merge_peer_document(chunk_doc.save())
        .expect("the chunk takes the folder");
    let mut index_doc = index
        .read(Clone::clone)
        .expect("the tree's own document reads");
    fs_protocol::tree::list_chunk(&mut index_doc, 1).expect("the chunk is listed");
    index
        .record()
        .merge_peer_document(index_doc.save())
        .expect("the tree takes the chunk");
    for (document, bytes) in [
        (
            fs_protocol::tree::document_id(),
            index
                .read(Clone::clone)
                .expect("the tree's own document reads")
                .save(),
        ),
        (
            fs_protocol::tree::chunk_document_id(1),
            chunk.read(Clone::clone).expect("the chunk reads").save(),
        ),
    ] {
        page.peer
            .write(
                DocumentRoute {
                    app_id: FILES.into(),
                    cluster_id: WORKSPACE.into(),
                    document,
                },
                &collab_types::DocumentMutation::Snapshot(bytes),
            )
            .expect("the person's page publishes it");
    }
}

/// A workspace whose person's page holds Files, and the agent of theirs on another computer,
/// served over that computer's session.
async fn agent_viewing_files(
    state: &std::path::Path,
) -> (
    crate::harness::hosted::Hosted,
    String,
    McpServer<SessionActions>,
    Browser,
) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let hosted = crate::harness::hosted::start(
        &root,
        &home,
        state,
        Options::offering(crate::common::catalogue),
    )
    .await;
    // tilo.sh made her root here, so it founds her own workspace, where her region is kept.
    tilo_server::servers::found(
        hosted
            .server
            .workspaces()
            .expect("the host holds workspaces"),
    )
    .await
    .unwrap();
    let mut page = Browser::open(&hosted, &browser_device, &home).await;
    let _files = page.create(FILES);
    page.put_layout();
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();
    let agent = Browser::open(&hosted, &computer, &home).await;
    // Its session is the agent's, as `tilo login` makes it say.
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_agent(
        SessionActions::new(&agent.peer, agent.workspace, home.cert.device_public_key),
        WORKSPACE.into(),
        "claude-code",
        &person,
    );
    (hosted, person, server, page)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_of_files_is_the_empty_folder_a_person_with_no_files_sees() {
    let state = crate::support::scratch_dir();
    let (_hosted, _person, server, _page) = agent_viewing_files(state.path()).await;
    let empty = view(&server).await;
    assert!(!empty.contains("Loading"), "{empty}");
    assert!(
        empty.contains("This folder is empty"),
        "an empty workspace is an empty folder, not a tree on its way: {empty}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_of_files_lists_the_folder_the_persons_files_hold() {
    let state = crate::support::scratch_dir();
    let (_hosted, person, server, page) = agent_viewing_files(state.path()).await;
    person_keeps_a_folder(&page, FOLDER, &person);
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();
    // The workspace holds the folder: what the window shows is the tree, not a skeleton.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let lines = view(&server).await;
        assert!(
            !lines.contains("Loading"),
            "the window shows the tree, never that it is on its way: {lines}"
        );
        if lines.contains(FOLDER) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{lines}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The person's page tells the workspace's server `action` of their formats with `argument`,
/// as the page does once it is in the workspace: what the server answers.
async fn page_tells(
    page: &Browser,
    server: [u8; 32],
    person: &str,
    action: &str,
    argument: serde_json::Value,
) -> serde_json::Value {
    use tilo_actions::remote::{ReceiptStatus, UpdateMark};
    let api = SessionActions::new(&page.peer, page.workspace, server);
    let caller =
        tilo_actions::remote::Caller::local_person(person.to_owned()).in_workspace(WORKSPACE);
    let serde_json::Value::Object(argument) = argument else {
        panic!("an argument is an object");
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target: tilo_actions::remote::formats::target(action),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let mut receipt = tilo_test_env::within("the server answers", api.invoke(&caller, request))
        .await
        .expect("the page's session answers")
        .expect("the server takes it");
    let until = tilo_test_env::hang_deadline();
    let mut seen = UpdateMark::Accepted;
    while matches!(receipt.status, ReceiptStatus::Accepted) {
        let state = tilo_actions::remote::settle_with(&api, &caller, invocation, Some(seen), until)
            .await
            .flatten()
            .expect("the server settles it");
        seen = UpdateMark::of(&state.update);
        receipt = state.receipt();
    }
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("{action}: {receipt:?}");
    };
    output.value.unwrap_or_default()
}

/// An agent reads a file's time in the Files window the server renders for it as its person
/// reads it on their page: on their clock and in their browser's zone, not the server's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_of_files_reads_times_on_the_persons_clock_in_their_zone() {
    const ZONE: &str = "Pacific/Auckland";
    let Ok(auckland) = jiff::tz::TimeZone::get(ZONE) else {
        // A system without a zone database renders every zone as UTC.
        return;
    };
    let state = crate::support::scratch_dir();
    let (hosted, person, server, page) = agent_viewing_files(state.path()).await;
    // Her page, in a browser in Auckland, tells the server the clock she chose in Region &
    // Formats and where her browser is.
    page_tells(
        &page,
        hosted.device,
        &person,
        tilo_actions::remote::formats::SET,
        json!({"formats": {"time_format": "H24"}}),
    )
    .await;
    page_tells(
        &page,
        hosted.device,
        &person,
        tilo_actions::remote::formats::BROWSER_ZONE,
        json!({"zone": ZONE}),
    )
    .await;
    // The folder last changed at 14:05 on 15 March 2026, UTC.
    let changed = jiff::Timestamp::from_second(1_773_583_500).unwrap();
    person_keeps_a_folder_from(
        &page,
        FOLDER,
        (u64::try_from(changed.as_millisecond()).unwrap(), &person),
    );
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();

    // What her page shows for it: the time on a 24-hour clock, in Auckland.
    let hers = ui::format::region::RegionFormats {
        time: ui::clock::TimeStyle::H24,
        ..ui::format::region::RegionFormats::default()
    };
    let on_her_page =
        ui::format::Formats::with(hers).timestamp(changed.to_zoned(auckland).datetime());
    let on_the_server = ui::format::Formats::with(hers)
        .timestamp(changed.to_zoned(jiff::tz::TimeZone::UTC).datetime());
    assert_ne!(on_her_page, on_the_server, "the zones read it apart");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let lines = loop {
        let lines = view(&server).await;
        if lines.contains(FOLDER) {
            break lines;
        }
        assert!(std::time::Instant::now() < deadline, "{lines}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(
        lines.contains(&on_her_page),
        "the agent reads {on_her_page:?}, as her page shows it: {lines}"
    );
    assert!(
        !lines.contains(&on_the_server),
        "never the server's {on_the_server:?}: {lines}"
    );
}

/// What the agent's view of Files shows once it lists the folder with `wanted` in it.
async fn agent_reads(server: &McpServer<SessionActions>, wanted: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let lines = view(server).await;
        if lines.contains(FOLDER) && lines.contains(wanted) {
            return lines;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent never reads {wanted:?}: {lines}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// What Files on the person's page shows for `changed` once the page read what the server keeps
/// of them: their formats, and the zone they chose, else the browser's own.
fn on_the_page(kept: &tilo_actions::remote::formats::Formats, changed: jiff::Timestamp) -> String {
    let formats =
        ui::format::region::RegionFormats::from_section(kept.formats.clone().unwrap_or_default());
    ui::format::region::install_here(
        Some(formats),
        ui::format::region::chosen_zone(kept.chosen_zone.as_deref()),
    );
    let shown = file_explorer::i18n::format_datetime(changed);
    ui::format::region::install_here(None, None);
    shown
}

/// A zone the person chooses in Region & Formats on their page is the one both their page and
/// their agent read times in; back on Automatic, both read the browser's again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zone_the_person_chooses_is_the_one_their_page_and_their_agent_read() {
    use tilo_actions::remote::formats::{BROWSER_ZONE, CHOOSE_ZONE, Formats, READ, SET};
    const BROWSER: &str = "Pacific/Auckland";
    const CHOSEN: &str = "Europe/Vienna";
    let (Ok(auckland), Ok(vienna)) = (
        jiff::tz::TimeZone::get(BROWSER),
        jiff::tz::TimeZone::get(CHOSEN),
    ) else {
        // A system without a zone database renders every zone as UTC.
        return;
    };
    let state = crate::support::scratch_dir();
    let (hosted, person, server, page) = agent_viewing_files(state.path()).await;
    let (on, device, who) = (&page, hosted.device, person.as_str());
    let tell = move |action: &'static str, argument: serde_json::Value| {
        page_tells(on, device, who, action, argument)
    };
    tell(SET, json!({"formats": {"time_format": "H24"}})).await;
    tell(BROWSER_ZONE, json!({"zone": BROWSER})).await;
    // The folder last changed at 14:05 on 15 March 2026, UTC.
    let changed = jiff::Timestamp::from_second(1_773_583_500).unwrap();
    person_keeps_a_folder_from(
        &page,
        FOLDER,
        (u64::try_from(changed.as_millisecond()).unwrap(), &person),
    );
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();
    let hers = ui::format::region::RegionFormats {
        time: ui::clock::TimeStyle::H24,
        ..ui::format::region::RegionFormats::default()
    };
    let read_in = |zone: &jiff::tz::TimeZone| {
        ui::format::Formats::with(hers).timestamp(changed.to_zoned(zone.clone()).datetime())
    };
    let (in_auckland, in_vienna) = (read_in(&auckland), read_in(&vienna));
    assert_ne!(in_auckland, in_vienna, "the zones read it apart");

    // She chooses Vienna, and her page reads what the server keeps, as it does on choosing.
    tell(CHOOSE_ZONE, json!({"zone": CHOSEN})).await;
    let kept: Formats = serde_json::from_value(tell(READ, json!({})).await).unwrap();
    assert_eq!(kept.chosen_zone.as_deref(), Some(CHOSEN));
    let hers_shows = on_the_page(&kept, changed);
    assert_eq!(hers_shows, in_vienna, "her page reads Vienna's clock");
    let lines = agent_reads(&server, &hers_shows).await;
    assert!(
        !lines.contains(&in_auckland),
        "the agent reads Vienna's {in_vienna:?}, not the browser's {in_auckland:?}: {lines}"
    );

    // Back to Automatic: the page reads the browser's own zone again, and so does the agent.
    tell(CHOOSE_ZONE, json!({"zone": null})).await;
    let kept: Formats = serde_json::from_value(tell(READ, json!({})).await).unwrap();
    assert_eq!(kept.chosen_zone, None);
    assert!(
        ui::format::region::chosen_zone(kept.chosen_zone.as_deref()).is_none(),
        "the page reads its browser's zone, {BROWSER}, again"
    );
    let lines = agent_reads(&server, &in_auckland).await;
    assert!(
        !lines.contains(&in_vienna),
        "the agent reads the browser's {in_auckland:?} again, not {in_vienna:?}: {lines}"
    );
}
