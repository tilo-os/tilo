//! Global search before anything is typed, through the overlay host: what
//! Super+Space opens in its first frame, the one control that widens it,
//! typing and clearing, and the same sections for agents and scripts
//! through the search action API.

use std::rc::Rc;

use automerge::AutoCommit;
use tilo_desktop_model::Collaborator;
use ui::widget::Container;
use vello::kurbo::Rect;
use winit::event::Modifiers;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use crate::shell::search::providers::{PeopleProvider, WaitingOnYouProvider};
use crate::shell::state::WorkspaceTree;

/// The demo's Home in front with only Morgan in it, Research behind it with
/// only Priya; the people and waiting providers registered beside the
/// fixture's actions and records.
fn host() -> (ShellOverlayHost, Chrome) {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.search_registry
        .register(Rc::new(PeopleProvider::new()));
    host.search_registry
        .register(Rc::new(WaitingOnYouProvider::new()));
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let active = tree.desktop.active;
        for workspace in &mut tree.desktop.workspaces {
            let (id, name) = if workspace.id == active {
                ("morgan", "Morgan")
            } else {
                ("priya", "Priya")
            };
            workspace.collaborators = vec![Collaborator::new(id, name)];
        }
        tree.desktop.collaborators.clear();
    }
    (host, chrome)
}

fn press(host: &mut ShellOverlayHost, key: &Key, modifiers: ModifiersState) -> bool {
    let mut handled = false;
    host.handle_shell_keyboard_event(
        key,
        Modifiers::from(modifiers),
        false,
        &AutoCommit::new(),
        &automerge::ROOT,
        &mut handled,
    )
}

/// The launcher sections the palette lists, in order, each once.
fn sections(host: &ShellOverlayHost) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in host.palette.visible_entries() {
        if let Some(section) = &entry.section
            && out.last() != Some(section)
        {
            out.push(section.clone());
        }
    }
    out
}

fn titles(host: &ShellOverlayHost) -> Vec<String> {
    host.palette
        .visible_entries()
        .iter()
        .map(|entry| entry.label.clone())
        .collect()
}

#[test]
fn super_space_opens_the_launcher_in_its_first_frame_scoped_to_the_workspace() {
    let (mut host, chrome) = host();
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .expect("the default shortcuts");
    // One key, one refresh: what the first frame after Super+Space paints,
    // from the state already here, with nothing waited for.
    assert!(press(
        &mut host,
        &Key::Named(NamedKey::Space),
        ModifiersState::SUPER
    ));
    assert!(host.palette_visible);
    assert_eq!(chrome.borrow().search_scope, SearchScope::Workspace);
    assert_eq!(sections(&host), ["Happening now"]);
    assert!(
        titles(&host).contains(&"Home".to_owned()),
        "Morgan's workspace: {:?}",
        titles(&host)
    );
    assert!(
        !titles(&host).contains(&"Research".to_owned()),
        "Research is another workspace"
    );
    assert_eq!(
        host.palette.scope_token_action(),
        Some("Search every workspace"),
        "the field's scope token is the control that widens it"
    );

    // Pressed again, global search closes, in either scope.
    assert!(press(
        &mut host,
        &Key::Named(NamedKey::Space),
        ModifiersState::SUPER
    ));
    assert!(!host.palette_visible);
}

#[test]
fn the_scope_token_widens_the_launcher_to_every_workspace() {
    let (mut host, chrome) = host();
    let doc = AutoCommit::new();
    host.open_global_search(&doc, &automerge::ROOT);
    host.refresh_open_palette(&doc, &automerge::ROOT);
    host.force_palette_settled();
    // Backspace in the empty field takes the token away, as a click on it
    // does.
    let (taffy, root) = super::overlay_event_absorb_tests::lay_out_host_with_theme(
        &mut host,
        1600.0,
        900.0,
        &Theme::dark(),
    );
    let backspace = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(NamedKey::Backspace),
        None,
        winit::event::ElementState::Pressed,
    ));
    assert!(
        ui::testing::Dispatch::new()
            .layout(&taffy, root)
            .send(&mut host, &backspace)
            .handled
    );
    host.refresh_open_palette(&doc, &automerge::ROOT);
    assert_eq!(chrome.borrow().search_scope, SearchScope::All);
    assert_eq!(host.palette.scope_token(), None);
    let rows = titles(&host);
    let home = rows.iter().position(|title| title == "Home");
    let research = rows.iter().position(|title| title == "Research");
    assert!(
        home.is_some() && home < research,
        "every workspace with someone in it, the one in front first: {rows:?}"
    );
}

#[test]
fn typing_replaces_the_launcher_and_clearing_brings_it_back() {
    let (mut host, _chrome) = host();
    let doc = AutoCommit::new();
    host.open_global_search(&doc, &automerge::ROOT);
    let launcher = titles(&host);
    assert!(launcher.contains(&"Home".to_owned()));

    host.palette.set_query("theme");
    host.refresh_search_results(&doc, &automerge::ROOT);
    assert!(
        !sections(&host).contains(&"Happening now".to_owned()),
        "typing turns the launcher into results: {:?}",
        sections(&host)
    );
    assert!(!titles(&host).is_empty(), "results for what was typed");

    host.palette.set_query("");
    host.refresh_search_results(&doc, &automerge::ROOT);
    assert_eq!(titles(&host), launcher, "clearing brings the launcher back");
}

