//! The Wi-Fi password field in quick settings.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Hit rect for the back-chevron in the drill-in header. Click here
    /// pops `nav_stack`. Sized 32×32 with a small expansion for forgiving
    /// touch targets — the painted glyph is 24×24 inside.
    pub(super) fn qs_back_chevron_rect(&self) -> Rect {
        DrillInList::back_rect(self.quick_settings_rect())
    }

    /// Raise the passphrase prompt for `ssid`.
    pub fn begin_wifi_password(&mut self, ssid: &str) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        {
            let mut st = chrome.borrow_mut();
            st.quick_settings.wifi_password =
                Some(crate::shell::quick_settings::WifiPasswordPrompt::new(ssid));
            st.bump();
        }
        self.sync_wifi_password_input();
    }

    /// Drop the prompt, clearing the passphrase with it. The buffer only
    /// exists while it is being typed; nothing keeps it afterwards.
    pub fn cancel_wifi_password(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        {
            let mut st = chrome.borrow_mut();
            if st.quick_settings.wifi_password.take().is_some() {
                st.bump();
            }
        }
        self.sync_wifi_password_input();
    }

    /// Attempt the join. `nmcli device wifi connect` blocks until it
    /// succeeds or fails, which can be seconds on a busy band — but a
    /// background join with no prompt left to report into would be
    /// worse. The prompt stays up on failure, carrying the reason.
    pub fn submit_wifi_password(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        self.sync_wifi_password_input();
        let password = self.wifi_password_input.value();
        let Some(ssid) = ({
            let st = chrome.borrow();
            st.quick_settings
                .wifi_password
                .as_ref()
                .filter(|p| p.can_submit(password))
                .map(|p| p.ssid.clone())
        }) else {
            return;
        };
        let password = password.to_owned();
        {
            let mut st = chrome.borrow_mut();
            if let Some(p) = st.quick_settings.wifi_password.as_mut() {
                p.connecting = true;
                p.error = None;
            }
            st.quick_settings.wifi_joining = Some(ssid.clone());
            st.bump();
        }
        self.qs_refresh.kick_wifi_join(ssid, Some(password), false);
    }

    /// Use a credential from another certified device without ever putting it
    /// into the visible password editor. A rejected saved secret leaves the
    /// ordinary prompt in place so the person can correct it.
    pub(super) fn try_connect_with_shared_wifi_password(&mut self) -> bool {
        let (Some(chrome), Some(bridge)) = (self.chrome.clone(), self.wifi_secret_bridge.as_ref())
        else {
            return false;
        };
        let scanned = {
            let state = chrome.borrow();
            state
                .quick_settings
                .wifi_password
                .as_ref()
                .and_then(|prompt| {
                    scanned_wifi_security(&state.quick_settings.wifi_networks, &prompt.ssid)
                        .map(|security| (prompt.ssid.clone(), security))
                })
        };
        // A network the last scan did not list has no known mode to match a
        // shared secret against.
        let Some((ssid, security)) = scanned else {
            return false;
        };
        let Ok(Some(password)) = bridge(WifiSecretAction::Lookup {
            ssid: ssid.clone(),
            security,
        }) else {
            return false;
        };
        if let Some(prompt) = chrome.borrow_mut().quick_settings.wifi_password.as_mut() {
            prompt.connecting = true;
            prompt.error = None;
        }
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.wifi_joining = Some(ssid.clone());
            state.bump();
        }
        self.qs_refresh.kick_wifi_join(ssid, Some(password), true);
        true
    }

    /// Put `text` in the passphrase field of the prompt that is up, caret
    /// at its end or, with `select_all`, selected as after a rejection.
    pub fn seed_wifi_password_for_tests(&mut self, text: &str, select_all: bool) {
        if !self.sync_wifi_password_input() {
            return;
        }
        self.wifi_password_input.set_text(text);
        if select_all {
            self.wifi_password_input.select_all();
        } else {
            self.wifi_password_input.set_cursor_pos(text.len());
        }
    }

    pub(super) fn new_wifi_password_input() -> TextInput {
        TextInput::local()
            .with_id("qs-wifi-password")
            .with_placeholder(t("portal-network-password"))
            .with_secret(true)
            .with_enter_submits(true)
    }

    /// Bind the passphrase field to the prompt up now. Another prompt, or
    /// none, gets a fresh field — empty and hidden — and the old value is
    /// wiped, so nothing typed for one network survives into the next.
    /// Whether a prompt is up.
    pub(super) fn sync_wifi_password_input(&mut self) -> bool {
        let current = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .map(|prompt| prompt.id)
        });
        if current != self.wifi_password_prompt {
            self.wifi_password_input = Self::new_wifi_password_input();
            self.wifi_password_prompt = current;
        }
        // The field is always focused; nothing else on the pane takes
        // the keyboard while the prompt owns it.
        self.wifi_password_input.set_focus(current.is_some());
        current.is_some()
    }

    pub(super) fn is_pointer_event(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        )
    }

    /// Whether a text field that arbitrates ahead of the Wi-Fi passphrase
    /// holds the keyboard: the gates [`Self::handle_shell_keyboard_event`]
    /// runs before its chords.
    pub(super) fn keyboard_held_ahead_of_wifi_password(&self) -> bool {
        let renaming = !self.palette_visible
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().editing_workspace_name.is_some());
        renaming
            || self.workspace_settings_overlay.member_sheet_is_open()
            || (self.overlay_flags().shortcut_help && self.shortcut_rebind.is_some())
    }

    /// Routes an event into the passphrase field, if a prompt is up: keys
    /// and IME text to edit it (Enter joins, Escape backs out), pointer
    /// events for its caret and show/hide control. `true` when the prompt
    /// used the event.
    pub(super) fn route_wifi_password_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.sync_wifi_password_input() {
            return false;
        }
        let field = self.qs_password_field_rect();
        let pointer = Self::is_pointer_event(event);
        let pressed_in_field = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) && self.last_cursor.is_some_and(|point| field.contains(point));
        let key = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => Some(logical_key),
            WindowEvent::Ime(_) => None,
            _ if pointer => None,
            _ => return false,
        };
        self.wifi_password_input.apply_layout_external(field);
        let mut used = false;
        let mut taffy = TaffyTree::<()>::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return false;
        };
        // The prompt keeps the field focused itself; the node is a stand-in
        // that must never become the frame's focus target.
        let mut focus_target = None;
        self.wifi_password_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                taffy: &taffy,
                handled: &mut used,
                focus_target: &mut focus_target,
                ..ctx.reborrow(ctx.path, node)
            },
        );
        self.wifi_password_input.set_focus(true);
        // The value stays in the field; drop the copy this hands out.
        let edited = self.wifi_password_input.take_dirty().is_some_and(|value| {
            drop(zeroize::Zeroizing::new(value));
            true
        });
        if let Some(chrome) = self.chrome.as_ref()
            && (used || edited || pointer)
        {
            let mut st = chrome.borrow_mut();
            // Typing clears a rejection message — it was about the old value.
            if edited && let Some(prompt) = st.quick_settings.wifi_password.as_mut() {
                prompt.error = None;
            }
            st.bump();
        }
        if self.wifi_password_input.take_submit() {
            self.submit_wifi_password();
            *ctx.handled = true;
            return true;
        }
        if !used && matches!(key, Some(Key::Named(NamedKey::Escape))) {
            self.cancel_wifi_password();
            *ctx.handled = true;
            return true;
        }
        // A key the field passes on (Tab, a shortcut chord) falls through;
        // a press on the field is the field's whatever it did with it.
        let claimed =
            (used && !matches!(event, WindowEvent::CursorMoved { .. })) || pressed_in_field;
        if claimed {
            *ctx.handled = true;
        }
        claimed
    }

    /// The inline passphrase field, where the Wi-Fi section leaves room for
    /// it; empty while it is not shown. Derived rather than stored so the
    /// hit tests and the paint cannot drift apart.
    pub(super) fn qs_password_field_rect(&self) -> Rect {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                self.qs_root
                    .wifi_password_field_rect(&chrome.borrow().quick_settings)
            })
            .unwrap_or(Rect::ZERO)
    }

    /// Cancel and Connect under the inline passphrase.
    pub(super) fn qs_password_button_rects(&self) -> (Rect, Rect) {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                self.qs_root
                    .wifi_password_button_rects(&chrome.borrow().quick_settings)
            })
            .unwrap_or((Rect::ZERO, Rect::ZERO))
    }

    /// The passphrase field, hidden until its show/hide control is used.
    /// Its status line and buttons are the Wi-Fi section's.
    pub(super) fn draw_qs_wifi_password_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        field: Rect,
    ) {
        self.wifi_password_input.apply_layout_external(field);
        self.wifi_password_input.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::default(),
        );
    }

    /// Root slider geometry exposed for integration click routing tests.
    pub fn quick_settings_slider_geometry(&self, i: usize) -> (Rect, f64) {
        QuickSettingsRoot::slider_geometry(self.quick_settings_rect(), i)
    }

    /// The Quick Settings target named `name` (`quick_settings_network`,
    /// `quick_settings_network_details`, `quick_settings_battery`, …) where
    /// it is on screen now, for integration click routing tests.
    pub fn quick_settings_target(&self, name: &str) -> Option<Rect> {
        self.quick_settings_semantic_rects()
            .into_iter()
            .find(|(target, _)| target == name)
            .map(|(_, rect)| rect)
    }
}
