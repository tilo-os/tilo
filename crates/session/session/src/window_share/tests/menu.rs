//! A window is shared with its workspace from its own menu. The row the desk offers, chosen as
//! a person chooses it, is one more caller of the shares' own actions: what it says is what the
//! shares' resource reads, and choosing it shares the window by its tab, or stops.

use super::*;
use crate::shell::state::WINDOW_MENU;

/// The row that shares a window with its workspace, by the id the desk's menu gives it.
const SHARE_ROW: &str = "window-share-with-workspace";
/// The row that stops sharing it.
const STOP_ROW: &str = "window-stop-sharing-with-workspace";

/// The rows of the menu of `tab` on `member` 's desk, as the session reads them once the menu is
/// up where a person pressed for it.
fn menu_of(member: &mut Member, tab: u64) -> Vec<Value> {
    member
        .session
        .invoke(
            WINDOW_MENU,
            "open",
            &arguments(serde_json::json!({ "tab": tab.to_string(), "anchor": "200,120" })),
        )
        .expect("the window's menu opens");
    member
        .session
        .read(WINDOW_MENU)
        .expect("the session publishes the window menu")["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// How the row `id` of `menu` reads, when the menu offers it.
fn label_of<'a>(menu: &'a [Value], id: &str) -> Option<&'a str> {
    menu.iter()
        .find(|row| row["id"].as_str() == Some(id))
        .and_then(|row| row["label"].as_str())
}

/// Choose row `id` of the menu that is up on `member` 's desk. Whether the desk took it.
fn choose(member: &mut Member, id: &str) -> bool {
    member
        .session
        .invoke(
            WINDOW_MENU,
            "choose",
            &arguments(serde_json::json!({ "row": id })),
        )
        .is_ok()
}

/// How many windows `member` shares or waits to share, as its shares' resource reads.
fn shared_count(member: &Member) -> usize {
    let reading = shares(&member.session);
    rows(&reading, "sharing").len() + rows(&reading, "starting").len()
}

/// A person shares a window with its workspace from the window's menu, and stops there too. The
/// row asks the shares by the tab alone; the window then stands in the shares' reading and in the
/// workspace's governance, as a share any other caller starts does. The menu says what that
/// reading says: the way back once the window is shared, how many devices watch it while one
/// does, and the share again once it ended. A device that only shows the window offers neither
/// row, and cannot share it.
#[test]
fn a_window_is_shared_with_its_workspace_from_its_menu_and_stopped_there() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x61).as_bytes());
    let mut host = member_minting_under(
        0x61,
        desk(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tilo_desktop_model::ids::compose_id(salt, 1),
        ),
        salt,
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = foreign_tab(&host.session);
    let mut viewer = member(0x62, showing(host.sync.publisher(), APP, tab));
    let shared_in_governance = |host: &Member| {
        host.session.handle().read(|tree| {
            tree.workspaces[0]
                .policy
                .grants
                .iter()
                .any(|grant| is_of_share(grant, &window_share_id(tab)))
        })
    };

    // The window is this device's, in a workspace it takes part in: its menu offers the share.
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, SHARE_ROW),
        Some("Share with workspace"),
        "the menu offers to share the window: {menu:?}"
    );
    assert_eq!(label_of(&menu, STOP_ROW), None, "nothing to stop yet");
    assert_eq!(shared_count(&host), 0, "opening the menu shares nothing");

    assert!(choose(&mut host, SHARE_ROW), "the row is chosen");
    let wanted = tab.to_string();
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .iter()
            .any(|shared| {
                shared["tab"].as_str() == Some(wanted.as_str())
                    && shared["app_id"].as_str() == Some(APP)
                    && shared["room"].as_str() == Some(WORKSPACE)
            })
    });
    assert!(
        shared_in_governance(&host),
        "the share is in the workspace's governance, where its other devices read it"
    );

    // The menu reads the share: it offers the way back, and no second share.
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, STOP_ROW),
        Some("Stop sharing with workspace"),
        "a shared window offers to stop: {menu:?}"
    );
    assert_eq!(label_of(&menu, SHARE_ROW), None);
    assert!(
        !choose(&mut host, SHARE_ROW),
        "a row the menu does not offer is refused"
    );

    // The device that shows the window offers neither row: the window is not its to share.
    let theirs = menu_of(&mut viewer, tab);
    assert!(!theirs.is_empty(), "the tab has a menu there");
    assert_eq!(label_of(&theirs, SHARE_ROW), None, "{theirs:?}");
    assert_eq!(label_of(&theirs, STOP_ROW), None, "{theirs:?}");
    assert!(!choose(&mut viewer, SHARE_ROW));
    settle(&mut host, &mut window, &mut viewer);
    assert_eq!(
        shared_count(&viewer),
        0,
        "the viewing device shares nothing"
    );

    // Someone watches: the row says so, from the same reading.
    watch(&mut viewer, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        rows(&shares(&host.session), "sharing")
            .first()
            .and_then(|shared| shared["viewers"].as_array())
            .is_some_and(|viewers| viewers.len() == 1)
    });
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, STOP_ROW),
        Some("Stop sharing with workspace (1 watching)"),
        "{menu:?}"
    );

    // Stopping from the menu ends the share everywhere it stood.
    assert!(choose(&mut host, STOP_ROW), "the stop is chosen");
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        shared_count(host) == 0
    });
    assert!(
        !shared_in_governance(&host),
        "the workspace no longer names the window as shared"
    );
    let menu = menu_of(&mut host, tab);
    assert_eq!(label_of(&menu, SHARE_ROW), Some("Share with workspace"));
    assert_eq!(label_of(&menu, STOP_ROW), None);
}

/// A share another caller starts and ends is what the menu says next: the desk keeps no flag of
/// its own beside the shares' reading.
#[test]
fn the_menu_follows_a_share_started_and_stopped_elsewhere() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0x63).as_bytes());
    let mut host = member_minting_under(
        0x63,
        desk(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tilo_desktop_model::ids::compose_id(salt, 1),
        ),
        salt,
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = foreign_tab(&host.session);
    let mut viewer = member(0x64, own_desk());
    let by_the_tab = arguments(serde_json::json!({ "tab": tab.to_string() }));

    host.session
        .invoke(SHARES_RESOURCE, "share", &by_the_tab)
        .expect("a script shares the window");
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, STOP_ROW),
        Some("Stop sharing with workspace"),
        "the menu reads a share it did not start: {menu:?}"
    );
    assert_eq!(label_of(&menu, SHARE_ROW), None);

    host.session
        .invoke(SHARES_RESOURCE, "stop", &by_the_tab)
        .expect("a script stops it");
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        shared_count(host) == 0
    });
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, SHARE_ROW),
        Some("Share with workspace"),
        "and one it did not stop: {menu:?}"
    );
    assert_eq!(label_of(&menu, STOP_ROW), None);
}
