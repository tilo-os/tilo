//! Tiles name their app; tabs in a room document can name their record.

use automerge::AutoCommit;
use tilo_desktop_model::apps::{BindRecordError, bind_tab_record, canonical_app_id, record_routes};
use tilo_desktop_model::projection::{EmptyStrips, project_owned_contents};
use tilo_desktop_model::{
    AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, compose_id,
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
        assert_eq!(canonical_app_id(&TileContent::WorkspaceActivity), None);
    }
    // Phase `a_bound_record_is_routed_and_cannot_be_rebound`.
    {
        let salt = 7;
        let (strip_id, group_id, tab_id) = (
            compose_id(salt, 1),
            compose_id(salt, 2),
            compose_id(salt, 3),
        );
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
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut doc = AutoCommit::new();
        project_owned_contents(&mut doc, &[strip], salt, None, EmptyStrips::Skip).unwrap();
        assert!(record_routes(&doc).is_empty());

        bind_tab_record(&mut doc, StripId(strip_id), group_id, tab_id, "rec-1").unwrap();
        bind_tab_record(&mut doc, StripId(strip_id), group_id, tab_id, "rec-1").unwrap();
        assert_eq!(
            record_routes(&doc).get(&tab_id).map(String::as_str),
            Some("rec-1")
        );
        assert!(matches!(
            bind_tab_record(&mut doc, StripId(strip_id), group_id, tab_id, "rec-2"),
            Err(BindRecordError::AlreadyBound)
        ));
        assert!(matches!(
            bind_tab_record(&mut doc, StripId(strip_id), group_id, 12345, "rec-1"),
            Err(BindRecordError::MissingTab)
        ));
    }
    // Phase `a_builtin_label_answers_to_its_package_when_one_exists`.
    {
        assert_eq!(AppKind::Other("Notes".into()).app_id(), "so.tilo.notes");
        assert_eq!(
            AppKind::Other("Files".into()).app_id(),
            AppKind::FileExplorer.app_id()
        );
        assert_eq!(AppKind::Other("Editor".into()).app_id(), "org.tilo.Editor");
        assert_eq!(
            AppKind::Other("org.gimp.GIMP".into()).app_id(),
            "org.gimp.GIMP"
        );
    }
}
