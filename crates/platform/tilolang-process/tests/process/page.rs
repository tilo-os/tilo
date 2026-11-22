//! A page: the instance draws into a window laid out in its own process, beside it, and the device
//! that started it asks for that window as a page on a connection of its own. What the person does
//! on the page acts on the window as their click or their typing does there.

use std::os::unix::net::UnixStream;

use tilo_server_protocol::page::PageAction;
use tilo_server_protocol::page_window::{self, Answer, Ask};
use tilolang_process::page::PageWindow;

use super::*;

/// A Notes notebook shown as a page in `host`: the device's end of its connection.
fn notes_page(host: &mut TilolangProcess) -> UnixStream {
    let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
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
    let record = InstanceDocument::new(automerge::AutoCommit::new(), manager, route.clone(), "ada")
        .keeping_content_in(tilo_content::ContentStore::memory());
    let kept = host.kept(NOTES).unwrap();
    let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
    let mut attributes = spec(1, "ada", &route);
    attributes.shows = Shows::Page;
    let hosted = Hosted::with_document(
        attributes,
        record,
        Arc::new(notes()),
        &kept,
        Box::new(app_end),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
        wake(),
    );
    let (ours, theirs) = UnixStream::pair().unwrap();
    host.show(hosted, PageWindow::new(ours, window_end));
    theirs
}

/// `ask` asked of the page on `device`, and the host's answer once its loop took it.
fn ask(host: &mut TilolangProcess, device: &mut UnixStream, ask: &Ask) -> Answer {
    page_window::write(device, ask).unwrap();
    let began = std::time::Instant::now();
    device.set_nonblocking(true).unwrap();
    loop {
        host.turn();
        let mut peek = [0; 1];
        if device.peek(&mut peek).is_ok() {
            break;
        }
        assert!(began.elapsed() < SETTLE_LIMIT, "the page did not answer");
    }
    device.set_nonblocking(false).unwrap();
    page_window::read(device).unwrap()
}

/// The number the page gives the element `label` names, in the HTML `html`.
fn numbered(html: &str, label: &str) -> Option<u32> {
    let at = html.find(&format!("aria-label=\"{label}\""))?;
    let start = html[..at].rfind('<')?;
    let tag = &html[start..html[at..].find('>')? + at];
    let number = tag.split("data-p=\"").nth(1)?.split('"').next()?;
    number.parse().ok()
}

#[test]
fn a_page_shows_the_instances_window_and_acts_on_it_as_the_person_did() {
    let mut host = TilolangProcess::new(None, wake());
    let mut device = notes_page(&mut host);
    let Answer::Shown { css, body } = ask(&mut host, &mut device, &Ask::Show { show: None }) else {
        panic!("the page was not shown");
    };
    assert!(css.contains("--tilo-ground"), "the theme's colours: {css}");
    let new_note = numbered(&body, "New note").unwrap_or_else(|| panic!("no New note in {body}"));
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

    // The page closes: its instance ends as a closed window's does.
    drop(device);
    host.turn();
    assert_eq!(host.instances().count(), 0);
}
