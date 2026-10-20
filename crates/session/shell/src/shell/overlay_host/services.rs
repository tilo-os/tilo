//! Host services: quick settings and status bridges, emoji, clipboard, OSD.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Replace the QS action dispatcher. Tests use this to swap in a
    /// recording impl that captures every `set_*` call instead of
    /// shelling out to `nmcli` / `brightnessctl` / `bluetoothctl` /
    /// `wpctl` / `powerprofilesctl`. Production builds keep the
    /// default [`crate::shell::quick_settings::actions::RealQsActions`].
    #[must_use]
    pub fn with_qs_actions(
        mut self,
        actions: Box<dyn crate::shell::quick_settings::actions::QsActions>,
    ) -> Self {
        let actions: std::rc::Rc<dyn crate::shell::quick_settings::actions::QsActions> =
            std::rc::Rc::from(actions);
        self.qs_actions = actions.clone();
        self.qs_action_slot.replace(
            actions,
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        );
        // Replacing where quick settings writes means this host is not
        // driving the host, so it must not read from it either.
        self.qs_refresh.disable_os_reads();
        self
    }

    /// Supply the durable Quick Settings arrangement explicitly. Production
    /// uses the persisted per-user configuration; embedders and interaction
    /// tests can inject a deterministic arrangement without reading or
    /// rewriting the desktop user's settings file.
    #[must_use]
    pub fn with_quick_settings_config(
        mut self,
        config: settings_store::hardware::QuickSettingsConfig,
    ) -> Self {
        self.qs_root = QuickSettingsRoot::with_config(config);
        self
    }

    /// Install the live-status bridge. Only the compositor's real-session
    /// constructors do this; without it the tray's readings stay `None`
    /// and it draws no indicators, which is what keeps golden renders
    /// identical across machines.
    #[must_use]
    pub fn with_status_bridge(mut self, bridge: crate::shell::status::StatusBridge) -> Self {
        self.status_bridge = Some(bridge);
        // A spawn failure means resource exhaustion on the host; the session
        // still runs, just without persistent audio-balance/mono transforms.
        self.session_audio_policy =
            match crate::shell::session_audio_policy::SessionAudioPolicyOwner::spawn() {
                Ok(owner) => Some(owner),
                Err(error) => {
                    tracing::warn!(
                        target: "tilo::audio_policy",
                        %error,
                        "failed to spawn the session audio policy worker"
                    );
                    None
                }
            };
        self
    }

    /// Audio devices that arrived or left since the last call.
    pub fn drain_audio_device_sounds(
        &self,
    ) -> Vec<settings_store::system_sounds::SystemSoundEvent> {
        self.session_audio_policy
            .as_ref()
            .map(crate::shell::session_audio_policy::SessionAudioPolicyOwner::drain_device_sounds)
            .unwrap_or_default()
    }

    /// Where the last paint repeated banners about the whole machine on the
    /// displays other than the one in use.
    #[cfg(test)]
    pub(crate) fn machine_banner_rects(&self) -> &[Rect] {
        &self.machine_banners
    }

    /// The displays the last paint edged for a ringing call.
    #[cfg(test)]
    pub(crate) fn call_edge_rects(&self) -> &[Rect] {
        &self.call_edges
    }

    /// The QS action dispatcher, for callers that actuate hardware
    /// outside the popover — the media keys, which drive the same
    /// volume and brightness the sliders do and must go through the same
    /// dispatcher so a test can record them.
    pub fn qs_actions(&self) -> &dyn crate::shell::quick_settings::actions::QsActions {
        self.qs_actions.as_ref()
    }

    /// Handle on the status bridge, for the paths that publish a reading
    /// rather than consume one (the volume keys, which must move the
    /// indicator in the frame they were pressed rather than up to a poll
    /// interval later).
    pub fn status_bridge(&self) -> Option<&crate::shell::status::StatusBridge> {
        self.status_bridge.as_ref()
    }

    /// Open the emoji picker at the focused field's caret (centred on the
    /// display with focus when the field reports none), or close it.
    pub fn toggle_emoji_picker(&mut self) {
        if self.layers.emoji_picker.is_open() {
            self.layers.emoji_picker.close();
        } else {
            let caret = self.text_caret;
            let outputs = self.layer_outputs();
            let viewport = self.layer_geometry(&outputs).display_for(caret);
            self.layers.emoji_picker.open_at(
                viewport,
                caret,
                &crate::shell::i18n::current_locale(),
            );
        }
        self.bump_chrome();
    }

    pub fn emoji_picker_open(&self) -> bool {
        self.layers.emoji_picker.is_open()
    }

    /// Where the focused text field's caret is, in desktop coordinates, or
    /// `None` when no field takes text or it has not said.
    pub fn set_text_caret(&mut self, caret: Option<Rect>) {
        self.text_caret = caret;
    }

    /// The emoji picked since the last call, for the compositor to commit
    /// into the focused field.
    pub fn take_emoji_commits(&mut self) -> Vec<String> {
        self.layers.emoji_picker.take_commits()
    }

    /// The clipboard history items chosen to paste (Super+V) since the last
    /// call, for the compositor to commit into the focused field.
    pub fn take_clipboard_pastes(&mut self) -> Vec<[u8; 32]> {
        std::mem::take(&mut self.clipboard_pastes)
    }

    /// Clipboard history choices to paste into an app that runs inside the
    /// shell, ready now: chosen one frame earlier, so the app has the
    /// keyboard back. The frame loop sends each that app's own paste
    /// (Ctrl+V), which reads the clipboard the choice just set.
    pub fn take_in_process_pastes(&mut self) -> usize {
        let (queued, ready) = self.in_process_pastes;
        self.in_process_pastes = (0, queued);
        ready
    }

    /// Make history item `id` the clipboard and close Quick Settings. Opened
    /// to paste (Super+V), the item also goes into the field history was
    /// summoned from, the way choosing it in any clipboard manager does;
    /// browsed from Quick Settings, it is only copied.
    pub(super) fn choose_clipboard_item(&mut self, id: [u8; 32]) {
        self.qs_selected_clipboard = Some(id);
        let _ = self.clipboard_manager_call(ClipboardManagerAction::Activate(id));
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        if state.quick_settings.paste_origin.take().is_some() {
            if focused_tab_is_wayland_window(&state) {
                self.clipboard_pastes.push(id);
            } else {
                // An app linked into the shell has no Wayland window to
                // receive a paste; its own paste path is called directly.
                self.in_process_pastes.0 = self.in_process_pastes.0.saturating_add(1);
            }
        }
        state.overlays.quick_settings = false;
        state.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
        state.bump();
    }

    /// The compositor committed `text` into a field; a secret field's emoji
    /// stay out of the recents.
    pub fn note_emoji_delivered(&mut self, text: &str, sensitive: bool) {
        self.layers.emoji_picker.note_delivered(text, sensitive);
    }

    /// Keep the emoji recents in the user's private state.
    pub fn attach_emoji_recents_store(
        &mut self,
        store: crate::shell::emoji_recents::FileRecentsStore,
    ) {
        self.layers.emoji_picker.attach_store(Box::new(store));
    }

    /// The picker's state, for tests and the control socket's state dump.
    pub fn emoji_picker(&self) -> &tilo_desktop::emoji_picker::EmojiPicker {
        &self.layers.emoji_picker
    }

    /// Open global search (the palette scoped to everything).
    pub fn open_global_search(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
        self.refresh_search_results(doc, root);
    }

    /// Open an installed app through the launcher the palette uses, so a
    /// hardware launch key gets the same placement, duplicate focusing and
    /// missing-package notice as a search result.
    pub fn launch_app(&self, package_id: &str, display_name: &str) {
        if let Some(chrome) = self.chrome.as_ref() {
            (self.app_launcher)(chrome, package_id, display_name);
        }
    }

    /// The chrome as the shell narrator reads it: the same semantics
    /// published to AT-SPI, whose live regions carry the notification
    /// banner, the hardware-key readout and a held pad's mapping.
    pub fn chrome_narration_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};
        Widget::accessibility(self)
            .unwrap_or_else(|| AccessNode::new(Role::Group).with_name(t("a11y-workspace")))
    }

    /// Paint what a held pad's controls do, if a pad map is up.
    pub(super) fn draw_pad_map(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let chrome = chrome.borrow();
        if let Some(map) = chrome.pad_map.as_ref() {
            map.draw(scene, transform, text_renderer, theme, self.overlay_area());
        }
    }

    /// Where the hardware-key readout (volume, brightness) was last
    /// painted; `None` while none is up.
    pub fn osd_rect(&self) -> Option<Rect> {
        self.osd_painted.get()
    }

    /// Paint the transient hardware-key readout, if one is up.
    pub(super) fn draw_osd(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.osd_painted.set(None);
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let now = Instant::now();
        // Expiring here rather than on a timer ties the slab's lifetime
        // to frames that actually paint it, not an idle compositor.
        {
            let mut st = chrome.borrow_mut();
            st.expire_osd(now);
        }
        let Some(osd) = chrome.borrow().osd else {
            self.osd_pin.set(None);
            return;
        };
        // The pad map names the mode itself.
        if chrome.borrow().pad_map.is_some()
            && matches!(osd.kind, crate::shell::osd::OsdKind::PadMode { .. })
        {
            return;
        }
        // On the display the person is working on when it was raised: across
        // a 2x panel and a 1x monitor, the desktop's bottom centre is on the
        // monitor, below the panel's last row. It stays there while it is up,
        // and the next press made from another display brings it there.
        let displays = self.layer_outputs();
        let (pin, restart) = crate::shell::osd::pin_display(
            self.osd_pin.get(),
            &osd,
            self.overlay_area(),
            &displays,
        );
        self.osd_pin.set(Some(pin));
        let osd = if restart {
            let mut st = chrome.borrow_mut();
            if let Some(live) = st.osd.as_mut() {
                live.shown_since = live.raised_at;
            }
            st.osd.unwrap_or(osd)
        } else {
            osd
        };
        let alpha = osd.alpha_in(theme, now);
        if alpha <= 0.0 {
            return;
        }
        // Clear of the caret there.
        let area = crate::shell::osd::OsdArea {
            viewport: pin.display,
            caret: self.text_caret,
        };
        self.osd_painted
            .set(osd.draw(scene, transform, text_renderer, theme, area, alpha));
    }

    /// Paint the passing acknowledgement ("Enrollment token copied"), if one
    /// is up. Same slot and frame path as the OSD: woken by
    /// [`Widget::next_wake_deadline`] for its fade and end, animated through
    /// the fade.
    pub(super) fn draw_acknowledgement(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.acknowledgement_reduce_motion.set(theme.reduce_motion);
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let now = ui::time::now();
        let Some(ack) = chrome.borrow().acknowledger.current(now) else {
            return;
        };
        let alpha = ack.alpha_at(now, theme.reduce_motion);
        ack.draw(
            scene,
            transform,
            text_renderer,
            theme,
            self.overlay_area(),
            alpha,
        );
    }

    /// The acknowledgement showing now, if any.
    pub(super) fn current_acknowledgement(
        &self,
        now: ui::time::Instant,
    ) -> Option<ui::widget::acknowledgement::Acknowledgement> {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().acknowledger.current(now))
    }

    /// Copy the poller's latest reading onto the chrome, bumping the
    /// repaint-triggering revision only when it actually changed.
    pub(super) fn sync_status_from_bridge(&self) {
        let Some(bridge) = self.status_bridge.as_ref() else {
            return;
        };
        let next = bridge.snapshot();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.status != next {
            // The bridge is the shell's one volume: a level that changed
            // there — a media key, the poller, the panel's own read — moves
            // the panel's slider too, so the two never show different values.
            if st.status.volume != next.volume
                && let Some(volume) = next.volume
            {
                st.quick_settings.volume = volume.level;
            }
            st.quick_settings.link.clone_from(&next.link);
            st.status = next;
            st.bump();
        }
    }
}
