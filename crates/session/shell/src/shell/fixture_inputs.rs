//! Explicit owned inputs for CPU and GPU shell fixtures.

pub struct ShellFixtureInputs {
    pub(crate) hosted: super::hosted_inputs::HostedTileInputs,
    pub(crate) text_renderer: ui::text_render::TextRenderer,
    pub(crate) theme: ui::theme::Theme,
}

impl ShellFixtureInputs {
    /// The owner supplies a private absolute temporary parent and a controlled renderer.
    /// # Errors
    /// Returns errors creating the owned Files child; never falls back to ambient paths.
    pub fn new(
        fixture_parent: &std::path::Path,
        text_renderer: ui::text_render::TextRenderer,
        theme: ui::theme::Theme,
    ) -> std::io::Result<Self> {
        Ok(Self {
            hosted: super::hosted_inputs::HostedTileInputs::fixture_in(fixture_parent)?,
            text_renderer,
            theme,
        })
    }
}

#[derive(Debug)]
pub enum ShellFixtureError {
    InvalidTree(&'static str),
    Hosted(super::hosted_inputs::HostedTileError),
}
impl std::fmt::Display for ShellFixtureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTree(reason) => write!(formatter, "Invalid shell fixture tree: {reason}"),
            Self::Hosted(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for ShellFixtureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Hosted(error) => Some(error),
            Self::InvalidTree(_) => None,
        }
    }
}

