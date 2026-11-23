//! A viewer's page: the app lays its view out in its own process and answers show and act there.

use std::os::unix::net::UnixStream;

use tilo_server_protocol::page::PageAction;
use tilo_server_protocol::page_window::{self, Answer, Ask};
use tilolang_process::page::PageWindow;

use super::*;

fn notes_page(
    host: &mut TilolangProcess,
    bus: &Rc<Bus>,
    route: &RouteSpec,
    number: u64,
    person: &str,
) -> UnixStream {
    let broker = Broker::on(bus, route.clone(), person);
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
    let hosted = Hosted::new(
        spec(number, person, route).showing(Shows::Page),
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(notes()),
        &tilolang_process::hosted::Kept::default(),
        Box::new(app_end),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    let (ours, theirs) = UnixStream::pair().unwrap();
    host.show(hosted, PageWindow::new(ours, window_end));
    theirs
}

fn ask(host: &mut TilolangProcess, device: &mut UnixStream, ask: &Ask) -> Answer {
    page_window::write(device, ask).unwrap();
    host.turn();
    page_window::read(device).unwrap()
}

fn show() -> Ask {
    Ask::Show {
        show: None,
        signed: None,
        invoker: None,
    }
}

fn act(id: u32, action: PageAction) -> Ask {
    Ask::Act {
        id,
        target: None,
        action,
        signed: None,
        invoker: None,
    }
}

fn numbered(html: &str, label: &str) -> Option<u32> {
    let at = html
        .find(&format!("aria-label=\"{label}\""))
        .or_else(|| html.find(&format!(">{label}<")))?;
    let tag = html.get(..at)?.rfind("data-p=\"")?;
    html.get(tag..)?.split('"').nth(1)?.parse().ok()
}

fn open_note(host: &mut TilolangProcess, device: &mut UnixStream, title: &str) {
    let Answer::Shown { body, .. } = ask(host, device, &show()) else {
        panic!("the page was not shown");
    };
    let new_note = numbered(&body, "New note").unwrap_or_else(|| panic!("no New note: {body}"));
    let _ = ask(host, device, &act(new_note, PageAction::Activate));
    let Answer::Shown { body, .. } = ask(host, device, &show()) else {
        panic!("the page was not shown again");
    };
    let at = body
        .find("<textarea")
        .unwrap_or_else(|| panic!("a new note's title: {body}"));
    let field = body[at..]
        .split('>')
        .next()
        .unwrap()
        .split("data-p=\"")
        .nth(1)
        .unwrap();
    let field: u32 = field.split('"').next().unwrap().parse().unwrap();
    let _ = ask(host, device, &act(field, PageAction::Value(title.into())));
}

#[test]
fn a_page_acts_on_the_apps_view_and_shows_another_members_write() {
    let mut host = TilolangProcess::new(wake());
    let bus = Rc::new(Bus::default());
    let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
    let mut ada = notes_page(&mut host, &bus, &route, 1, "ada");
    let mut bob = notes_page(&mut host, &bus, &route, 2, "bob");

    // Ada's note is the workspace's: asked for again, the page shows it.
    open_note(&mut host, &mut ada, "Groceries");
    let Answer::Shown { body, .. } = ask(&mut host, &mut ada, &show()) else {
        panic!("the page was not shown again");
    };
    assert!(
        body.contains("Groceries"),
        "the title typed is there: {body}"
    );

    // Bob's write reaches Ada's open page as a patch.
    open_note(&mut host, &mut bob, "From Bob");
    let Answer::Patched(patches) = ask(&mut host, &mut ada, &Ask::Changes) else {
        panic!("the page answered no change");
    };
    let reached: String = patches.into_iter().map(|patch| patch.html).collect();
    assert!(
        reached.contains("From Bob"),
        "another member's write reaches the open page: {reached}"
    );

    drop((ada, bob));
    host.turn();
    assert_eq!(host.instances().count(), 0);
}
