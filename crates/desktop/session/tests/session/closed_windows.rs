//! An app's window that closes while it has the keyboard.

use super::*;
use crate::common::window::{Client, KEY_A, pump as pump_windows};

/// The app the windows are of: Wine names every window of one program the same way.
const APP: &str = "notepad.exe";

/// The window with the keyboard, by its title.
fn focused_title(session: &mut Session) -> Option<String> {
    session.compositor().focused_window()?.title
}

/// A program's second window closes while it has the keyboard, the way Wine closes a dialog: the
/// window's role goes and its surface stays, for the next time the program shows it. The keyboard
/// goes back to the program's window that is still open, and the next key reaches that window:
/// one that went to the closed window's surface would be lost, as a Windows program drops a key
/// for a window it hid.
#[test]
fn a_closed_window_hands_the_keyboard_to_the_window_still_open() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = Client::connect(session.compositor());
    pump_windows(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.keyboard.is_some()
    });
    let main = client.map(&mut session, APP, (400, 300), |toplevel| {
        toplevel.set_title("Untitled - Notepad".to_owned());
    });
    let dialog = client.open_toplevel_with(APP, |toplevel| toplevel.set_title("Open".to_owned()));
    let (main_surface, dialog_surface) = (client.surface_id(main), client.surface_id(dialog));
    client.pump(&mut session, |_, seen| seen.ready(dialog));
    let (width, height) = client.seen.size_at(dialog).unwrap_or((300, 200));
    client.show(dialog, width, height, crate::common::window::WINDOW_BYTES);
    client.pump(&mut session, |session, seen| {
        focused_title(session).as_deref() == Some("Open")
            && seen.keyboard_on.as_ref() == Some(&dialog_surface)
    });

    client.close(dialog);
    client.pump(&mut session, |session, _| {
        focused_title(session).as_deref() == Some("Untitled - Notepad")
    });
    crate::common::window::tap_key(&mut session, KEY_A);
    client.pump(&mut session, |_, seen| seen.pressed(KEY_A) == 1);
    let pressed = client
        .seen
        .keys
        .iter()
        .position(|key| *key == (KEY_A, true))
        .expect("the key was delivered");
    assert_eq!(
        client.seen.keys_on[pressed],
        Some(main_surface),
        "the key reaches the window still open, not the closed one"
    );
}

/// The installer's pages: Wine names every window of one program the same way.
const SETUP: &str = "msiexec.exe";

/// A client of the session whose keyboard came to it.
fn setup_client(session: &mut Session) -> Client {
    let mut client = Client::connect(session.compositor());
    pump_windows(session, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.keyboard.is_some()
    });
    client
}

/// Open the page `title` of the installer, as far as its first configure: drawn by
/// [`draw_page`].
fn open_page(
    session: &mut Session,
    client: &mut Client,
    title: &str,
    parent: Option<usize>,
) -> usize {
    let parent = parent.map(|parent| client.toplevel(parent).clone());
    let page = client.open_toplevel_with(SETUP, |toplevel| {
        toplevel.set_title(title.to_owned());
        toplevel.set_parent(parent.as_ref());
    });
    client.pump(session, |_, seen| seen.ready(page));
    page
}

/// Draw `page` at the size it was configured to.
fn draw_page(client: &mut Client, page: usize) {
    let (width, height) = client.seen.size_at(page).unwrap_or((400, 300));
    client.show(page, width, height, crate::common::window::WINDOW_BYTES);
}

/// The page `title` has the keyboard, and the next key reaches it: not another page, not the
/// session, and not a client the session cut off.
#[track_caller]
fn types_into(session: &mut Session, client: &mut Client, page: usize, title: &str) {
    let surface = client.surface_id(page);
    client.pump(session, |session, seen| {
        focused_title(session).as_deref() == Some(title)
            && seen.keyboard_on.as_ref() == Some(&surface)
    });
    let before = client.seen.pressed(KEY_A);
    crate::common::window::tap_key(session, KEY_A);
    client.pump(session, |_, seen| seen.pressed(KEY_A) > before);
    let pressed = client
        .seen
        .keys
        .iter()
        .rposition(|key| *key == (KEY_A, true))
        .expect("the key was delivered");
    assert_eq!(
        client.seen.keys_on[pressed],
        Some(surface),
        "the key reaches the page {title}"
    );
}

