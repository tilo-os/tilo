//! The visual-review hooks: each screenshot scenario stages the chrome in
//! the state it shows, as the engine, the network and the person would
//! leave it.

use super::*;

impl BrowserWidget {
    /// Stage the window for a screenshot scenario: the chrome state each one
    /// shows, as the engine, the network and the person would leave it.
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("pip-active") => self.debug_show_active_pip(),
            Some("pip-empty") => self.debug_show_empty_pip_feedback(),
            Some("settings") => self.debug_show_settings(),
            Some("history-paused") => self.debug_show_history_paused(),
            Some("loading") => self.debug_show_loading(),
            Some("error") => self.debug_show_load_error(),
            Some("permission") => self.debug_request_permission(),
            Some("page-dialog") => self.debug_page_dialog(),
            Some("suggestions") => {
                self.debug_force_typed_suggestions("youtube.com");
                self.suggestions.move_selection(1);
            }
            Some("library-history") => self.debug_show_library("history"),
            Some("library-bookmarks") => self.debug_show_library("bookmarks"),
            Some("library-extensions") => self.debug_show_library("extensions"),
            Some("library-filtered") => {
                self.debug_show_library("history");
                self.debug_set_library_filter("rust");
            }
            Some(downloads) if downloads.starts_with("downloads-") => {
                self.debug_show_downloads(downloads.trim_start_matches("downloads-"));
            }
            _ => {}
        }
        self.structure_dirty = true;
    }

    /// Test hook: pin a suggestion list into view (bypasses focus/query)
    /// so the overlay can be rendered headlessly for inspection.
    pub fn debug_force_suggestions(&mut self, items: &[crate::history::HistoryEntry]) {
        self.suggestions
            .set_suggestions(items.iter().map(omnibox::Row::history).collect());
        self.force_suggestions = true;
    }

    /// Screenshot hook: the suggestions for `typed`, as the omnibox ranks
    /// them over visited pages that include a few on `YouTube`.
    pub(super) fn debug_force_typed_suggestions(&mut self, typed: &str) {
        let mut entries = crate::history::sample_entries();
        for (url, title, last_visit, visit_count) in [
            ("https://www.youtube.com/", "YouTube", 9, 40),
            (
                "https://www.youtube.com/watch?v=jNQXAC9IVRw",
                "Me at the zoo - YouTube",
                8,
                2,
            ),
        ] {
            entries.push(crate::history::HistoryEntry {
                url: url.into(),
                title: title.into(),
                last_visit,
                visit_count,
            });
        }
        let mut index = omnibox::HistoryIndex::default();
        index.rebuild(&entries);
        let matches = index.search(typed, omnibox::MAX_ROWS, 10);
        let rows = omnibox::build_rows(typed, &matches, &[], &[], "", omnibox::MAX_ROWS);
        self.suggestions
            .set_search_engine(&crate::settings::search_engine_name(
                &self.settings.search_engine,
            ));
        self.suggestions.set_suggestions(rows);
        self.force_suggestions = true;
    }

    /// Headless visual-review hook: show the settled no-video guidance note.
    pub fn debug_show_empty_pip_feedback(&mut self) {
        self.set_pip_feedback(Some(PipFeedback::persistent(PipFeedbackKind::NoVideo)));
    }

    /// Headless visual-review hook for the toolbar's adopted `PiP` state.
    pub fn debug_show_active_pip(&mut self) {
        self.active_pip = true;
        self.active_pip_element = Some(MediaElementInfo {
            element_id: "media:debug".into(),
            viewport_rect: [0.14, 0.16, 0.72, 0.62],
            intrinsic_size: [1_920, 1_080],
            label: "The active film".into(),
        });
    }

    /// Headless visual-review hook for the recoverable navigation error state.
    pub fn debug_show_load_error(&mut self) {
        self.current_uri = "https://offline.example.test".into();
        self.initial_url = self.current_uri.clone();
        // The offline state owns the address shown in visual review.  Without
        // this, the headless engine's normal startup address made the chrome
        // claim DuckDuckGo while the recovery panel described a different
        // failed destination.
        self.address.set(self.current_uri.clone());
        self.title = "Couldn’t open this page".into();
        self.load_error =
            Some("The site did not respond. Check your connection and try again.".into());
        self.forced_load_error = true;
    }

    /// Headless visual-review hook for the in-flight navigation state.
    pub fn debug_show_loading(&mut self) {
        self.current_uri = "https://loading.example.test".into();
        self.initial_url = self.current_uri.clone();
        self.address.set(self.current_uri.clone());
        self.title = "Opening loading.example.test".into();
        self.progress = 0.42;
        self.forced_loading = true;
    }

    /// Seed the responsive breakpoint before a single-frame offscreen render.
    pub fn debug_set_viewport_width(&mut self, width: u32) {
        self.compact_chrome = is_compact_chrome(f64::from(width), self.compact_breakpoint);
        // The desktop Library cap also needs the viewport before its first
        // retained layout pass. Production windows already have this rect by
        // the time a user opens Library; headless review does not.
        self.rect = Rect::new(0.0, 0.0, f64::from(width), 0.0);
    }

    /// Populate and open a browser-library section for deterministic review.
    /// Stage the same bounded local filter that the framework and actions use.
    pub fn debug_set_library_filter(&mut self, query: &str) {
        self.library_filter.set(bounded_library_filter(query));
        self.structure_dirty = true;
    }

    pub fn debug_show_library(&mut self, section: &str) {
        // Visits over the past days, so the history shows its day groups.
        let now = u64::try_from(ui::clock::local_now().timestamp()).unwrap_or_default();
        for (url, title, ago) in [
            (
                "https://en.wikipedia.org/wiki/Web_browser",
                "Web browser - Wikipedia",
                4 * 24 * 3600,
            ),
            ("https://www.rust-lang.org/learn", "Learn Rust", 26 * 3600),
            (
                "https://developer.mozilla.org/en-US/docs/Web",
                "MDN Web Docs",
                3 * 3600,
            ),
            (
                "https://example.org/long-form",
                "Long-form reading",
                20 * 60,
            ),
        ] {
            self.history.record_at(url, title, now.saturating_sub(ago));
        }
        self.library_section = match section {
            "bookmarks" => LibrarySection::Bookmarks,
            "downloads" => LibrarySection::Downloads,
            "extensions" => LibrarySection::Extensions,
            _ => LibrarySection::History,
        };
        self.show_library = true;
        self.show_settings = false;
    }

    /// Deterministic download-model fixtures for visual review. These use the
    /// same lifecycle methods as live engine events rather than a parallel UI
    /// representation, so every captured row has a real resource/action shape.
    pub fn debug_show_downloads(&mut self, state: &str) {
        self.downloads = Downloads::default();
        match state {
            "empty" => {}
            "error" => {
                self.downloads.requested(
                    "review-failed".into(),
                    "https://example.org/archive.zip".into(),
                    "project-assets.zip",
                );
                self.downloads.failed(
                    "review-failed",
                    "The connection was interrupted. Your partial file was removed.".into(),
                    false,
                );
            }
            "completed" => {
                self.downloads.requested(
                    "review-complete".into(),
                    "https://example.org/guide.pdf".into(),
                    "Design-guide.pdf",
                );
                self.downloads.finished(
                    "review-complete",
                    "/home/alex/Downloads/Design-guide.pdf".into(),
                );
            }
            _ => {
                self.downloads.requested(
                    "review-progress".into(),
                    "https://example.org/tilo-sdk.tar.zst".into(),
                    "Tilo-SDK.tar.zst",
                );
                self.downloads.destination(
                    "review-progress",
                    "/home/alex/Downloads/Tilo-SDK.tar.zst".into(),
                );
                self.downloads.progress(
                    "review-progress",
                    503_316_480,
                    Some(1_073_741_824),
                    0.46875,
                );
            }
        }
        self.library_section = LibrarySection::Downloads;
        self.show_library = true;
        self.show_settings = false;
        self.structure_dirty = true;
    }

    /// Headless visual-review hook: the passwords panel, or the address
    /// autofill panel.
    pub fn debug_show_autofill(&mut self, addresses: bool) {
        self.review_stage = Some(ReviewStage::Autofill { addresses });
    }

    /// Headless visual-review hook: a site asking for the camera.
    pub fn debug_request_permission(&mut self) {
        self.capabilities.apply(WebViewEvent::PermissionRequested {
            id: 1,
            origin: "https://meet.example.org".into(),
            permissions: "camera and microphone".into(),
        });
        self.structure_dirty = true;
    }

    /// Headless visual-review hook: a page asking whether to go ahead.
    pub fn debug_page_dialog(&mut self) {
        // Asked of this engine already, so the next sync keeps the question.
        self.page_dialogs.sync(&self.engine);
        self.page_dialogs
            .apply(&WebViewEvent::PageDialog(ui::webview::PageDialog {
                id: 1,
                kind: ui::webview::PageDialogKind::Confirm,
                origin: "https://shop.example.org".into(),
                message: "Remove all 3 items from your basket?".into(),
                default_text: String::new(),
            }));
        self.structure_dirty = true;
    }

    pub fn debug_show_settings(&mut self) {
        self.show_settings = true;
        self.show_library = false;
    }

    /// Headless visual-review hook for the history-pause boundary. Existing
    /// entries deliberately remain visible: pausing recording is not a
    /// private-storage mode and must not hide prior durable history.
    pub fn debug_show_history_paused(&mut self) {
        if self.history.is_empty() {
            self.history
                .record_at("https://example.org/reading", "Existing reading", 2);
            self.history
                .record_at("https://www.rust-lang.org/learn", "Learn Rust", 1);
        }
        self.history_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.show_library = true;
        self.show_settings = false;
        self.library_section = LibrarySection::History;
    }

    /// Real-engine integration hook used by `browser --shots-live --probe-pip`.
    /// It enters through the same probe/state machine as the toolbar button;
    /// only the synthetic pointer event is skipped.
    #[doc(hidden)]
    pub fn debug_request_picture_in_picture(&mut self) {
        self.begin_pip_probe();
    }
}
