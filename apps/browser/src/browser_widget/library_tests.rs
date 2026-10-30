//! The items the library keeps — browsing history, bookmarks, downloads — as
//! the actions a person, a script and an agent reach them, and the private
//! window, which keeps none of them.

use super::tests::{bookmarked, layout_at, synced};
use super::*;
use ui::tilo_actions::OperationKind;
use ui::webview::WebViewEngine;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn history_privacy_effects_are_local_truthful_and_recoverable() {
    // Phase `sensitive_local_effects_publish_truthful_contracts_and_completed_results`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut w);
        w.history.record_at("https://private.example", "Private", 1);
        let resources = [w.published()];
        let browser = &resources[0];
        let history = browser
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.history")
            .unwrap();
        let clear_history = history
            .actions
            .iter()
            .find(|action| action.id == "clear-history")
            .unwrap();
        assert_eq!(clear_history.kind, OperationKind::Effect);
        assert_eq!(
            clear_history.locus,
            ui::tilo_actions::ExecutionLocus::ClientLocal
        );
        assert_eq!(
            clear_history.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(clear_history.contract.validate_declaration().is_empty());

        let invocation = ui::tilo_actions::InvocationId::new();
        let started = w
            .admit(invocation, BrowserOperation::ClearHistory)
            .map(Some)
            .unwrap();
        let Some(ui::framework::Effected::Done(output)) = started else {
            panic!("clearing in-memory local history is synchronous");
        };
        assert!(clear_history.contract.result.validate(&output).is_ok());
        assert!(w.history.is_empty());

        let settings = browser
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.settings")
            .unwrap();
        let clear_site_data = settings
            .actions
            .iter()
            .find(|action| action.id == "request-site-data-clear")
            .unwrap();
        let started = w
            .admit(
                ui::tilo_actions::InvocationId::new(),
                BrowserOperation::RequestWebsiteDataClear,
            )
            .map(Some)
            .unwrap();
        let Some(ui::framework::Effected::Done(output)) = started else {
            panic!("the engine accepted the site-data request synchronously");
        };
        assert!(clear_site_data.contract.result.validate(&output).is_ok());
        assert_eq!(
            output.value,
            Some(serde_json::Value::String(
                "Website-data clear request sent to the local browser engine.".into()
            )),
            "the descriptor must not overclaim a backend completion it cannot observe"
        );
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::ClearWebsiteData)
        );
    }

    // Phase `history_pause_is_local_and_keeps_existing_history_searchable_and_clearable`.
    {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let (mut doc, root) = synced(&mut w);
        w.history
            .record_at("https://sensitive.example", "Sensitive", 1);
        let resources = [w.published()];
        let pause = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.session")
            .unwrap();
        assert_eq!(pause.kind, "browser.history-pause");
        let toggle = pause
            .actions
            .iter()
            .find(|action| action.id == "toggle-pause")
            .unwrap();
        assert_eq!(toggle.kind, OperationKind::Effect);
        assert_eq!(toggle.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
        assert!(toggle.contract.validate_declaration().is_empty());

        let started = w
            .admit(
                ui::tilo_actions::InvocationId::new(),
                BrowserOperation::ToggleHistoryPause,
            )
            .map(Some)
            .unwrap();
        let Some(ui::framework::Effected::Done(output)) = started else {
            panic!("history pausing changes only this running browser");
        };
        assert!(toggle.contract.result.validate(&output).is_ok());
        let resources = [w.published()];
        let history = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.history")
            .unwrap();
        assert_eq!(
            history.children.len(),
            1,
            "pausing only stops new recording"
        );
        assert!(
            history
                .actions
                .iter()
                .find(|action| action.id == "clear-history")
                .is_some_and(|action| action.contract.availability.is_available()),
            "existing durable history remains clearable while recording is paused"
        );
        w.search_index.sync(&w.history);
        assert!(
            ui::search::SearchProvider::search(&w.search_index.provider(), "Sensitive")
                .iter()
                .any(|result| result.title == "Sensitive")
        );
    }
}

