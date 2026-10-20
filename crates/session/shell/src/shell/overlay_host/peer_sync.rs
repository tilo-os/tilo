//! Peer sync, presence bridges, the directory, pairing and invitations.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Fixture hosts start with explicit desktop controls, which skip launch
    /// notifications. Tests of that drain switch to the persisted source the
    /// session uses.
    #[cfg(test)]
    pub(crate) fn enable_persisted_launch_drain_for_test(&mut self) {
        self.desktop_controls = DesktopControlsSource::Persisted(
            settings_store::hardware::DesktopControlsConfig::default(),
        );
    }

    /// Configure peer-sync palette commands during host construction.
    #[must_use]
    pub fn with_peer_sync_enabled(mut self, enabled: bool) -> Self {
        self.set_peer_sync_enabled(enabled);
        self
    }

    /// Update peer-sync command availability for a live session. This is
    /// needed when trusted first-login provisioning releases the desktop and
    /// starts its transport without a restart.
    pub fn set_peer_sync_enabled(&mut self, enabled: bool) {
        // `ActionsProvider` reads the flag off chrome, not this struct.
        self.peer_sync_enabled = enabled;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().peer_sync_enabled = enabled;
        }
    }

    /// Attach the registry exported by this compositor's portal backend.
    /// Headless and screenshot hosts keep an empty private registry.
    pub fn set_session_inhibitors(&mut self, registry: tilo_power_mgmt::InhibitRegistry) {
        self.session_action_policy.attach(registry);
    }

    /// The registry the session's idle loop consults before suspending.
    pub fn session_inhibitors(&self) -> tilo_power_mgmt::InhibitRegistry {
        self.session_action_policy.inhibitors()
    }

    pub fn set_power_bridge(&mut self, bridge: tilo_power_mgmt::PowerBridge) {
        self.power_bridge = Some(bridge);
    }

    /// For a session that does not own the seat: another desktop owns the
    /// machine, so this one never suspends, restarts, shuts down or switches
    /// its user, from any entry point.
    pub fn detach_from_host(&mut self) {
        self.session_action_policy.detach_from_host();
        self.layers.power_menu.detach_from_host();
    }

    /// Whether peer-sync palette entries are registered.
    pub fn peer_sync_enabled(&self) -> bool {
        self.peer_sync_enabled
    }

    /// Session-local presence controls as shown in trusted chrome. Used by
    /// semantic inspection so an agent can verify what it is publishing and
    /// whether this view is attached to somebody else.
    pub(crate) fn presence_snapshot(&self) -> (ui::StatusSharing, Option<String>, Option<String>) {
        (
            self.status_sharing,
            self.local_status.clone(),
            self.followed_peer.clone(),
        )
    }

    /// Install the closure that carries out [`PeerSyncRequest`]s. Without
    /// one, requests fail visibly as unavailable when the frame pump drains
    /// them.
    #[must_use]
    pub fn with_peer_sync_bridge(mut self, bridge: PeerSyncBridge) -> Self {
        self.set_peer_sync_bridge(bridge);
        self
    }

    /// [`Self::with_peer_sync_bridge`] for a host that is already
    /// built — the native compositor constructs its host inside a
    /// `FrameBuilder` before it has a network handle to bridge to.
    pub fn set_peer_sync_bridge(&mut self, bridge: PeerSyncBridge) {
        self.peer_sync.set_bridge(bridge);
    }

    /// Carry peer-sync requests out on `worker`, off the frame loop: the
    /// session network's, whose requests wait on relays and the disk. The
    /// frame pump applies each outcome once it is back.
    pub fn set_peer_sync_worker(&mut self, worker: PeerSyncWorker) {
        self.peer_sync.set_worker(worker);
    }

    /// Attach the compositor-owned clipboard history to trusted shell chrome.
    pub fn set_clipboard_manager_bridge(&mut self, bridge: ClipboardManagerBridge) {
        self.clipboard_manager_bridge = Some(bridge);
    }

    /// Attach the authenticated personal-device and local-output control
    /// surface. An absent bridge renders an honest unprovisioned state.
    pub fn set_device_fabric_bridge(&mut self, bridge: DeviceFabricBridge) {
        self.device_fabric_bridge = Some(std::rc::Rc::clone(&bridge));
        self.search_registry.register_or_replace(std::rc::Rc::new(
            crate::shell::search::providers::DeviceContinuityActionsProvider::new(
                std::rc::Rc::clone(&bridge),
            ),
        ));
        if let Some(realtime) = self.realtime_continuity_bridge.as_ref() {
            self.search_registry.register_or_replace(std::rc::Rc::new(
                crate::shell::search::providers::CallContinuityActionsProvider::new(
                    std::rc::Rc::clone(realtime),
                    bridge,
                ),
            ));
        }
        self.last_search_query = "\0continuity-provider-rebound\0".into();
        self.last_search_revision.clear();
    }

    /// Attach trusted shell-only call handoff controls. The app sees semantic
    /// lifecycle edges, never the certified-device picker or raw source IDs.
    pub fn set_realtime_continuity_bridge(&mut self, bridge: RealtimeContinuityBridge) {
        self.realtime_continuity_bridge = Some(std::rc::Rc::clone(&bridge));
        if let Some(devices) = self.device_fabric_bridge.as_ref() {
            self.search_registry.register_or_replace(std::rc::Rc::new(
                crate::shell::search::providers::CallContinuityActionsProvider::new(
                    bridge,
                    std::rc::Rc::clone(devices),
                ),
            ));
        }
        self.last_search_query = "\0call-continuity-provider-rebound\0".into();
        self.last_search_revision.clear();
    }

    pub(super) fn realtime_continuity_call(
        &self,
        action: RealtimeContinuityUiAction,
    ) -> Result<crate::shell::collab_broker_capnp::RealtimeContinuityUiView, String> {
        self.realtime_continuity_bridge.as_ref().map_or_else(
            || Ok(crate::shell::collab_broker_capnp::RealtimeContinuityUiView::default()),
            |bridge| bridge(action),
        )
    }

    pub(super) fn device_fabric_call(
        &self,
        action: DeviceFabricUiAction,
    ) -> Result<DeviceFabricView, String> {
        self.device_fabric_bridge.as_ref().map_or_else(
            || Err("Personal device fabric is not available in this session".into()),
            |bridge| bridge(action),
        )
    }

    /// Attach the encrypted same-person credential store. Sessions without a
    /// provisioned identity simply keep the existing password prompt.
    pub fn set_wifi_secret_bridge(&mut self, bridge: WifiSecretBridge) {
        self.wifi_secret_bridge = Some(bridge);
    }

    pub(super) fn clipboard_manager_call(
        &self,
        action: ClipboardManagerAction,
    ) -> Vec<crate::compositor::selection::ClipboardHistorySnapshot> {
        self.clipboard_manager_bridge
            .as_ref()
            .map_or_else(Vec::new, |bridge| bridge(action))
    }

    pub(super) fn filtered_clipboard_history(
        &self,
    ) -> Vec<crate::compositor::selection::ClipboardHistorySnapshot> {
        let mut history = self.clipboard_manager_call(ClipboardManagerAction::Snapshot);
        if self.qs_clipboard_query.trim().is_empty() {
            return history;
        }
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot).ok();
        history.retain(|item| {
            clipboard_history_matches_query(item, &self.qs_clipboard_query, fabric.as_ref())
        });
        history
    }

    /// Supply identities the host has already verified (for example, through
    /// Contacts or a previous in-person verification). The invite sheet may
    /// reuse only these exact keys for its “someone I know” choices.
    pub fn set_remembered_invitees(&mut self, invitees: Vec<RememberedInvitee>) {
        self.peer_sync_overlay.set_remembered_invitees(invitees);
    }

    /// Install the session-local bridge that binds application viewports to
    /// the participant selected in the People popover.
    pub fn set_follow_bridge(&mut self, bridge: FollowBridge) {
        self.follow_bridge = Some(bridge);
    }

    /// Install the peer-presence publisher for the visual Follow cue. The
    /// compositor provides this only with an authenticated peer network.
    pub fn set_follow_presence_bridge(&mut self, bridge: FollowPresenceBridge) {
        self.follow_presence_bridge = Some(bridge);
    }

    pub fn set_status_presence_bridge(&mut self, bridge: StatusPresenceBridge) {
        self.status_presence_bridge = Some(bridge);
        if self.status_sharing != ui::StatusSharing::Off && self.local_status.is_some() {
            self.publish_local_status(self.local_status.clone());
        }
    }

    pub fn set_active_tab_presence_bridge(&mut self, bridge: ActiveTabPresenceBridge) {
        self.active_tab_presence_bridge = Some(bridge);
        self.published_active_tab = None;
    }

    pub(super) fn refresh_active_tab_presence(&mut self) {
        let current = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            let room = state
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room)?;
            Some((room, state.active_tab_id()?))
        });
        if current == self.published_active_tab {
            return;
        }
        let Some(bridge) = self.active_tab_presence_bridge.as_ref() else {
            self.published_active_tab = current;
            return;
        };
        if let Some((room, tab)) = self.published_active_tab {
            bridge(room, tab, false);
        }
        if let Some((room, tab)) = current {
            bridge(room, tab, true);
        }
        self.published_active_tab = current;
    }

    /// Carry `job` out, applying its outcome now when it is known now and
    /// from [`Self::pump_peer_sync`] once the worker has it.
    pub(super) fn dispatch_peer_sync(
        &mut self,
        job: peer_sync_dispatch::Job,
        follow: peer_sync_dispatch::FollowUp,
    ) {
        if let Some((follow, outcome)) = self.peer_sync.submit(job, follow) {
            self.apply_peer_sync(follow, outcome);
        }
    }

    /// Apply one request's outcome: to the card, or as the answer of the
    /// action that asked.
    pub(super) fn apply_peer_sync(
        &mut self,
        follow: peer_sync_dispatch::FollowUp,
        outcome: Result<PeerSyncResponse, String>,
    ) {
        use peer_sync_dispatch::FollowUp;
        match follow {
            FollowUp::Card {
                request,
                copy_result,
            } => {
                self.peer_sync_overlay
                    .finish_typed_request(&request, outcome, copy_result);
                if matches!(
                    request,
                    PeerSyncRequest::IssueInviteFor { .. } | PeerSyncRequest::CancelInvitation(_)
                ) {
                    self.refresh_pending_invitations();
                }
            }
            FollowUp::Action {
                request,
                request_id,
            } => {
                if matches!(
                    request,
                    PeerSyncRequest::IssueInvitation(_) | PeerSyncRequest::CancelInvitation(_)
                ) {
                    self.refresh_pending_invitations();
                }
                let output = outcome.and_then(|response| {
                    crate::shell::peer_sync_overlay::network_action_output(&request, response)
                });
                self.complete_peer_sync_action(request_id, output);
            }
            FollowUp::Pairing { request } => self.apply_pairing(&request, outcome),
            FollowUp::Directory { request } => self.apply_directory(&request, outcome),
            FollowUp::Ignore => {}
        }
    }

    /// Bring the person into each site workspace whose join installed: its
    /// site's app opens there (off the loop), and the arrival line names it
    /// and who is here. While the directory sheet is up, its rows follow
    /// the joins. Answers whether anything changed.
    pub(super) fn pump_arrivals(&mut self) -> bool {
        let now = Instant::now();
        self.arrival_line.expire(now);
        if self.directory_sheet.visible() {
            self.directory_sheet.set_joins(self.peer_sync.site_joins());
        }
        let arrivals = self.peer_sync.arrive();
        let Some(arrival) = arrivals.last() else {
            return false;
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return true;
        };
        let shown = {
            let state = chrome.borrow();
            state
                .tree()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == arrival.room)
                .map(|workspace| {
                    let people = workspace
                        .collaborators
                        .iter()
                        .map(|person| crate::shell::arrival_line::Present {
                            name: person.name.clone(),
                            id: person.id.clone(),
                            colour: None,
                            is_agent: false,
                        })
                        .collect::<Vec<_>>();
                    (workspace.name.clone(), people)
                })
        };
        if let Some((name, people)) = shown {
            self.arrival_line.show(&name, people, now);
            self.introduce_joined(arrival.room);
        }
        true
    }

    /// Open "Join a workspace…" and ask the directories for what they list.
    pub(in crate::shell) fn open_directory(&mut self) {
        let request = self.directory_sheet.open();
        self.run_directory_request(request);
    }

    /// Carry out what the directory sheet asked: reading directories and
    /// joining off the frame loop, switching and the join field here.
    pub(super) fn run_directory_request(
        &mut self,
        request: crate::shell::directory_sheet::DirectoryRequest,
    ) {
        use crate::shell::directory_sheet::DirectoryRequest;
        let network = match request {
            DirectoryRequest::Fetch { site } => PeerSyncRequest::Directory { site },
            DirectoryRequest::Join(listing) => PeerSyncRequest::JoinListed(listing),
            DirectoryRequest::Open(listing) => {
                self.directory_sheet.close();
                if let Some(room) = listing.room {
                    self.switch_to_room(room, None);
                }
                return;
            }
            DirectoryRequest::LinkOrCode(text) => {
                self.directory_sheet.close();
                self.peer_sync_overlay.prompt_connect_with(&text);
                // A link or a code the person typed joins at once; the field
                // shows it joining, or why not.
                let text: String = text.split_whitespace().collect();
                if !text.is_empty() {
                    self.run_peer_sync_request(&PeerSyncRequest::AcceptInvite(text), false);
                }
                return;
            }
        };
        self.dispatch_directory(network);
    }

    /// Run one of the directory's network requests on the worker.
    pub(super) fn dispatch_directory(&mut self, request: PeerSyncRequest) {
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: request.clone(),
                require_pending: false,
                workspace: self.active_workspace_id(),
            },
            peer_sync_dispatch::FollowUp::Directory { request },
        );
    }

    /// Show the workspace a join or Open leads to: `workspace` when a slot
    /// was made for it, else the one showing `room`. The workspace motion
    /// is the one every switch takes.
    pub(super) fn switch_to_room(
        &self,
        room: crate::shell::state::RoomId,
        workspace: Option<crate::shell::state::WorkspaceId>,
    ) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let target = workspace.or_else(|| {
            state
                .tree()
                .desktop
                .workspaces
                .iter()
                .find(|candidate| candidate.room == room)
                .map(|candidate| candidate.id)
        });
        if let Some(target) = target {
            state.overlays.overview = false;
            state.switch_to_workspace(target);
        }
    }

    /// Show what a directory request answered: the listings on the sheet,
    /// the workspace a join made, or why not; a workspace's publication in
    /// its settings; a report's acknowledgement.
    pub(super) fn apply_directory(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<PeerSyncResponse, String>,
    ) {
        match (request, outcome) {
            (PeerSyncRequest::Directory { .. }, Ok(PeerSyncResponse::Directory(listings))) => {
                self.directory_sheet.show(listings);
            }
            (
                PeerSyncRequest::JoinListed(_),
                Ok(PeerSyncResponse::JoinedWorkspace { room, workspace }),
            ) => {
                self.directory_sheet.joined();
                self.switch_to_room(room, workspace);
            }
            (PeerSyncRequest::Directory { .. } | PeerSyncRequest::JoinListed(_), Err(error)) => {
                self.directory_sheet.failed(&error);
            }
            (
                PeerSyncRequest::Publication(room) | PeerSyncRequest::Publish { room, .. },
                outcome,
            ) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    match outcome {
                        Ok(PeerSyncResponse::Publication(publication)) => {
                            if publication.moved {
                                // Making it public moved it to the person's
                                // host: say so once, in a sentence.
                                let name = state
                                    .tree()
                                    .desktop
                                    .workspaces
                                    .iter()
                                    .find(|workspace| workspace.room == publication.room)
                                    .map(|workspace| workspace.name.clone())
                                    .unwrap_or_default();
                                state.push_notification(
                                    t_args("directory-moved", &[("name", name.into())]),
                                    String::new(),
                                );
                            }
                            if publication.room != *room {
                                state.set_publication(
                                    *room,
                                    Ok(crate::shell::network::Publication::private(*room)),
                                );
                            }
                            state.set_publication(publication.room, Ok(publication));
                        }
                        Ok(_) => {
                            state.set_publication(*room, Err(t("peer-sync-unexpected-response")));
                        }
                        Err(error) => state.set_publication(*room, Err(error)),
                    }
                    state.bump();
                }
            }
            // The room's governance answers with the bot list itself, as
            // its change reaches this device; only a refusal is said here.
            (PeerSyncRequest::AskForBot { ask, .. }, Err(error)) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    let title = match ask {
                        crate::shell::network::BotAsk::Add { .. } => t("bots-add-failed"),
                        crate::shell::network::BotAsk::Remove { .. } => t("bots-remove-failed"),
                    };
                    chrome.borrow_mut().push_warning_notification(title, &error);
                }
            }
            (PeerSyncRequest::AskForBot { .. }, Ok(_)) => {}
            (PeerSyncRequest::ReportWorkspace { .. }, outcome) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    match outcome {
                        Ok(_) => {
                            state.push_notification(
                                t("directory-reported"),
                                t("directory-reported-body"),
                            );
                        }
                        Err(error) => {
                            state.push_warning_notification(t("directory-report-failed"), &error);
                        }
                    }
                }
            }
            (_, Ok(_)) => {
                self.directory_sheet
                    .failed(&t("peer-sync-unexpected-response"));
            }
            (_, Err(error)) => {
                tracing::debug!(%error, "a directory request failed");
            }
        }
    }

    /// Route one event to the directory sheet while it is open; whether it
    /// took it. Every key belongs to it, as to the join field.
    pub(super) fn handle_directory_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let outcome = self
            .directory_sheet
            .handle_event(event, modifiers, self.last_cursor);
        if let Some(request) = outcome.request {
            self.run_directory_request(request);
        }
        outcome.consumed
    }

    pub(super) fn draw_directory_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.directory_sheet
            .draw(self.rect, scene, transform, text_renderer, theme);
    }

    /// Carry out one pairing request on the worker.
    /// Open "Pair a new device" and ask the network for its link.
    pub(in crate::shell) fn open_pairing(&mut self) {
        self.peer_sync_overlay.prompt_pair();
        self.dispatch_pairing(PeerSyncRequest::InviteDevice);
    }

    pub(super) fn dispatch_pairing(&mut self, request: PeerSyncRequest) {
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: request.clone(),
                require_pending: false,
                workspace: self.active_workspace_id(),
            },
            peer_sync_dispatch::FollowUp::Pairing { request },
        );
    }

    /// Show what a pairing request answered: the link on the sheet, a
    /// notification for each device newly asking, the answer's outcome.
    pub(super) fn apply_pairing(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<PeerSyncResponse, String>,
    ) {
        match (request, outcome) {
            (
                PeerSyncRequest::InviteDevice,
                Ok(PeerSyncResponse::PairingInvite {
                    link,
                    expires_at_ms,
                }),
            ) => {
                self.peer_sync_overlay.show_pairing(link);
                self.pairing_until_ms = expires_at_ms;
            }
            (PeerSyncRequest::InviteDevice, Err(error)) => {
                self.peer_sync_overlay.pairing_failed(&error);
            }
            (PeerSyncRequest::DevicesAsking, Ok(PeerSyncResponse::DevicesAsking(asking))) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    for device in &asking {
                        self.device_labels
                            .insert(device.digest, device.label.clone());
                        state.push_device_asking(device);
                    }
                }
            }
            (PeerSyncRequest::AnswerDevice { digest, approve }, Ok(_)) => {
                let label = self.device_labels.remove(digest).unwrap_or_default();
                if *approve {
                    self.peer_sync_overlay.pairing_done(&label);
                    if let Some(chrome) = self.chrome.as_ref() {
                        chrome.borrow_mut().push_notification(
                            t_args("pairing-done", &[("device", label.into())]),
                            String::new(),
                        );
                    }
                }
                self.pairing_until_ms = 0;
            }
            (PeerSyncRequest::AnswerDevice { .. }, Err(error)) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.borrow_mut().push_warning_notification(
                        t_args("pairing-answer-failed", &[("error", error.into())]),
                        String::new(),
                    );
                }
            }
            (PeerSyncRequest::DevicesAsking, Err(error)) => {
                tracing::debug!(%error, "the devices asking could not be read");
            }
            _ => {}
        }
    }

    /// While a pairing link of this computer's admits a device, ask its host
    /// for the devices asking under it now and then; and send the person's
    /// answers to them.
    pub(super) fn pump_pairing(&mut self) {
        let answers = self
            .chrome
            .as_ref()
            .map_or_else(Vec::new, |chrome| chrome.borrow_mut().take_device_answers());
        for (digest, approve) in answers {
            self.dispatch_pairing(PeerSyncRequest::AnswerDevice { digest, approve });
        }
        if self.pairing_until_ms <= tilo_clock::unix_now_ms()
            || self.peer_sync.in_flight(&PeerSyncRequest::DevicesAsking)
            || self
                .pairing_polled_at
                .is_some_and(|at| at.elapsed() < PAIRING_POLL_INTERVAL)
        {
            return;
        }
        self.pairing_polled_at = Some(std::time::Instant::now());
        self.dispatch_pairing(PeerSyncRequest::DevicesAsking);
    }

    /// The workspace a peer-sync request made now is for.
    pub(super) fn active_workspace_id(&self) -> Option<crate::shell::state::WorkspaceId> {
        self.chrome
            .as_ref()
            .map(|chrome| chrome.borrow().tree().desktop.active)
    }

    pub(super) fn complete_peer_sync_action(
        &self,
        request_id: Option<u64>,
        result: Result<serde_json::Value, String>,
    ) {
        if let (Some(request_id), Some(chrome)) = (request_id, self.chrome.as_ref()) {
            chrome
                .borrow_mut()
                .complete_peer_sync_request(request_id, result);
        }
    }

    /// Drain the palette's peer-sync intents and act on them, once per
    /// frame from [`Widget::update`]. `/invite` mints a token; `/connect`
    /// opens the token prompt. Both write their outcome into chrome; a
    /// network request's outcome lands here once the worker has it.
    /// Answers whether anything changed.
    pub(super) fn pump_peer_sync(&mut self) -> bool {
        let answered = self.peer_sync.answered();
        let mut changed = !answered.is_empty();
        for (follow, outcome) in answered {
            self.apply_peer_sync(follow, outcome);
        }
        changed |= self.pump_arrivals();
        // One enrollment poll at a time: a poll that installs the room
        // publishes the state file on the worker.
        let polling = self
            .peer_sync
            .in_flight(&PeerSyncRequest::PollEnrollment(0))
            || self
                .peer_sync
                .in_flight(&PeerSyncRequest::CancelEnrollment(0));
        if !polling && let Some(request) = self.peer_sync_overlay.pending_enrollment_request() {
            self.run_peer_sync_request(&request, false);
        }
        self.pump_pairing();
        let actions = self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
            chrome.borrow_mut().take_peer_sync_requests()
        });
        changed |= !actions.is_empty();
        for action in actions {
            let (request_id, action) = match action {
                PeerSyncAction::Tracked { request_id, action } => (Some(request_id), *action),
                action => (None, action),
            };
            match action {
                PeerSyncAction::IssueInviteFor { name, copy } => {
                    let _ = copy;
                    self.open_invite_manager();
                    self.peer_sync_overlay.prompt_invite_for(name);
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::PromptInvite => {
                    self.open_invite_manager();
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::PromptConnect => {
                    self.peer_sync_overlay.prompt_connect();
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::BrowseDirectory => {
                    self.open_directory();
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::ReportWorkspace { room, reason } => {
                    self.dispatch_directory(PeerSyncRequest::ReportWorkspace { room, reason });
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::Publish { room, listing } => {
                    self.dispatch_directory(PeerSyncRequest::Publish { room, listing });
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::AskForBot { room, ask } => {
                    self.dispatch_directory(PeerSyncRequest::AskForBot { room, ask });
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::ReadPublication { room } => {
                    if !self
                        .peer_sync
                        .in_flight(&PeerSyncRequest::Publication(room))
                    {
                        self.dispatch_directory(PeerSyncRequest::Publication(room));
                    }
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::PairDevice => {
                    self.open_pairing();
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::LeaveSite { room } => {
                    self.dispatch_peer_sync(
                        peer_sync_dispatch::Job {
                            request: PeerSyncRequest::LeaveSite(room),
                            require_pending: false,
                            workspace: self.active_workspace_id(),
                        },
                        peer_sync_dispatch::FollowUp::Ignore,
                    );
                    self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
                }
                PeerSyncAction::Network { workspace, request } => {
                    self.run_network_action(workspace, request, request_id);
                }
                #[expect(
                    clippy::unreachable,
                    reason = "the palette queues a tracked request around one plain action, never another tracked one"
                )]
                PeerSyncAction::Tracked { .. } => unreachable!("tracked requests are unwrapped"),
            }
        }
        changed
    }

    /// Carry out one invitation action's network request against the
    /// workspace it was authorized in; its JSON output answers the action
    /// `request_id` once the request is back. Resend and cancel are scoped
    /// to that workspace's pending invitations, so an id from another
    /// workspace is not pending here.
    pub(super) fn run_network_action(
        &mut self,
        workspace: crate::shell::state::WorkspaceId,
        request: PeerSyncRequest,
        request_id: Option<u64>,
    ) {
        let active = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map(|active| active.id)
        });
        if active != Some(workspace) {
            self.complete_peer_sync_action(
                request_id,
                Err("the active workspace changed before the invitation request ran".into()),
            );
            return;
        }
        let require_pending = matches!(
            request,
            PeerSyncRequest::ResendInvitation(_) | PeerSyncRequest::CancelInvitation(_)
        );
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: request.clone(),
                require_pending,
                workspace: Some(workspace),
            },
            peer_sync_dispatch::FollowUp::Action {
                request,
                request_id,
            },
        );
    }

    pub(super) fn run_peer_sync_request(&mut self, request: &PeerSyncRequest, copy_result: bool) {
        // The card's request is still being carried out: a second press
        // would mint or redeem twice.
        if self.peer_sync.in_flight(request) {
            return;
        }
        self.peer_sync_overlay.begin_request(request);
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: request.clone(),
                require_pending: false,
                workspace: self.active_workspace_id(),
            },
            peer_sync_dispatch::FollowUp::Card {
                request: request.clone(),
                copy_result,
            },
        );
    }

    pub(super) fn open_invite_manager(&mut self) {
        if !self.may_invite_here() {
            if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().push_notification(
                    t("host-invite-unavailable-title"),
                    t("host-invite-unavailable-body"),
                );
            }
            return;
        }
        let (context, room_invitees) = self.chrome.as_ref().map_or_else(
            || (InviteContext::default(), Vec::new()),
            |chrome| {
                let state = chrome.borrow();
                let local_user = state.local_user_id.clone();
                let tree = state.tree();
                let workspace_name = tree.desktop.active_workspace().map_or_else(
                    || t("host-this-workspace"),
                    |workspace| workspace.name.clone(),
                );
                let room_invitees = tree
                    .desktop
                    .active_workspace()
                    .map(|workspace| tree.trusted_identity_projection(workspace.room))
                    .map(|projection| {
                        projection
                            .humans
                            .into_iter()
                            .filter(|human| {
                                let root = hex::encode(human.root_public_key);
                                local_user != root && local_user != format!("ed25519:{root}")
                            })
                            .map(|human| RememberedInvitee {
                                name: human.display_name,
                                user_public_key: human.root_public_key,
                                verification: crate::shell::i18n::t(
                                    "invite-workspace-verified-identity",
                                ),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let snapshot = (
                    InviteContext {
                        workspace_name,
                        inviter_name: local_user,
                    },
                    room_invitees,
                );
                drop(tree);
                drop(state);
                snapshot
            },
        );
        // Best-effort: without a running network there is nothing to start.
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: PeerSyncRequest::PrepareInvitation,
                require_pending: false,
                workspace: self.active_workspace_id(),
            },
            peer_sync_dispatch::FollowUp::Ignore,
        );
        self.peer_sync_overlay.set_invite_context(context);
        self.peer_sync_overlay
            .merge_remembered_invitees(room_invitees);
        self.peer_sync_overlay.prompt_invite();
        self.refresh_pending_invitations();
    }

    pub(super) fn refresh_pending_invitations(&mut self) {
        let request = PeerSyncRequest::ListPendingInvitations;
        self.dispatch_peer_sync(
            peer_sync_dispatch::Job {
                request: request.clone(),
                require_pending: false,
                workspace: self.active_workspace_id(),
            },
            peer_sync_dispatch::FollowUp::Card {
                request,
                copy_result: false,
            },
        );
    }

    /// Route one event to the peer-sync card, returning whether it was
    /// consumed. While a prompt is open every key belongs to it — a
    /// base64 token shares characters with shell chords, so anything
    /// that leaked past would both corrupt the token and fire an action.
    pub(super) fn handle_peer_sync_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let PeerSyncEventOutcome {
            consumed,
            request,
            copy_result,
        } = self
            .peer_sync_overlay
            .handle_event(event, modifiers, self.last_cursor);
        if let Some(request) = request {
            self.run_peer_sync_request(&request, copy_result);
        }
        consumed
    }
}
