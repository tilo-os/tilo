//! An agent reads the person's Files window: the folders the workspace's tree holds, and an empty
//! workspace as the empty folder the person sees — never a skeleton of a tree on its way. The
//! window's own document says which folder it shows; the tree is a document of its own, which the
//! window opens beside it.

use std::time::Instant;

use serde_json::{Value, json};
use tilo_action_services::formats;

use crate::language::files_support::{
    Session, folder_at, kept_when, person_calls, person_files, said, tree_if_kept,
};
use crate::language::host::{self, Host};

/// A folder the person keeps in the workspace.
const FOLDER: &str = "Launch plan";

/// The lines the Files window of the device's workspace shows an agent, asked as the person's own
/// agent asks it.
async fn view(session: &Session<'_>, workspace: &str) -> String {
    // A window still opening answers that its read waits, or shows that it is starting: it is
    // read again, as an agent does, while a device under load takes its time to start the app.
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (lines, failed) = said(
            &session
                .tool(workspace, "tilo_view", &json!({"path": "file-explorer"}))
                .await,
        );
        let starting = lines.contains("Files is starting");
        if !failed && !starting {
            return lines;
        }
        assert!(
            failed && lines.contains("not finished yet") || starting,
            "{lines}",
        );
        assert!(Instant::now() < deadline, "the window never reads: {lines}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The person's own dispatch tells the workspace `action` of their formats with `argument`, as
/// their page does once it is in the workspace: what the workspace answers.
async fn page_tells(host: &Host, action: &str, argument: Value) -> Value {
    person_calls(host, formats::target(action), argument, None)
        .await
        .unwrap_or_else(|error| panic!("the person's formats action answers: {error}"))
}

/// The person keeps the folder `FOLDER`, and the time it was last changed.
async fn person_keeps_a_folder(host: &Host) -> u64 {
    let workspace = host.workspace().to_owned();
    person_files(
        host,
        &workspace,
        "new-folder",
        json!({"parent": "", "name": FOLDER}),
    )
    .await
    .expect("the person's folder is made");
    let tree = kept_when(host, &workspace, |tree| {
        crate::language::files_support::folder_at(tree, "", FOLDER)
    })
    .await;
    crate::language::files_support::folder_node(&tree, "", FOLDER)
        .expect("the folder is in the tree")
        .mtime
}

/// An empty workspace is the empty folder a person with no files sees, not a tree on its way.
#[test]
fn an_agents_view_of_files_is_the_empty_folder_a_person_with_no_files_sees() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);
        let empty = view(&session, &workspace).await;
        assert!(!empty.contains("Loading"), "{empty}");
        assert!(
            empty.contains("This folder is empty"),
            "an empty workspace is an empty folder, not a tree on its way: {empty}",
        );
    });
}

/// The agent's view of Files lists the folder the person's Files hold, never a skeleton.
#[test]
fn an_agents_view_of_files_lists_the_folder_the_persons_files_hold() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);
        person_keeps_a_folder(&host).await;
        // The workspace holds the folder: what the window shows is the tree, not a skeleton.
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let lines = view(&session, &workspace).await;
            assert!(
                !lines.contains("Loading"),
                "the window shows the tree, never that it is on its way: {lines}",
            );
            if lines.contains(&format!("list_row \"{FOLDER}\"")) {
                break;
            }
            assert!(Instant::now() < deadline, "{lines}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
}

/// A time `mtime` as `zone` reads it on the person's 24-hour clock.
fn read_in_zone(mtime: u64, zone: tilo_ui_base::format::TimeZone) -> String {
    read_with(
        tilo_ui_base::format::RegionFormats {
            time: tilo_ui_base::clock::TimeStyle::H24,
            ..tilo_ui_base::format::RegionFormats::default()
        },
        mtime,
        zone,
    )
}

/// The time `mtime` as `zone` reads it with `region`.
fn read_with(
    region: tilo_ui_base::format::RegionFormats,
    mtime: u64,
    zone: tilo_ui_base::format::TimeZone,
) -> String {
    let at = tilo_ui_base::format::Timestamp::from_millisecond(i64::try_from(mtime).unwrap())
        .expect("a time")
        .to_zoned(zone)
        .datetime();
    tilo_ui_base::format::Formats::with(region).timestamp(at)
}