#[test]
fn every_launcher_row_has_its_reason_as_its_description() {
    let (mut host, chrome) = host();
    let mut message = tilo_desktop::notifications::Notification::new("Alex", "The pitch?")
        .with_from("Alex", "alex");
    message.source_workspace = Some(chrome.borrow().tree().desktop.active.0);
    chrome.borrow_mut().push_notification_full(message);
    let doc = AutoCommit::new();
    host.open_global_search(&doc, &automerge::ROOT);
    assert_eq!(sections(&host), ["Happening now", "Waiting on you"]);
    for entry in host.palette.visible_entries() {
        assert!(
            entry
                .description
                .as_deref()
                .is_some_and(|line| !line.is_empty())
                || !entry.description_facts.is_empty(),
            "{:?} says nothing about why it is there",
            entry.label
        );
    }
    let home = host
        .palette
        .visible_entries()
        .into_iter()
        .find(|entry| entry.label == "Home")
        .expect("Home's row");
    assert_eq!(home.faces.len(), 1, "Morgan's face on the workspace's row");
    assert_eq!(home.faces[0].name, "Morgan");
    assert_eq!(
        home.outcome.as_deref(),
        Some("Open"),
        "what choosing it does"
    );
    let alex = host
        .palette
        .visible_entries()
        .into_iter()
        .find(|entry| entry.label == "Alex")
        .expect("Alex's message");
    assert_eq!(alex.outcome.as_deref(), Some("Open"));
}

#[test]
fn the_launcher_reads_through_the_search_action_api() {
    let (host, chrome) = host();
    let doc = AutoCommit::new();
    let listing = |every_workspace: bool| {
        let state = chrome.borrow();
        host.launcher_listing(
            &crate::shell::search::QueryArgs {
                needle: "",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            },
            every_workspace,
            &crate::shell::search::action_api::AllVisible,
        )
    };
    let here = listing(false);
    assert_eq!(here["scope"], "workspace");
    let sections = here["sections"].as_array().expect("sections");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["id"], "happening-now");
    assert_eq!(sections[0]["name"], "Happening now");
    let rows = sections[0]["groups"][0]["rows"].as_array().expect("rows");
    let home = rows
        .iter()
        .find(|row| row["title"] == "Home")
        .expect("Home's row");
    assert_eq!(home["provider"], "people");
    assert_eq!(home["reason"], serde_json::json!(["Morgan is here"]));
    assert_eq!(home["people"], serde_json::json!(["Morgan"]));

    let everywhere = listing(true);
    assert_eq!(everywhere["scope"], "all");
    let titles: Vec<&str> = everywhere["sections"][0]["groups"][0]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| row["title"].as_str())
        .collect();
    assert!(titles.contains(&"Research"), "{titles:?}");
}

/// Continue is where the windows were, as their apps declared it to the
/// window bar: a window that names its note shows up, newest first, and a
/// window that declares no location does not.
#[test]
fn continue_is_where_the_windows_were_as_their_bars_said() {
    use ui::window_toolbar::{ToolbarLocation, WindowToolbar};
    let (mut host, chrome) = host();
    host.search_registry.register(Rc::new(
        crate::shell::search::providers::ContinueProvider::without_broker(std::sync::Arc::new(
            crate::shell::search::app_content_index::AppContentIndex::in_memory(),
        )),
    ));
    let tabs: Vec<u64> = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.desktop
            .active_workspace()
            .expect("Home")
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect()
    };
    let at = |label: &str, anchor: &str| WindowToolbar {
        location: Some(ToolbarLocation {
            label: label.into(),
            anchor: Some(anchor.into()),
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    };
    host.declare_tab_toolbar_for_tests(tabs[0], Some(at("Pitch deck", "pages/pitch")));
    host.declare_tab_toolbar_for_tests(tabs[1], Some(WindowToolbar::default()));
    let doc = AutoCommit::new();
    host.open_global_search(&doc, &automerge::ROOT);
    assert_eq!(sections(&host), ["Happening now", "Continue"]);
    let continuing: Vec<String> = host
        .palette
        .visible_entries()
        .iter()
        .filter(|entry| entry.section.as_deref() == Some("Continue"))
        .map(|entry| entry.label.clone())
        .collect();
    assert_eq!(
        continuing,
        ["Pitch deck"],
        "only the window that named where it is"
    );
}

#[test]
fn all_apps_follows_the_listed_apps_and_lists_every_app() {
    use crate::shell::search::launcher::LauncherSection;
    use ui::widget::command_palette::CommandEntry;
    let in_section = |id: &str, label: &str, section: LauncherSection| {
        let mut entry = CommandEntry::new(id, label).group(section.label());
        entry.section = Some(section.label());
        entry
    };
    let all = palette_search::all_apps_entry(&[
        in_section("result:0", "Pitch", LauncherSection::Continue),
        in_section("result:1", "Notes", LauncherSection::Apps),
    ])
    .expect("All apps under the apps the launcher lists");
    assert_eq!(all.label, "All apps");
    assert_eq!(
        all.section.as_deref(),
        Some(LauncherSection::Apps.label().as_str())
    );
    assert!(
        palette_search::all_apps_entry(&[in_section(
            "result:0",
            "Pitch",
            LauncherSection::Continue
        )])
        .is_none(),
        "no apps listed, no way to every app"
    );

    let (mut host, _chrome) = host();
    let mut doc = AutoCommit::new();
    host.open_global_search(&doc, &automerge::ROOT);
    host.palette.set_entries(vec![all]);
    host.palette.set_cursor(0);
    host.activate_selected_result(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    assert_eq!(
        host.active_kind_filter,
        [crate::shell::search::ResourceKind::Application],
        "every app, as the Apps chip lists them"
    );
    assert!(host.palette_visible, "still in search, now listing apps");
}
