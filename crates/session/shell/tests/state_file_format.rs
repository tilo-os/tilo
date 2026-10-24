//! `state.json` is the one durable copy of a person's desktop. Whatever the
//! Rust shape behind it, a file this build wrote must load and write back
//! byte-for-byte the same document — otherwise an upgrade silently rewrites
//! or drops somebody's layout.

use std::path::PathBuf;

use shell::shell::state::{
    Access, AppKind, CapOverride, NativeDocumentReference, RoomId, Scope, Strip, StripId, Subject,
    Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId, WorkspaceTree,
};

const FIXTURE: &str = "tests/fixtures/state_v1.json";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

fn room(n: u8) -> RoomId {
    RoomId::parse(&format!("00000000-0000-4000-8000-0000000000{n:02x}")).expect("room uuid")
}

fn tab(id: u64, title: &str, content: TileContent) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: content,
        app_title: None,
    }
}

/// A saved shortcut, and one of its schedule's fire times claimed.
fn saved_shortcuts() -> (
    Vec<tilo_automation::Shortcut>,
    Vec<tilo_automation::ScheduleClaim>,
) {
    (
        serde_json::from_str(r#"[{"id":"tidy","name":"Tidy","steps":[]}]"#).unwrap(),
        vec![tilo_automation::ScheduleClaim {
            shortcut: "tidy".into(),
            trigger: 0,
            due_ms: 1_790_326_800_000,
            device: "laptop".into(),
        }],
    )
}

/// Every persisted field holding something, including the integer-keyed
/// maps a JSON object can only carry as string keys.
#[allow(
    clippy::too_many_lines,
    reason = "the fixture sets every persisted field once, read top to bottom"
)]
fn populated_tree() -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let mut ws = Workspace::in_room(WorkspaceId(1), room(1), "Studio");
    ws.set_owner("owner-key");
    ws.grant_sandbox(
        CapOverride::grant(
            Subject::app("org.example.Editor"),
            Scope::FsPath("/srv/doc".into()),
            Access::Write,
        )
        .with_id("cap-fixed"),
    );
    ws.share_surface(7, 42);
    (ws.shortcuts, ws.schedule_claims) = saved_shortcuts();
    let mut strip = Strip::new(StripId(2), "Main");
    strip.window_groups.push(WindowGroup {
        id: 3,
        tabs: vec![
            tab(
                4,
                "Terminal",
                TileContent::AppPlaceholder {
                    kind: AppKind::Terminal,
                    label: "Terminal".into(),
                },
            ),
            tab(
                5,
                "Gimp",
                TileContent::LocalApp {
                    app_id: "org.gimp.GIMP".into(),
                    label: "GIMP".into(),
                    note: String::new(),
                },
            ),
        ],
        active_tab: 5,
        width: WindowWidth::Fraction(420),
        maximized: true,
        focus_history: Vec::new(),
    });
    strip.window_groups.push(WindowGroup {
        id: 6,
        tabs: vec![tab(
            8,
            "Files",
            TileContent::Stub {
                kind: AppKind::Other("Files".into()),
                label: "Files".into(),
            },
        )],
        active_tab: 8,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    ws.strips.push(strip);
    tree.desktop.workspaces.push(ws);
    let mut joined = Workspace::in_room(WorkspaceId(9), room(2), "Joined");
    joined.strips.push(Strip::new(StripId(10), "Shared"));
    tree.desktop.workspaces.push(joined);

    tree.desktop.active = WorkspaceId(1);
    tree.desktop.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.desktop
        .active_strip
        .insert(WorkspaceId(9), StripId(10));
    tree.desktop.active_group.insert(StripId(2), 6);
    // What the shell's own room engine kept: no longer read, still written
    // back exactly as the file had it.
    for (key, value) in [
        (
            "room_authority",
            serde_json::json!({
                room(2).to_string(): { "Peer": "iroh:peer-endpoint" },
                room(3).to_string(): "Unpinned",
            }),
        ),
        (
            "room_peer_names",
            serde_json::json!({ room(2).to_string(): { "peer-endpoint": "Ada" } }),
        ),
        ("room_peer_identities", serde_json::json!({})),
        (
            "room_share_floor",
            serde_json::json!({ room(2).to_string(): 11 }),
        ),
        // Rooms another device's continuation once made workspaces for.
        (
            "continuation_rooms",
            serde_json::json!([room(3).to_string()]),
        ),
    ] {
        tree.retired_fields.insert(key.into(), value);
    }
    tree.continuation_tabs.insert(5);
    tree.continuation_owned_tabs.insert(5, room(3));
    tree.continuation_closed_tabs.insert(12, room(3));
    tree.native_document_tabs.insert(
        8,
        NativeDocumentReference {
            provider_id: "org.example.Editor".into(),
            path: "/srv/doc/notes.md".into(),
        },
    );
    tree
}

fn canonical(json: &str) -> serde_json::Value {
    serde_json::from_str(json).expect("state json")
}

#[test]
fn state_file_fixture_is_current_and_round_trips_unchanged() {
    // Phase `state_file_fixture_is_current`.
    // Rewrite the checked-in fixture from the current shape. Only for a
    // deliberate, reviewed format change; a refactor must never need it.
    {
        let path = fixture_path();
        let written = serde_json::to_string_pretty(&populated_tree()).expect("serialize");
        if std::env::var_os("TILO_BLESS_STATE_FIXTURE").is_some() {
            std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("mkdir");
            std::fs::write(&path, format!("{written}\n")).expect("write fixture");
        }
        let fixture = std::fs::read_to_string(&path).expect("read the state.json fixture");
        assert_eq!(
            canonical(&written),
            canonical(&fixture),
            "the persisted state shape changed; a changed state.json format needs a \
             deliberate migration, not a refactor"
        );
    }

    // Phase `state_file_round_trips_unchanged`.
    {
        let fixture = std::fs::read_to_string(fixture_path()).expect("read the state.json fixture");
        let tree: WorkspaceTree = serde_json::from_str(&fixture).expect("fixture loads");
        let rewritten = serde_json::to_string_pretty(&tree).expect("serialize");
        assert_eq!(canonical(&rewritten), canonical(&fixture));
    }

    // Phase `a_file_from_when_workspaces_kept_tasks_loads_without_them`.
    {
        let fixture = canonical(
            &std::fs::read_to_string(fixture_path()).expect("read the state.json fixture"),
        );
        let mut old = fixture.clone();
        old["workspaces"][0]["tasks"] = serde_json::json!([{
            "id": "task-1",
            "title": "Review",
            "created_by": "owner-key",
            "assigned_agent": null,
            "route": "manual",
            "status": "queued",
            "resource": null,
            "created_at": 10,
            "updated_at": 20,
            "audit": []
        }]);
        let tree: WorkspaceTree = serde_json::from_value(old).expect("an older file loads");
        assert_eq!(serde_json::to_value(&tree).expect("serialize"), fixture);
    }
}