/// An agent reads a file's time in the Files window as its person reads it on their page: on their
/// clock and in their browser's zone, not the device's.
#[test]
fn an_agents_view_of_files_reads_times_on_the_persons_clock_in_their_zone() {
    const ZONE: &str = "Pacific/Auckland";
    let Ok(auckland) = tilo_ui_base::format::TimeZone::get(ZONE) else {
        // A system without a zone database renders every zone as UTC.
        return;
    };
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);
        // The person's page tells the workspace the clock they chose in Region & Formats and
        // where their browser is.
        page_tells(
            &host,
            formats::SET,
            json!({"formats": {"time_format": "H24"}}),
        )
        .await;
        page_tells(&host, formats::BROWSER_ZONE, json!({"zone": ZONE})).await;
        let mtime = person_keeps_a_folder(&host).await;

        let on_her_page = read_in_zone(mtime, auckland.clone());
        let on_the_device = read_in_zone(mtime, tilo_ui_base::format::TimeZone::UTC);
        assert_ne!(on_her_page, on_the_device, "the zones read it apart");

        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let lines = loop {
            let lines = view(&session, &workspace).await;
            if lines.contains(&format!("list_row \"{FOLDER}\"")) {
                break lines;
            }
            assert!(Instant::now() < deadline, "{lines}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert!(
            lines.contains(&on_her_page),
            "the agent reads {on_her_page:?}, as her page shows it: {lines}",
        );
        assert!(
            !lines.contains(&on_the_device),
            "never the device's {on_the_device:?}: {lines}",
        );
    });
}

/// What the agent's view of Files shows once it lists the folder with `wanted` in it.
async fn agent_reads(session: &Session<'_>, workspace: &str, wanted: &str) -> String {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let lines = view(session, workspace).await;
        if lines.contains(&format!("list_row \"{FOLDER}\"")) && lines.contains(wanted) {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never reads {wanted:?}: {lines}",
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The time on the person's page for `kept` formats and `mtime`, as their page reads what the
/// workspace keeps of them: the zone they chose, else their browser's.
fn on_the_page(kept: &formats::Formats, mtime: u64, browser: &str) -> String {
    let region =
        tilo_ui_base::format::RegionFormats::from_section(kept.formats.clone().unwrap_or_default());
    let zone = tilo_ui_base::format::TimeZone::get(kept.chosen_zone.as_deref().unwrap_or(browser))
        .unwrap_or(tilo_ui_base::format::TimeZone::UTC);
    read_with(region, mtime, zone)
}

/// A zone the person chooses in Region & Formats on their page is the one both their page and
/// their agent read times in; back on Automatic, both read the browser's again.
#[test]
fn a_zone_the_person_chooses_is_the_one_their_page_and_their_agent_read() {
    use formats::{BROWSER_ZONE, CHOOSE_ZONE, READ, SET};
    const BROWSER: &str = "Pacific/Auckland";
    const CHOSEN: &str = "Europe/Vienna";
    let (Ok(auckland), Ok(vienna)) = (
        tilo_ui_base::format::TimeZone::get(BROWSER),
        tilo_ui_base::format::TimeZone::get(CHOSEN),
    ) else {
        // A system without a zone database renders every zone as UTC.
        return;
    };
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);
        page_tells(&host, SET, json!({"formats": {"time_format": "H24"}})).await;
        page_tells(&host, BROWSER_ZONE, json!({"zone": BROWSER})).await;
        let mtime = person_keeps_a_folder(&host).await;
        let (in_auckland, in_vienna) = (
            read_in_zone(mtime, auckland.clone()),
            read_in_zone(mtime, vienna.clone()),
        );
        assert_ne!(in_auckland, in_vienna, "the zones read it apart");

        // She chooses Vienna, and her page reads what the workspace keeps, as it does on choosing.
        page_tells(&host, CHOOSE_ZONE, json!({"zone": CHOSEN})).await;
        let kept: formats::Formats =
            serde_json::from_value(page_tells(&host, READ, json!({})).await).unwrap();
        assert_eq!(kept.chosen_zone.as_deref(), Some(CHOSEN));
        let hers_shows = on_the_page(&kept, mtime, BROWSER);
        assert_eq!(hers_shows, in_vienna, "her page reads Vienna's clock");
        let lines = agent_reads(&session, &workspace, &hers_shows).await;
        assert!(
            !lines.contains(&in_auckland),
            "the agent reads Vienna's {in_vienna:?}, not the browser's {in_auckland:?}: {lines}",
        );

        // Back to Automatic: the page reads the browser's own zone again, and so does the agent.
        page_tells(&host, CHOOSE_ZONE, json!({"zone": null})).await;
        let kept: formats::Formats =
            serde_json::from_value(page_tells(&host, READ, json!({})).await).unwrap();
        assert_eq!(kept.chosen_zone, None);
        assert!(
            tilo_ui_base::format::region::chosen_zone(kept.chosen_zone.as_deref()).is_none(),
            "the page reads its browser's zone, {BROWSER}, again",
        );
        let lines = agent_reads(&session, &workspace, &in_auckland).await;
        assert!(
            !lines.contains(&in_vienna),
            "the agent reads the browser's {in_auckland:?} again, not {in_vienna:?}: {lines}",
        );
    });
}

