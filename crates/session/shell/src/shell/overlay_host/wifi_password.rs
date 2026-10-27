//! The Wi-Fi password field in quick settings.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use ui::widget::{FieldRole, Presentation, PromptOutcome, SecretKind};

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

    /// Join with what the prompt holds, as Connect and Enter do. The
    /// join runs off this thread; the prompt stays up, busy, until it
    /// succeeds or fails, and on failure says why.
    pub fn submit_wifi_password(&mut self) {
        if !self.sync_wifi_password_input() {
            return;
        }
        self.wifi_prompt.submit();
        self.settle_wifi_prompt();
    }

    /// Carry out what the person did in the prompt: join with the password
    /// or back out.
    pub(super) fn settle_wifi_prompt(&mut self) {
        match self.wifi_prompt.take_outcome() {
            Some(PromptOutcome::Cancel) => self.cancel_wifi_password(),
            Some(PromptOutcome::Submit(answer)) => self.join_with_wifi_password(answer),
            None => {}
        }
    }

    fn join_with_wifi_password(&mut self, mut answer: ui::widget::SecretAnswer) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(ssid) = ({
            let st = chrome.borrow();
            st.quick_settings
                .wifi_password
                .as_ref()
                .filter(|p| p.can_submit(&answer.secret))
                .map(|p| p.ssid.clone())
        }) else {
            return;
        };
        // The answer's own buffer moves on to the join. The field keeps
        // its text, so a rejection can select it for retyping.
        let password = std::mem::take(&mut *answer.secret);
        {
            let mut st = chrome.borrow_mut();
            if let Some(p) = st.quick_settings.wifi_password.as_mut() {
                p.connecting = true;
                p.error = None;
            }
            st.quick_settings.wifi_joining = Some(ssid.clone());
            st.bump();
        }
        self.wifi_prompt.set_busy(Some(t("qs-connecting")));
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
        self.sync_wifi_password_input();
        self.qs_refresh.kick_wifi_join(ssid, Some(password), true);
        true
    }

    /// Put `text` in the passphrase field of the prompt that is up, caret
    /// at its end or, with `select_all`, selected as after a rejection.
    pub fn seed_wifi_password_for_tests(&mut self, text: &str, select_all: bool) {
        if !self.sync_wifi_password_input() {
            return;
        }
        if let Some(field) = self.wifi_prompt.field_mut(FieldRole::Secret) {
            field.set_text(text);
            if select_all {
                field.select_all();
            } else {
                field.set_cursor_pos(text.len());
            }
        }
    }

    /// The passphrase field of the prompt.
    pub fn wifi_password_field(&self) -> Option<&TextInput> {
        self.wifi_prompt.field(FieldRole::Secret)
    }

    /// A fresh prompt: an empty, hidden field with the keyboard.
    pub(super) fn new_wifi_prompt() -> SecretPrompt {
        let mut prompt = SecretPrompt::new(SecretKind::Secret)
            .with_presentation(Presentation::Inline)
            .with_id("qs-wifi-password")
            .with_label(FieldRole::Secret, t("portal-network-password"))
            .with_primary_label(t("portal-connect"))
            .with_cancel_label(t("portal-cancel"));
        prompt.set_notice(Some(t("qs-wifi-password-for")));
        Widget::set_focus(&mut prompt, true);
        prompt
    }

    /// Bind the prompt to the one Quick Settings asks with now. Another
    /// prompt, or none, gets a fresh one — empty and hidden — and the old
    /// value is wiped, so nothing typed for one network survives into the
    /// next. The join's progress and its failure are shown in it. Whether
    /// a prompt is up.
    pub(super) fn sync_wifi_password_input(&mut self) -> bool {
        let current = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .quick_settings
                .wifi_password
                .as_ref()
                .map(|prompt| (prompt.id, prompt.connecting, prompt.error.clone()))
        });
        let id = current.as_ref().map(|(id, _, _)| *id);
        if id != self.wifi_password_prompt {
            self.wifi_prompt = Self::new_wifi_prompt();
            self.wifi_password_prompt = id;
        }
        let Some((_, connecting, error)) = current else {
            Widget::set_focus(&mut self.wifi_prompt, false);
            return false;
        };
        // Nothing else on the pane takes the keyboard while the prompt is
        // up; Tab walks its own field and buttons.
        if !Widget::is_focused(&self.wifi_prompt) {
            Widget::set_focus(&mut self.wifi_prompt, true);
        }
        if connecting != self.wifi_prompt.busy().is_some() {
            self.wifi_prompt
                .set_busy(connecting.then(|| t("qs-connecting")));
        }
        if let Some(error) = error
            && !connecting
            && self.wifi_prompt.error() != Some(error.as_str())
        {
            self.wifi_prompt.reject(error);
        }
        true
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

    /// Routes an event into the prompt, if one is up: keys and IME text to
    /// its field and buttons (Enter joins, Escape backs out, Tab walks
    /// them), pointer and touch events for its caret, its show/hide control
    /// and its buttons. `true` when the prompt used the event.
    pub(super) fn route_wifi_password_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.sync_wifi_password_input() {
            return false;
        }
        let pointer = Self::is_pointer_event(event);
        if !pointer
            && !matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::CursorLeft { .. }
            )
        {
            return false;
        }
        // A press is placed where the pointer last was, which may have
        // been over a window whose motion never reached this tree.
        if let (WindowEvent::MouseInput { .. }, Some(point)) = (event, self.last_cursor) {
            let moved = ui::synthetic_input::cursor_moved(
                point.x * ctx.scale_factor,
                point.y * ctx.scale_factor,
            );
            self.forward_to_wifi_prompt(&moved, ctx);
        }
        let used = self.forward_to_wifi_prompt(event, ctx);
        if let Some(chrome) = self.chrome.as_ref()
            && (used || pointer)
        {
            let mut st = chrome.borrow_mut();
            // Typing clears a rejection message — it was about the old value.
            if self.wifi_prompt.error().is_none()
                && let Some(prompt) = st.quick_settings.wifi_password.as_mut()
            {
                prompt.error = None;
            }
            st.bump();
        }
        self.settle_wifi_prompt();
        if used {
            *ctx.handled = true;
        }
        used
    }

    /// Give `event` to the prompt, which is drawn outside any widget tree;
    /// `true` when it used the event.
    fn forward_to_wifi_prompt(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let mut used = false;
        let mut taffy = TaffyTree::<()>::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return false;
        };
        // The prompt keeps its own focus; the node is a stand-in that must
        // never become the frame's focus target.
        let mut focus_target = None;
        self.wifi_prompt.handle_event(
            event,
            &mut ui::widget::EventCtx {
                taffy: &taffy,
                handled: &mut used,
                focus_target: &mut focus_target,
                ..ctx.reborrow(ctx.path, node)
            },
        );
        used
    }

    /// The inline prompt, where the Wi-Fi section leaves room for it;
    /// empty while it is not shown. Derived rather than stored so the hit
    /// tests and the paint cannot drift apart.
    pub(super) fn qs_password_rect(&self) -> Rect {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                self.qs_root
                    .wifi_password_rect(&chrome.borrow().quick_settings)
            })
            .unwrap_or(Rect::ZERO)
    }

    /// The prompt — field, status line, Cancel and Connect — where the
    /// Wi-Fi section leaves room for it. The section is told how tall the
    /// prompt is, so it can leave exactly that much.
    pub(super) fn draw_qs_wifi_prompt(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        self.wifi_prompt.place(rect, text_renderer, theme);
        self.wifi_prompt
            .paint(scene, transform, text_renderer, theme);
        let height = ui::num::u32_of_f64(
            self.wifi_prompt
                .preferred_height(rect.width(), text_renderer, theme)
                .ceil(),
        );
        if let Some(chrome) = self.chrome.as_ref() {
            let mut st = chrome.borrow_mut();
            if let Some(prompt) = st.quick_settings.wifi_password.as_mut()
                && prompt.height != height
            {
                prompt.height = height;
                st.bump();
            }
        }
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
