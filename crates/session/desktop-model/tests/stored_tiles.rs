//! What a build wrote of its windows opens in this one as the same windows.
//!
//! A tile is stored as its JSON wherever a layout is kept: a page's own layout document
//! (`tile_json`), the layout a workspace shares (each window's `tab`), and the steps a session
//! keeps of a layout it could not write yet (a document of the device's store, each step a [`Workspace`]).
//! The bytes under `golden/tiles/` were written by the build before the variants retired in
//! place left the desktop's wire enums: one workspace holding a window of every kind a tile is
//! stored as. Each reads here as the same window, and a tile written again is the same JSON,
//! so a change to how a tile is written fails here instead of in a layout someone kept.

use automerge::AutoCommit;
use tilo_desktop_model::{AppKind, Layout, TileContent, Workspace, WorkspaceId};
use tilo_desktop_model::{desktop_doc, shared_resource};

/// The windows the build before wrote, in their order, as this build reads them.
fn written() -> Vec<(u64, TileContent)> {
    vec![
        (
            30,
            TileContent::AppPlaceholder {
                kind: AppKind::Terminal,
                label: "Terminal".into(),
            },
        ),
        (
            31,
            TileContent::Stub {
                kind: AppKind::Other("so.tilo.gone".into()),
                label: "Gone".into(),
            },
        ),
        (
            32,
            TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
        ),
        (
            33,
            TileContent::HostedWindow {
                app_id: "xterm".into(),
                label: "xterm".into(),
                host: "ab".repeat(32),
                host_label: "Laptop".into(),
            },
        ),
    ]
}

fn identity() -> WorkspaceId {
    WorkspaceId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("the fixture's workspace")
}

fn golden(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/golden/tiles/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("golden bytes");
    hex::decode(text.split_whitespace().collect::<String>()).expect("hex")
}

fn tiles(workspace: &Workspace) -> Vec<(u64, TileContent)> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab.tile.clone()))
        .collect()
}

fn the_workspace(tree: &Layout) -> &Workspace {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.id() == identity())
        .expect("the workspace the layout was written with")
}

tilo_test_env::scenario! {
    fn every_kind_of_window_the_build_before_wrote_opens_as_itself;

    phase a_pages_own_layout_document;
    {
        let doc = AutoCommit::load(&golden("layout-document.hex")).expect("a layout document");
        let mut tree = Layout::default();
        desktop_doc::rebuild(&doc, &mut tree);
        assert_eq!(tiles(the_workspace(&tree)), written());
    }

    phase the_layout_a_workspace_shares;
    {
        let doc = AutoCommit::load(&golden("shared-layout.hex")).expect("a shared layout");
        let desktop = shared_resource::read(&doc, None).expect("the shared layout reads");
        assert_eq!(tiles(the_workspace(&desktop.tree)), written());
    }

    phase a_layout_step_a_session_kept;
    {
        let path = format!(
            "{}/tests/golden/tiles/workspace.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(path).expect("the kept workspace");
        let workspace: Workspace = serde_json::from_str(&text).expect("a kept step's workspace");
        assert_eq!(workspace.id(), identity());
        assert_eq!(tiles(&workspace), written());

        // Written again, each tile is the JSON the build before wrote.
        let kept: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        let kept_tiles: Vec<&serde_json::Value> = kept["strips"][0]["window_groups"]
            .as_array()
            .expect("groups")
            .iter()
            .map(|group| &group["tabs"][0]["tile"])
            .collect();
        let again: Vec<serde_json::Value> = written()
            .iter()
            .map(|(_, tile)| serde_json::to_value(tile).expect("a tile writes"))
            .collect();
        assert_eq!(kept_tiles, again.iter().collect::<Vec<_>>());
    }
}
