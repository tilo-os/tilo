//! Opening by id holds the window's place at once: a resource opens as a
//! new window at the end of the strip it names or as a tab of the window it
//! names, its ids answered before the app is up, the person's focus kept,
//! and the window waits to be opened at the resource. Undo closes what the
//! run opened. The palette's strip actions go through the same functions,
//! each change of the person's its own to take back.

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::state::{
    AppKind, RoomId, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    WorkspaceTree,
};

use super::{Actor, Place, Target};

const MAIL: &str = "so.tilo.mail";

fn chrome() -> (Chrome, RoomId, u64) {
    let mut tree = WorkspaceTree::default();
    let workspace = Workspace::new(WorkspaceId(7), "Hiring");
    let room = workspace.room();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let main = crate::shell::state::StripId(tree.mint_id());
    tree.desktop.workspaces[0]
        .strips
        .push(crate::shell::state::Strip {
            id: main,
            name: "Main".into(),
            window_groups: Vec::new(),
        });
    let (group, tab) = (tree.mint_id(), tree.mint_id());
    tree.desktop.workspaces[0].strips[0]
        .window_groups
        .push(WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: "Notes".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Notes".into()),
                    label: "Notes".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    tree.seed_doc_from_cache();
    (ChromeState::new(tree), room, group)
}

fn agent() -> Actor {
    Actor {
        id: "agent:planner".into(),
        run: Some("tasks.issue:hiring-review".into()),
        is_agent: true,
    }
}

fn publisher(resource: &str) -> Option<String> {
    resource.starts_with("mail.").then(|| MAIL.to_owned())
}

fn windows_of(chrome: &Chrome, strip: StripId) -> Vec<(u64, Vec<u64>)> {
    let state = chrome.borrow();
    let tree = state.tree();
    tree.desktop.workspaces[0]
        .strips
        .iter()
        .find(|candidate| candidate.id == strip)
        .map(|strip| {
            strip
                .window_groups
                .iter()
                .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_resource_opens_in_a_held_place_by_id_and_undo_closes_it() {
    let (chrome, room, notes) = chrome();
    let agent = agent();
    let strip = super::create_strip(
        &mut chrome.borrow_mut(),
        room,
        "Ada Lovelace",
        &agent,
        false,
    )
    .expect("a strip");
    let focus = chrome.borrow().focused_strip_id();

    let message = Target::parse("mail.message:work:INBOX:42").unwrap();
    assert_eq!(
        message,
        Target::Resource("mail.message:work:INBOX:42".into())
    );
    let opened = super::open(
        &mut chrome.borrow_mut(),
        room,
        &message,
        Place::NewWindow { strip: Some(strip) },
        true,
        &agent,
        &publisher,
    )
    .expect("the mail opens in a window of its own");
    assert_eq!(opened.app, MAIL);
    assert_eq!(opened.strip, strip);
    assert_eq!(
        windows_of(&chrome, strip),
        vec![(opened.group, vec![opened.tab])]
    );
    assert_eq!(
        chrome.borrow().focused_strip_id(),
        focus,
        "the person stays where they are"
    );

    let reply = Target::parse("mail.draft:7").unwrap();
    let tab = super::open(
        &mut chrome.borrow_mut(),
        room,
        &reply,
        Place::TabOf(opened.group),
        true,
        &agent,
        &publisher,
    )
    .expect("the draft opens as a tab of the mail's window");
    assert_eq!(tab.group, opened.group);
    assert_eq!(
        windows_of(&chrome, strip),
        vec![(opened.group, vec![opened.tab, tab.tab])]
    );
    let waiting: Vec<_> = chrome
        .borrow_mut()
        .desktop_layout
        .openings_due(1)
        .into_iter()
        .map(|opening| (opening.tab, opening.resource))
        .collect();
    assert_eq!(
        waiting,
        vec![
            (opened.tab, "mail.message:work:INBOX:42".to_owned()),
            (tab.tab, "mail.draft:7".to_owned()),
        ],
        "both wait for their window to open them at their resource"
    );
    let listed = super::list_windows(&chrome.borrow(), room, &|_| None).unwrap();
    assert!(listed.iter().any(|window| window.tab == tab.tab
        && window.resource.as_deref() == Some("mail.draft:7")
        && window.app.as_deref() == Some(MAIL)));
    assert!(listed.iter().any(|window| window.group == notes));

    let (undone, _) = super::undo(
        &mut chrome.borrow_mut(),
        room,
        Some("tasks.issue:hiring-review"),
        &agent,
    )
    .expect("the run is undone");
    assert_eq!(undone.changes, 3, "{undone:?}");
    assert!(undone.kept.is_empty(), "{undone:?}");
    let state = chrome.borrow();
    let tree = state.tree();
    let strips = &tree.desktop.workspaces[0].strips;
    assert_eq!(strips.len(), 1, "the run's strip went with its windows");
    assert_eq!(strips[0].window_groups.len(), 1);
    assert_eq!(strips[0].window_groups[0].id, notes);
}

#[test]
fn a_place_that_is_gone_is_refused_before_anything_opens() {
    let (chrome, room, _) = chrome();
    let agent = agent();
    let mail = Target::parse("mail.message:work:INBOX:1").unwrap();
    for place in [
        Place::TabOf(999),
        Place::Beside(999),
        Place::NewWindow {
            strip: Some(StripId(999)),
        },
    ] {
        assert!(
            super::open(
                &mut chrome.borrow_mut(),
                room,
                &mail,
                place,
                true,
                &agent,
                &publisher
            )
            .is_err(),
            "{place:?}"
        );
    }
    assert!(
        super::undo(&mut chrome.borrow_mut(), room, None, &agent).is_err(),
        "nothing changed, so nothing is undone"
    );
}

#[test]
fn targets_are_told_apart_by_their_address() {
    let hash = "a".repeat(64);
    assert_eq!(
        Target::parse(&format!("tilo.content:{hash}")),
        Ok(Target::Content(hash.clone()))
    );
    assert!(Target::parse("tilo.content:xyz").is_err());
    assert_eq!(
        Target::parse("https://api.github.com/users/ada/repos?sort=updated"),
        Ok(Target::Url(
            "https://api.github.com/users/ada/repos?sort=updated".into()
        ))
    );
    assert!(Target::parse("").is_err());
    assert!(Target::parse("plain words").is_err());
}

#[test]
fn the_palettes_strip_actions_take_the_same_path_and_each_is_the_persons_own() {
    let (chrome, room, _) = chrome();
    let made = chrome.borrow_mut().create_strip("Review").expect("a strip");
    assert_eq!(
        chrome.borrow().focused_strip_id(),
        Some(made),
        "the person goes to the strip they made"
    );
    chrome
        .borrow_mut()
        .rename_strip(made, "Reviews")
        .expect("renamed");
    let person = Actor::person(&chrome.borrow(), room);
    let (renamed, _) = super::undo(&mut chrome.borrow_mut(), room, None, &person).unwrap();
    assert_eq!(renamed.changes, 1, "the rename alone");
    let name = |chrome: &Chrome| {
        chrome.borrow().tree().desktop.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == made)
            .map(|strip| strip.name.clone())
    };
    assert_eq!(name(&chrome).as_deref(), Some("Review"));
    let (created, _) = super::undo(&mut chrome.borrow_mut(), room, None, &person).unwrap();
    assert_eq!(created.changes, 1);
    assert_eq!(name(&chrome), None);
}
