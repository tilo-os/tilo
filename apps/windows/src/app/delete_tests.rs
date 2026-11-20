//! Deleting a setup: what its confirmation and its page say, and what it takes with it.

use super::*;

/// Every word `widget` says, as a screen reader reads them.
fn said(widget: &dyn Widget, out: &mut Vec<String>) {
    if let Some(node) = widget.accessibility() {
        out.extend(
            [node.name, node.value, node.description]
                .into_iter()
                .flatten(),
        );
    }
    widget.for_each_child(&mut |child| said(child, out));
}

/// Whether the window says `text`, as a name, value or description.
fn says(window: &WindowsWindow, text: &str) -> bool {
    let mut out = Vec::new();
    said(window, &mut out);
    out.iter().any(|said| said.contains(text))
}

/// A window over `app`, outside the framework.
fn window(app: WindowsApp) -> WindowsWindow {
    WindowsWindow::new(app, None)
}

/// Press what queues `intent` in `window`, as its control does.
fn press(window: &mut WindowsWindow, intent: Intent) {
    window.app.borrow().intents.push(intent);
    window.settle();
}

/// Deleting one program of a setup deletes the setup: the confirmation names every program
/// that goes with it, and the page and the button say the action is the setup's rather than
/// one program's. No other setup is touched.
#[test]
fn deleting_a_setup_names_every_program_it_removes() {
    let mut window = window(WindowsApp::new_offline_scenario("confirm-delete-setup"));
    assert!(window.dialog_open());
    assert!(
        says(&window, "Delete PuTTY 0.85 (64-bit) and its data?"),
        "the dialog names the setup"
    );
    assert!(
        says(
            &window,
            "This removes PSFTP, PuTTY, PuTTYGen and Pageant, everything stored in their \
             shared private capsule, and closes any of them that are open. You can't undo \
             this."
        ),
        "the dialog names every program that goes with it"
    );
    assert!(
        !says(&window, "its private capsule and everything stored in it"),
        "the dialog never says one program's capsule while the setup's apps all go"
    );
    assert!(says(&window, "Delete setup and data"));
    assert!(
        says(
            &window,
            "PSFTP, PuTTY, PuTTYGen and Pageant came from one setup and share their \
             files, so they are removed together."
        ),
        "the page says the programs are one setup's"
    );
    let app = Rc::clone(&window.app);
    assert_eq!(
        app.borrow().confirm_delete_data.as_deref(),
        Some("wine-putty")
    );

    press(&mut window, Intent::ConfirmDelete);
    let app = app.borrow();
    assert!(
        !app.catalogue
            .apps
            .iter()
            .any(|item| item.capsule == "wine-putty"),
        "every program of the setup went with its capsule"
    );
    assert!(
        app.catalogue
            .apps
            .iter()
            .any(|item| item.name == "Visual Studio"),
        "another setup keeps its apps"
    );
}

/// A program alone in its capsule is still said as one app: the setup's wording is for a
/// setup that installs more than one.
#[test]
fn deleting_a_lone_program_is_said_as_one_app() {
    let window = window(WindowsApp::new_offline_scenario("confirm-delete"));
    assert!(window.dialog_open());
    assert!(says(&window, "Delete app and data?"));
    assert!(says(
        &window,
        "This removes Adobe Acrobat Reader, its private capsule and everything stored in it. \
         You can't undo this."
    ));
}