/// Bookmark removal and row control identities.
#[test]
fn bookmark_rows_are_reversible_uniquely_addressed_actions() {
    // Phase `bookmark_row_controls_use_unique_normalized_identities`.
    {
        fn test_ids(widget: &dyn Widget, into: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                into.push(id.to_owned());
            }
            for child in widget.children() {
                test_ids(child, into);
            }
        }
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        w.debug_show_library("bookmarks");
        let urls = [
            "https://www.rust-lang.org/learn",
            "https://developer.mozilla.org/",
            "https://legacy.example/",
        ];
        w.present_state(&bookmarked(&urls.map(|url| (url, url))));
        let (mut doc, root) = synced(&mut w);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

        let mut ids = Vec::new();
        test_ids(&w, &mut ids);
        let removes: Vec<&String> = ids.iter().filter(|id| id.ends_with(".remove")).collect();
        assert_eq!(removes.len(), urls.len());
        for at in 0..urls.len() {
            let id = ui::tilo_resource::Id::from_raw(format!("b{at}"));
            let control = format!("{}.remove", bookmark_resource_id(&id));
            assert!(
                removes.iter().any(|id| **id == control),
                "each row needs a durable, unique semantic key: {control}"
            );
        }

        let mut semantic = ui::a11y_tree::SemanticTree::new();
        semantic.refresh(&w, &node_map, None, &[]);
        assert!(
            semantic.duplicate_ids().is_empty(),
            "bookmark controls must not collapse in the retained a11y tree"
        );

        // The composed tree is not otherwise synchronized in this layout-only
        // regression test; keep the document borrow explicitly live.
        w.sync(&doc, &root);
    }
}

/// Download portal cancellation, worker failure, duplicate ids and scheme rejection.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn downloads_cancel_reject_and_keep_stable_actions() {
    // Phase `portal_cancel_cancels_the_paused_engine_download`.
    {
        ui::host::install_default(ui::host::HostServices::recording().0);
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/archive.zip".into(),
        });
        widget.apply_engine_events();
        assert!(widget.download_picker.is_some());
        widget.apply_engine_events();
        assert_eq!(
            widget.downloads.entries()[0].status,
            DownloadStatus::Cancelled
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id } if id == "test-download-1"
        )));
    }

    // Phase `portal_worker_failure_cancels_webkit_but_preserves_the_actionable_error`.
    {
        ui::host::install_default(ui::host::HostServices::recording().0);
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/archive.zip".into(),
        });
        widget.apply_engine_events();
        let id = widget.downloads.entries()[0].id.clone();
        let (sender, disconnected) = std::sync::mpsc::channel();
        drop(sender);
        widget.download_picker = Some((id.clone(), disconnected));
        widget.poll_download_picker();
        // Fold the engine's cancellation event too: it must not replace the
        // portal failure with the much less useful generic "Cancelled".
        widget.apply_engine_events();
        let entry = widget.downloads.get(&id).unwrap();
        assert_eq!(entry.status, DownloadStatus::Failed);
        assert!(
            entry
                .error
                .as_deref()
                .is_some_and(|error| error.contains("save picker"))
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id: cancelled } if cancelled == &id
        )));
    }

    // Phase `duplicate_engine_ids_are_cancelled_without_replacing_history`.
    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        assert!(widget.downloads.requested(
            "test-download-1".into(),
            "https://persisted.test/old".into(),
            "old.bin",
        ));
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/new.bin".into(),
        });
        widget.apply_engine_events();
        assert_eq!(
            widget.downloads.get("test-download-1").unwrap().source_url,
            "https://persisted.test/old"
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id } if id == "test-download-1"
        )));
    }

    // Phase `download_operations_reject_non_web_schemes_and_materialize_stable_actions`.
    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        assert!(
            widget
                .perform_browser_operation(BrowserOperation::StartDownload {
                    url: "file:///etc/passwd".into(),
                })
                .is_err()
        );
        assert!(
            !engine
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::StartDownload { .. }))
        );

        widget.debug_show_downloads("completed");
        let resource = widget
            .resource_by_id("browser.downloads/review-complete")
            .expect("download resource");
        assert_eq!(resource.kind, "browser.download");
        assert_eq!(
            resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["read-download", "open-download", "reveal-download"]
        );
        for action in &resource.actions {
            assert_eq!(action.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
        }
    }
}