/// The window goes to `place`, as a press of a place does.
async fn goes_to(session: &Session<'_>, workspace: &str, place: &str) {
    runs(session, workspace, "files#go", json!({"place": place})).await;
}

/// The window runs `action` with `args`, as a press of the control that fronts it does: a window
/// still taking its time answers that its call waits, and is asked again.
async fn runs(session: &Session<'_>, workspace: &str, action: &str, args: Value) {
    let (said, failed) = tried(session, workspace, action, args.clone()).await;
    assert!(!failed, "the window runs {action} with {args}: {said}");
}

/// [`runs`] as the window answered it: its words and whether it refused.
async fn tried(
    session: &Session<'_>,
    workspace: &str,
    action: &str,
    args: Value,
) -> (String, bool) {
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let (said, failed) = said(
            &session
                .tool(workspace, "tilo_do", &json!({"path": action, "args": args}))
                .await,
        );
        if !failed || !said.contains("not finished yet") {
            return (said, failed);
        }
        assert!(
            Instant::now() < deadline,
            "the window never runs {action} with {args}: {said}",
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The lines the window shows at `place` once it has got there: what `ready` says of them. A
/// press of a place is the agent's call, and the window draws it when its next frame reads it.
async fn at_place(
    session: &Session<'_>,
    workspace: &str,
    place: &str,
    ready: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        goes_to(session, workspace, place).await;
        let lines = view(session, workspace).await;
        if ready(&lines) {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "the window never shows {place:?}: {lines}",
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// A folder on this device is shown only once the person picks it: nothing of the machine's own
/// folders is read before, and the folder the person picked reads with nothing beside it.
#[test]
fn a_folder_on_this_device_is_shown_only_once_the_person_picks_it() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);
        // The workspace's own folder is where Files opens: what the workspace's tree holds, and
        // no device path of this machine's.
        let opened = view(&session, &workspace).await;
        assert!(
            opened.contains("This folder is empty") && !opened.contains("Couldn’t open"),
            "Files opens on the workspace's own folder: {opened}",
        );
        // The person's own files, and this machine's, are reached by no standing grant.
        for place in ["~", "/etc"] {
            let lines = at_place(&session, &workspace, place, |lines| {
                lines.contains("Couldn’t open")
            })
            .await;
            assert!(
                lines.contains("Couldn’t open"),
                "{place} is refused, and the window says so: {lines}",
            );
        }
        // Back again, over the places the window came from, to the workspace's own folder.
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let lines = view(&session, &workspace).await;
            if !lines.contains("Couldn’t open") {
                assert!(
                    lines.contains("This folder is empty"),
                    "the workspace's own folder reads again: {lines}",
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the window never goes back to the workspace's own folder: {lines}",
            );
            // A press of Back the window has nowhere to go on has done all it can: the window
            // is read again either way.
            let _ = tried(&session, &workspace, "files#back", json!({})).await;
        }

        // The person picks a folder, and its files are shown; the folder beside it was never
        // chosen and stays out of reach.
        let picked = tempfile::tempdir().expect("a folder to pick");
        std::fs::write(picked.path().join("in-the-pick.txt"), b"kept").expect("a file to pick");
        let beside = tempfile::tempdir().expect("a folder beside it");
        std::fs::write(beside.path().join("outside-the-pick.txt"), b"no")
            .expect("a file beside it");
        let reference = host
            .daemon()
            .keep_chosen_folder(&workspace, "so.tilo.file-explorer", picked.path(), false)
            .expect("the person's pick is kept");
        // The choice is kept in the device's records and read by the daemon as it changes: the
        // window is sent to it again until the folder it names lists.
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let lines = loop {
            goes_to(&session, &workspace, &reference.to_string_lossy()).await;
            let lines = view(&session, &workspace).await;
            if lines.contains("in-the-pick.txt") {
                break lines;
            }
            assert!(
                Instant::now() < deadline,
                "the picked folder never lists: {lines}",
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert!(!lines.contains("outside-the-pick"), "{lines}");

        let lines = at_place(
            &session,
            &workspace,
            &beside.path().to_string_lossy(),
            |lines| lines.contains("Couldn’t open"),
        )
        .await;
        assert!(
            lines.contains("Couldn’t open"),
            "a folder the person never picked is refused: {lines}",
        );
        assert!(!lines.contains("outside-the-pick"), "{lines}");
    });
}

/// A member the workspace does not let read its files sees nothing of them: the folder the person
/// keeps is not on its window, and the window says why.
#[test]
fn a_member_the_workspace_lets_read_no_files_sees_nothing() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        person_keeps_a_folder(&host).await;
        // The person runs a turn as an agent of theirs, and the workspace refuses the agent
        // Files' tree: what their own answer for an agent says.
        let agent = host.turn_agent("viewer", "Files reader");
        let owner = host.person().to_owned();
        let tree = collab_types::document_node(
            &workspace,
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        );
        let refused = tilo_action_policy::Grant::new(
            "the-agent-reads-no-files",
            tilo_action_policy::Grantee::agent("viewer", &owner),
            tree,
            tilo_action_types::KindSet::READ,
            tilo_action_policy::Answer::Deny,
            tilo_action_policy::Grantee::person(&owner),
        );
        tilo_workspace_service::workspaces::fixtures::answer_grant(
            host.daemon().workspaces(),
            &workspace,
            refused,
        )
        .await
        .expect("the person refuses their agent Files");
        // The window itself is the agent's to open: its own session is what the app keeps while
        // it shows anything, and what the person refused is the files.
        let window = tilo_action_policy::Grant::new(
            "the-agent-keeps-its-window",
            tilo_action_policy::Grantee::agent("viewer", &owner),
            tilo_action_policy::TreePath::workspace(&workspace).app(fs_protocol::tree::FILES_APP),
            tilo_action_types::KindSet::READ_UNDOABLE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(&owner),
        );
        tilo_workspace_service::workspaces::fixtures::answer_grant(
            host.daemon().workspaces(),
            &workspace,
            window,
        )
        .await
        .expect("the person lets their agent open the window");

        let session = Session::Agent(&agent);
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let lines = view(&session, &workspace).await;
            assert!(
                !lines.contains(FOLDER),
                "the agent reads none of the workspace's files: {lines}",
            );
            if lines.contains("Couldn’t open") || lines.contains("This folder is empty") {
                break;
            }
            assert!(Instant::now() < deadline, "{lines}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
}

/// What the workspace's tree keeps once it reads as `ready` says, after a press of the window:
/// a press the window refused says so in its notice, and fails here in its words.
async fn tree_keeps(
    host: &Host,
    session: &Session<'_>,
    workspace: &str,
    ready: impl Fn(&fs_protocol::tree::Tree) -> bool,
) {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(tree) = tree_if_kept(host, workspace).await
            && ready(&tree)
        {
            return;
        }
        let lines = view(session, workspace).await;
        assert!(!lines.contains("Couldn’t"), "the window refused: {lines}");
        assert!(
            Instant::now() < deadline,
            "the tree never keeps the press: {lines}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// In the workspace's own folder, Files' presses change the workspace's tree: a folder is made,
/// renamed, has another dragged into it, is copied and pasted, and goes to the trash, and the
/// tree keeps each change.
#[test]
fn files_presses_change_the_workspaces_own_folder() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        // The workspace has Files installed, as a person's has, and the person presses its
        // window: each press changes the tree as them.
        host.install_app("so.tilo.file-explorer").await;
        let key = host.key("Desktop");
        let session = Session::Key(&key);
        let opened = view(&session, &workspace).await;
        assert!(opened.contains("This folder is empty"), "{opened}");

        runs(
            &session,
            &workspace,
            "files#new_folder",
            json!({"name": "Drafts"}),
        )
        .await;
        tree_keeps(&host, &session, &workspace, |tree| {
            folder_at(tree, "", "Drafts")
        })
        .await;

        let renamed = json!({"path": "Drafts", "name": "Plans"});
        runs(&session, &workspace, "files#rename_entry", renamed).await;
        tree_keeps(&host, &session, &workspace, |tree| {
            folder_at(tree, "", "Plans") && !folder_at(tree, "", "Drafts")
        })
        .await;

        runs(
            &session,
            &workspace,
            "files#new_folder",
            json!({"name": "Notes"}),
        )
        .await;
        let dropped = json!({"paths": ["Notes"], "folder": "Plans"});
        runs(&session, &workspace, "files#drop_into", dropped).await;
        tree_keeps(&host, &session, &workspace, |tree| {
            folder_at(tree, "Plans", "Notes") && !folder_at(tree, "", "Notes")
        })
        .await;

        let copied = json!({"paths": ["Plans"], "cut": false});
        runs(&session, &workspace, "files#carry", copied).await;
        runs(&session, &workspace, "files#paste", json!({})).await;
        tree_keeps(&host, &session, &workspace, |tree| {
            folder_at(tree, "Plans (2)", "Notes") && folder_at(tree, "Plans", "Notes")
        })
        .await;

        let trashed = json!({"paths": ["Plans (2)"]});
        runs(&session, &workspace, "files#trash_entries", trashed).await;
        tree_keeps(&host, &session, &workspace, |tree| {
            !folder_at(tree, "", "Plans (2)") && folder_at(tree, "Plans", "Notes")
        })
        .await;
    });
}

/// A press of the window is decided as whoever pressed it, never as Files: an agent the
/// workspace does not let change its files presses New folder, the window says why, and the
/// tree holds no such folder.
#[test]
fn a_press_by_a_member_who_may_change_no_files_is_refused_and_changes_nothing() {
    host::run_with(&["files"], |host| async move {
        let workspace = host.workspace().to_owned();
        host.install_app("so.tilo.file-explorer").await;
        let agent = host.turn_agent("viewer", "Files reader");
        let owner = host.person().to_owned();
        let tree = collab_types::document_node(
            &workspace,
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        );
        let refused = tilo_action_policy::Grant::new(
            "the-agent-changes-no-files",
            tilo_action_policy::Grantee::agent("viewer", &owner),
            tree,
            tilo_action_types::KindSet::UNDOABLE,
            tilo_action_policy::Answer::Deny,
            tilo_action_policy::Grantee::person(&owner),
        );
        tilo_workspace_service::workspaces::fixtures::answer_grant(
            host.daemon().workspaces(),
            &workspace,
            refused,
        )
        .await
        .expect("the person refuses their agent changes to the files");
        let window = tilo_action_policy::Grant::new(
            "the-agent-keeps-its-window",
            tilo_action_policy::Grantee::agent("viewer", &owner),
            tilo_action_policy::TreePath::workspace(&workspace).app(fs_protocol::tree::FILES_APP),
            tilo_action_types::KindSet::READ_UNDOABLE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(&owner),
        );
        tilo_workspace_service::workspaces::fixtures::answer_grant(
            host.daemon().workspaces(),
            &workspace,
            window,
        )
        .await
        .expect("the person lets their agent open the window");

        let session = Session::Agent(&agent);
        view(&session, &workspace).await;
        runs(
            &session,
            &workspace,
            "files#new_folder",
            json!({"name": "Drafts"}),
        )
        .await;
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let lines = view(&session, &workspace).await;
            if lines.contains("Couldn’t make the folder") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the window never says why: {lines}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let kept = tree_if_kept(&host, &workspace).await;
        assert!(
            kept.is_none_or(|tree| !folder_at(&tree, "", "Drafts")),
            "a refused press changes nothing of the tree",
        );
    });
}
