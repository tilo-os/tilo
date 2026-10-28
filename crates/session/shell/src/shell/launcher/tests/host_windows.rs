//! Host windows of unconfined launches: what closing them stops, how a
//! Wine session's windows are told apart, and who hears the running set
//! change.

use super::*;

#[cfg(target_os = "linux")]
fn host_window_tree(tabs: &[(u64, &'static str, u64)]) -> crate::shell::state::WorkspaceTree {
    use crate::shell::state::{Tab, WindowGroup, WindowWidth};
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
        id: 100,
        tabs: tabs
            .iter()
            .map(|(id, app_id, surface)| Tab {
                id: *id,
                title: (*app_id).into(),
                tile: crate::shell::state::TileContent::Foreign {
                    app_id: (*app_id).into(),
                    surface_id: *surface,
                },
                app_title: None,
            })
            .collect(),
        active_tab: tabs[0].0,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    tree.seed_doc_from_cache();
    tree
}

#[cfg(target_os = "linux")]
fn process_is_live(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(") ") else {
        return false;
    };
    // A killed child we have not `wait`ed is a zombie: it has already
    // exited. A07's cmdline scan skips empty zombie cmdlines.
    !rest.starts_with('Z')
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn terminate_unconfined_launches_stops_the_launcher_and_its_children() {
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "wine-winefile", None, false);
    assert_eq!(
        verified_package_for_descendant(standin.pid()).as_deref(),
        Some("wine-winefile"),
        "the stand-in must be in the launch table"
    );
    terminate_unconfined_launches("wine-winefile");
    tilo_test_env::wait_until(
        "the unconfined host launch and its child exit after terminate",
        || !process_is_live(standin.pid()) && !process_is_live(standin.descendant()),
    );
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn terminate_unconfined_launches_leaves_a_confined_launch_alone() {
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "so.tilo.terminal", None, true);
    terminate_unconfined_launches("so.tilo.terminal");
    assert!(
        process_is_live(standin.pid()),
        "a sandboxed launch is asked to close, not signalled"
    );
    unregister_launched_process(standin.pid());
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn closing_the_last_host_window_stops_the_unconfined_launch() {
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "wine-winefile", None, false);
    remember_launch_named("wine-winefile", &["wine-winefile".to_string()], "Wine File");
    let chrome = crate::shell::chrome_state::ChromeState::new(host_window_tree(&[(
        1,
        "wine-winefile",
        501,
    )]));
    crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), 1)
        .expect("close the last Wine File window");
    tilo_test_env::wait_until(
        "Wine File's host process tree exits after its last window closes",
        || !process_is_live(standin.pid()) && !process_is_live(standin.descendant()),
    );
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn closing_a_wineboot_window_stops_the_winefile_launch() {
    // A07's VM close is group.close. Wine File's tab announces
    // wineboot.exe, not the desktop id the launch table uses.
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "wine-winefile", None, false);
    remember_launch_named("wine-winefile", &["wine-winefile".to_string()], "Wine File");
    let chrome =
        crate::shell::chrome_state::ChromeState::new(host_window_tree(&[(1, "wineboot.exe", 501)]));
    crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), 1)
        .expect("close the last Wine File window");
    tilo_test_env::wait_until(
        "Wine File's host process tree exits after its wineboot.exe window closes",
        || !process_is_live(standin.pid()) && !process_is_live(standin.descendant()),
    );
}

