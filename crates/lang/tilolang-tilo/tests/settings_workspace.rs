//! Settings' People and access page: the workspace in front's people and their roles, the agents
//! that work there, what it grants beyond those roles, and which app opens each kind of file.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::{Actor, Value};

use crate::fixtures::invoker::{Answered as _, Sent};
use crate::fixtures::texts;
use crate::settings_panes::{invoke, open_window, settings};

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The workspace's access as the session publishes it: the workspace in front with its people,
/// its roles and which app opens what, one grant beyond their roles, and one agent at work.
const ACCESS: &str = r#"{
  "known": true,
  "workspaces": [
    {
      "id": "ws-1",
      "name": "Harbour",
      "grants": [
        {
          "id": "grant-1",
          "section": "Files",
          "holder": "Ada",
          "allows": "Read and change it",
          "expiry": "until it is taken back",
          "granted_by": "Bo",
          "app": "so.tilo.files",
          "path": "/home/ada/harbour/plans",
          "workspace": "ws-1",
          "holder_kind": "person",
          "granted_by_name": "Bo",
          "granted_by_kind": "person",
          "granted_by_you": false,
          "device": false,
          "can_revoke": true
        }
      ]
    }
  ],
  "outside": [],
  "shares": [],
  "governance": {
    "workspace": "ws-1",
    "name": "Harbour",
    "you": "ada",
    "owner": "bo",
    "can_govern": true,
    "roles": [
      {"id": "viewer", "name": "Viewer"},
      {"id": "commenter", "name": "Commenter"},
      {"id": "editor", "name": "Editor"},
      {"id": "moderator", "name": "Moderator"}
    ],
    "people": [
      {"person": "bo", "name": "Bo", "role": "owner", "role_name": "Owner",
       "owner": true, "administrator": false, "you": false,
       "can_change": false, "can_remove": false},
      {"person": "ada", "name": "Ada", "role": "editor", "role_name": "Editor",
       "owner": false, "administrator": false, "you": true,
       "can_change": false, "can_remove": false},
      {"person": "cy", "name": "Cy", "role": "viewer", "role_name": "Viewer",
       "owner": false, "administrator": false, "you": false,
       "can_change": true, "can_remove": true}
    ]
  },
  "defaults": [
    {"resource": "ext:pdf", "label": "PDF", "workspace": "Tilo Viewer", "yours": "Tilo Viewer"}
  ]
}"#;

/// The workspace's agents, as its record keeps them: one at work, one retired.
const AGENTS: &str = r#"{
  "agents": [
    {"id": "a1", "name": "Researcher", "acts_for": "ada", "state": "active",
     "created_at": 1700000000000, "spend_limit_micros": 0, "sub_at_once": 4},
    {"id": "a2", "name": "Old helper", "acts_for": "ada", "state": "retired",
     "created_at": 1600000000000, "spend_limit_micros": 0, "sub_at_once": 4}
  ]
}"#;

/// The page over [`ACCESS`] and [`AGENTS`].
fn access_page() -> (tilolang_tilo::host::Host, tilolang_runtime::ViewInstance) {
    let (host, _) = settings();
    crate::fixtures::set_reading(
        &host,
        "workspace/access",
        &serde_json::from_str(ACCESS).expect("the access reading reads"),
    );
    crate::fixtures::set_reading(
        &host,
        "workspace:tilo.workspace.agent",
        &serde_json::from_str(AGENTS).expect("the agents reading reads"),
    );
    invoke(&host, "open_pane", vec![Value::text("workspace")]);
    let view = open_window(&host);
    (host, view)
}

/// The page names everyone in the workspace with their role, the owner's role as a fact, and an
/// agent as a subject of the workspace's grants.
#[test]
fn the_workspace_page_names_its_people_their_roles_and_its_agents() {
    let (_host, view) = access_page();
    let shown = texts(&view);
    for words in [
        "People",
        "You",
        "Cy",
        "Bo",
        "Editor",
        "Viewer",
        "Owner",
        "the workspace is theirs",
        "Agents",
        "Researcher",
        "Old helper",
        "Active",
        "Retired",
    ] {
        assert!(
            shown.iter().any(|text| text.contains(words)),
            "{words} shows on the workspace page: {shown:?}"
        );
    }
    assert!(
        shown.iter().any(|text| text.contains("Permissions")),
        "the grants beyond roles are listed: {shown:?}"
    );
    assert!(
        shown.iter().any(|text| text == "Ada"),
        "the grant names its holder: {shown:?}"
    );
}

/// A role is changed through the workspace's own verb, as the page's menu names it, and a member
/// is removed the same way.
#[test]
fn changing_a_role_and_removing_a_member_are_the_workspaces_verbs() {
    let (host, view) = access_page();
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    // The role menu names each role a member may be given; choosing one is the change.
    let editor = crate::fixtures::labelled(view.tree(), "ui.MenuItem", "Editor");
    view.fire_answering(&host, &me(), &editor, "choose")
        .expect("the role is chosen");
    let asked = sent.last_args_on("tilo.workspace", "set-role");
    for (named, value) in [
        ("person", Value::text("cy")),
        ("role", Value::text("editor")),
    ] {
        assert_eq!(
            asked
                .iter()
                .find(|(asked, _)| asked == named)
                .map(|(_, value)| value.clone()),
            Some(value),
            "the role change names {named}: {asked:?}"
        );
    }

    let remove = crate::fixtures::labelled(view.tree(), "ui.Button", "Remove Cy");
    view.press_answering(&host, &me(), &remove)
        .expect("the member is removed");
    let asked = sent.last_args_on("tilo.workspace", "remove-member");
    assert_eq!(
        asked
            .iter()
            .find(|(named, _)| named == "person")
            .map(|(_, value)| value.clone()),
        Some(Value::text("cy")),
        "the removal names the member: {asked:?}"
    );
}

/// The permission rows say what each grants and to whom, and taking one back is the workspace's
/// own verb; a kind of file's opener moves to the next app through the workspace's own verbs.
#[test]
fn permissions_and_default_apps_change_through_the_workspaces_verbs() {
    let (host, view) = access_page();
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    let shown = texts(&view);
    for words in ["Permissions", "Given by Bo", "Files"] {
        assert!(
            shown.iter().any(|text| text.contains(words)),
            "{words} shows under Permissions: {shown:?}"
        );
    }
    let take_back = crate::fixtures::labelled(view.tree(), "ui.Button", "Take it back");
    view.press_answering(&host, &me(), &take_back)
        .expect("the grant is taken back");
    let asked = sent.last_args_on("tilo.workspace", "revoke");
    assert_eq!(
        asked
            .iter()
            .find(|(named, _)| named == "id")
            .map(|(_, value)| value.clone()),
        Some(Value::text("grant-1"))
    );

    let everyone = crate::fixtures::labelled(view.tree(), "ui.Button", "Everyone");
    view.press_answering(&host, &me(), &everyone)
        .expect("the workspace's opener moves");
    let asked = sent.last_args_on("tilo.workspace", "next-default");
    assert_eq!(
        asked
            .iter()
            .find(|(named, _)| named == "resource")
            .map(|(_, value)| value.clone()),
        Some(Value::text("ext:pdf"))
    );
    let mine = crate::fixtures::labelled(view.tree(), "ui.Button", "Mine");
    view.press_answering(&host, &me(), &mine)
        .expect("the person's opener moves");
    assert_eq!(
        sent.called(),
        (
            "tilo.workspace".to_owned(),
            "next-personal-default".to_owned()
        )
    );
}
