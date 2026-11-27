//! The quick settings on a device that refused: a search for networks that failed says why and
//! offers to look again, a network whose password it would not take says so on that network's row
//! and stays open for the next password, the menu at a saved network's row offers to forget it, and
//! a paired accessory opens its own page with what the device knows of it, the devices it can move
//! to, and the ways to rename it, disconnect it or forget it.
//!
//! The scenario stages the readings the connectivity service publishes when the device refuses;
//! what the views call is recorded as the app's own actions, so a press is held to the action it
//! ran rather than to the words it drew.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::UiNode;
use tilolang_runtime::delegate::Invocation;

use crate::shell_staged::StagedWindow;

/// The scenario staged in every test here.
const SCENARIO: &str = "the quick settings, refused";

/// A laptop's desktop with the quick settings open on the scenario's readings.
fn staged() -> StagedWindow {
    let mut theme = tilo_ui_base::Theme::light();
    theme.reduce_motion = true;
    // Tall enough that every section of the panel stands where a person can
    // press it: the shell draws the panel under the bar, and a laptop's own
    // height would put the last sections below it.
    let shell = StagedWindow::staged(SCENARIO, (1280, 1600), theme);
    // A view that failed draws "This couldn't be shown" and the reason under
    // it: the test reads the reason, so a failing panel says why rather than
    // only drawing a hole.
    let failed = shell
        .instance
        .host()
        .expect("the window runs a host")
        .runtime()
        .take_view_failures();
    assert!(failed.is_empty(), "a view failed: {failed:?}");
    let tree: UiNode = shell.instance.root_tree().expect("the window's tree");
    if let Some(reason) = tree
        .of_kind("ui.Text")
        .into_iter()
        .find(|node| node.key.contains("/reason"))
    {
        panic!("a view failed: {:?}", reason.props);
    }
    shell
}

/// Every word the window shows, as a screen reader reads them.
fn said(shell: &StagedWindow) -> Vec<String> {
    tilo_ui_base::a11y::texts(shell.harness.root_widget())
}

/// Whether any word the window shows holds `words`.
fn shows(shell: &StagedWindow, words: &str) -> bool {
    said(shell).iter().any(|text| text.contains(words))
}

/// Press the field `name` so the keyboard types into it.
fn press_field(shell: &mut StagedWindow, name: &str) {
    // A field's accessible node names it but stands nowhere: what stands is the widget, whose
    // rect the window's inspection holds.
    let at = shell
        .harness
        .inspection()
        .root
        .nodes()
        .into_iter()
        .find(|node| {
            node.semantics.as_ref().is_some_and(|access| {
                access.role == tilo_ui_base::a11y::Role::TextInput
                    && access.name.as_deref() == Some(name)
            }) && node.rect[2] > 1.0
                && node.rect[3] > 1.0
        })
        .map_or_else(
            || {
                let fields: Vec<(Option<String>, tilo_ui_base::a11y::Role)> = shell
                    .accessible()
                    .into_iter()
                    .filter(|node| node.role == tilo_ui_base::a11y::Role::TextInput)
                    .map(|node| (node.name, node.role))
                    .collect();
                panic!("the panel offers the field `{name}`: {fields:?}")
            },
            |node| node.rect,
        );
    shell
        .harness
        .click_at(at[0] + at[2] / 2.0, at[1] + at[3] / 2.0);
    shell.exchange();
}