pub(crate) fn validate(
    tree: &super::state::WorkspaceTree,
    inputs: &super::hosted_inputs::HostedTileInputs,
) -> Result<(), ShellFixtureError> {
    use super::state::{AppKind, TileContent};
    if tree.persist_path.is_some() || tree.fs_acl_path.is_some() {
        return Err(ShellFixtureError::InvalidTree(
            "persistence and filesystem ACL paths must be absent",
        ));
    }
    if !inputs.is_fixture() || inputs.files.is_none() || inputs.file_services().is_none() {
        return Err(ShellFixtureError::Hosted(
            super::hosted_inputs::HostedTileError::RequiredServiceMissing(
                "explicit fixture Files inputs/services",
            ),
        ));
    }
    for workspace in &tree.desktop.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let supported = match &tab.tile {
                        TileContent::AppPlaceholder {
                            kind: AppKind::Terminal | AppKind::FileExplorer,
                            ..
                        } => true,
                        TileContent::AppPlaceholder {
                            kind: AppKind::Other(name),
                            ..
                        } => ["Notes", "Editor", "Word"]
                            .iter()
                            .any(|allowed| name.eq_ignore_ascii_case(allowed)),
                        _ => false,
                    };
                    if !supported {
                        return Err(ShellFixtureError::InvalidTree("unsupported fixture tile"));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;
    use crate::shell::{
        frame::FrameBuilder, state::WorkspaceTree, test_support::DenyAmbientStarts,
    };

    fn inputs(parent: &std::path::Path) -> ShellFixtureInputs {
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog).unwrap();
        ShellFixtureInputs::new(parent, renderer, ui::theme::Theme::dark()).unwrap()
    }

    #[test]
    fn fixture_overlays_start_no_ambient_services_and_honour_unavailable_overrides() {
        fn fixture_constructor_and_actual_overlay_actions_start_no_ambient_services() {
            let parent = tempfile::tempdir().unwrap();
            let inputs = inputs(parent.path());
            let guard = DenyAmbientStarts::new();
            let mut frame = FrameBuilder::with_fixture_inputs(
                WorkspaceTree::bootstrap_demo(),
                (3840, 2160),
                inputs,
            )
            .unwrap();
            assert_eq!(
                frame.host().fixture_provider_ids(),
                vec!["actions", "notes", "editor", "records"]
            );
            assert_eq!(frame.chrome().active_local_user(), "shell-fixture");
            assert!(!frame.paint().encoding().is_empty());
            let mut document = automerge::AutoCommit::new();
            frame.pump_apps(&mut document, &automerge::ROOT);
            click(&mut frame, "topbar_tray_pill", &mut document);
            assert!(frame.chrome().overlays.quick_settings);
            click(&mut frame, "topbar_tray_pill", &mut document);
            assert!(!frame.chrome().overlays.quick_settings);
            click(&mut frame, "topbar_overview_button", &mut document);
            assert!(frame.chrome().overlays.overview);
            click(&mut frame, "topbar_overview_button", &mut document);
            assert!(!frame.chrome().overlays.overview);
            rebuild(&mut frame, &mut document);
            assert_eq!(guard.attempts(), 0);
        }

        fn fixture_projection_unavailable_never_starts_worker() {
            let guard = DenyAmbientStarts::new();
            let mut controller = crate::shell::projection::ProjectionController::unavailable();
            controller.refresh();
            controller.connect_selected();
            controller.disconnect();
            assert!(matches!(
                controller.status,
                crate::shell::projection::ProjectionStatus::Error(_)
            ));
            assert!(controller.devices.is_empty());
            assert_eq!(guard.attempts(), 0);
        }

        fn fixture_unavailable_qs_overrides_every_dispatcher_default() {
            use os_actions::Dispatcher;
            let actions = crate::shell::quick_settings::actions::UnavailableQsActions;
            let results = [
                actions.set_brightness(0.0),
                actions.set_volume(0.0),
                actions.set_wifi(false),
                actions.set_bluetooth(false),
                actions.set_wired(false),
                actions.set_airplane_mode(false),
                actions.set_hotspot(false),
                actions.set_mobile_broadband(false),
                actions.set_vpn_connection("fixture", false),
                actions.connect_wifi("fixture", None),
                actions.set_power_profile(os_actions::PowerProfile::Balanced),
                actions.toggle_mute(),
                actions.set_mute(false),
            ];
            assert!(results.iter().all(Result::is_err));
        }

        fn fixture_ambient_guard_trips_before_the_following_operation() {
            let guard = DenyAmbientStarts::new();
            let following = std::sync::atomic::AtomicUsize::new(0);
            let result = std::panic::catch_unwind(|| {
                crate::shell::test_support::ambient_start("guard-self-test");
                following.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            assert!(result.is_err());
            assert_eq!(following.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(guard.attempts(), 1);
        }

        fixture_constructor_and_actual_overlay_actions_start_no_ambient_services();
        fixture_projection_unavailable_never_starts_worker();
        fixture_unavailable_qs_overrides_every_dispatcher_default();
        fixture_ambient_guard_trips_before_the_following_operation();
    }

    // Same cursor/press/release wire sequence as redraw_bench::dispatch_click.
    fn click(frame: &mut FrameBuilder, target: &str, document: &mut automerge::AutoCommit) {
        use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};
        let center = frame
            .widget_registry()
            .get(target)
            .expect("real widget target")
            .center();
        let mut focus = None;
        let cursor = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(center.x, center.y),
        };
        let _ = frame.dispatch_event(
            &cursor,
            &Modifiers::default(),
            document,
            &automerge::ROOT,
            &mut focus,
        );
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        let pressed = frame.dispatch_event(
            &press,
            &Modifiers::default(),
            document,
            &automerge::ROOT,
            &mut focus,
        );
        let released = frame.dispatch_event(
            &release,
            &Modifiers::default(),
            document,
            &automerge::ROOT,
            &mut focus,
        );
        assert!(pressed || released, "{target} handles click");
        frame.pump_apps(document, &automerge::ROOT);
        frame.relayout();
        assert!(!frame.paint().encoding().is_empty());
    }

    fn rebuild(frame: &mut FrameBuilder, document: &mut automerge::AutoCommit) {
        let original = frame.chrome().tree().desktop.active;
        frame.with_chrome(|chrome| {
            chrome.add_workspace("Fixture room");
        });
        frame.pump_apps(document, &automerge::ROOT);
        frame.relayout();
        frame.with_chrome(|chrome| {
            chrome.tree_mut().set_active(original);
        });
        frame.pump_apps(document, &automerge::ROOT);
        frame.relayout();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn fixture_workspaces_and_rebuilds_keep_explicit_records_and_reject_persistence() {
        fn fixture_empty_workspaces_use_same_root_without_app_services() {
            let parent = tempfile::tempdir().unwrap();
            let inputs = inputs(parent.path());
            let guard = DenyAmbientStarts::new();
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.desktop.workspaces[0].name = "Home".into();
            tree.append_empty_workspace("Work");
            tree.append_empty_workspace("Play");
            let mut frame = FrameBuilder::with_fixture_inputs(tree, (3840, 2160), inputs).unwrap();
            let mut document = automerge::AutoCommit::new();
            frame.pump_apps(&mut document, &automerge::ROOT);
            assert_eq!(frame.chrome().tree().desktop.workspaces.len(), 3);
            assert!(frame.chrome().records().open_records().is_empty());
            assert_eq!(
                frame.host().fixture_provider_ids(),
                vec!["actions", "notes", "editor", "records"]
            );
            assert!(!frame.paint().encoding().is_empty());
            click(&mut frame, "topbar_tray_pill", &mut document);
            click(&mut frame, "topbar_tray_pill", &mut document);
            click(&mut frame, "topbar_overview_button", &mut document);
            click(&mut frame, "topbar_overview_button", &mut document);
            assert_eq!(guard.attempts(), 0);
        }

        fn fixture_rebuild_keeps_explicit_actor_records_and_services() {
            use crate::shell::{
                crdt_text::CrdtTextBody, notes_app::BODY_KEY, tab_documents::tab_body,
            };
            let parent = tempfile::tempdir().unwrap();
            let inputs = inputs(parent.path());
            let child = inputs.hosted.files.as_ref().unwrap().initial_dir.clone();
            let guard = DenyAmbientStarts::new();
            let tree = WorkspaceTree::bootstrap_demo();
            let strip = tree.desktop.workspaces[0].strips[0].id;
            let note = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            let files = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[1].id;
            let mut frame = FrameBuilder::with_fixture_inputs(tree, (3840, 2160), inputs).unwrap();
            let mut document = automerge::AutoCommit::new();
            frame.pump_apps(&mut document, &automerge::ROOT);
            let store = frame.chrome().records().clone();
            // A fixture may describe existing tabs, but constructing it must not
            // fabricate document authority for every persisted sibling. Seed the
            // one record this test explicitly owns before exercising its Notes
            // operation and rebuild custody.
            store
                .create_record(&crate::shell::tab_documents::scratch_tab_record(note))
                .expect("fixture explicitly creates its Notes record");
            // The actual Notes CRDT operation edits the actual tab-bound record.
            store
                .with(&ui::record::RecordId::for_tab(note), |doc| {
                    let mut body = CrdtTextBody::new(BODY_KEY);
                    body.sync(doc, &automerge::ROOT);
                    body.splice(doc, &automerge::ROOT, 0..0, "Retained fixture note");
                })
                .expect("actual Notes tab record bound");
            frame.pump_apps(&mut document, &automerge::ROOT);
            assert_eq!(
                tab_body(&store, note, BODY_KEY).as_deref(),
                Some("Retained fixture note")
            );
            let room_index = frame.host().room_records();
            let home = frame
                .chrome()
                .tree()
                .desktop
                .active_workspace()
                .unwrap()
                .room;
            let record = crate::shell::tab_documents::scratch_tab_record(note);
            room_index
                .borrow_mut()
                .seed_observed([crate::shell::room_records::ObservedRecord {
                    id: record.id.clone(),
                    kind: record.kind.clone(),
                    home: Some(home),
                    package_id: "notes".into(),
                }]);
            assert!(
                room_index
                    .borrow_mut()
                    .reference(home, &record.id, &record.kind)
            );
            assert_records_query(&mut frame, &mut document, &record.id, &record.kind);
            rebuild(&mut frame, &mut document);
            assert_eq!(frame.chrome().active_local_user(), "shell-fixture");
            assert_eq!(
                frame
                    .chrome()
                    .tree()
                    .desktop
                    .workspaces
                    .last()
                    .unwrap()
                    .owner
                    .as_deref(),
                Some("shell-fixture")
            );
            assert!(std::rc::Rc::ptr_eq(
                &room_index,
                &frame.host().room_records()
            ));
            assert!(room_index.borrow().holds(home, &record.id));
            assert_eq!(
                tab_body(frame.chrome().records(), note, BODY_KEY).as_deref(),
                Some("Retained fixture note")
            );
            assert_rebuilt_files(&mut frame, strip, files, &child, &mut document);
            assert_eq!(
                frame.host().fixture_provider_ids(),
                vec!["actions", "notes", "editor", "records"]
            );
            assert_eq!(guard.attempts(), 0);
        }

        fn fixture_directory_lives_through_rebuild_and_last_chrome_clone() {
            let parent = tempfile::tempdir().unwrap();
            let inputs = inputs(parent.path());
            let child = inputs.hosted.files.as_ref().unwrap().initial_dir.clone();
            assert_eq!(child.parent(), Some(parent.path()));
            let guard = DenyAmbientStarts::new();
            let mut frame = FrameBuilder::with_fixture_inputs(
                WorkspaceTree::bootstrap_demo(),
                (3840, 2160),
                inputs,
            )
            .unwrap();
            let last = frame.chrome_handle();
            rebuild(&mut frame, &mut automerge::AutoCommit::new());
            assert!(child.is_dir());
            drop(frame);
            assert!(child.is_dir());
            drop(last);
            assert!(
                !child.exists(),
                "last Chrome drop releases child without removing parent"
            );
            assert!(parent.path().is_dir());
            assert_eq!(guard.attempts(), 0);
        }

        fn fixture_rejects_persistent_or_unsupported_tree_before_start() {
            use crate::shell::state::{AppKind, TileContent};
            let parent = tempfile::tempdir().unwrap();
            let guard = DenyAmbientStarts::new();
            for variant in 0..7 {
                let mut tree = WorkspaceTree::bootstrap_demo();
                match variant {
                    0 => tree.persist_path = Some(parent.path().join("state.json")),
                    1 => tree.fs_acl_path = Some(parent.path().join("acl.json")),
                    _ => {
                        tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile =
                            match variant {
                                2 => TileContent::AppPlaceholder {
                                    kind: AppKind::Browser,
                                    label: "Browser".into(),
                                },
                                3 => TileContent::Foreign {
                                    app_id: "foreign".into(),
                                    surface_id: 1,
                                },
                                4 => TileContent::LocalApp {
                                    app_id: "local".into(),
                                    label: "Local".into(),
                                    note: String::new(),
                                },
                                5 => TileContent::PeerWindow {
                                    app_id: "peer".into(),
                                    label: "Peer".into(),
                                    note: String::new(),
                                    missing: None,
                                },
                                _ => TileContent::AppPlaceholder {
                                    kind: AppKind::Other("live".into()),
                                    label: "Unsupported".into(),
                                },
                            }
                    }
                }
                assert!(matches!(
                    FrameBuilder::with_fixture_inputs(tree, (3840, 2160), inputs(parent.path())),
                    Err(ShellFixtureError::InvalidTree(_))
                ));
            }
            let mut missing = inputs(parent.path());
            missing.hosted.files = None;
            assert!(matches!(
                FrameBuilder::with_fixture_inputs(
                    WorkspaceTree::bootstrap_demo(),
                    (3840, 2160),
                    missing
                ),
                Err(ShellFixtureError::Hosted(_))
            ));
            assert_eq!(guard.attempts(), 0);
        }

        fixture_empty_workspaces_use_same_root_without_app_services();
        fixture_rebuild_keeps_explicit_actor_records_and_services();
        fixture_directory_lives_through_rebuild_and_last_chrome_clone();
        fixture_rejects_persistent_or_unsupported_tree_before_start();
    }

    fn assert_records_query(
        frame: &mut FrameBuilder,
        document: &mut automerge::AutoCommit,
        record_id: &ui::record::RecordId,
        record_kind: &ui::record::RecordKind,
    ) {
        frame.host_mut().set_palette_query("tab");
        frame
            .host_mut()
            .refresh_search_results_for_shots(document, &automerge::ROOT);
        let catalog = frame.host().action_catalog(
            "tab",
            &frame.chrome(),
            document,
            &automerge::ROOT,
            &crate::shell::search::action_api::AllVisible,
        );
        assert!(
            catalog.contains(&crate::shell::search::action_api::ResourceRef::new(
                "records",
                record_id.as_str(),
            )),
            "actual Records provider returns indexed tab"
        );
        let title = crate::shell::room_records::record_title(record_id, record_kind);
        assert!(
            frame
                .host()
                .palette_rows_detailed()
                .iter()
                .any(|(_, label, _)| label == &title)
        );
    }

    fn assert_rebuilt_files(
        frame: &mut FrameBuilder,
        strip: crate::shell::state::StripId,
        files_tab: u64,
        child: &std::path::Path,
        document: &mut automerge::AutoCommit,
    ) {
        use ui::widget::{TileContent, Widget};
        let group = frame.host_mut().group_at_for_tests(strip, 0).unwrap();
        assert!(group.set_active_tab(files_tab));
        let host = group.active_host_mut().unwrap();
        host.sync(document, &automerge::ROOT);
        let TileContent::App(app) = host.content_mut() else {
            panic!("actual Files app");
        };
        let files = app
            .as_any_mut()
            .unwrap()
            .downcast_mut::<file_explorer::files_app::FileExplorerWidget>()
            .unwrap();
        files.refresh();
        let entries = files.displayed_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Welcome.txt");
        assert_eq!(entries[0].path, child.join("Welcome.txt"));
        assert!(
            files
                .open_entry(0, &mut file_explorer::files_app::SystemRunner)
                .unwrap()
        );
        assert_eq!(files.status_line(), Some("Document opening is unavailable"));
    }
}
