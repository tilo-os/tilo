//! A member writes a note on the workspace's Notes page, served by the
//! server's device for their browser: what they typed is the workspace's, the
//! note they open is the one shown, and the page opened again shows both.

use super::super::*;
use super::Membership;
use std::time::Instant;
use tilo_actions::remote::Caller;
use tilo_server_protocol::page::{OpenedPage, PageAction, PageAddress, PagePatch, PageRefusal};
use ui::permissions::Role;

/// The number of the first element whose opening tag carries all of `marks`.
fn element(html: &str, marks: &[&str]) -> Option<u32> {
    html.split('<').find_map(|tag| {
        let open = tag.split('>').next()?;
        if !marks.iter().all(|mark| open.contains(mark)) {
            return None;
        }
        open.split("data-p=\"")
            .nth(1)?
            .split('"')
            .next()?
            .parse()
            .ok()
    })
}

/// The targets of the rows that open a note, in the order the page lists them.
fn note_rows(html: &str) -> Vec<String> {
    html.split('<')
        .filter_map(|tag| {
            let open = tag.split('>').next()?;
            if !open.contains("data-a=\"activate\"") {
                return None;
            }
            let target = open.split("data-t=\"").nth(1)?.split('"').next()?;
            target.starts_with("note-").then(|| target.to_owned())
        })
        .collect()
}

/// What the open note's title field holds.
fn title_field(html: &str) -> String {
    html.split("<textarea")
        .nth(1)
        .and_then(|rest| rest.split_once('>'))
        .and_then(|(_, rest)| rest.split_once("</textarea>"))
        .map(|(text, _)| text.to_owned())
        .unwrap_or_default()
}

fn opened(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    member: &Caller,
    show: Option<&str>,
) -> OpenedPage {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let answer = actions
            .open_page(
                workspace,
                member,
                PageAddress {
                    workspace: WORKSPACE.into(),
                    app: "notes".into(),
                    show: show.map(str::to_owned),
                },
            )
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the page is answered");
        match answer {
            Ok(page) => return page,
            Err(PageRefusal::Unavailable(why)) if Instant::now() < deadline => {
                println!("not yet: {why}");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(refusal) => panic!("the page was refused: {refusal:?}"),
        }
    }
}

fn acted(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    page: u64,
    act: (u32, Option<String>, PageAction),
) -> Vec<PagePatch> {
    let started = Instant::now();
    let patches = actions
        .act_on_page(workspace, page, Some(act))
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the act is answered")
        .expect("the act is taken");
    println!("acted in {:?}", started.elapsed());
    patches
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_writes_notes_on_the_workspaces_page_and_finds_them_again() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let device = enrol(&root, "home-server", DeviceCapabilities::member());
    let page_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = start(&root, &device, state.path()).await;
    let page = Browser::open(&server, &page_device, &device).await;
    let membership = Membership::of(
        &server,
        &device,
        &guest_device,
        state.path().join("member-content"),
    );
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    super::super::resource_support::admit_person(
        &server,
        &device,
        WORKSPACE,
        &guest_device,
        tilo_peer_state::join::Role::Editor,
    )
    .await;
    membership.becomes(Role::Editor).await;
    let workspace = page.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);

    // Nobody opened Notes in this workspace yet: the page founds its notebook.
    let started = Instant::now();
    let notes = opened(&actions, workspace, &member, None);
    println!("opened in {:?}", started.elapsed());
    assert_eq!(notes.title, "Notes");

    let mut body = notes.body.clone();
    for title in ["Groceries", "Garden plan"] {
        let new_note = element(&body, &["data-a=\"activate\"", "aria-label=\"New note\""])
            .unwrap_or_else(|| panic!("the page offers a new note: {body}"));
        let patches = acted(
            &actions,
            workspace,
            notes.page,
            (new_note, None, PageAction::Activate),
        );
        let field = patches
            .iter()
            .find_map(|patch| element(&patch.html, &["data-a=\"value\""]))
            .unwrap_or_else(|| panic!("the new note's title is a field: {patches:?}"));
        // The browser focuses the title the view asked it to, then sends the
        // field's whole text after each key.
        acted(
            &actions,
            workspace,
            notes.page,
            (field, None, PageAction::Focus),
        );
        let mut patches = Vec::new();
        for typed in (1..=title.len()).map(|end| &title[..end]) {
            patches.extend(acted(
                &actions,
                workspace,
                notes.page,
                (field, None, PageAction::Value(typed.into())),
            ));
        }
        assert!(
            patches.iter().any(|patch| patch.html.contains(title)),
            "the note's row says what was typed: {patches:?}"
        );
        // The page as it stands now, for the next new note.
        body = opened(&actions, workspace, &member, None).body;
    }
    let rows = note_rows(&body);
    assert_eq!(rows.len(), 2, "two notes are listed: {body}");
    assert!(
        body.contains("Groceries") && body.contains("Garden plan"),
        "the page opened again lists both notes by their titles: {body}"
    );

    // Each note opened by its own address shows its own title.
    let mut titles: Vec<String> = rows
        .iter()
        .map(|target| title_field(&opened(&actions, workspace, &member, Some(target)).body))
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        ["Garden plan", "Groceries"],
        "each note's address shows that note"
    );

    // What each more open page holds: its app worker and its replica.
    let before = workers_rss_kib();
    let more: Vec<u64> = (0..4)
        .map(|_| opened(&actions, workspace, &member, None).page)
        .collect();
    println!(
        "four more pages hold {} KiB more of workers' memory",
        workers_rss_kib().saturating_sub(before)
    );
    for page in more {
        actions.close_page(workspace, page);
    }
}

/// The resident memory of this process's children, the app workers among them.
fn workers_rss_kib() -> u64 {
    let me = std::process::id().to_string();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let parent = stat.rsplit_once(") ")?.1.split(' ').nth(1)?;
            (parent == me).then_some(())?;
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))?
                .trim()
                .trim_end_matches(" kB")
                .parse::<u64>()
                .ok()
        })
        .sum()
}
