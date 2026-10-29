//! Tiles name their app; tabs in a room document can name their record.

use automerge::AutoCommit;
use tilo_desktop_model::apps::{canonical_app_id, record_routes};
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::{
    AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, compose_id, shared_record,
};

#[test]
fn app_tiles_have_one_canonical_id_that_routes_and_labels_them() {
    // Phase `every_app_tile_has_one_canonical_id`.
    {
        let terminal = TileContent::Stub {
            kind: AppKind::Terminal,
            label: "Terminal".into(),
        };
        assert_eq!(
            canonical_app_id(&terminal),
            Some("so.tilo.terminal".to_owned())
        );
        let foreign = TileContent::Foreign {
            app_id: "org.gimp.GIMP".into(),
            surface_id: 3,
        };
        assert_eq!(canonical_app_id(&foreign).as_deref(), Some("org.gimp.GIMP"));
    }
    // Phase `a_bound_record_is_routed`.
    {
        let salt = 7;
        let (strip_id, group_id, tab_id) = (
            compose_id(salt, 1),
            compose_id(salt, 2),
            compose_id(salt, 3),
        );
        let mut workspace = Workspace::new(WorkspaceId(compose_id(salt, 4)), "Home");
        let mut strip = Strip::new(StripId(strip_id), "Main");
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "org.example.Notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        let mut desktop = SharedDesktop {
            tree: DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            ..SharedDesktop::default()
        };
        let mut doc = AutoCommit::new();
        shared_record::write(&mut doc, &desktop).unwrap();
        assert!(record_routes(&doc).is_empty());

        desktop.records.push((tab_id, "rec-1".into()));
        shared_record::write(&mut doc, &desktop).unwrap();
        assert_eq!(
            record_routes(&doc).get(&tab_id).map(String::as_str),
            Some("rec-1")
        );
    }
    // Phase `a_builtin_label_answers_to_its_package_when_one_exists`.
    {
        assert_eq!(AppKind::Other("Notes".into()).app_id(), "so.tilo.notes");
        assert_eq!(
            AppKind::Other("Files".into()).app_id(),
            AppKind::FileExplorer.app_id()
        );
        assert_eq!(
            AppKind::Other("Scratch".into()).app_id(),
            "org.tilo.Scratch"
        );
        assert_eq!(
            AppKind::Other("org.gimp.GIMP".into()).app_id(),
            "org.gimp.GIMP"
        );
    }
}
