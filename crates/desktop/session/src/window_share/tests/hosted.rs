//! What a shell says of a window that runs on another device while nothing of it is shown there:
//! the session reads how the window stands, its tab says it instead of saying nothing, and the
//! shares' reading says the same in a word (`hosted`).

use super::*;

/// How the reading of `member` says the window its shell lists in `tab` stands, with what it says
/// the window waits for.
fn stands(member: &Member, tab: u64) -> Option<(String, String)> {
    let wanted = tab.to_string();
    rows(&shares(&member.session), "hosted")
        .iter()
        .find(|hosted| hosted["tab"].as_str() == Some(wanted.as_str()))
        .map(|hosted| {
            let said = |field: &str| hosted[field].as_str().unwrap_or_default().to_owned();
            (said("state"), said("waiting_for"))
        })
}

/// Whether `member` 's shell says `words` : what a reader who cannot see the shell, and a tool
/// that lists it, read there.
fn shell_says(member: &Member, words: &str) -> bool {
    member
        .session
        .accessible_labels()
        .iter()
        .any(|label| label == words)
}

/// Pass until the reading of `viewer` says the window in `tab` stands as `wanted` . The device
/// `seed` goes on saying who it is meanwhile where `heard_as` names it, as a device in a
/// workspace does: one that fell silent is forgotten there.
#[track_caller]
fn until_stands(
    host: &mut Member,
    window: &mut Option<TestClient>,
    viewer: &mut Member,
    (tab, wanted): (u64, &str),
    heard_as: Option<(u8, &str)>,
) {
    until(host, window, viewer, |_, viewer, _| {
        if let Some((seed, label)) = heard_as {
            heard(viewer, seed, label);
        }
        stands(viewer, tab).is_some_and(|(state, _)| state == wanted)
    });
}

/// A window that runs only on its device stands on every other shell, shared or not. Its tab there
/// never stands empty: it says the app and the device the window runs on, and the session reads
/// why nothing of it is shown. Nothing is heard of its device: it is away. Its device is in the
/// workspace and shares no such window: it is not shared. Its device shares it: the share's own
/// tile stands in the tab. The share ends: it is not shared again, and the tab says so again.
#[test]
fn a_window_on_another_device_is_said_not_shared_until_it_is_and_again_once_its_share_ends() {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0xA1).as_bytes());
    let mut host = member_minting_under(
        0xA1,
        shell(
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
    let tab = tab_of(&host.session, APP);
    // The other shell lists the window as the workspace's layout does: as one that runs on its
    // device, before anything is shared.
    let mut viewer = member_minting_under(0xA2, showing(device(0xA1), APP, tab), 1);
    let where_it_runs = "Notes runs on Framework";
    let named = Some((0xA1, "Framework"));

    // Nothing was heard of the window's device in the workspace.
    until_stands(&mut host, &mut window, &mut viewer, (tab, "away"), None);
    assert!(
        shell_says(&viewer, where_it_runs),
        "the tab says where the window runs: {:?}",
        viewer.session.accessible_labels()
    );

    // Its device is in the workspace, and shares no such window with it.
    until_stands(
        &mut host,
        &mut window,
        &mut viewer,
        (tab, "not-shared"),
        named,
    );
    assert!(shell_says(&viewer, where_it_runs));
    assert!(viewer.session.handle().peer_surface_tabs().is_empty());

    // Its device shares it: the share's own tile takes the tab.
    share(&mut host, APP, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !rows(&shares(&host.session), "sharing").is_empty()
    });
    carry_governance(&host.session, &viewer.session);
    until_stands(&mut host, &mut window, &mut viewer, (tab, "shown"), named);
    assert_eq!(viewer.session.handle().peer_surface_tabs(), vec![tab]);
    assert!(
        !shell_says(&viewer, where_it_runs),
        "a tab that shows the window's share does not say it is elsewhere: {:?}",
        viewer.session.accessible_labels()
    );

    // The share ends: the tab is not left empty, and says the window is not shared again.
    host.session
        .invoke(
            SHARES_RESOURCE,
            "stop",
            &arguments(serde_json::json!({ "tab": tab.to_string() })),
        )
        .expect("the share is stopped");
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        !host.session.handle().read(|tree| {
            tree.workspaces[0]
                .policy
                .grants
                .iter()
                .any(|grant| is_of_share(grant, &window_share_id(tab)))
        })
    });
    carry_governance(&host.session, &viewer.session);
    until_stands(
        &mut host,
        &mut window,
        &mut viewer,
        (tab, "not-shared"),
        named,
    );
    assert!(viewer.session.handle().peer_surface_tabs().is_empty());
    assert!(
        shell_says(&viewer, where_it_runs),
        "the tab says where the window runs once its share ended: {:?}",
        viewer.session.accessible_labels()
    );
}

/// A window this device asks its device for, of which nothing has arrived, is awaited: its tab
/// says it waits, and never that the window is not shared.
#[test]
fn a_window_this_device_asks_for_is_awaited_until_something_of_it_arrives() {
    let host = device(0xB1);
    let salt = tilo_desktop_model::ids::device_salt_of_key(host.as_bytes());
    let tab = tilo_desktop_model::ids::compose_id(salt, 1);
    let mut sharer = member_minting_under(0xB1, own_shell(), salt);
    let mut viewer = member_minting_under(0xB2, showing(host, APP, tab), 1);
    let mut window = None;
    let named = Some((0xB1, "Framework"));
    until_stands(
        &mut sharer,
        &mut window,
        &mut viewer,
        (tab, "not-shared"),
        named,
    );

    watch(&mut viewer, tab);
    until_stands(
        &mut sharer,
        &mut window,
        &mut viewer,
        (tab, "awaited"),
        named,
    );
    assert!(shell_says(&viewer, "Notes runs on Framework"));
    assert!(
        viewer.session.handle().peer_surface_tabs().is_empty(),
        "nothing of the window arrived"
    );
    assert_eq!(
        viewer.session.handle().elsewhere(tab),
        crate::window_share::elsewhere::Elsewhere::Awaited {
            waiting_for: stands(&viewer, tab)
                .map(|(_, waits)| waits)
                .unwrap_or_default()
        },
        "the tab is told what the reading says the window waits for"
    );
}