#[test]
fn an_auto_restart_helper_restarts_until_it_dies_too_quickly() {
    let command = held("orca", &["run-host"]).command;
    assert!(take_helper_restart("orca", Instant::now()).is_none());
    note_restartable_session_helper("orca", "Screen Reader", command.clone());
    let later = Instant::now() + HELPER_RESTART_LIMIT;
    assert_eq!(
        take_helper_restart("orca", later),
        Some(("Screen Reader".to_string(), command))
    );
    // Dead again within the limit of that restart: given up for good.
    assert!(take_helper_restart("orca", later + Duration::from_secs(1)).is_none());
    assert!(take_helper_restart("orca", later + HELPER_RESTART_LIMIT * 2).is_none());
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn an_opened_wine_window_is_not_a_crash_and_helpers_are_not_toasted() {
    let _ = drain_events();
    clear_pending_placements();
    clear_launched_for_test();

    let wine = LauncherStandIn::spawn();
    let geoclue = LauncherStandIn::spawn();
    let applet = LauncherStandIn::spawn();
    register_launched_process(wine.pid(), "wine-winefile", None, false);
    register_launched_process(geoclue.pid(), "geoclue-demo-agent", None, false);
    register_launched_process(applet.pid(), "nm-applet", None, false);
    set_pending_placement(
        "wine-winefile",
        crate::shell::tab_binding::Placement::NewGroup,
    );
    remember_launch_named("wine-winefile", &["wine-winefile".to_string()], "Wine File");
    note_session_helper("geoclue-demo-agent");
    note_session_helper("nm-applet");
    // The compositor records the toplevel the client actually announced.
    record_host_window_open(501, "wineboot.exe");
    record_host_window_open(502, "control.exe");
    assert_eq!(
        launch_key_for_surface(501).as_deref(),
        Some("wine-winefile"),
        "wineboot.exe is the window Wine File opened"
    );
    assert_eq!(
        launch_key_for_surface(502).as_deref(),
        Some("wine-winefile")
    );

    let chrome = crate::shell::chrome_state::ChromeState::new(host_window_tree(&[
        (1, "wineboot.exe", 501),
        (2, "control.exe", 502),
    ]));
    // group.close of the Wine File group. Wine Mono's window stays.
    crate::shell::window_actions::close_tab_unrecorded(&mut chrome.borrow_mut(), 1)
        .expect("close Wine File's wineboot.exe window");
    assert!(
        process_is_live(wine.pid())
            && process_is_live(geoclue.pid())
            && process_is_live(applet.pid()),
        "a second X11 window keeps Wine File, and helpers stay up"
    );
    crate::shell::window_actions::close_tab_unrecorded(&mut chrome.borrow_mut(), 2)
        .expect("close the last X11 window");
    tilo_test_env::wait_until("Wine File exits after its last window closes", || {
        !process_is_live(wine.pid()) && !process_is_live(wine.descendant())
    });
    assert!(
        process_is_live(geoclue.pid())
            && process_is_live(geoclue.descendant())
            && process_is_live(applet.pid())
            && process_is_live(applet.descendant()),
        "closing Wine File must not signal Geoclue or the NetworkManager applet"
    );

    let crashed = "The app crashed before opening. Open Applications and try again.";
    let event = |package_id: &str, display_name: &str| LaunchEvent {
        package_id: package_id.to_string(),
        display_name: display_name.to_string(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: LaunchEventKind::Failed {
            code: None,
            stderr_tail: String::new(),
        },
    };
    for (id, name) in [
        ("wine-winefile", "Wine File"),
        ("geoclue-demo-agent", "Geoclue Demo agent"),
        ("nm-applet", "NetworkManager Applet"),
    ] {
        let notice = event(id, name)
            .failure_notice()
            .expect("a signal exit is the crash notice");
        assert_eq!(notice.body, crashed, "{name} is classified as a crash");
        push_event(event(id, name));
    }
    let mut host = crate::shell::overlay_host::ShellOverlayHost::with_fixture_content(Box::new(
        ui::widget::Container::new(),
    ))
    .with_chrome(chrome.clone());
    host.enable_persisted_launch_drain_for_test();
    let _ = host.drain_launch_events();
    drop(host);
    let posted: Vec<(String, String)> = chrome
        .borrow()
        .notifications()
        .iter()
        .map(|note| (note.title.clone(), note.body.clone()))
        .collect();
    assert!(
        posted
            .iter()
            .all(|(title, body)| body != crashed && !title.starts_with("Couldn't open")),
        "an opened Wine File window and session helpers must not toast a crash: {posted:?}"
    );

    unregister_launched_process(wine.pid());
    unregister_launched_process(geoclue.pid());
    unregister_launched_process(applet.pid());
    clear_pending_placements();
    clear_launched_for_test();
}

#[cfg(target_os = "linux")]
fn wine_session_with_a_second_x11_window() -> crate::shell::state::WorkspaceTree {
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    let mut groups = Vec::new();
    for i in 1..=20u64 {
        groups.push(WindowGroup {
            id: i,
            tabs: vec![Tab {
                id: 1_000_u64.saturating_add(i),
                title: format!("Notes {i}"),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: format!("Notes {i}"),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: 1_000_u64.saturating_add(i),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    for (group_id, tab_id, title, app_id, surface) in [
        (9_001, 9_101, "Wine File", "wineboot.exe", 501),
        (9_002, 9_102, "Wine Mono Installer", "control.exe", 502),
    ] {
        groups.push(WindowGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: title.into(),
                tile: TileContent::Foreign {
                    app_id: app_id.into(),
                    surface_id: surface,
                },
                app_title: None,
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    let strip = tree.desktop.workspaces[0].strips[0].id;
    tree.desktop.workspaces[0].strips[0].window_groups = groups;
    // Focus a Tilo window, not either X11 group. A07's installer took
    // focus; the original Wine File group is the one closed first.
    tree.desktop.set_active_group(strip, 1);
    tree.seed_doc_from_cache();
    tree
}

#[cfg(target_os = "linux")]
fn group_close_is_visible(
    chrome: &crate::shell::chrome_state::Chrome,
    registry: &crate::shell::search::SearchRegistry,
    doc: &automerge::AutoCommit,
    id: &str,
) -> bool {
    use crate::shell::chrome_state::SearchScope;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use automerge::ROOT;
    let state = chrome.borrow();
    ActionCatalog::build_for_resource(
        registry,
        &ResourceRef::new("desktop-windows", id),
        SearchScope::All,
        None,
        &state,
        doc,
        &ROOT,
        &AllVisible,
    )
    .select(&ResourceRef::new("desktop-windows", id))
    .is_some()
}

#[cfg(target_os = "linux")]
fn invoke_group_close(
    chrome: &crate::shell::chrome_state::Chrome,
    registry: &crate::shell::search::SearchRegistry,
    doc: &mut automerge::AutoCommit,
    id: &str,
) {
    use crate::shell::chrome_state::SearchScope;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use automerge::ROOT;
    let catalog = {
        let state = chrome.borrow();
        ActionCatalog::build_for_resource(
            registry,
            &ResourceRef::new("desktop-windows", id),
            SearchScope::All,
            None,
            &state,
            doc,
            &ROOT,
            &AllVisible,
        )
    };
    catalog
        .select(&ResourceRef::new("desktop-windows", id))
        .unwrap_or_else(|| panic!("{id} must stay a visible action"))
        .invoke(&mut ui::widget::Edits::lend(doc), &ROOT, chrome, [])
        .unwrap_or_else(|err| panic!("{id} failed: {err}"));
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn a_second_x11_window_stays_group_close_addressable() {
    use std::rc::Rc;

    use automerge::{AutoCommit, ROOT};

    use crate::shell::search::providers::DesktopWindowsProvider;
    use crate::shell::search::{SearchProvider, SearchRegistry};

    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "wine-winefile", None, false);
    remember_launch_named("wine-winefile", &["wine-winefile".to_string()], "Wine File");
    let chrome =
        crate::shell::chrome_state::ChromeState::new(wine_session_with_a_second_x11_window());
    let mut doc = AutoCommit::new();
    let mut registry = SearchRegistry::new();
    registry.register(Rc::new(DesktopWindowsProvider::new()));
    let wine = "group.close:9001";
    let mono = "group.close:9002";
    assert!(
        group_close_is_visible(&chrome, &registry, &doc, wine),
        "exact group.close resolves the unfocused Wine File group"
    );
    assert!(
        group_close_is_visible(&chrome, &registry, &doc, mono),
        "exact group.close resolves the other X11 group past the palette page"
    );
    {
        let state = chrome.borrow();
        let page = DesktopWindowsProvider::new().query_page(
            crate::shell::search::SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[],
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            },
            None,
            crate::shell::search::SearchBudget::PALETTE,
        );
        assert!(
            !page
                .results
                .iter()
                .any(|row| row.id == mono || row.id == wine),
            "the empty palette page truncates both X11 closes: {:?}",
            page.results.iter().map(|row| &row.id).collect::<Vec<_>>()
        );
    }
    invoke_group_close(&chrome, &registry, &mut doc, wine);
    assert!(
        process_is_live(standin.pid()) && process_is_live(standin.descendant()),
        "Wine Mono's window is still open, so the wine-winefile launch stays"
    );
    assert!(
        group_close_is_visible(&chrome, &registry, &doc, mono),
        "group.close of the remaining X11 group stays in the catalogue"
    );
    invoke_group_close(&chrome, &registry, &mut doc, mono);
    tilo_test_env::wait_until("the launch exits after its last X11 window closes", || {
        !process_is_live(standin.pid()) && !process_is_live(standin.descendant())
    });
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn closing_one_of_two_host_windows_leaves_the_launch() {
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "wine-winefile", None, false);
    remember_launch_named("wine-winefile", &["wine-winefile".to_string()], "Wine File");
    let chrome = crate::shell::chrome_state::ChromeState::new(host_window_tree(&[
        (1, "wine-winefile", 501),
        (2, "wine-winefile", 502),
    ]));
    crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), 1)
        .expect("close one of two windows");
    assert!(
        process_is_live(standin.pid()),
        "a second window of the same host app keeps the launch"
    );
    unregister_launched_process(standin.pid());
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn running_set_changes_wake_observers_on_launch_and_exit() {
    // The background portal's monitor waits on exactly this receiver.
    let mut changes = subscribe_running_changes();
    changes.borrow_and_update();
    let standin = LauncherStandIn::spawn();
    register_launched_process(standin.pid(), "org.example.Running", None, true);
    let launched = changes.has_changed().unwrap();
    let running = verified_running_packages().contains(&"org.example.Running".to_string());
    changes.borrow_and_update();
    unregister_launched_process(standin.pid());
    let exited = changes.has_changed().unwrap();
    let gone = !verified_running_packages().contains(&"org.example.Running".to_string());
    assert!(launched, "a launch must wake running-set observers");
    assert!(running);
    assert!(exited, "an exit must wake running-set observers");
    assert!(gone);
}
