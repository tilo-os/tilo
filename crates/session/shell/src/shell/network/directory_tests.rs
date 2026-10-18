//! Joining a public workspace from a site's directory, and the community
//! workspace once Settings no longer has a switch for it: whoever was in it
//! stays in it, and whoever left it finds it in the directory.

use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::rooms::RoomRole;

use super::site_join::SiteJoin;
use super::site_join_tests::{Site, computer, held, key};
use crate::shell::state::RoomId;

/// Point this computer's community at `site` for the test's life.
fn community_is(home: &mut tilo_test_env::TestHome, dir: &std::path::Path, site: &url::Url) {
    let config = dir.join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).unwrap();
    home.env().set(super::community::CONFIG_ENV, &config);
}

/// A site whose `entry.json` names only a directory: the relay's own.
fn directory_site(relay_origin: &str) -> (test_http::Responder, url::Url) {
    let directory = format!("{relay_origin}/peer/v1/directory");
    let responder = test_http::Responder::handle(move |request| {
        (request.method == "GET" && request.target == "/entry.json").then(|| {
            test_http::Reply::status(200)
                .header("content-type", "application/json")
                .body(serde_json::json!({ "directory": directory }).to_string())
        })
    });
    let entry = url::Url::parse(&format!("{}/", responder.url())).unwrap();
    (responder, entry)
}

#[test]
#[serial_test::serial]
fn a_computer_joins_a_public_workspace_from_the_directory_with_its_default_role() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // No community site: only what the person types is asked.
    let none = dir.path().join("none.toml");
    std::fs::write(&none, "site = \"\"\n").unwrap();
    home.env().set(super::community::CONFIG_ENV, &none);
    let site = Site::start(dir.path());
    let room = RoomId::parse(&site.room).unwrap();
    site.runtime
        .block_on(site.relay().rooms().unwrap().publish(
            &site.room,
            &site.host_cert,
            Listing {
                description: "Where people hang out".into(),
                default_role: RoomRole::Commenter,
            },
        ))
        .unwrap();
    let relay_origin = site.web.as_ref().unwrap().plain_origin.clone().unwrap();
    let (_responder, entry) = directory_site(&relay_origin);
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's computer");

    let listed = net.directory(Some(entry.as_str())).unwrap();
    let lounge = listed
        .iter()
        .find(|listing| listing.room == Some(room))
        .expect("the public workspace is listed")
        .clone();
    assert_eq!(lounge.entry.name, "Lounge");
    assert_eq!(lounge.entry.description, "Where people hang out");
    assert_eq!(lounge.entry.default_role, RoomRole::Commenter);
    assert!(!lounge.joined);

    let joined = net.join_listed(&lounge).unwrap();
    let (SiteJoin::Held { room: got } | SiteJoin::Enrolling { room: got, .. }) = joined;
    assert_eq!(got, room);
    held(&net, &tree, room);
    let role = site
        .runtime
        .block_on(site.relay().rooms().unwrap().summary(&site.room))
        .unwrap()
        .members
        .into_iter()
        .find(|member| member.user_public_key == ada.verifying_key().to_bytes())
        .map(|member| member.role);
    assert_eq!(
        role,
        Some(tilo_peer_state::join::MemberRole::Commenter),
        "the owner's default role, nobody deciding the join"
    );
    assert!(
        net.directory(Some(entry.as_str()))
            .unwrap()
            .iter()
            .any(|listing| listing.room == Some(room) && listing.joined),
        "listed as joined once held"
    );
    assert!(
        net.site_room_labels()
            .iter()
            .any(|label| label.room == room),
        "held through the web, so its menu offers Leave"
    );
    net.leave_site(room).unwrap();
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn whoever_joined_the_community_in_settings_stays_in_it_and_it_is_in_the_directory() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    community_is(&mut home, dir.path(), &site.entry);
    let room = RoomId::parse(&site.room).unwrap();
    let ada = key(1);
    let computer_dir = dir.path().join("computer");

    // Joined the way Settings' switch left it: held, never left.
    let (net, tree) = computer(&computer_dir, ada.clone(), "Ada's computer");
    assert_eq!(
        net.keep_community(&site.entry),
        super::community::Pass::Held
    );
    held(&net, &tree, room);
    drop(net);
    drop(tree);
    crate::shell::persist::wait_for_saves();

    // After the update the switch is gone; the workspace is not.
    let (net, tree) = computer(&computer_dir, ada.clone(), "Ada's computer");
    assert!(!net.left_site(&site.entry));
    held(&net, &tree, room);
    let community = net
        .directory(None)
        .unwrap()
        .into_iter()
        .find(|listing| listing.entry.community)
        .expect("the site's own workspace is listed");
    assert!(community.joined, "listed as joined");
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn whoever_left_the_community_in_settings_joins_it_again_from_the_directory() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    community_is(&mut home, dir.path(), &site.entry);
    let room = RoomId::parse(&site.room).unwrap();
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's computer");
    assert_eq!(
        net.keep_community(&site.entry),
        super::community::Pass::Held
    );
    held(&net, &tree, room);
    // Left with Settings' switch, as it was.
    net.leave_community(&site.entry).unwrap();
    assert!(net.left_site(&site.entry));

    let listed = net.directory(None).unwrap();
    let community = listed
        .first()
        .filter(|listing| listing.entry.community)
        .expect("the site's own workspace comes first")
        .clone();
    assert!(!community.joined);
    net.join_listed(&community).unwrap();
    held(&net, &tree, room);
    assert!(
        !net.left_site(&site.entry),
        "joining from the directory is joining again: it is followed as before"
    );
    drop(net);
    site.shutdown();
}