/// Private mode personal state, indicator and discovery-failure cleanup.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn private_mode_is_empty_indicated_and_leaves_no_root() {
    // Phase `private_mode_has_empty_personal_state_and_no_continuation_channel`.
    {
        let mut widget = BrowserWidget::with_engine_mode(
            TestEngine::new(),
            false,
            crate::browsing_mode::BrowsingMode::Private,
        );
        assert!(widget.history.is_empty());
        assert!(widget.bookmarks.entries().is_empty());
        assert_eq!(widget.settings, Settings::default());
        widget
            .history
            .record_at("https://private.test", "Private", 1);
        widget.clear_history_durably().unwrap();
        assert!(widget.history.is_empty());
        let mut document = AutoCommit::new();
        assert!(
            widget
                .continuation_metadata(&document, &automerge::ROOT)
                .is_none()
        );
        let metadata = ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
            bytes: br#"{"url":"https://normal.test"}"#.to_vec(),
        };
        assert!(!widget.restore_continuation_metadata(
            &metadata,
            &mut Edits::lend(&mut document),
            &automerge::ROOT,
        ));
    }

    // Phase `private_indicator_action_stays_visible_in_compact_chrome`.
    {
        let mut widget = BrowserWidget::with_engine_mode(
            TestEngine::new(),
            true,
            crate::browsing_mode::BrowsingMode::Private,
        );
        widget.debug_set_viewport_width(390);
        layout_at(&mut widget, 390.0, 844.0);
        let indicator = widget.find_rect("browser.private-indicator").unwrap();
        assert!(indicator.width() > 0.0 && indicator.height() > 0.0);
        // The mark lives in the tinted toolbar; no banner takes a row
        // from the page.
        let toolbar = widget.find_rect("browser.toolbar").unwrap();
        assert_eq!(toolbar.intersect(indicator), indicator);
        let surface = widget.find_rect("browser.surface").unwrap();
        assert!(
            (surface.y0 - toolbar.y1).abs() < 1.0,
            "the page starts right under the toolbar: {toolbar:?} {surface:?}"
        );
    }

    // Phase `private_discovery_failure_filter_worker_keeps_removed_root_absent`.
    #[cfg(unix)]
    {
        use std::io::{Read, Write};

        let fixture = tempfile::tempdir().unwrap();
        let isolated_executable = fixture.path().join("isolated/bin/browser-test");
        std::fs::create_dir_all(isolated_executable.parent().unwrap()).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &isolated_executable).unwrap();
        let done = fixture.path().join("filter-loader.done");
        let root = crate::storage::allocate_private_root().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/filters", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = tilo_test_env::wait_for("the filter worker to connect", || {
                match listener.accept() {
                    Ok((stream, _)) => Some(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                    Err(error) => panic!("filter endpoint failed: {error}"),
                }
            });
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let body = b"||private-discovery.fixture^\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let output = std::process::Command::new(&isolated_executable)
            .args([
                "--exact",
                "browser_widget::library_tests::private_discovery_failure_filter_worker_does_not_recreate_root_child",
                "--ignored",
                "--nocapture",
            ])
            .env_remove("TILO_CHROMIUM_HOST")
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .env("TILO_TEST_FILTER_LIST_URL", url)
            .env("TILO_TEST_FILTER_LOADER_DONE", &done)
            .output()
            .unwrap();
        server.join().unwrap();
        let remained = root.exists();
        if remained {
            std::fs::remove_dir_all(&root).unwrap();
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!remained, "child left or recreated its private root");
    }
}

#[test]
#[ignore = "child entry point for private discovery/filter lifecycle regression"]
#[cfg(unix)]
fn private_discovery_failure_filter_worker_does_not_recreate_root_child() {
    let root =
        std::path::PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").expect("private root"));
    let done = std::path::PathBuf::from(
        std::env::var_os("TILO_TEST_FILTER_LOADER_DONE").expect("completion marker"),
    );
    crate::storage::configure(root.to_str()).unwrap();
    let mut widget = BrowserWidget::new_with_mode(crate::browsing_mode::BrowsingMode::Private);
    assert!(
        !root.exists(),
        "Chromium discovery failure must remove the allocated root"
    );
    let mut document = AutoCommit::new();
    widget.sync(&document, &automerge::ROOT);
    tilo_test_env::wait_until("the legacy filter worker to finish", || done.exists());
    drop(widget);
    assert!(
        !root.exists(),
        "completed unavailable-engine filter bootstrap recreated the private root"
    );
}
