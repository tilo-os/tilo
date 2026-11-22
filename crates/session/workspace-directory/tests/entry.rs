//! A site, its directory and its entry, over a real `tilo_net` connection to
//! a local responder: what a page and the desktop both do.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use test_http::{Reply, Responder};
use tilo_server_protocol::directory::{DirectoryEntry, DirectoryPage};
use tilo_server_protocol::workspaces::Role;
use tilo_workspace_directory::{Way, client, fetch_directory, read_site, through_entry};

/// The workspace every test in this file joins.
const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

fn entry(name: &str) -> DirectoryEntry {
    DirectoryEntry {
        workspace: WORKSPACE.to_owned(),
        name: name.to_owned(),
        description: format!("All about {name}"),
        default_role: Role::Commenter,
        entry: format!("directory/{WORKSPACE}/"),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: None,
        community: false,
        invitation_only: false,
        wait_list: None,
    }
}

/// Joining through an entry asks the entry nothing here (there is no client to
/// ask with): the device enters the workspace its path names on the session
/// it opens with the entry's host, where the entry decides. An entry below a host's API prefix and one at a
/// site's own path name the same workspace, and a path that names none is
/// refused before anything is asked.
#[test]
fn an_entry_join_enters_the_workspace_its_path_names_and_asks_nothing() {
    for entry in [
        format!("https://workspaces.tilo.test/peer/v1/directory/{WORKSPACE}/"),
        format!("https://tilo.test/directory/{WORKSPACE}/"),
    ] {
        let entry = url::Url::parse(&entry).expect("the entry's address");
        let granted = through_entry(&entry, None).expect("an entry names its workspace");
        assert_eq!(granted.workspace, WORKSPACE);
        assert_eq!(
            granted.way,
            Way::Entry {
                entry: entry.clone(),
                host: None,
            },
            "the device enters through the entry itself, holding no invitation"
        );
        assert_eq!(granted.invitation(), None);
    }
    for nameless in [
        "https://tilo.test/entry/".to_owned(),
        "https://tilo.test/directory/not-a-workspace/".to_owned(),
    ] {
        let entry = url::Url::parse(&nameless).expect("the entry's address");
        assert!(
            through_entry(&entry, None).is_err(),
            "{nameless} names no workspace to enter"
        );
    }
}

#[test]
fn a_site_says_its_home_workspace_and_its_directory() {
    let responder = Responder::handle(|request: &test_http::Request| {
        let reply = if request.target.ends_with("entry.json") {
            serde_json::json!({
                "home": {
                    "name": "tilo.sh",
                    "host": { "origin": "https://workspaces.tilo.sh/" },
                    "admission": "https://workspaces.tilo.sh/",
                },
                "directory": "directory",
            })
            .to_string()
        } else {
            serde_json::to_string(&DirectoryPage {
                host: None,
                workspaces: vec![entry("Coffee")],
            })
            .expect("a directory page")
        };
        Some(
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(reply),
        )
    });
    let site = url::Url::parse(&format!("{}/", responder.url())).expect("the site's address");
    let client = client().expect("a client");
    let read = tilo_rt::block_on(read_site(&client, &site))
        .expect("the app runtime")
        .expect("a site read");
    let home = read.home.expect("the site's home workspace");
    assert_eq!(home.name, "tilo.sh");
    assert_eq!(
        home.admission.as_ref().map(url::Url::as_str),
        Some("https://workspaces.tilo.sh/")
    );
    let directory = read.directory.expect("the site's directory");
    let page = tilo_rt::block_on(fetch_directory(&client, &directory))
        .expect("the app runtime")
        .expect("a directory read");
    assert_eq!(page.workspaces.len(), 1);
    assert_eq!(page.workspaces[0].name, "Coffee");

    let first = responder.next_request();
    assert_eq!(first.method, "GET");
    assert!(first.target.ends_with("entry.json"), "{}", first.target);
    let second = responder.next_request();
    assert_eq!(second.method, "POST", "a directory route is a POST");
    assert!(second.target.ends_with("directory"), "{}", second.target);
}