/// A program shows its next page as a new window and destroys the one before at once, the way a
/// Windows installer's wizard does: the new page has the keyboard, and the next key reaches it.
/// The configure that told the old page it lost the focus crosses its destroy, and the client acks
/// it as it must; that ack once cost the client its connection, so the new page saw the keyboard
/// come and never a key.
#[test]
fn a_window_that_replaces_the_one_before_keeps_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = setup_client(&mut session);
    let welcome = open_page(&mut session, &mut client, "Setup - Welcome", None);
    draw_page(&mut client, welcome);
    types_into(&mut session, &mut client, welcome, "Setup - Welcome");
    let folder = open_page(&mut session, &mut client, "Setup - Folder", None);
    draw_page(&mut client, folder);
    client.close(welcome);
    types_into(&mut session, &mut client, folder, "Setup - Folder");
}

/// The next page shown and the one before destroyed in one message to the session: the same.
#[test]
fn a_page_shown_and_its_predecessor_destroyed_together_keeps_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = setup_client(&mut session);
    let welcome = open_page(&mut session, &mut client, "Setup - Welcome", None);
    draw_page(&mut client, welcome);
    types_into(&mut session, &mut client, welcome, "Setup - Welcome");
    let folder = open_page(&mut session, &mut client, "Setup - Folder", None);
    client.replace(folder, welcome, crate::common::window::WINDOW_BYTES);
    types_into(&mut session, &mut client, folder, "Setup - Folder");
}

/// The next page is a dialog of the one before (its child), and its parent goes as it shows: the
/// dialog keeps the keyboard.
#[test]
fn a_dialog_whose_parent_goes_as_it_shows_keeps_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = setup_client(&mut session);
    let welcome = open_page(&mut session, &mut client, "Setup - Welcome", None);
    draw_page(&mut client, welcome);
    types_into(&mut session, &mut client, welcome, "Setup - Welcome");
    let folder = open_page(&mut session, &mut client, "Setup - Folder", Some(welcome));
    draw_page(&mut client, folder);
    client.close(welcome);
    types_into(&mut session, &mut client, folder, "Setup - Folder");
}

/// The page before goes first and the next one shows after: the next page takes the keyboard.
#[test]
fn a_page_shown_after_the_one_before_went_takes_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = setup_client(&mut session);
    let welcome = open_page(&mut session, &mut client, "Setup - Welcome", None);
    draw_page(&mut client, welcome);
    types_into(&mut session, &mut client, welcome, "Setup - Welcome");
    let folder = open_page(&mut session, &mut client, "Setup - Folder", None);
    client.close(welcome);
    client.pump(&mut session, |session, _| {
        focused_title(session).as_deref() != Some("Setup - Welcome")
    });
    draw_page(&mut client, folder);
    types_into(&mut session, &mut client, folder, "Setup - Folder");
}

/// A whole wizard: each page replaces the one before as the last one did, and each in turn has
/// the keyboard.
#[test]
fn every_page_of_a_wizard_keeps_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = setup_client(&mut session);
    let mut page = open_page(&mut session, &mut client, "Setup - Welcome", None);
    draw_page(&mut client, page);
    types_into(&mut session, &mut client, page, "Setup - Welcome");
    for title in ["Setup - Licence", "Setup - Folder", "Setup - Install"] {
        let next = open_page(&mut session, &mut client, title, None);
        client.replace(next, page, crate::common::window::WINDOW_BYTES);
        types_into(&mut session, &mut client, next, title);
        page = next;
    }
}
