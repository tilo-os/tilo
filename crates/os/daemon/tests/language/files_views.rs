//! An agent reads the person's Files window: the folders the workspace's tree holds, and an empty
//! workspace as the empty folder the person sees — never a skeleton of a tree on its way. The
//! window's own document says which folder it shows; the tree is a document of its own, which the
//! window opens beside it.

use std::time::Instant;

use serde_json::{Value, json};
use tilo_action_services::formats;

use crate::language::files_support::{Session, kept_when, person_calls, person_files, said};
use crate::language::host::{self, Host};

/// A folder the person keeps in the workspace.
const FOLDER: &str = "Launch plan";

/// The lines the Files window of the device's workspace shows an agent, asked as the person's own
/// agent asks it.
async fn view(session: &Session<'_>, workspace: &str) -> String {
    let (lines, failed) = said(
        &session
            .tool(workspace, "tilo_view", &json!({"path": "file-explorer"}))
            .await,
    );
    assert!(!failed, "{lines}");
    lines
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
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let lines = view(&session, &workspace).await;
            assert!(
                !lines.contains("Loading"),
                "the window shows the tree, never that it is on its way: {lines}",
            );
            if lines.contains(FOLDER) {
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

        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let lines = loop {
            let lines = view(&session, &workspace).await;
            if lines.contains(FOLDER) {
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
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let lines = view(session, workspace).await;
        if lines.contains(FOLDER) && lines.contains(wanted) {
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
