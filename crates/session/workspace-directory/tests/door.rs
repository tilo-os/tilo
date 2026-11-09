//! A site, its directory and its door, over a real `tilo_net` connection to
//! a local responder: what a page and the desktop both do.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a fixture that cannot build its invitation or see its own reply has no fallback"
)]

use test_http::{Reply, Responder};
use tilo_relay_protocol::directory::{DirectoryEntry, DirectoryPage};
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_workspace_directory::{Person, client, fetch_directory, join, read_site};

/// The workspace every test in this file joins.
const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// An invitation for `person` to `workspace`, minted as a host mints one.
fn invitation_for(workspace: &str, person: [u8; 32]) -> String {
    let host = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
    let host_device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut issuer = tilo_user_identity::DeviceCert {
        user_public_key: host.verifying_key().to_bytes(),
        device_public_key: host_device.verifying_key().to_bytes(),
        device_label: "The book club's host".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut issuer, &host).expect("a certified host");
    let grant = tilo_workspace_join::InviteGrant {
        workspace_id: tilo_workspace_join::room_workspace_id(workspace).expect("a workspace's id"),
        room: workspace.to_owned(),
        issuer,
        role: tilo_workspace_join::MemberRole::Viewer,
        nonce: [0x33; 16],
        recipient_user_public_key: Some(person),
        invitee_name: None,
        expires_at_ns: now.saturating_add(3_600_000_000_000),
        transport: Vec::new(),
    };
    let signed =
        tilo_workspace_join::issue_invitation(grant, &host_device).expect("a signed invitation");
    tilo_workspace_join::encode_invitation_text(&signed)
}

fn entry(name: &str) -> DirectoryEntry {
    DirectoryEntry {
        workspace: WORKSPACE.to_owned(),
        name: name.to_owned(),
        description: format!("All about {name}"),
        default_role: WorkspaceRole::Commenter,
        door: format!("directory/{WORKSPACE}/"),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: None,
        community: false,
    }
}

#[test]
fn a_door_round_trip_carries_the_invitation_it_granted() {
    let person = Person {
        user: [0x21; 32],
        name: "Ada Lovelace".to_owned(),
    };
    let invitation = invitation_for(WORKSPACE, person.user);
    let answer = serde_json::json!({
        "invitation": invitation,
        "admissionToken": "one-time-token",
        "origin": "https://rooms.tilo.sh/",
    })
    .to_string();
    let responder = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(answer),
    ]);
    let door = url::Url::parse(&format!(
        "{}/peer/v1/directory/{WORKSPACE}/",
        responder.url()
    ))
    .expect("the door's address");
    let client = client().expect("a client");
    let granted = tilo_rt::block_on(join(&client, &door, &person))
        .expect("the app runtime")
        .expect("a granted join");
    assert_eq!(granted.workspace, WORKSPACE);
    assert_eq!(
        granted.invitation, invitation,
        "the invitation the door answered with is the one carried"
    );

    let request = responder.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.target,
        format!("/peer/v1/directory/{WORKSPACE}/join")
    );
    let body = request.body_text();
    assert!(body.contains(&hex::encode(person.user)), "{body}");
    assert!(body.contains("Ada Lovelace"), "{body}");
}

#[test]
fn a_door_that_refuses_says_why_in_its_own_words() {
    let person = Person {
        user: [0x22; 32],
        name: "Grace".to_owned(),
    };
    let responder = Responder::start(vec![
        Reply::status(403)
            .header("Content-Type", "application/json")
            .body(r#"{"error":"this workspace is full"}"#),
    ]);
    let door = url::Url::parse(&format!(
        "{}/peer/v1/directory/{WORKSPACE}/",
        responder.url()
    ))
    .expect("the door's address");
    let client = client().expect("a client");
    let refused = tilo_rt::block_on(join(&client, &door, &person))
        .expect("the app runtime")
        .expect_err("a refused join");
    assert_eq!(refused, "this workspace is full");
}

#[test]
fn a_site_says_its_home_workspace_and_its_directory() {
    let responder = Responder::handle(|request: &test_http::Request| {
        let reply = if request.target.ends_with("entry.json") {
            serde_json::json!({
                "home": {
                    "name": "tilo.sh",
                    "host": { "origin": "https://rooms.tilo.sh/" },
                    "admission": "https://rooms.tilo.sh/",
                },
                "directory": "directory",
            })
            .to_string()
        } else {
            serde_json::to_string(&DirectoryPage {
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
        Some("https://rooms.tilo.sh/")
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
