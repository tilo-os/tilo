//! Secret prompts, peer streams, native terminals and other foreign surfaces.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Shell chrome on screen that holds a secret: the administrator
    /// authentication and network-secrets sheets, a shortcut step that asks
    /// for a secret answer, the quick-settings pane
    /// while it asks for a Wi-Fi passphrase, the display sheet while it asks
    /// for an `AirPlay` PIN or password, a Bluetooth pairing dialog, and the
    /// setup steps that restore or back up the identity (passphrases and a
    /// nearby device's pairing code). A capture always draws these as the placeholder — see
    /// [`crate::shell::capture_protection`].
    pub fn secret_prompt_rects(&self) -> Vec<Rect> {
        let mut rects = Vec::new();
        let now = Instant::now();
        if let Some(rect) = self
            .onboarding
            .as_ref()
            .and_then(crate::onboarding_view::OnboardingView::secret_rect)
        {
            rects.push(rect);
        }
        if self.portal_dialogs.holds_secret()
            && let Some(layout) = self.portal_dialogs.layout(self.overlay_area())
        {
            rects.push(layout.sheet);
        }
        let asks_passphrase = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().quick_settings.wifi_password.is_some());
        if asks_passphrase && self.qs_motion.is_visible(now) {
            // The dropdown's own paint clip: it covers the pane at every
            // point of its open/close scale.
            rects.push(self.quick_settings_rect().inflate(8.0, 16.0));
        }
        rects.extend(self.projection_overlay.secret_prompt_rect(now));
        rects.extend(self.pairing.secret_rect());
        rects
    }

    /// Draw Bluetooth pairing questions from `handle` instead of the
    /// session's. Tests use a private handle so their prompts never reach
    /// another host.
    #[doc(hidden)]
    pub fn set_pairing_prompt_handle(&mut self, handle: crate::bluetooth::PairingPromptHandle) {
        self.pairing = crate::bluetooth_prompt::PairingOverlay::new(handle);
    }

    /// The Bluetooth pairing dialog, when one is open.
    pub fn pairing_prompt(&self) -> Option<&crate::bluetooth_prompt::PairingPromptView> {
        self.pairing.view()
    }

    /// Serve administrator-authentication and network-secrets sheets (and
    /// every other portal sheet) from `queue` instead of the session's.
    /// Tests use a private queue so their sheets never reach another host.
    #[doc(hidden)]
    pub fn set_portal_dialog_queue(
        &mut self,
        queue: crate::shell::portal_dialog::PortalDialogQueue,
    ) {
        self.portal_dialogs = PortalDialogController::new(queue);
        self.register_authentication_actions();
    }

    /// On-screen rect of the tile hosting foreign wayland surface
    /// `surface_id`, or `None` when no *visible* tile hosts it. Walks the
    /// live widget tree, not persisted state, so the answer is what's
    /// actually painted — what the compositor needs to size the client at.
    pub fn foreign_surface_rect(&mut self, surface_id: u64) -> Option<Rect> {
        let wanted = format!("foreign-surface-{surface_id}");
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — the pointer comes from a `Box<GroupRow>` owned by
            // the content tree, the host outlives the content, and
            // `&mut self` gives us unaliased access for this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
                let Some(host) = group.active_host_mut() else {
                    continue;
                };
                let ui::widget::TileContent::App(app) = host.content() else {
                    continue;
                };
                if app.test_id() == Some(wanted.as_str()) {
                    let rect = app.get_rect();
                    if rect.width() > 0.0 && rect.height() > 0.0 {
                        return Some(rect);
                    }
                }
            }
        }
        None
    }

    /// Replace one matching peer-window card with a live, session-local
    /// decoder. Returns false when the remote app has no corresponding tab;
    /// the caller may retry when the replicated window description arrives.
    pub fn mount_peer_stream(&mut self, stream: &crate::shell::peer_view::PeerStreamStart) -> bool {
        let crate::shell::peer_view::PeerStreamStart {
            source_peer,
            surface: surface_id,
            local_peer,
            ref app_id,
            ref codec,
            display_size,
            hdr_static_metadata,
            ref input_sink,
            ref keyframe_sink,
            tab,
        } = *stream;
        let key = (source_peer, surface_id);
        if let Some(tab_id) = self.peer_stream_tabs.get(&key).copied() {
            for &row_ptr in self.group_row_ptrs.values() {
                // SAFETY: rows are boxed by this host and `&mut self` makes
                // the traversal exclusive, as in `foreign_surface_rect`.
                let row: &mut GroupRow = unsafe { &mut *row_ptr };
                for group in &mut row.groups {
                    let Some(host) = group.host_for_tab_mut(tab_id) else {
                        continue;
                    };
                    let Some(app) = host.app_mut() else {
                        continue;
                    };
                    // A repeated stream start for a mounted share: the sender
                    // may have switched codec, so the view follows it.
                    if let Some(view) = app
                        .as_any_mut()
                        .and_then(|any| any.downcast_mut::<crate::shell::peer_view::PeerView>())
                    {
                        view.restart_stream(codec, display_size);
                        return true;
                    }
                }
            }
            // A workspace/root rebuild invalidated the runtime widget while
            // leaving the session alive. Forget only the runtime binding;
            // the caller's retained start descriptor remounts it below.
            self.peer_stream_tabs.remove(&key);
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let tab_id = self.stream_tab(&chrome, app_id, tab);
        let Some(tab_id) = tab_id else {
            return false;
        };
        let mut peer_view = crate::shell::peer_view::PeerView::new_for_stream(
            surface_id,
            local_peer,
            codec,
            crate::diagnostics::PeerSurfaceMetrics::default(),
            &crate::shell::hdr_display::DisplayCapabilities::sdr_default(),
            hdr_static_metadata,
        );
        peer_view.restart_stream(codec, display_size);
        peer_view.set_input_sink(std::sync::Arc::clone(input_sink));
        peer_view.set_keyframe_sink(std::sync::Arc::clone(keyframe_sink));
        peer_view.set_picture_wake(self.peer_picture_wake.clone());
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: identical to `foreign_surface_rect`: rows are boxed by
            // this host and `&mut self` makes this traversal exclusive.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
                if let Some(host) = group.host_for_tab_mut(tab_id) {
                    host.replace_content(ui::widget::TileContent::App(Box::new(peer_view)));
                    self.peer_stream_tabs.insert(key, tab_id);
                    return true;
                }
            }
        }
        false
    }

    /// The tab a stream starting now mounts into: exactly the one it names,
    /// wherever it is, or for a stream naming none the first free tab of its
    /// app in the active workspace.
    pub(super) fn stream_tab(&self, chrome: &Chrome, app_id: &str, tab: u64) -> Option<u64> {
        let state = chrome.borrow();
        let tree = state.tree();
        let unclaimed = |id: u64| !self.peer_stream_tabs.values().any(|claimed| *claimed == id);
        if tab != 0 {
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|candidate| {
                    candidate.id == tab
                        && matches!(
                            candidate.tile,
                            crate::shell::state::TileContent::HostedWindow { .. }
                                | crate::shell::state::TileContent::PeerWindow { .. }
                        )
                })
                .map(|candidate| candidate.id)
                .filter(|id| unclaimed(*id))
        } else {
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group.tabs.iter().find_map(|tab| match &tab.tile {
                            crate::shell::state::TileContent::PeerWindow {
                                app_id: candidate,
                                ..
                            }
                            | crate::shell::state::TileContent::HostedWindow {
                                app_id: candidate,
                                ..
                            } if candidate == app_id && unclaimed(tab.id) => Some(tab.id),
                            _ => None,
                        })
                    })
                })
            })
        }
    }

    /// Decode the next encoded unit (sequence number `seq`) into the mounted
    /// peer view. Returns whether the decoder took it.
    pub fn push_peer_stream_frame(
        &mut self,
        source_peer: u64,
        surface_id: u64,
        seq: u64,
        encoded: &[u8],
        latency_ns: Option<u64>,
    ) -> bool {
        self.peer_stream_view_mut(source_peer, surface_id)
            .is_some_and(|view| {
                matches!(
                    view.push_encoded_with_latency(seq, encoded, latency_ns),
                    crate::shell::peer_view::PushOutcome::Decoded
                        | crate::shell::peer_view::PushOutcome::DroppedOldest
                        | crate::shell::peer_view::PushOutcome::Pending
                )
            })
    }

    /// How the loop learns that a peer-stream picture finished decoding:
    /// peer views never wait for their pictures, so `wake` (run on a media
    /// thread) brings the loop back to [`Self::collect_late_peer_pictures`].
    /// Views mounted from now on use it.
    pub fn set_peer_picture_wake(&mut self, wake: media::realtime::PictureWake) {
        self.peer_picture_wake = Some(wake);
    }

    /// Show every peer-stream picture that finished decoding after its frame
    /// was pushed. A window that stops changing sends no further frame to
    /// carry its last picture out of the decoder, so the loop collects it.
    /// Returns whether any view took a new picture.
    pub fn collect_late_peer_pictures(&mut self) -> bool {
        if self.peer_stream_tabs.is_empty() {
            return false;
        }
        let streams: Vec<(u64, u64)> = self.peer_stream_tabs.keys().copied().collect();
        let mut collected = false;
        for (source_peer, surface_id) in streams {
            if let Some(view) = self.peer_stream_view_mut(source_peer, surface_id)
                && view.picture_pending()
            {
                collected |= view.collect_late_picture().is_some();
            }
        }
        collected
    }

    /// The view showing the stream of `surface_id` from `source_peer`.
    pub fn peer_stream_view_mut(
        &mut self,
        source_peer: u64,
        surface_id: u64,
    ) -> Option<&mut crate::shell::peer_view::PeerView> {
        let tab_id = self
            .peer_stream_tabs
            .get(&(source_peer, surface_id))
            .copied()?;
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same row ownership invariant as `mount_peer_stream`.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
                let Some(host) = group.host_for_tab_mut(tab_id) else {
                    continue;
                };
                let app = host.app_mut()?;
                return app
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<crate::shell::peer_view::PeerView>());
            }
        }
        None
    }

    /// Whether a view shows the stream of `surface_id` from `source_peer`.
    pub fn has_peer_stream(&self, source_peer: u64, surface_id: u64) -> bool {
        self.peer_stream_tabs
            .contains_key(&(source_peer, surface_id))
    }

    /// Drop a session-local peer decoder and restore the persisted
    /// peer-window card underneath it.
    pub fn unmount_peer_stream(&mut self, source_peer: u64, surface_id: u64) -> bool {
        let Some(tab_id) = self.peer_stream_tabs.remove(&(source_peer, surface_id)) else {
            return false;
        };
        let tile = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| tab.id == tab_id)
                            .map(|tab| tab.tile.clone())
                    })
                })
            })
        });
        let Some(tile) = tile else {
            return false;
        };
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same row ownership invariant as `mount_peer_stream`.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
                if let Some(host) = group.host_for_tab_mut(tab_id) {
                    host.replace_content(crate::shell::to_ui_tile(&tile));
                    return true;
                }
            }
        }
        false
    }

    /// Whether any tab on screen is still holding a place for an app that
    /// hasn't drawn yet (see [`ui::widget::WindowGroup::any_tile_opening`]).
    /// A group in this state requests animation frames for as long as it
    /// waits — deliberately indistinguishable from a wedged animation from
    /// the outside; this method is how a caller waiting for the tree to go
    /// quiet tells them apart.
    pub fn any_tile_opening(&mut self) -> bool {
        let mut any_opening = false;
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — the pointer comes from a `Box<GroupRow>` owned by
            // the content tree, the host outlives the content, and
            // `&mut self` gives us unaliased access for this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            // Do not short-circuit: asking every group is what latches a
            // newly published client frame as presented. Returning at the
            // first opening group would leave later foreign tabs opening
            // forever even though their buffers are already available.
            any_opening = row
                .groups
                .iter()
                .map(ui::widget::WindowGroup::any_tile_opening)
                .fold(any_opening, |seen, opening| opening || seen);
        }
        any_opening
    }

    /// Drain app-owned requests for shell chrome after the app's event path
    /// has run. The request carries stable resource identity, so opening
    /// Activity does not depend on a file's current path or display label.
    pub fn connect_native_terminal(
        &mut self,
        connection: u64,
        writer: std::sync::mpsc::Sender<Vec<u8>>,
    ) {
        if self.terminal_device_shell.is_none() {
            let provider = crate::shell::search::providers::terminal_device_shell::TerminalDeviceShellProvider::default();
            self.search_registry
                .register(std::rc::Rc::new(provider.clone()));
            self.terminal_device_shell = Some(provider);
        }
        if let Some(provider) = &self.terminal_device_shell {
            provider.connect(connection, writer);
        }
    }

    pub fn complete_native_terminal(
        &mut self,
        connection: u64,
        request: u64,
        result: Result<serde_json::Value, String>,
    ) {
        if let Some(provider) = &self.terminal_device_shell {
            provider.complete(connection, request, result);
        }
    }

    pub fn disconnect_native_terminal(&mut self, connection: u64) {
        if let Some(provider) = &self.terminal_device_shell {
            provider.disconnect(connection);
        }
    }

    pub fn request_native_host_shell_allow(&mut self, writer: std::sync::mpsc::Sender<Vec<u8>>) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let (prompt, pending) = crate::shell::host_shell_allow::native_prompt(writer);
        let mut chrome = chrome.borrow_mut();
        chrome.pending_grants.push_back(prompt);
        chrome.bump();
        self.host_shell_allows.push(pending);
    }

    /// Settings may request topology through the same admitted app-host channel.
    /// Same identity and topology observations never mint an input lease.
    /// # Errors
    /// Rejects non-Settings callers, locked or non-owner sessions, and unavailable peers.
    pub fn request_device_display_move(
        &self,
        app_id: &str,
        request: settings_store::device_displays::DeviceDisplayMove,
    ) -> Result<(), String> {
        let admitted = app_id == "so.tilo.settings"
            && self.chrome.as_ref().is_some_and(|chrome| {
                let state = chrome.borrow();
                !state.session_locked() && is_active_workspace_owner(&state)
            });
        if !admitted {
            return Err(t("device-fabric-error-layout-changed"));
        }
        let bridge = self
            .device_fabric_bridge
            .as_ref()
            .ok_or_else(|| t("device-fabric-error-no-identity"))?;
        bridge(DeviceFabricUiAction::MoveDisplay(request)).map(|_| ())
    }

    pub(super) fn active_tab_app_id(
        chrome: &crate::shell::chrome_state::Chrome,
        source_tab: crate::shell::state::TabId,
    ) -> Option<String> {
        let state = chrome.borrow();
        state
            .tree()
            .desktop
            .active_workspace()
            .and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .any(|tab| tab.id == source_tab)
                            .then(|| state.tab_app_id(strip.id, group.id, source_tab))
                            .flatten()
                    })
                })
            })
    }
}