/// The action `name` the window last ran, as the device's own verb: its arguments as the device's
/// action API takes them.
fn asked(shell: &StagedWindow, name: &str) -> serde_json::Map<String, serde_json::Value> {
    shell
        .called
        .0
        .borrow()
        .iter()
        .rev()
        .find_map(|invocation| match invocation {
            Invocation::Action {
                app, name: asked, ..
            } if asked == name => {
                assert_eq!(app.as_str(), "so.tilo.device", "the device's own verb");
                Some(invocation.api_arguments())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            let called: Vec<String> = shell
                .called
                .0
                .borrow()
                .iter()
                .map(|invocation| match invocation {
                    Invocation::Action { app, name, .. } => format!("{app}/{name}"),
                    Invocation::Edit { .. } => format!("{invocation:?}"),
                })
                .collect();
            panic!("the panel asks `{name}`: it ran {called:?}")
        })
}

/// A search that failed says why where the networks stand, and looking again asks the device to
/// search: the person is not left with an empty list that reads as "none in reach".
#[test]
fn a_search_that_failed_says_why_and_looks_again() {
    let mut shell = staged();
    assert!(
        shows(&shell, "NetworkManager is not running"),
        "the reason the search failed: {:?}",
        said(&shell)
    );
    assert!(shows(&shell, "Look again"), "{:?}", said(&shell));

    shell.press("Look again");
    assert_eq!(asked(&shell, "scan-wifi"), serde_json::Map::new());
}

/// A password the network would not take is said on that network's row, and the field under it
/// stays open: the person types another password there and joins with it.
#[test]
fn a_wrong_password_stays_on_its_row_and_is_typed_again() {
    let mut shell = staged();
    assert!(
        shows(&shell, "Secrets were required, but not provided"),
        "the network's own row says why: {:?}",
        said(&shell)
    );
    assert!(
        !shows(&shell, "Enter the password for Studio"),
        "no field is open until the row is pressed: {:?}",
        said(&shell)
    );

    shell.press("Studio");
    assert!(
        shows(&shell, "Enter the password for Studio"),
        "the field stands under the row that was refused: {:?}",
        said(&shell)
    );
    assert!(
        shows(&shell, "Secrets were required, but not provided"),
        "with the reason still on it: {:?}",
        said(&shell)
    );

    // The person types the next password in the field and joins.
    press_field(&mut shell, "Password");
    shell.harness.type_text("open sesame");
    shell.exchange();
    shell.press("Join");
    let asked = asked(&shell, "connect-wifi");
    assert_eq!(asked["network"], serde_json::json!("Studio"));
    assert_eq!(asked["password"], serde_json::json!("open sesame"));
}

/// The menu at a saved network's row shows what it offers: the person chooses to forget the
/// network, and the press forgets it through its own action.
#[test]
fn a_saved_networks_menu_offers_forgetting_it() {
    let mut shell = staged();
    assert!(
        !shows(&shell, "Forget this network"),
        "the menu is closed until its button is pressed: {:?}",
        said(&shell)
    );

    shell.press("This network");
    assert!(
        shows(&shell, "Forget this network"),
        "the menu's rows stand over the panel: {:?}",
        said(&shell)
    );
    assert!(
        shows(&shell, "Stop joining by itself"),
        "{:?}",
        said(&shell)
    );

    shell.press("Forget this network");
    // The profile the row stands for is its own address, which the action is
    // called on: nothing of it is an argument.
    assert_eq!(asked(&shell, "forget"), serde_json::Map::new());
}

/// An accessory's row opens its own page: what the device knows of it, the name this machine
/// calls it by, where it stands and the other devices it can move to, and the ways to disconnect
/// and forget it. Forgetting it closes the page again.
#[test]
fn an_accessory_opens_its_page_and_is_forgotten_from_it() {
    let mut shell = staged();
    assert!(
        shows(&shell, "Sony WH-1000XM5"),
        "the accessory's row: {:?}",
        said(&shell)
    );

    shell.press("Sony WH-1000XM5");
    for fact in [
        "AC:80:0A:11:22:33",
        "-48 dBm",
        "82%",
        "This device",
        "Connected here",
        "Fern's desk",
        "Rename",
        "Disconnect",
        "Forget this device",
    ] {
        assert!(
            shows(&shell, fact),
            "the accessory's page shows {fact}: {:?}",
            said(&shell)
        );
    }

    shell.press("Forget this device");
    let asked = asked(&shell, "forget");
    assert_eq!(asked, serde_json::Map::new(), "forgot on its own address");
    assert!(
        shows(&shell, "Quick settings"),
        "the panel comes back once the page closes: {:?}",
        said(&shell)
    );
    assert!(!shows(&shell, "Connected here"), "{:?}", said(&shell));
}

/// Renaming on the page runs the accessory's own rename with what was typed: every list that
/// draws the accessory reads the name the device then publishes.
#[test]
fn renaming_an_accessory_runs_its_own_rename() {
    let mut shell = staged();
    shell.press("Sony WH-1000XM5");
    // The field starts on the name it renames; the person types into it, and
    // what it holds is what the device is asked to call the accessory.
    press_field(&mut shell, "Sony WH-1000XM5");
    shell.harness.type_text(" studio");
    shell.exchange();
    let typed = shell
        .accessible()
        .into_iter()
        .find(|node| node.role == tilo_ui_base::a11y::Role::TextInput)
        .and_then(|node| node.value)
        .unwrap_or_default();
    assert!(
        typed.contains(" studio"),
        "the field takes what is typed: {typed:?}"
    );
    let rename = shell
        .accessible()
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Rename"));
    assert!(
        rename.as_ref().is_some_and(|node| !node.state.disabled),
        "the button that renames is there and enabled: {rename:?}"
    );
    // The keyboard leaves the field for the button that runs what it holds, and Enter runs it:
    // the same route a person tabbing through the page takes.
    shell.harness.key("Tab");
    assert_eq!(shell.holds_keyboard().as_deref(), Some("Rename"));
    shell.harness.key("Enter");
    shell.exchange();
    let asked = asked(&shell, "rename");
    assert_ne!(asked["name"], serde_json::json!("Sony WH-1000XM5"));
    assert!(
        asked["name"]
            .as_str()
            .is_some_and(|name| name.contains(" studio")),
        "the name asked for is what the field holds: {:?}",
        asked["name"]
    );
}

/// The tree the window is sent carries the accessory's page, not only the words a screen reader
/// reads: what a shot draws is the panel it stood on. The desktop keeps a panel of the people it
/// is with in its tree, shown or not, so the press is held to the panel that showed it rather
/// than to how many panels the shell keeps.
#[test]
fn the_accessory_page_is_a_page_of_the_panel() {
    let mut shell = staged();
    let before: Vec<String> = {
        let tree: UiNode = shell.instance.root_tree().expect("the window's tree");
        tree.of_kind("ui.FloatingPanel")
            .into_iter()
            .map(|panel| panel.key.clone())
            .collect()
    };
    shell.press("Sony WH-1000XM5");
    let tree: UiNode = shell.instance.root_tree().expect("the window's tree");
    let panels: Vec<&UiNode> = tree.of_kind("ui.FloatingPanel");
    let keys: Vec<String> = panels.iter().map(|panel| panel.key.clone()).collect();
    assert_eq!(
        keys, before,
        "the page stands in the panel that showed the quick settings, and opens no panel of its \
         own: {tree:#?}"
    );
    // The page is that panel's: its tree carries what only the accessory's page shows.
    let pages = panels
        .iter()
        .filter(|panel| format!("{panel:?}").contains("\"AC:80:0A:11:22:33\""))
        .count();
    assert_eq!(
        pages, 1,
        "one panel carries the accessory's page: {tree:#?}"
    );
}
