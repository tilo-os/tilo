//! Native platform entry points share the shell and UI libraries.

#[cfg(target_os = "linux")]
mod linux_main;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux_main::run()
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
    use shell::compositor::macos::{Options, prepare};
    use ui::a11y::AccessNode;
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};

    fn find_semantic<'a>(
        node: &'a ui::headless::InspectionNode,
        name: &str,
    ) -> Option<&'a AccessNode> {
        fn in_access<'a>(node: &'a AccessNode, name: &str) -> Option<&'a AccessNode> {
            if node.name.as_deref() == Some(name) {
                return Some(node);
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, name))
        }
        node.semantics
            .as_ref()
            .and_then(|access| in_access(access, name))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_semantic(child, name))
            })
    }

    fn find_semantic_description<'a>(
        node: &'a ui::headless::InspectionNode,
        description: &str,
    ) -> Option<&'a AccessNode> {
        fn in_access<'a>(node: &'a AccessNode, description: &str) -> Option<&'a AccessNode> {
            if node.description.as_deref() == Some(description) {
                return Some(node);
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, description))
        }
        node.semantics
            .as_ref()
            .and_then(|access| in_access(access, description))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_semantic_description(child, description))
            })
    }

    fn click_rendered_file_row(driver: &mut ui::headless::Driver, name: &str) {
        let inspection = driver.inspection();
        let node = find_semantic(&inspection.root, name)
            .unwrap_or_else(|| panic!("{name} semantic node is absent"));
        let bounds = node
            .bounds
            .unwrap_or_else(|| panic!("{name} semantic node has no bounds"))
            .to_rect();
        assert!(
            bounds.area() > 0.0,
            "{name} needs a nonempty pointer target"
        );
        let center = bounds.center();
        driver.click_at(center.x, center.y);
    }

    fn files_driver(
        root: &std::path::Path,
    ) -> (
        shell::shell::state::WorkspaceTreeHandle,
        ui::record::RecordStore,
        ui::headless::Driver,
    ) {
        let files = root.join("Documents");
        std::fs::create_dir(&files).unwrap();
        for index in 0..80 {
            std::fs::write(
                files.join(format!("Document-{index:03}.txt")),
                format!("Fixture document {index}\n"),
            )
            .unwrap();
        }
        let mut session = prepare(&Options {
            state_dir: root.join("state"),
            files_dir: files,
            shots: None,
        })
        .unwrap();
        shell::shell::window_actions::open_hosted(
            &mut session.chrome.borrow_mut(),
            shell::shell::state::AppKind::FileExplorer,
            "Files",
            shell::shell::window_actions::Destination::Beside,
        );
        assert!(
            session.host.tick_workspace_content_for_tests(),
            "the out-of-band fixture open must reconcile into the hosted desktop"
        );
        let tree = session.tree.clone();
        let records = session.records.clone();
        let driver =
            ui::headless::Driver::new(ui::State::new(), Box::new(session.host), (1280, 800));
        (tree, records, driver)
    }

    fn options(root: &std::path::Path) -> Options {
        Options {
            state_dir: root.join("state"),
            files_dir: root.to_path_buf(),
            shots: None,
        }
    }

    #[test]
    #[serial_test::serial]
    fn macos_session_starts_persists_and_refuses_invalid_state() {
        // Phase `help_succeeds_without_starting_a_session`.
        {
            assert_eq!(
                shell::compositor::macos::run_args(&["--help".into()]),
                std::process::ExitCode::SUCCESS
            );
        }
        // Phase `native_workspace_changes_survive_reopening`.
        {
            let directory = tempfile::tempdir().unwrap();
            let options = options(directory.path());
            {
                let session = prepare(&options).unwrap();
                assert_eq!(session.tree.read().desktop.workspaces.len(), 1);
                assert!(
                    session
                        .tree
                        .read()
                        .desktop
                        .workspaces
                        .iter()
                        .all(|ws| ws.collaborators.is_empty())
                );
                assert!(
                    prepare(&options).is_err(),
                    "a second session must not overwrite this profile"
                );
                session.tree.write().append_empty_workspace("Mac work");
                session.tree.read().queue_save();
            }
            let session = prepare(&options).unwrap();
            assert!(
                session
                    .tree
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .any(|ws| ws.name == "Mac work")
            );
        }
        // Phase `invalid_workspace_state_is_preserved_and_refused`.
        {
            let directory = tempfile::tempdir().unwrap();
            let options = options(directory.path());
            std::fs::create_dir_all(&options.state_dir).unwrap();
            let path = options.state_dir.join("state.json");
            std::fs::write(&path, "broken state").unwrap();
            assert!(prepare(&options).is_err());
            assert_eq!(std::fs::read_to_string(path).unwrap(), "broken state");
            assert!(shell::compositor::macos::parse(&["--state-dir".into()]).is_err());
            assert!(shell::compositor::macos::parse(&["--backend".into(), "drm".into()]).is_err());
        }
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn macos_input_reaches_the_palette_and_hosted_files() {
        // Phase `macos_search_shortcuts_open_the_real_shell_palette`.
        {
            let directory = tempfile::tempdir().unwrap();
            let session = prepare(&options(directory.path())).unwrap();
            let mut driver =
                ui::headless::Driver::new(ui::State::new(), Box::new(session.host), (1280, 800));
            for (modifier, key, opens) in [
                ("meta", "Space", false),
                ("alt", "Space", true),
                ("meta", "k", true),
            ] {
                driver.set_modifiers(&[modifier.into()]);
                driver.key(key);
                driver.set_modifiers(&[]);
                driver
                    .expect(
                        &ui::headless::SemanticExpectation::named("command_palette_input")
                            .expanded(opens),
                    )
                    .unwrap();
                driver.key("Escape");
                driver
                    .expect(
                        &ui::headless::SemanticExpectation::named("command_palette_input")
                            .expanded(false),
                    )
                    .unwrap();
            }
        }
        // Phase `hosted_files_receive_physical_pixel_wheel_at_each_scale`.
        {
            for (scale, physical_delta) in [(1.0, -60.0), (2.0, -120.0)] {
                let directory = tempfile::tempdir().unwrap();
                let (_tree, _records, mut driver) = files_driver(directory.path());
                driver.set_input_scale(scale).unwrap();
                let before = find_semantic(&driver.inspection().root, "Document-000.txt")
                    .and_then(|node| node.bounds)
                    .expect("first row bounds before scrolling");
                driver.send(&WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(100.0 * scale, 400.0 * scale),
                });
                driver.send(&WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                        0.0,
                        physical_delta,
                    )),
                    phase: TouchPhase::Moved,
                });
                let outside = find_semantic(&driver.inspection().root, "Document-000.txt")
                    .and_then(|node| node.bounds)
                    .expect("first row bounds after wheel outside Files");
                assert_eq!(outside.y0, before.y0, "scale {scale} outside Files");
                driver.send(&WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(600.0 * scale, 400.0 * scale),
                });
                driver.send(&WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                        0.0,
                        physical_delta,
                    )),
                    phase: TouchPhase::Moved,
                });
                let after = find_semantic(&driver.inspection().root, "Document-000.txt")
                    .and_then(|node| node.bounds)
                    .expect("first row bounds after scrolling");
                assert_eq!(after.y0 - before.y0, -60.0, "scale {scale}");
            }
        }
        // Phase `hosted_files_pointer_selection_and_enter_open_the_seeded_document`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (tree_handle, records, mut driver) = files_driver(directory.path());
            click_rendered_file_row(&mut driver, "Document-000.txt");
            assert!(
                find_semantic(&driver.inspection().root, "Document-000.txt")
                    .is_some_and(|node| node.state.selected),
                "the physical click must select the rendered Files row"
            );
            driver.key("Enter");
            let files_status = find_semantic(&driver.inspection().root, "Files activity")
                .and_then(|node| node.value.clone());
            let tree = tree_handle.read();
            let titles = tree
                .desktop
                .active_workspace()
                .unwrap()
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .map(|tab| tab.title.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                titles,
                vec!["Files", "Document-000.txt"],
                "Files status: {files_status:?}"
            );
            drop(tree);
            let inspection = driver.inspection();
            let document = find_semantic_description(&inspection.root, "org.nirios.Editor")
                .expect("the registered native Editor must expose the opened document");
            assert_eq!(document.name.as_deref(), Some("Document-000.txt"));
            assert_eq!(document.value.as_deref(), Some("Fixture document 0\n"));
            assert!(
                records.is_ready(&ui::record::RecordId::for_file(
                    directory
                        .path()
                        .join("Documents/Document-000.txt")
                        .canonicalize()
                        .unwrap()
                )),
                "the visible editor must be checked out on the file's canonical record"
            );
            drop(inspection);
            drop(driver);
            // A normal restart releases the old session's store ownership too.
            // Retaining this assertion handle deliberately keeps its writer lock.
            drop(records);
            drop(tree_handle);
            let reopened = prepare(&Options {
                state_dir: directory.path().join("state"),
                files_dir: directory.path().join("Documents"),
                shots: None,
            })
            .unwrap();
            let reference = reopened
                .tree
                .read()
                .native_document_tabs
                .values()
                .next()
                .cloned()
                .expect("the local profile keeps the hosted document identity");
            assert_eq!(reference.provider_id, "org.nirios.Editor");
            assert_eq!(reference.path.file_name().unwrap(), "Document-000.txt");
            let reopened_driver =
                ui::headless::Driver::new(ui::State::new(), Box::new(reopened.host), (1280, 800));
            let reopened_inspection = reopened_driver.inspection();
            let reopened_document =
                find_semantic_description(&reopened_inspection.root, "org.nirios.Editor")
                    .expect("normal restart restores the real hosted document");
            assert_eq!(
                reopened_document.value.as_deref(),
                Some("Fixture document 0\n")
            );
        }
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn hosted_files_show_the_real_directory_and_current_disk_content() {
        // Phase `files_tile_uses_the_supplied_real_directory`.
        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("mac-note.txt"), "real content").unwrap();
            let inputs =
                shell::shell::hosted_inputs::HostedTileInputs::for_host_directory(directory.path())
                    .unwrap();
            let tile = shell::shell::state::TileContent::AppPlaceholder {
                kind: shell::shell::state::AppKind::FileExplorer,
                label: "Files".into(),
            };
            let ui::widget::TileContent::App(mut app) = shell::shell::try_to_ui_tile(
                &tile,
                terminal::HostedProcessServices::default(),
                &inputs,
            )
            .unwrap() else {
                panic!("Files must be a real app, not a notice");
            };
            let files = app
                .as_any_mut()
                .unwrap()
                .downcast_mut::<file_explorer::FileExplorerWidget>()
                .unwrap();
            assert!(
                files
                    .entries()
                    .iter()
                    .any(|entry| entry.name == "mac-note.txt")
            );
        }
        // Phase `hosted_files_expose_each_rendered_row_with_its_own_bounds`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (_tree, _records, driver) = files_driver(directory.path());
            let inspection = driver.inspection();
            let first = find_semantic(&inspection.root, "Document-000.txt")
                .expect("the hosted Files row must reach the shell semantic tree");
            let second = find_semantic(&inspection.root, "Document-001.txt")
                .expect("the next hosted Files row must reach the shell semantic tree");
            let first = first
                .bounds
                .expect("the first rendered row needs real bounds");
            let second = second
                .bounds
                .expect("the next rendered row needs real bounds");
            assert!(first.to_rect().area() > 0.0);
            assert!(second.to_rect().area() > 0.0);
            assert!(
                first.y1 <= second.y0,
                "row bounds must not alias: {first:?} {second:?}"
            );
        }
        // Phase `hosted_files_empty_host_intent_poll_preserves_history_owner`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (_tree, _records, mut driver) = files_driver(directory.path());
            let heads = driver.state_mut().doc.get_heads();
            let inspection = driver.inspection();
            assert!(
                find_semantic(&inspection.root, "Document-000.txt").is_some(),
                "the real hosted Files draw must retain its History owner"
            );
            assert!(
                driver.targets().contains_key("history-open"),
                "the real hosted Files draw must retain its History control"
            );
            driver.click_named("history-open").unwrap();
            assert!(
                driver
                    .inspection_json()
                    .unwrap()
                    .contains("Select a file with versioned history"),
                "History must remain live after an empty host-intent poll"
            );
            assert_eq!(
                driver.state_mut().doc.get_heads(),
                heads,
                "an empty host-intent poll must not mutate the incidental outer document"
            );
        }
        // Phase `hosted_files_refuse_conflicting_document_record_without_publishing_a_tab`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (tree_handle, records, mut driver) = files_driver(directory.path());
            let path = directory
                .path()
                .join("Documents/Document-000.txt")
                .canonicalize()
                .unwrap();
            let conflicting = ui::record::RecordCheckout::new(
                ui::record::RecordId::for_file(&path),
                ui::record::RecordKind::new("another-document-kind"),
            );
            records.create_record(&conflicting).unwrap();

            click_rendered_file_row(&mut driver, "Document-000.txt");
            driver.key("Enter");
            driver.profile_frame();

            let tree = tree_handle.read();
            let titles = tree
                .desktop
                .active_workspace()
                .unwrap()
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .map(|tab| tab.title.as_str())
                .collect::<Vec<_>>();
            assert_eq!(titles, vec!["Files"]);
            assert!(tree.native_document_tabs.is_empty());
            drop(tree);

            let inspection = driver.inspection();
            let status = find_semantic(&inspection.root, "Files activity")
                .and_then(|node| node.value.as_deref())
                .unwrap_or("");
            assert!(status.contains("document storage unavailable"));
            assert!(
                find_semantic_description(&inspection.root, "org.nirios.Editor").is_none(),
                "a refused record identity must not publish an Editor document"
            );
        }
        // Phase `deleted_hosted_document_reopens_as_a_visible_unavailable_file`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (_tree, records, mut driver) = files_driver(directory.path());
            click_rendered_file_row(&mut driver, "Document-000.txt");
            driver.key("Enter");
            driver.profile_frame();
            let document_path = directory
                .path()
                .join("Documents/Document-000.txt")
                .canonicalize()
                .unwrap();
            drop(driver);
            drop(records);
            std::fs::remove_file(&document_path).unwrap();

            let reopened = prepare(&Options {
                state_dir: directory.path().join("state"),
                files_dir: directory.path().join("Documents"),
                shots: None,
            })
            .unwrap();
            let reopened_driver =
                ui::headless::Driver::new(ui::State::new(), Box::new(reopened.host), (1280, 800));
            let inspection = reopened_driver.inspection();
            let unavailable = find_semantic(&inspection.root, "Document-000.txt unavailable")
                .expect("the deleted document must reopen as a visible unavailable-file state");
            let detail = unavailable
                .description
                .as_deref()
                .expect("the unavailable state must explain the failed path");
            assert!(detail.contains(document_path.to_string_lossy().as_ref()));
            assert!(detail.contains("unavailable"));
            assert!(
                find_semantic_description(&inspection.root, "org.nirios.Editor").is_none(),
                "an unavailable file must not masquerade as editable document content"
            );
        }
        // Phase `externally_changed_hosted_document_reopens_with_current_disk_content`.
        {
            let directory = tempfile::tempdir().unwrap();
            let (tree, records, mut driver) = files_driver(directory.path());
            click_rendered_file_row(&mut driver, "Document-000.txt");
            driver.key("Enter");
            driver.profile_frame();
            records.try_flush().unwrap();
            let document_path = directory
                .path()
                .join("Documents/Document-000.txt")
                .canonicalize()
                .unwrap();
            drop(driver);
            drop(records);
            drop(tree);
            std::fs::write(&document_path, "Current external revision\n").unwrap();

            let reopened = prepare(&Options {
                state_dir: directory.path().join("state"),
                files_dir: directory.path().join("Documents"),
                shots: None,
            })
            .unwrap();
            let reopened_driver =
                ui::headless::Driver::new(ui::State::new(), Box::new(reopened.host), (1280, 800));
            let inspection = reopened_driver.inspection();
            let document = find_semantic_description(&inspection.root, "org.nirios.Editor")
                .expect("the changed file must remain a real Editor document");
            assert_eq!(document.name.as_deref(), Some("Document-000.txt"));
            assert_eq!(
                document.value.as_deref(),
                Some("Current external revision\n")
            );
            assert!(!document.state.invalid);
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    shell::compositor::macos::run()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("The Tilo shell currently supports Linux and macOS.");
    std::process::ExitCode::FAILURE
}
