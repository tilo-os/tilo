//! Construction, action brokers and onboarding.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Attach the session's live application action directory after the
    /// collaboration broker has bound. Kept explicit so bare/headless hosts
    /// do not fabricate a broker.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each provider is moved at its last use, into the live actions provider and into the host; neither is held past it"
    )]
    pub fn attach_app_action_broker(
        &mut self,
        broker: crate::shell::collab_broker::AppActionBroker,
    ) {
        if let Some(chrome) = &self.chrome {
            chrome.borrow_mut().native_app_opening =
                Some(crate::shell::launcher::NativeAppOpening(broker.downgrade()));
        }
        self.room_activity.attach_task_broker(broker.clone());
        self.attach_shortcut_trigger_broker(broker.clone());
        self.search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ShortcutsProvider::new(broker.clone()),
        ));
        // Apps' content, from the session's persistent index: found whether
        // or not the app runs, opened through the broker like a live hit.
        let app_content =
            std::rc::Rc::new(crate::shell::search::providers::AppContentProvider::new(
                crate::shell::search::app_content_index::session(),
                broker.clone(),
            ));
        self.search_registry
            .register(std::rc::Rc::clone(&app_content)
                as std::rc::Rc<dyn crate::shell::search::SearchProvider>);
        self.search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ContinueProvider::new(
                crate::shell::search::app_content_index::session(),
                broker.clone(),
            ),
        ));
        // The catalogue again, now able to run a closed app's offered action
        // for an app this device does not have yet.
        self.search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::StoreCatalogueProvider::with_action_sessions(
                crate::shell::certified_action_session::ActionSessionSource::new(broker.clone()),
            ),
        ));
        self.search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::StoreCatalogueProvider::installed_with_action_sessions(
                crate::shell::certified_action_session::ActionSessionSource::new(broker.clone()),
            ),
        ));
        let app_actions =
            std::rc::Rc::new(crate::shell::search::providers::LiveAppActionsProvider::new(broker));
        app_actions.set_app_content(app_content);
        self.search_registry
            .register(std::rc::Rc::clone(&app_actions)
                as std::rc::Rc<dyn crate::shell::search::SearchProvider>);
        self.app_actions = Some(app_actions);
        self.last_search_query = "\u{0}app-action-provider-attached\u{0}".into();
    }

    /// Settles on the next change to any app invocation's status, for a
    /// control reply waiting on its job. `None` before the broker attaches.
    pub fn next_invocation_change(&self) -> Option<tilo_actions::remote::Pending<()>> {
        self.app_actions.as_ref()?.next_invocation_change()
    }

    /// Settles on the next change to any app invocation or publication, for
    /// a control listing waiting to settle. `None` before the broker
    /// attaches.
    pub fn next_listing_change(&self) -> Option<tilo_actions::remote::Pending<()>> {
        self.app_actions.as_ref()?.next_listing_change()
    }

    /// The attached broker's invocation and publication generations.
    pub fn app_action_generation(&self) -> Option<(u64, u64)> {
        self.app_actions
            .as_ref()
            .map(|actions| actions.change_generation())
    }

    /// Whether a listing for `args`, read right after
    /// [`Self::action_catalog_query`], is final: `false` while an index build
    /// or an app directory page that would change it is still owed.
    pub fn action_listing_complete(&self, args: &crate::shell::search::QueryArgs<'_>) -> bool {
        !self.search_registry.indexes_settling(args)
    }

    /// Move the focused workspace's old tasks into its tracker, once.
    pub(super) fn import_room_tasks(&mut self) {
        let (Some(tracker), Some(chrome)) = (&self.tracker, &self.chrome) else {
            return;
        };
        let Some((room, person)) = ({
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| (workspace.room(), state.active_local_user_in(&tree)))
        }) else {
            return;
        };
        let triggers = &self.shortcut_triggers;
        self.task_imports.pump(room, &person, tracker, || {
            triggers
                .broker()
                .map(crate::shell::certified_action_session::ActionSessionSource::new)
        });
    }

    /// The rooms' trackers, for the composer to follow the runs it asks for.
    pub(crate) fn attach_tracker(
        &mut self,
        tracker: Option<std::sync::Arc<crate::shell::broker_services::NativeTrackerRuns>>,
    ) {
        self.tracker = tracker;
    }

    /// Registers every built-in search provider and hands back the shared
    /// records index alongside the registry. Split out of [`Self::new`],
    /// which was otherwise dominated by this one flat setup block.
    pub(super) fn build_search_registry(
        qs_actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        session_action_policy: crate::shell::search::providers::SessionActionPolicy,
    ) -> (
        crate::shell::search::SearchRegistry,
        std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
        crate::shell::search::providers::actions::SharedQsActions,
        std::rc::Rc<crate::shell::search::providers::FilesProvider>,
    ) {
        // Start the remote-app refresh with the shell, not with the user's
        // first Win+K query. A persisted metadata snapshot makes search
        // useful immediately; this refresh quietly makes it current.
        let _ = crate::shell::catalogue::available_apps();
        // Flathub's cached metadata and the block list, read on a worker.
        crate::shell::catalogue::refresh_shared();
        let search_preferences = std::rc::Rc::new(std::cell::RefCell::new(
            crate::shell::search::SearchPreferences::load(),
        ));
        let mut search_registry = crate::shell::search::SearchRegistry::with_preferences_handle(
            std::rc::Rc::clone(&search_preferences),
        );
        let provider_catalog = search_registry.provider_catalog_handle();
        let qs_action_slot = std::rc::Rc::new(
            crate::shell::search::providers::actions::SharedQsDispatcher::new(
                qs_actions,
                crate::shell::quick_settings::actions::QsActionAvailability::detect(),
            ),
        );
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ActionsProvider::with_all_sources(
                search_preferences,
                provider_catalog,
                qs_action_slot.clone(),
            ),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::permission_prompts::PermissionPromptsProvider::default(
            ),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::DesktopWindowsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ContinueFromDeviceProvider::default(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::SessionActionsProvider::new(session_action_policy),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::SettingsPagesProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::tray_items::TrayItemsProvider::global(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::now_playing::NowPlayingProvider::new(
                std::rc::Rc::new(crate::shell::now_playing::MprisSource),
            ),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ApplicationsProvider::new(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::NativeApplicationsProvider::default(),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::device_local_actions::DeviceLocalActionsProvider,
        ));
        // Apps to get: the catalogue search shares with the App Store. Its
        // rows rank just below installed apps of equal match.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::StoreCatalogueProvider::new(),
        ));
        // What the apps here create and do while closed, from their signed
        // manifests: found whether or not the app ever ran.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::StoreCatalogueProvider::installed(),
        ));
        // Applications Tilo did not package: unconfined, separate chip
        // from the one above.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::HostApplicationsProvider::new(),
        ));
        // Downloaded programs running now that are not in the person's apps
        // yet, to add.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::downloaded_programs::DownloadedProgramsProvider::default(),
        ));
        let records = crate::shell::search::providers::RecordsProvider::new();
        let room_records = records.index();
        search_registry.register(std::rc::Rc::new(records));
        // One file index: the palette's Files and the file chooser's search.
        let files = std::rc::Rc::new(crate::shell::search::providers::FilesProvider::new());
        search_registry.register(files.clone());
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::PeopleProvider::new(),
        ));
        // What someone asked of the person, before anything is typed.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::WaitingOnYouProvider::new(),
        ));
        // Where the windows were: the launcher's Continue. Reopening at an
        // anchor goes through the broker once one is attached.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ContinueProvider::without_broker(
                crate::shell::search::app_content_index::session(),
            ),
        ));
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::QuickAnswersProvider::new(),
        ));
        // The web, the last tier: searching it for what was typed, and the
        // web services people search by name.
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::WebProvider::new(),
        ));
        (search_registry, room_records, qs_action_slot, files)
    }

    pub fn new(content: Box<dyn Widget>) -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("overlay production inputs");
        let qs_actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions> =
            std::rc::Rc::new(crate::shell::quick_settings::actions::RealQsActions);
        let session_action_policy = crate::shell::search::providers::SessionActionPolicy::default();
        let (search_registry, room_records, qs_action_slot, files) =
            Self::build_search_registry(qs_actions.clone(), session_action_policy.clone());
        let mut host = Self::from_inputs(
            content,
            OverlayInputs {
                search_registry,
                app_launcher: std::rc::Rc::new(|chrome, package_id, display_name| {
                    let _ = crate::shell::search::providers::applications::launch_installed_app(
                        chrome,
                        package_id,
                        display_name,
                    );
                }),
                room_records,
                collaboration_preferences: CollaborationPreferences::load(),
                shortcut_config: settings_store::hardware::ShortcutConfig::load(),
                shortcut_triggers: crate::shell::automation_triggers::TriggerHost::for_session(),
                desktop_controls: DesktopControlsSource::Persisted(
                    settings_store::hardware::DesktopControlsConfig::load(),
                ),
                qs_root: QuickSettingsRoot::load_persisted(),
                qs_refresh: crate::shell::quick_settings::refresh::QsRefresh::new(),
                qs_actions,
                qs_action_slot,
                room_activity: crate::shell::room_activity::RoomActivityPane::new_persistent(),
                projection_overlay: ProjectionOverlay::default(),
                portal_dialogs: PortalDialogController::default(),
                history_narrator: Some(
                    crate::shell::history_narrative::LocalHistoryNarrator::default(),
                ),
                status_narrator: crate::shell::history_narrative::LocalHistoryNarrator::default(),
                history_describer: crate::shell::history_narrative::HistoryDescriber::default(),
                session_action_policy,
                now_playing: std::rc::Rc::new(crate::shell::now_playing::MprisSource),
            },
        );
        // Offer Switch user only where the display manager can open a greeter
        // beside this session; greetd cannot.
        let user_switching = os_actions::user_switching_supported();
        host.session_action_policy
            .set_user_switching(user_switching);
        host.layers.power_menu = PowerMenu::for_session(user_switching);
        host.pairing = crate::bluetooth_prompt::PairingOverlay::new(
            crate::bluetooth::PairingPromptHandle::global(),
        );
        // The live session keeps the desk choices made on the desk itself.
        host.desk = desk::DeskState::persisting();
        host.file_picker_overlay.set_search(files);
        host
    }

    pub(crate) fn with_fixture_content(content: Box<dyn Widget>) -> Self {
        let qs_actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions> =
            std::rc::Rc::new(crate::shell::quick_settings::actions::UnavailableQsActions);
        let qs_action_slot = std::rc::Rc::new(
            crate::shell::search::providers::actions::SharedQsDispatcher::new(
                qs_actions.clone(),
                crate::shell::quick_settings::actions::QsActionAvailability::default(),
            ),
        );
        let mut search_registry = crate::shell::search::SearchRegistry::new();
        search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ActionsProvider::with_shared_qs_actions(
                qs_action_slot.clone(),
            ),
        ));
        let records = crate::shell::search::providers::RecordsProvider::new();
        let room_records = records.index();
        search_registry.register(std::rc::Rc::new(records));
        Self::from_inputs(
            content,
            OverlayInputs {
                search_registry,
                app_launcher: std::rc::Rc::new(|_, _, _| {}),
                room_records,
                collaboration_preferences: CollaborationPreferences::from_values(
                    &serde_json::Map::new(),
                ),
                shortcut_config: settings_store::hardware::ShortcutConfig::default(),
                shortcut_triggers: crate::shell::automation_triggers::TriggerHost::new(
                    std::sync::Arc::new(crate::shell::automation_triggers::SystemClock),
                    None,
                ),
                desktop_controls: DesktopControlsSource::Explicit(
                    settings_store::hardware::DesktopControlsConfig::default(),
                ),
                qs_root: QuickSettingsRoot::with_config(
                    settings_store::hardware::QuickSettingsConfig::default(),
                ),
                qs_refresh: crate::shell::quick_settings::refresh::QsRefresh::without_os_reads(),
                qs_actions,
                qs_action_slot,
                room_activity: crate::shell::room_activity::RoomActivityPane::with_local_user(
                    "shell-fixture".into(),
                ),
                history_narrator: None,
                status_narrator: crate::shell::history_narrative::LocalHistoryNarrator::disabled(),
                history_describer: crate::shell::history_narrative::HistoryDescriber::disabled(),
                portal_dialogs: PortalDialogController::new(
                    crate::shell::portal_dialog::PortalDialogQueue::default(),
                ),
                projection_overlay: ProjectionOverlay::from_controller(
                    crate::shell::projection::ProjectionController::unavailable(),
                ),
                session_action_policy:
                    crate::shell::search::providers::SessionActionPolicy::default(),
                now_playing: std::rc::Rc::new(crate::shell::now_playing::NoPlayers),
            },
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one shared initialization body for the host interaction surfaces"
    )]
    pub(super) fn from_inputs(content: Box<dyn Widget>, mut inputs: OverlayInputs) -> Self {
        inputs.search_registry.register_or_replace(std::rc::Rc::new(
            crate::shell::search::providers::authentication_prompts::AuthenticationPromptsProvider::new(inputs.portal_dialogs.queue().clone()),
        ));
        let file_picker_overlay = FilePickerOverlay::default();
        inputs.search_registry.register_or_replace(std::rc::Rc::new(
            crate::shell::search::providers::file_picker::FilePickerProvider::new(
                file_picker_overlay.actions(),
            ),
        ));
        // Palette entries come from the search registry on every
        // keystroke. New providers slot in via `SearchRegistry::register`;
        // the palette picks up their resource-kind chips automatically.
        let mut palette = CommandPalette::new();
        palette.set_bypass_fuzzy(true);
        // The shell paints scope + kind controls into the palette's own
        // reserved filter band, inside the card.
        palette.set_host_filter_strip(true);
        let collaboration_preferences = inputs.collaboration_preferences;
        Self {
            rect: Rect::ZERO,
            interaction: ui::InteractionEnvironment::default(),
            touch_gestures: ui::gesture::TouchGestureArena::new(),
            shortcut_binding_rects: Vec::new(),
            shortcut_reset_rects: Vec::new(),
            shortcut_rebind: None,
            shortcut_rebind_message: None,
            shortcut_guide_focus: None,
            shortcut_guide_scroll: 0.0,
            shortcut_guide_max_scroll: 0.0,
            shortcut_help_done_width: 0.0,
            shortcut_config: inputs.shortcut_config,
            nested_shortcuts: None,
            shortcut_triggers: shortcut_triggers::ShortcutTriggerState::new(
                inputs.shortcut_triggers,
            ),
            desktop_controls: inputs.desktop_controls,
            shortcut_help_drawn_rect: Rect::ZERO,
            content,
            desktop_close_reports: Vec::new(),
            parked_workspaces: parked_workspaces::ParkedWorkspaces::default(),
            workspace_slide: None,
            onboarding: None,
            onboarding_node: None,
            onboarding_released: false,
            onboarding_focus_reset: false,
            strip_navigator: tilo_desktop::strip_navigator::StripNavigator::default(),
            strip_switcher_revision: None,
            outputs: Vec::new(),
            palette,
            app_actions: None,
            palette_visible: false,
            search_registry: inputs.search_registry,
            app_launcher: inputs.app_launcher,
            search_results: Vec::new(),
            param_flow: None,
            palette_offer: None,
            secondary_flow: None,
            expanded_kinds: Vec::new(),
            more_rows: Vec::new(),
            present_kinds: Vec::new(),
            search_chrome_revision: 0,
            search_indexes_settling: false,
            search_artwork: 0,
            pending_action_admissions: Vec::new(),
            pending_launchpad_app: None,
            launchpad_quiet_at: None,
            pending_launchpad_actions: Vec::new(),
            last_search_query: String::new(),
            last_search_revision: String::new(),
            auto_selected_row: None,
            active_kind_filter: Vec::new(),
            app_chooser: None,
            search_filters: SearchFilters::default(),
            search_filter_inputs: None,
            room_records: inputs.room_records,
            palette_layout_dirty: false,
            palette_layout_host: None,
            content_node: None,
            palette_node: None,
            chrome: None,
            last_cursor: None,
            overview_zoom: ui::motion::Presence::closed(),
            overview_naming: None,
            overview_thumbnails: None,
            overview_scroll_y: 0.0,
            overview_selection: None,
            overview_window_selection: None,
            overview_window_interrupted: false,
            overview_press: None,
            overview_panel_cache: None,
            overview_panel_cache_fills: 0,
            overview_metrics: OverviewMetrics::default(),
            overview_layout_theme: Theme::default(),
            shortcut_navigation_step: shortcut_navigation_step(&Theme::default()),
            overview_layout_viewport: Rect::ZERO,
            qs_motion: qs_motion::QsMotion::closed(),
            input_source: ui::tokens::MotionSource::default(),
            workspace_switch_owed: false,
            workspace_switch_source: ui::tokens::MotionSource::default(),
            workspace_switch_latency: crate::compositor::switch_latency::SwitchLatency::default(),
            qs_was_open: false,
            notification_center: NotificationCenter::new(),
            qs_type_lines: CenterLayout::metrics(&Theme::dark(), &mut TextRenderer::new()),
            qs_panel: QsPanelMetrics::for_theme(&Theme::dark()),
            qs_clipboard_history_start: 0,
            qs_clipboard_wheel_remainder: 0.0,
            qs_selected_clipboard: None,
            qs_clipboard_query: String::new(),
            qs_device_fabric_start: 0,
            qs_device_fabric_wheel_remainder: 0.0,
            qs_selected_source: None,
            layers: OverlayStack::default(),
            session_action_channel: std::sync::mpsc::channel(),
            power_bridge: None,
            session_action_policy: inputs.session_action_policy,
            top_bar_reveal: top_bar_reveal::TopBarReveal::default(),
            fullscreen_bar_reveal: top_bar_reveal::FullscreenBarReveal::default(),
            top_edge_swipe: None,
            strip_pan_active: false,
            qs_refresh: inputs.qs_refresh,
            workspace_settings_overlay: WorkspaceSettingsOverlay::default(),
            workspace_rename_submit_held: false,
            palette_was_visible: false,
            palette_transition: ui::motion::Presence::closed(),
            faces: ui::widget::FaceMotions::default(),
            group_row_ptrs: HashMap::new(),
            strip_pan_quantum: None,
            content_built_for_ws: None,
            content_built_empty: None,
            workspace_content_layout_dirty: false,
            last_noted_focus: None,
            axis_gather_pending: None,
            add_workspace_rect: Rect::ZERO,
            chrome_top_y: CHROME_TOP_OFFSET_PX,
            permission_prompt: None,
            permission_prompt_id: None,
            permission_prompt_app: None,
            portal_prompts: None,
            host_shell_allows: Vec::new(),
            terminal_device_shell: None,
            file_picker_overlay,
            portal_dialogs: inputs.portal_dialogs,
            peer_sync_enabled: false,
            peer_sync: peer_sync_dispatch::PeerSyncDispatch::default(),
            pairing_until_ms: 0,
            pairing_polled_at: None,
            certification_seen: 0,
            site_rooms_for_shots: None,
            device_labels: std::collections::HashMap::new(),
            clipboard_manager_bridge: None,
            device_fabric_bridge: None,
            realtime_continuity_bridge: None,
            wifi_secret_bridge: None,
            follow_bridge: None,
            follow_presence_bridge: None,
            status_presence_bridge: None,
            active_tab_presence_bridge: None,
            published_active_tab: None,
            local_status: collaboration_preferences.manual_status,
            local_status_origin: ui::StatusOrigin::Manual,
            status_sharing: collaboration_preferences.sharing,
            peer_sync_overlay: PeerSyncOverlay::default(),
            directory_sheet: crate::shell::directory_sheet::DirectorySheet::default(),
            arrival_pulses: Vec::new(),
            notification_stack: NotificationStack::new(),
            acknowledgement_reduce_motion: std::cell::Cell::new(false),
            osd_painted: std::cell::Cell::new(None),
            osd_pin: std::cell::Cell::new(None),
            machine_banners: Vec::new(),
            call_edges: Vec::new(),
            motion: std::cell::Cell::new(ui::tokens::EffectiveMotion::new(
                ui::tokens::Motion::default(),
                false,
            )),
            last_chrome_revision: u64::MAX,
            brought_forward_shown: 0,
            brought_forward_reveal: None,
            peer_stream_tabs: std::collections::HashMap::new(),
            peer_picture_wake: None,
            // User intent is durable, while the root filters it against the
            // live hardware snapshot when it paints.  Tests that construct a
            // root directly still receive deterministic defaults.
            qs_root: inputs.qs_root,
            qs_navigation_release_pending: false,
            qs_slider_touch: ui::widget::SliderTouch::default(),
            now_playing_source: inputs.now_playing,
            now_playing_card: crate::shell::quick_settings::now_playing::NowPlayingCard::new(),
            now_playing_preferred: None,
            now_playing_tick: std::cell::Cell::new(None),
            desk: desk::DeskState::default(),
            qs_actions: inputs.qs_actions,
            qs_action_slot: inputs.qs_action_slot,
            qs_aux_drill_in: DrillInList::default(),
            qs_wifi_scanned_at: None,
            qs_hosted_section: None,
            status_bridge: None,
            status_copied: std::cell::RefCell::default(),
            session_audio_policy: None,
            room_activity: inputs.room_activity,
            history_narrator: inputs.history_narrator,
            status_narrator: inputs.status_narrator,
            history_describer: inputs.history_describer,
            status_narrative_request: None,
            activity_awareness_workspace: None,
            activity_refresh: ActivityRefreshGate::default(),
            activity_served: None,
            activity_pending_reveal: None,
            collaborators_overlay: CollaboratorsOverlay::default(),
            people_overlay: people_overlay::from_settings(),
            live_layout_bridge: None,
            published_layout: None,
            collaborators_appearing: ui::motion::Appearing::closed(),
            collaborators_painted: Scene::new(),
            collaborators_painted_panel: Rect::ZERO,
            projection_overlay: inputs.projection_overlay,
            wifi_prompt: Self::new_wifi_prompt(),
            wifi_password_prompt: None,
            display_identify: crate::shell::display_identify::DisplayIdentify::default(),
            window_previews: crate::shell::window_previews::WindowPreviews::default(),
            overview_row_drawn: Vec::new(),
            text_caret: None,
            clipboard_pastes: Vec::new(),
            in_process_pastes: (0, 0),
            pairing: crate::bluetooth_prompt::PairingOverlay::default(),
            expanded_collaborator: None,
            followed_peer: None,
            native_media: None,
            followed_tab: None,
            followed_session: None,
            followed_room: None,
            follow_banner: tilo_desktop::follow_banner::FollowBanner::default(),
            follow_ended_by_own_input: false,
            follow_cursor_targets: Vec::new(),
            agent_markers: agent_markers::AgentMarkers::default(),
            person_markers: tilo_desktop::person_cursor::PersonCursors::default(),
            person_quick_composer:
                crate::shell::person_quick_composer::PersonQuickComposer::default(),
            cursor_chat: cursor_chat::CursorChatComposer::default(),
            self_presence: presence_self::SelfPresence {
                chosen: collaboration_preferences.availability,
                visibility: collaboration_preferences.visibility.clone(),
                ..presence_self::SelfPresence::default()
            },
            presence_notices: arrival::PresenceNotices {
                introduced: collaboration_preferences.introduced,
                ..arrival::PresenceNotices::default()
            },
            #[cfg(feature = "hosted-test-control")]
            public_for_shots: false,
            person_quick_composer_node: None,
            peer_sync_node: None,
            app_sheet: None,
            app_sheet_leaving: None,
            app_sheet_nodes: None,
            app_sheet_changed: false,
            agent_actions: HashMap::new(),
            tracker: None,
            task_imports: task_import::TaskImports::default(),
            agent_quick_composer: crate::shell::agent_quick_composer::AgentQuickComposer::default(),
            agent_quick_composer_node: None,
            bar_popovers: bar_popovers::BarPopovers::default(),
            selection_requests: crate::shell::selection_agent::SelectionRequests::default(),
            feedback_point: crate::shell::feedback_entry::FeedbackPoint::default(),
            feedback_picker: crate::shell::feedback_pick::NativePicker::default(),
            window_toolbars: window_toolbars::WindowToolbars::default(),
            hosted_windows_revision: None,
            desktop_control: desktop_control_chips::DesktopControlMarks::default(),
            bar_tooltip: ui::widget::tooltip::TooltipHost::new(),
            shared_desk: None,
            capture_studio: ui::widget::CaptureStudio::default(),
            instant_captures: Vec::new(),
            magnifier_changed: false,
            capture_node: None,
            screenshot_markup: None,
            screenshot_markup_node: None,
            capture_mirrors: Vec::new(),
            capture_mirror_nodes: Vec::new(),
            capture_mirrors_structure_dirty: false,
            closed_capture_mirrors: Vec::new(),
            pending_floating_surfaces: Vec::new(),
            pending_floating_surface_updates: Vec::new(),
        }
    }

    pub(super) fn register_authentication_actions(&mut self) {
        let queue = self.portal_dialogs.queue().clone();
        let provider = self.onboarding.as_ref().map_or_else(
            || crate::shell::search::providers::authentication_prompts::AuthenticationPromptsProvider::new(queue.clone()),
            |view| crate::shell::search::providers::authentication_prompts::AuthenticationPromptsProvider::with_first_use_scope(queue.clone(), view.authentication_first_use_scope()),
        );
        self.search_registry
            .register_or_replace(std::rc::Rc::new(provider));
    }

    pub(super) fn register_onboarding_actions(&mut self) {
        if let Some(view) = self.onboarding.as_ref() {
            self.search_registry.register(view.semantic_provider());
        }
        self.register_authentication_actions();
    }

    /// Activate first-login chrome on an already constructed compositor
    /// host. Production uses this after resolving the canonical XDG path.
    pub fn enable_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.onboarding = Some(crate::onboarding_view::OnboardingView::new(identity_dir));
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Start the first-identity setup the identity chooser asked for
    /// ([`OverlayFlags::identity_setup`](crate::shell::chrome_state::OverlayFlags)):
    /// the same ceremony a first login runs, with its backup and recovery
    /// steps, and only on a device that has no identity.
    pub(super) fn start_requested_identity_setup(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if !chrome.borrow().overlays.identity_setup {
            return;
        }
        {
            let mut state = chrome.borrow_mut();
            state.overlays.identity_setup = false;
            state.bump();
        }
        if self.onboarding.is_some() {
            return;
        }
        let store = match crate::shell::identity_provisioning::DiskIdentityStore::from_env() {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%error, "cannot resolve the identity directory to set one up");
                return;
            }
        };
        if store.identity_present() {
            return;
        }
        if let Some(identity_dir) = store.path().parent().map(std::path::Path::to_path_buf) {
            self.enable_onboarding(identity_dir);
        }
    }

    #[cfg(test)]
    pub(crate) fn enable_test_onboarding(&mut self, view: crate::onboarding_view::OnboardingView) {
        self.onboarding = Some(view);
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Activate recovery-only identity chrome. Existing identity metadata is
    /// authoritative when its Secret Service item is unavailable, so this
    /// surface offers unlock/retry and restore but never replacement minting.
    pub fn enable_recovery_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.onboarding = Some(crate::onboarding_view::OnboardingView::new_for_recovery(
            identity_dir,
        ));
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Activate the identity-only OOBE for a Tilo session installed on an
    /// existing Linux host. It retains the trusted chrome boundary and the
    /// identity/backup gates of full-system onboarding.
    pub fn enable_host_desktop_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.onboarding =
            Some(crate::onboarding_view::OnboardingView::new_for_host_desktop(identity_dir));
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Activate host-session recovery-only identity chrome.
    pub fn enable_host_desktop_recovery_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.onboarding = Some(
            crate::onboarding_view::OnboardingView::new_for_host_desktop_recovery(identity_dir),
        );
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
    }

    /// Open the shell-owned exporter for the active identity. The Settings
    /// app may request this surface, but never receives private material or
    /// implements its own file-picker or encryption flow.
    ///
    /// # Errors
    ///
    /// Returns an error when another identity flow is active, the active
    /// identity cannot be located, or its backup ceremony cannot be opened.
    pub fn open_portable_identity_backup(&mut self) -> Result<(), String> {
        if self.onboarding.is_some() {
            return Err(crate::shell::i18n::t("identity-backup-busy"));
        }
        let identity_dir = runtime_identity::identity_dir_from_env().map_err(|error| {
            crate::shell::i18n::t_args(
                "identity-backup-locate-failed",
                &[("error", error.to_string().into())],
            )
        })?;
        self.onboarding =
            Some(crate::onboarding_view::OnboardingView::new_for_portable_backup(identity_dir)?);
        self.register_onboarding_actions();
        self.onboarding_released = false;
        self.onboarding_focus_reset = false;
        Ok(())
    }

    pub(crate) fn take_original_copy_request(
        &mut self,
    ) -> Option<crate::onboarding_migration::OriginalCopyRequest> {
        self.onboarding.as_mut()?.take_original_copy_request()
    }

    pub(crate) fn identity_setup_active(&self) -> bool {
        self.onboarding
            .as_ref()
            .is_some_and(crate::onboarding_view::OnboardingView::identity_setup_active)
    }

    pub(crate) fn take_existing_files_request(&mut self) -> Option<crate::retained_files::Request> {
        self.onboarding.as_mut()?.take_existing_files_request()
    }

    pub(crate) fn take_existing_files_release(&mut self) -> Option<String> {
        self.onboarding.as_mut()?.take_existing_files_release()
    }

    pub(crate) fn existing_files_outcome(&mut self, operation: &str, available: bool) {
        if let Some(view) = self.onboarding.as_mut() {
            view.existing_files_outcome(operation, available);
        }
    }

    #[doc(hidden)]
    pub fn set_onboarding_visual_state_for_snapshot(
        &mut self,
        state: crate::onboarding_view::OnboardingVisualState,
    ) {
        if let Some(onboarding) = self.onboarding.as_mut() {
            onboarding.set_visual_state_for_snapshot(state);
        }
    }

    /// Whether first-login chrome is still withholding the desktop.
    pub fn onboarding_active(&self) -> bool {
        self.onboarding.is_some()
    }

    pub fn projection(&self) -> &crate::shell::projection::ProjectionController {
        self.projection_overlay.controller()
    }

    /// Whether a receiver session is being established or is live. The
    /// compositor uses this to turn the frame encoder on only while needed.
    pub fn projection_capture_requested(&self) -> bool {
        self.projection_overlay.capture_requested()
    }
}
