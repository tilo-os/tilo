//! A viewer's page: the app's code draws into a window laid out in its own process, beside it, and
//! the device that started it asks for that window as a page on a connection of its own. What the person does
//! on the page acts on the window as their click or their typing does there.

use std::os::unix::net::UnixStream;

use tilo_server_protocol::page::PageAction;
use tilo_server_protocol::page_window::{self, Answer, Ask};
use tilolang_process::page::PageWindow;

use super::*;

/// A Notes notebook shown as a page in `host`: the device's end of its connection.
fn notes_page(host: &mut TilolangProcess) -> UnixStream {
    let workspace = uuid::Uuid::new_v4();
    let route = RouteSpec::app(NOTES, workspace);
    let broker = Broker::new(route.clone());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some("ada".to_owned()),
    }));
    let _ = manager.poll();
    // The device's content store, which the session opens for the app.
    let content = tilo_content::ContentStore::memory();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let kept = tilolang_process::hosted::Kept {
        content: Some(content.clone()),
        ..host.kept(NOTES)
    };
    let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
    let mut attributes = spec(1, "ada", &route);
    attributes.shows = Shows::Page;
    let hosted = Hosted::new(
        attributes,
        connection,
        (store, content),
        Arc::new(notes()),
        &kept,
        Box::new(app_end),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    let (ours, theirs) = UnixStream::pair().unwrap();
    host.show(hosted, PageWindow::new(ours, window_end));
    theirs
}

/// `ask` asked of the page on `device`, and the host's answer: its next turn takes it.
fn ask(host: &mut TilolangProcess, device: &mut UnixStream, ask: &Ask) -> Answer {
    page_window::write(device, ask).unwrap();
    // The host answers what was asked before its turn, in that turn.
    host.turn();
    page_window::read(device).unwrap()
}

/// The number the page gives the innermost element that `label` names, by its accessible name or
/// its text, in the HTML `html`.
fn numbered(html: &str, label: &str) -> Option<u32> {
    let at = html
        .find(&format!("aria-label=\"{label}\""))
        .or_else(|| html.find(&format!(">{label}<")))?;
    let tag = html.get(..at)?.rfind("data-p=\"")?;
    let number = html.get(tag..)?.split('"').nth(1)?;
    number.parse().ok()
}

#[test]
fn a_page_shows_the_apps_view_and_acts_on_it_as_the_person_did() {
    let mut host = TilolangProcess::new(wake());
    let mut device = notes_page(&mut host);
    let Answer::Shown { css, body } = ask(&mut host, &mut device, &Ask::Show { show: None }) else {
        panic!("the page was not shown");
    };
    assert!(css.contains("--tilo-ground"), "the theme's colours: {css}");
    let new_note = numbered(&body, "New note").unwrap_or_else(|| {
        let at = body.find("New").unwrap_or_default();
        panic!(
            "no New note in {}",
            body.get(at.saturating_sub(1500)..).unwrap_or(&body)
        )
    });
    assert!(!body.contains("<textarea"), "no note is open yet: {body}");

    // The person presses New note: the note opens, its title taking the caret.
    let Answer::Patched(patches) = ask(
        &mut host,
        &mut device,
        &Ask::Act {
            id: new_note,
            target: None,
            action: PageAction::Activate,
        },
    ) else {
        panic!("the press was not answered");
    };
    assert!(
        patches.iter().any(|patch| patch.html.contains("<textarea")),
        "a new note's title field: {patches:?}"
    );
    // Nothing more changed since.
    assert_eq!(
        ask(&mut host, &mut device, &Ask::Changes),
        Answer::Patched(Vec::new())
    );

    // The page closes: it ends as a closed window's does.
    drop(device);
    host.turn();
    assert_eq!(host.instances().count(), 0);
}
