//! A site and its directory, over a real `tilo_net` connection to a local
//! responder: what a page and the desktop both do.
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
use tilo_workspace_directory::{client, fetch_directory, read_site};

/// The workspace every test in this file names.
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

/// A site's `entry.json` names its home workspace, its directory and the host's
/// admission entry, and nothing of the host itself: a native client reads no
/// host to dial from it, so its `home.host` says only where the host answers.
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

/// A site that names its host with the host's own `web-host.json` line in
/// `home.host.info` reads the same: the field is not read by a native client,
/// so what a site puts there changes nothing here.
#[test]
fn a_sites_host_info_is_not_read() {
    let host_line = serde_json::json!({
        "server_cert": {
            "user_public_key": "01".repeat(32),
            "device_public_key": "02".repeat(32),
            "device_label": "workspaces.tilo.sh",
            "issued_at_ns": 1,
            "expires_at_ns": null,
            "signature": "03".repeat(64),
        },
        "origin": "https://workspaces.tilo.sh",
    });
    let responder = Responder::handle(move |request: &test_http::Request| {
        if request.target.ends_with("entry.json") {
            Some(
                Reply::status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "home": {
                                "name": "tilo.sh",
                                "host": {
                                    "origin": "https://workspaces.tilo.sh/",
                                    "info": serde_json::to_string(&host_line)
                                        .expect("a host's line"),
                                },
                                "admission": "https://workspaces.tilo.sh/",
                            },
                            "directory": "directory",
                        })
                        .to_string(),
                    ),
            )
        } else {
            Some(
                Reply::status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::to_string(&DirectoryPage {
                            host: None,
                            workspaces: Vec::new(),
                        })
                        .expect("a directory page"),
                    ),
            )
        }
    });
    let site = url::Url::parse(&format!("{}/", responder.url())).expect("the site's address");
    let client = client().expect("a client");
    let read = tilo_rt::block_on(read_site(&client, &site))
        .expect("the app runtime")
        .expect("a site read");
    let home = read.home.expect("the site's home workspace");
    assert_eq!(home.name, "tilo.sh");
}
