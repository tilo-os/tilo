//! The lock screen's input + authentication state machine.
//!
//! While the session is locked ([`crate::compositor::idle::SessionManager::is_locked`]),
//! the compositor routes **all** keyboard input into this controller instead
//! of to apps/chrome — that redirection is the input half of the lock's
//! security boundary. Only a credential the [`PasswordVerifier`] accepts
//! produces [`UnlockOutcome::Unlocked`]; nothing else can, so the session
//! cannot be bypassed.
//!
//! # Asynchronous verification
//!
//! Verifying a credential can be slow (a PAM helper subprocess today,
//! biometrics later), and running that on the compositor's run loop would
//! freeze the whole desktop for the duration. So [`submit`] never verifies
//! inline: it hands `(username, password)` to a worker thread and returns
//! immediately, leaving the session locked. The run loop polls
//! [`poll_verification`] each tick; the session unlocks **only** when that
//! poll observes the worker's success. While a verify is in flight the
//! field is frozen and [`is_verifying`] is `true`.
//!
//! The password is typed into the one password prompt every part of Tilo
//! asks with ([`LockPrompt`], a [`SecretPrompt`]): its field, its error line
//! and its show/hide button are the lock screen's.
//!
//! [`submit`]: LockController::submit
//! [`poll_verification`]: LockController::poll_verification
//! [`is_verifying`]: LockController::is_verifying

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::lock_auth::PasswordVerifier;
use ui::text_selection::prev_char_boundary;
use ui::widget::{EventCtx, FieldRole, Presentation, SecretKind, SecretPrompt, Widget};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::Zeroizing;

use crate::shell::i18n::t;

/// The non-leaky error shown if the verify worker vanishes without a result
/// (thread panic / dropped channel). Fail-closed: treat it as "stay locked",
/// never as an unlock.
const WORKER_LOST_MSG: &str = "Password verification is unavailable";

/// Upper bound on the password length. Generous for any real passphrase;
/// bounds memory against a key held down / a paste flood while locked.
const MAX_PASSWORD_LEN: usize = 1024;

/// Result of feeding input to the controller: did this input unlock?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The session remains locked (wrong/empty password, or a non-submit
    /// key). The overlay stays up and input stays captured.
    StillLocked,
    /// The credential was accepted — the caller must clear the compositor
    /// lock and restore normal presentation + input.
    Unlocked,
}

/// What the keyboard drawn for touch and assistive technology hand the lock
/// screen: they edit the field without a keyboard event behind them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockKey {
    /// Printable text to append (one typed character or a `type` string).
    Text(String),
    /// Delete the last character of the password.
    Backspace,
    /// Attempt to unlock with the password (Enter).
    Submit,
    /// Clear the password without attempting (Escape).
    Cancel,
    /// Consumed but with no effect on the password. Still swallowed —
    /// never routed to apps while locked.
    Ignored,
}

/// The lock screen's password prompt: the one password prompt every part of
/// Tilo asks with ([`SecretPrompt`]), as the body of the lock's own card.
/// It holds the password while it is typed, says "Verifying…" while it is
/// checked and why it was refused, and is drawn by
/// [`crate::compositor::lock_screen`].
pub struct LockPrompt {
    prompt: RefCell<SecretPrompt>,
    /// Physical pixels per layout unit where the lock screen last placed
    /// the prompt: a pointer's position is divided by it.
    scale: std::cell::Cell<f64>,
    /// Where the unlock action beside the field was last drawn, in the
    /// lock scene's physical pixels.
    submit: std::cell::Cell<vello::kurbo::Rect>,
    /// Touch is the input in use: the field and the unlock action are
    /// touch targets and no key is named to unlock.
    touch: std::cell::Cell<bool>,
}

impl std::fmt::Debug for LockPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the password, nor its length.
        f.write_str("LockPrompt")
    }
}

impl Default for LockPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl LockPrompt {
    /// What an input method needs to serve this field: its caret and its
    /// secret purpose. The prompt is the same widget every password field is,
    /// so it answers for itself rather than from the painted rect.
    fn ime_field(&self) -> Option<ui::ime::ImeWindowState> {
        ui::ime::ImeWindowState::from_widget(&*self.prompt.borrow())
    }

    pub fn new() -> Self {
        let mut prompt = SecretPrompt::new(SecretKind::Secret)
            .with_presentation(Presentation::DialogBody)
            .with_id("lock-password")
            .with_label(FieldRole::Secret, t("lock-screen-password"))
            .with_notice(unlock_hint(false));
        // The field holds focus for as long as the session is locked.
        Widget::set_focus(&mut prompt, true);
        Self {
            prompt: RefCell::new(prompt),
            scale: std::cell::Cell::new(1.0),
            submit: std::cell::Cell::new(vello::kurbo::Rect::ZERO),
            touch: std::cell::Cell::new(false),
        }
    }

    /// Touch is (or is no longer) the input in use. With a keyboard the
    /// line under the field says to press Enter; by touch the unlock action
    /// and the keyboard drawn for it say how.
    pub fn set_touch(&self, touch: bool) {
        let mut prompt = self.prompt.borrow_mut();
        prompt.set_touch_mode(touch);
        if self.touch.replace(touch) != touch {
            prompt.set_notice(unlock_hint(touch));
        }
    }

    /// The prompt, to lay out, paint or read.
    pub fn borrow(&self) -> std::cell::Ref<'_, SecretPrompt> {
        self.prompt.borrow()
    }

    /// The prompt, to lay out, paint or edit.
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, SecretPrompt> {
        self.prompt.borrow_mut()
    }

    /// Where the lock screen placed the prompt: `scale` physical pixels per
    /// layout unit.
    pub fn set_scale(&self, scale: f64) {
        if scale.is_finite() && scale > 0.0 {
            self.scale.set(scale);
        }
    }

    /// Where the lock screen drew the unlock action, in the scene's
    /// physical pixels.
    pub fn set_submit(&self, rect: vello::kurbo::Rect) {
        self.submit.set(rect);
    }

    /// Physical pixels per layout unit where the prompt was last placed.
    pub fn scale(&self) -> f64 {
        self.scale.get()
    }

    /// Where the unlock action was last placed, in physical pixels.
    pub fn submit_rect(&self) -> vello::kurbo::Rect {
        self.submit.get()
    }

    /// Whether `point` (physical pixels) is on the unlock action.
    fn on_submit(&self, point: vello::kurbo::Point) -> bool {
        self.submit.get().contains(point)
    }

    /// What the password field holds.
    fn len(&self) -> usize {
        self.borrow()
            .field(FieldRole::Secret)
            .map_or(0, |field| field.value().chars().count())
    }

    /// Replace what the field holds with `edit` of it; the shown error goes,
    /// as it does when the person types.
    fn edit(&self, edit: impl FnOnce(&mut String)) {
        let mut prompt = self.borrow_mut();
        let Some(field) = prompt.field_mut(FieldRole::Secret) else {
            return;
        };
        let mut value = Zeroizing::new(field.value().to_owned());
        edit(&mut value);
        // Setting the text of a secret field zeroes the old buffer.
        field.set_text(&value);
        prompt.clear_error();
    }

    /// Take the password out of the field, leaving it empty: from here it
    /// lives only in the returned buffer.
    fn take(&self) -> Zeroizing<String> {
        let mut prompt = self.borrow_mut();
        let password = Zeroizing::new(
            prompt
                .field(FieldRole::Secret)
                .map(|field| field.value().to_owned())
                .unwrap_or_default(),
        );
        prompt.wipe();
        password
    }

    /// Whether an input method is composing in the field: it owns Enter
    /// and Escape then.
    fn composing(&self) -> bool {
        self.borrow()
            .field(FieldRole::Secret)
            .is_some_and(ui::widget::TextInput::is_composing)
    }
}

/// Whether `event` pastes: Ctrl+V, Shift+Insert, or a middle click.
fn pastes(event: &WindowEvent, modifiers: Modifiers) -> bool {
    match event {
        WindowEvent::KeyboardInput { event: key, .. } => {
            let state = modifiers.state();
            match &key.logical_key {
                Key::Character(c) => state.control_key() && c.eq_ignore_ascii_case("v"),
                Key::Named(NamedKey::Insert) => state.shift_key(),
                Key::Named(NamedKey::Paste) => true,
                _ => false,
            }
        }
        WindowEvent::MouseInput { button, .. } => *button == winit::event::MouseButton::Middle,
        _ => false,
    }
}

/// The verify answer's text in the person's words: a refused password or a
/// The line under the lock's field while nothing more urgent shows: how
/// to unlock with a keyboard, nothing by touch.
fn unlock_hint(touch: bool) -> Option<String> {
    (!touch).then(|| t("lock-screen-press-enter"))
}

/// verifier that could not check it, each with the way on.
pub fn refusal_line(error: &str) -> String {
    if error == super::lock_auth::UNAVAILABLE_MSG || error == WORKER_LOST_MSG {
        t("lock-screen-cannot-verify")
    } else {
        t("lock-screen-sign-in-failed")
    }
}

/// A verification currently running on a worker thread. The controller holds
/// the receiving end of a one-shot channel; the worker sends the `verify`
/// result exactly once. Dropping this cancels the controller's interest (the
/// worker still completes but its result is discarded).
struct PendingVerify {
    rx: Receiver<Result<(), String>>,
}

/// Lock-screen input + auth. Holds the password prompt, the last error, the
/// injected [`PasswordVerifier`], and any in-flight verification. See the
/// module docs for the security invariant + the async model.
pub struct LockController {
    verifier: Arc<dyn PasswordVerifier>,
    username: String,
    prompt: LockPrompt,
    error: Option<String>,
    /// `Some` while a verify runs on a worker thread; `None` when idle.
    pending: Option<PendingVerify>,
    /// What a keyboard event is handed to the prompt with: the lock has no
    /// widget tree of its own.
    events: EventScratch,
    /// Where the pointer last was over the lock scene (physical pixels).
    pointer: Option<vello::kurbo::Point>,
}

/// The context a keyboard event reaches the prompt in.
struct EventScratch {
    taffy: ui::taffy::TaffyTree<()>,
    node: ui::taffy::NodeId,
    document: automerge::AutoCommit,
}

impl EventScratch {
    fn new() -> Self {
        let mut taffy = ui::taffy::TaffyTree::<()>::new();
        let node = taffy
            .new_leaf(ui::taffy::Style::default())
            .unwrap_or_else(|_| ui::taffy::NodeId::from(0_u64));
        Self {
            taffy,
            node,
            document: automerge::AutoCommit::new(),
        }
    }

    fn with<R>(
        &mut self,
        modifiers: Modifiers,
        scale_factor: f64,
        f: impl FnOnce(&mut EventCtx<'_>) -> R,
    ) -> R {
        let mut handled = false;
        let mut focus = None;
        f(&mut EventCtx {
            scale_factor,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(&mut self.document),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: self.node,
            taffy: &self.taffy,
        })
    }
}

impl LockController {
    /// Build a controller that authenticates `username` via `verifier`.
    pub fn new(verifier: Arc<dyn PasswordVerifier>, username: impl Into<String>) -> Self {
        Self {
            verifier,
            username: username.into(),
            prompt: LockPrompt::new(),
            error: None,
            pending: None,
            events: EventScratch::new(),
            pointer: None,
        }
    }

    /// The password prompt, for the lock screen to draw.
    pub fn prompt(&self) -> &LockPrompt {
        &self.prompt
    }

    /// Reset transient state on a fresh lock: clear any half-typed password,
    /// stale error, and any in-flight verification so the screen starts empty
    /// each time it engages. Dropping `pending` cancels interest in a verify
    /// left over from a prior lock episode.
    pub fn reset(&mut self) {
        self.prompt = LockPrompt::new();
        self.error = None;
        self.pending = None;
    }

    /// Whether a verification is currently running on the worker thread. The
    /// overlay uses this to show a "Verifying…" state and freeze the field.
    pub fn is_verifying(&self) -> bool {
        self.pending.is_some()
    }

    /// Append printable text (a typed char or a pasted/`type`d string),
    /// clamped to `MAX_PASSWORD_LEN`. Control characters are ignored.
    /// Typing clears a previously shown error.
    pub fn push_str(&mut self, s: &str) {
        let room = MAX_PASSWORD_LEN.saturating_sub(self.prompt.len());
        let text: Zeroizing<String> =
            Zeroizing::new(s.chars().filter(|c| !c.is_control()).take(room).collect());
        if text.is_empty() {
            return;
        }
        self.error = None;
        self.prompt.edit(|value| value.push_str(&text));
    }

    /// Append a single printable character (see [`Self::push_str`]).
    pub fn push_char(&mut self, c: char) {
        let mut buffer = [0_u8; 4];
        self.push_str(c.encode_utf8(&mut buffer));
    }

    /// Remove the last user-perceived character. Clears any shown error.
    pub fn backspace(&mut self) {
        self.error = None;
        self.prompt.edit(|value| {
            let boundary = prev_char_boundary(value, value.len());
            value.truncate(boundary);
        });
    }

    /// Clear the password (Escape). Also clears any shown error.
    pub fn clear_input(&mut self) {
        self.error = None;
        self.prompt.edit(String::clear);
    }

    /// Submit a private semantic credential to the ordinary verification worker.
    ///
    /// # Errors
    /// Refuses a concurrent attempt or an empty, oversized, or control-containing
    /// credential without retaining it or changing the current attempt.
    pub fn submit_secret(&mut self, password: Zeroizing<String>) -> Result<(), &'static str> {
        if self.is_verifying() {
            return Err("Password verification is already running");
        }
        if password.is_empty()
            || password.chars().count() > MAX_PASSWORD_LEN
            || password.chars().any(char::is_control)
        {
            return Err("The password is empty or exceeds the supported input limits");
        }
        self.prompt.edit(|value| {
            value.clear();
            value.push_str(&password);
        });
        drop(password);
        self.submit();
        if self.is_verifying() {
            Ok(())
        } else {
            Err(WORKER_LOST_MSG)
        }
    }

    /// Begin an unlock attempt with the typed password, **asynchronously**.
    ///
    /// This never verifies inline. An empty field, or a verify already in
    /// flight, is a no-op. Otherwise it moves the typed password onto a worker
    /// thread that runs the injected verifier, empties the field (the
    /// plaintext now lives only on the worker), and returns
    /// [`UnlockOutcome::StillLocked`] immediately — so the caller's run loop
    /// is never blocked by a slow verify. The actual unlock is delivered later
    /// by [`Self::poll_verification`]; `submit` itself can **never** return
    /// `Unlocked`.
    pub fn submit(&mut self) -> UnlockOutcome {
        if self.prompt.len() == 0 || self.pending.is_some() {
            return UnlockOutcome::StillLocked;
        }
        // Move the credential onto a worker thread. The controller keeps no
        // plaintext copy; the password lives only in the closure until the
        // verify returns.
        let verifier = Arc::clone(&self.verifier);
        let username = self.username.clone();
        let password = self.prompt.take();
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("tilo-lock-verify".to_string())
            .spawn(move || {
                let result = verifier.verify(&username, &password);
                // If the controller reset/unlocked meanwhile, the receiver is
                // gone and this send is a harmless no-op.
                let _ = tx.send(result);
            }) {
            Ok(_handle) => {
                // The flight record says an attempt was made and how it
                // ended, never what was typed.
                eprintln!("tilo compositor: lock: unlock attempt started");
                self.error = None;
                self.pending = Some(PendingVerify { rx });
                self.prompt
                    .borrow_mut()
                    .set_busy(Some(t("lock-screen-verifying")));
            }
            Err(_) => {
                // Could not spawn the verify worker — fail closed, non-leaky,
                // never crash the compositor loop.
                self.refuse(WORKER_LOST_MSG.to_string());
            }
        }
        UnlockOutcome::StillLocked
    }

    /// The attempt failed: say why in the prompt, its field empty and ready.
    fn refuse(&mut self, message: String) {
        self.pending = None;
        let touch = self.prompt.touch.get();
        let mut prompt = self.prompt.borrow_mut();
        prompt.wipe();
        prompt.reject(refusal_line(&message));
        // Under the error, once typing takes it down: how to try again.
        prompt.set_notice(unlock_hint(touch));
        drop(prompt);
        self.error = Some(message);
    }

    /// Poll a pending verification for its result. The run loop calls this
    /// every tick; it is the **only** method that can return
    /// [`UnlockOutcome::Unlocked`].
    ///
    /// * No verify in flight, or still running → `None` (keep the lock).
    /// * Worker reported success → reset + `Some(Unlocked)`.
    /// * Worker reported failure → record the (non-leaky) error, stay locked,
    ///   `Some(StillLocked)`.
    /// * Worker vanished without a result → fail-closed non-leaky error,
    ///   `Some(StillLocked)`.
    pub fn poll_verification(&mut self) -> Option<UnlockOutcome> {
        let pending = self.pending.as_ref()?;
        match pending.rx.try_recv() {
            Ok(Ok(())) => {
                eprintln!("tilo compositor: lock: unlock attempt verified");
                self.reset();
                Some(UnlockOutcome::Unlocked)
            }
            Ok(Err(message)) => {
                eprintln!("tilo compositor: lock: unlock attempt refused");
                self.refuse(message);
                Some(UnlockOutcome::StillLocked)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // The worker dropped its sender without a result — treat as a
                // failed check, never as an unlock.
                eprintln!("tilo compositor: lock: unlock attempt lost its verifier");
                self.refuse(WORKER_LOST_MSG.to_string());
                Some(UnlockOutcome::StillLocked)
            }
        }
    }

    /// Route a key from the keyboard drawn for touch or from assistive
    /// technology into the controller.
    ///
    /// While a verify is in flight the field is frozen: every key is swallowed
    /// (still never routed to apps) so a second attempt can't race the first
    /// and the "Verifying…" state stays stable until the result arrives.
    pub fn handle_key(&mut self, key: LockKey) -> UnlockOutcome {
        if self.is_verifying() {
            return UnlockOutcome::StillLocked;
        }
        match key {
            LockKey::Text(s) => {
                self.push_str(&s);
                UnlockOutcome::StillLocked
            }
            LockKey::Backspace => {
                self.backspace();
                UnlockOutcome::StillLocked
            }
            LockKey::Cancel => {
                self.clear_input();
                UnlockOutcome::StillLocked
            }
            LockKey::Ignored => UnlockOutcome::StillLocked,
            LockKey::Submit => self.submit(),
        }
    }

    /// Route a keyboard, input-method or pointer event into the password
    /// prompt: the one entry point for every physical key while locked, from
    /// the backend or the control socket. The prompt edits its field as every
    /// text field does (caret, selection, paste, input methods) and shows or
    /// hides the password from its own button; Enter starts an attempt and
    /// Escape clears the field.
    /// What the seat's input method needs to serve this field: its caret, its
    /// purpose (a secret, so a keyboard offers no suggestions), and nothing
    /// else. `None` while verifying, when the field refuses edits.
    pub fn ime_field(&self) -> Option<ui::ime::ImeWindowState> {
        if self.is_verifying() {
            return None;
        }
        self.prompt.ime_field()
    }

    pub fn handle_event(&mut self, event: &WindowEvent, modifiers: &Modifiers) -> UnlockOutcome {
        if self.is_verifying() {
            return UnlockOutcome::StillLocked;
        }
        let pressed = match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                Some(&key.logical_key)
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::Touch(_) => None,
            _ => return UnlockOutcome::StillLocked,
        };
        // The unlock action beside the field.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(vello::kurbo::Point::new(position.x, position.y));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } if self
                .pointer
                .is_some_and(|point| self.prompt.on_submit(point)) =>
            {
                return self.submit();
            }
            WindowEvent::Touch(touch)
                if touch.phase == winit::event::TouchPhase::Started
                    && self.prompt.on_submit(vello::kurbo::Point::new(
                        touch.location.x,
                        touch.location.y,
                    )) =>
            {
                return self.submit();
            }
            _ => {}
        }
        if pastes(event, *modifiers) {
            // The clipboard is the locked session's: what it holds must not
            // reach a field anyone at the machine can reveal.
            return UnlockOutcome::StillLocked;
        }
        if !self.prompt.composing() {
            match pressed {
                Some(Key::Named(NamedKey::Enter)) => return self.submit(),
                Some(Key::Named(NamedKey::Escape)) => {
                    self.clear_input();
                    return UnlockOutcome::StillLocked;
                }
                _ => {}
            }
        }
        let before = self.prompt.len();
        {
            let mut prompt = self.prompt.borrow_mut();
            let prompt = &mut *prompt;
            // Pointer positions are physical pixels of the lock scene.
            let scale_factor = self.prompt.scale.get();
            self.events.with(*modifiers, scale_factor, |ctx| {
                prompt.handle_event(event, ctx);
            });
        }
        let after = self.prompt.len();
        if after > MAX_PASSWORD_LEN {
            self.prompt.edit(|value| {
                let end = value
                    .char_indices()
                    .nth(MAX_PASSWORD_LEN)
                    .map_or(value.len(), |(index, _)| index);
                value.truncate(end);
            });
        }
        if after != before {
            self.error = None;
        }
        UnlockOutcome::StillLocked
    }

    /// Number of characters the field holds (never shown, only counted for
    /// the control socket's state).
    pub fn buffer_len(&self) -> usize {
        self.prompt.len()
    }

    /// The last authentication error, if any (shown under the field).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::lock_auth::{FixedVerifier, PasswordVerifier};
    use std::sync::{Condvar, Mutex};

    fn controller(correct: &str) -> LockController {
        LockController::new(Arc::new(FixedVerifier::new(correct)), "alice")
    }

    /// Poll a controller until its in-flight verify resolves.
    fn poll_to_completion(c: &mut LockController) -> UnlockOutcome {
        tilo_test_env::wait_for("the verify to resolve", || c.poll_verification())
    }

    /// Submit the current buffer and drive the async verify to completion.
    /// Asserts the invariant that `submit` never unlocks synchronously.
    fn submit_and_wait(c: &mut LockController) -> UnlockOutcome {
        assert_eq!(
            c.submit(),
            UnlockOutcome::StillLocked,
            "submit must start the verify asynchronously, never unlock inline"
        );
        poll_to_completion(c)
    }

    #[test]
    fn password_submission_unlocks_only_on_the_right_password() {
        {
            let mut c = controller("hunter2");
            c.push_str("wrong");
            assert_eq!(c.buffer_len(), 5);
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
            // Buffer cleared, error shown, still locked.
            assert_eq!(c.buffer_len(), 0);
            assert_eq!(c.error(), Some("Incorrect username or password"));
        }
        {
            let mut c = controller("hunter2");
            c.push_str("hunter2");
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::Unlocked);
            // Reset after a successful unlock.
            assert_eq!(c.buffer_len(), 0);
            assert_eq!(c.error(), None);
            assert!(!c.is_verifying());
        }
        {
            let mut c = controller("hunter2");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            // No verify was started, so nothing to poll.
            assert!(!c.is_verifying());
            assert_eq!(c.poll_verification(), None);
            assert_eq!(c.error(), None);
        }
        {
            let mut c = controller("hunter2");
            c.push_str("bad");
            assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
            assert!(c.error().is_some());
            c.push_char('h');
            assert_eq!(c.error(), None);
            assert_eq!(c.buffer_len(), 1);
        }
    }

    #[test]
    fn password_editing_works_on_graphemes_and_ignores_controls() {
        {
            let mut c = controller("hunter2");
            c.push_str("hunt");
            c.backspace();
            assert_eq!(c.buffer_len(), 3);
            c.clear_input();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("unused");
            c.push_str("e\u{301}");
            assert_eq!(
                c.buffer_len(),
                2,
                "the fixture contains two Unicode scalars"
            );
            c.backspace();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("unused");
            c.push_str("👩‍💻");
            assert_eq!(
                c.buffer_len(),
                3,
                "the fixture contains three Unicode scalars"
            );
            c.backspace();
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let mut c = controller("hunter2");
            c.push_str("a\nb\tc");
            // Only the three printable chars are kept.
            assert_eq!(c.buffer_len(), 3);
        }
        {
            let mut c = controller("pw");
            assert_eq!(
                c.handle_key(LockKey::Text("p".into())),
                UnlockOutcome::StillLocked
            );
            assert_eq!(
                c.handle_key(LockKey::Text("x".into())),
                UnlockOutcome::StillLocked
            );
            assert_eq!(c.handle_key(LockKey::Backspace), UnlockOutcome::StillLocked);
            assert_eq!(c.buffer_len(), 1);
            // "p" + submit is wrong ("pw" expected): submit is async, so it
            // returns StillLocked; the failed result arrives via the poll.
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::StillLocked);
            // Now type the correct password and submit → unlock via the poll.
            c.handle_key(LockKey::Text("pw".into()));
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
        }
        {
            let mut c = controller("pw");
            c.push_str("ab");
            assert_eq!(c.handle_key(LockKey::Ignored), UnlockOutcome::StillLocked);
            assert_eq!(c.buffer_len(), 2);
        }
    }

    fn key(c: &mut LockController, key: &winit::keyboard::Key) -> UnlockOutcome {
        let event = ui::synthetic_input::key_input(key, ElementState::Pressed);
        c.handle_event(&event, &Modifiers::default())
    }

    fn typed(c: &mut LockController, text: &str) {
        for ch in text.chars() {
            key(c, &winit::keyboard::Key::Character(ch.to_string().into()));
        }
    }

    /// Enter is named under the field only while a keyboard is in use: by
    /// touch the unlock action and the drawn keyboard say how, before a
    /// refusal and after it.
    #[test]
    fn the_press_enter_hint_shows_only_with_a_keyboard() {
        let line = |prompt: &LockPrompt| prompt.borrow().notice_line().map(|(line, _)| line);
        let prompt = LockPrompt::new();
        prompt.set_touch(true);
        assert_eq!(line(&prompt), None, "by touch nothing names a key");
        prompt.set_touch(false);
        assert_eq!(line(&prompt), Some(t("lock-screen-press-enter")));

        let mut c = controller("hunter2");
        c.prompt().set_touch(true);
        typed(&mut c, "wrong");
        assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
        c.prompt().borrow_mut().clear_error();
        assert_eq!(
            line(c.prompt()),
            None,
            "a refusal by touch leaves no Enter hint behind"
        );
    }

    #[test]
    fn the_password_is_typed_into_the_shared_prompt_and_refused_in_it() {
        use winit::keyboard::{Key, NamedKey};
        let mut c = controller("hunter2");
        {
            let prompt = c.prompt().borrow();
            assert_eq!(prompt.kind(), SecretKind::Secret);
            assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
            assert_eq!(
                prompt.notice_line().map(|(line, _)| line),
                Some(t("lock-screen-press-enter"))
            );
        }
        typed(&mut c, "wrong");
        assert_eq!(c.buffer_len(), 5);
        assert_eq!(
            key(&mut c, &Key::Named(NamedKey::Enter)),
            UnlockOutcome::StillLocked
        );
        assert!(c.is_verifying());
        assert_eq!(
            c.prompt().borrow().busy(),
            Some(t("lock-screen-verifying").as_str())
        );
        assert_eq!(
            c.buffer_len(),
            0,
            "the password left the field for the verifier"
        );
        assert_eq!(poll_to_completion(&mut c), UnlockOutcome::StillLocked);
        {
            let prompt = c.prompt().borrow();
            assert_eq!(prompt.busy(), None);
            assert_eq!(
                prompt.error(),
                Some(t("lock-screen-sign-in-failed").as_str()),
                "the refusal is said under the field"
            );
            assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
        }
        // Typing takes the error down; Escape empties the field.
        typed(&mut c, "x");
        assert_eq!(c.prompt().borrow().error(), None);
        assert_eq!(c.error(), None);
        key(&mut c, &Key::Named(NamedKey::Escape));
        assert_eq!(c.buffer_len(), 0);

        typed(&mut c, "hunter2");
        key(&mut c, &Key::Named(NamedKey::Backspace));
        typed(&mut c, "2");
        assert_eq!(
            key(&mut c, &Key::Named(NamedKey::Enter)),
            UnlockOutcome::StillLocked
        );
        assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
        assert_eq!(c.buffer_len(), 0);
        assert_eq!(c.prompt().borrow().error(), None);
    }

    #[test]
    fn the_unlock_action_beside_the_field_submits_the_password() {
        let mut c = controller("hunter2");
        c.prompt()
            .set_submit(vello::kurbo::Rect::new(100.0, 100.0, 144.0, 144.0));
        typed(&mut c, "hunter2");
        let moved = ui::synthetic_input::cursor_moved(120.0, 120.0);
        let _ = c.handle_event(&moved, &Modifiers::default());
        let press = ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            winit::event::MouseButton::Left,
        );
        assert_eq!(
            c.handle_event(&press, &Modifiers::default()),
            UnlockOutcome::StillLocked
        );
        assert!(c.is_verifying(), "a click on it starts the attempt");
        assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
    }

    #[test]
    fn nothing_is_pasted_into_the_lock_screen() {
        let mut c = controller("pw");
        let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let paste = crate::shell::test_control::keyboard_input_event(
            &Key::Character(winit::keyboard::SmolStr::new("v")),
            true,
        );
        assert!(pastes(&paste, ctrl));
        let _ = c.handle_event(&paste, &ctrl);
        assert_eq!(c.buffer_len(), 0);
        let insert =
            crate::shell::test_control::keyboard_input_event(&Key::Named(NamedKey::Insert), true);
        let shift = Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
        assert!(pastes(&insert, shift));
        let typed = crate::shell::test_control::keyboard_input_event(
            &Key::Character(winit::keyboard::SmolStr::new("v")),
            true,
        );
        assert!(!pastes(&typed, Modifiers::default()));
    }

    #[test]
    fn a_verifier_that_cannot_check_says_so_in_the_prompt() {
        let mut c = LockController::new(
            Arc::new(crate::compositor::lock_auth::UnavailableVerifier),
            "alice",
        );
        c.push_str("pw");
        assert_eq!(submit_and_wait(&mut c), UnlockOutcome::StillLocked);
        assert_eq!(
            c.prompt().borrow().error(),
            Some(t("lock-screen-cannot-verify").as_str())
        );
    }

    /// A verifier that parks in `verify` until the test releases a gate, so
    /// tests can observe the "verify in flight" window deterministically and
    /// prove the controller does not block on it.
    struct GateVerifier {
        gate: Arc<(Mutex<bool>, Condvar)>,
        result: Result<(), String>,
    }

    impl GateVerifier {
        fn new(result: Result<(), String>) -> (Self, Arc<(Mutex<bool>, Condvar)>) {
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            (
                Self {
                    gate: Arc::clone(&gate),
                    result,
                },
                gate,
            )
        }
    }

    impl PasswordVerifier for GateVerifier {
        fn verify(&self, _username: &str, _password: &str) -> Result<(), String> {
            let (lock, cvar) = &*self.gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            // Nothing below touches the mutex — drop the guard before the
            // (lock-independent) clone rather than holding it needlessly.
            drop(released);
            self.result.clone()
        }
    }

    fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, cvar) = &**gate;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    #[test]
    fn async_verification_never_blocks_and_can_be_cancelled() {
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            // Submit returns while the verifier is still parked on its gate,
            // which only this thread releases: it cannot have waited on the
            // verify, so the compositor run loop would not be blocked here.
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            // The verify is in flight; polling never unlocks while it is parked.
            assert!(c.is_verifying());
            assert_eq!(c.poll_verification(), None);
            assert_eq!(c.poll_verification(), None);
            assert!(c.is_verifying(), "still locked + verifying while parked");
            // Release the worker: only now does a poll observe success and unlock.
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
            assert!(!c.is_verifying());
        }
        {
            let (v, gate) = GateVerifier::new(Err("Incorrect username or password".to_string()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("wrong");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert_eq!(c.poll_verification(), None, "no result while parked");
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::StillLocked);
            assert_eq!(c.error(), Some("Incorrect username or password"));
            assert!(!c.is_verifying());
            assert_eq!(c.buffer_len(), 0);
        }
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert!(c.is_verifying());
            // Keystrokes during a verify must not edit the field or start a
            // second attempt.
            assert_eq!(
                c.handle_key(LockKey::Text("x".into())),
                UnlockOutcome::StillLocked
            );
            assert_eq!(c.buffer_len(), 0, "text is swallowed while verifying");
            assert_eq!(c.handle_key(LockKey::Submit), UnlockOutcome::StillLocked);
            assert!(
                c.is_verifying(),
                "a second submit does not replace the verify"
            );
            release(&gate);
            assert_eq!(poll_to_completion(&mut c), UnlockOutcome::Unlocked);
        }
        {
            let (v, gate) = GateVerifier::new(Ok(()));
            let mut c = LockController::new(Arc::new(v), "alice");
            c.push_str("pw");
            assert_eq!(c.submit(), UnlockOutcome::StillLocked);
            assert!(c.is_verifying());
            // A fresh lock resets: the in-flight verify is abandoned and can no
            // longer unlock the session.
            c.reset();
            assert!(!c.is_verifying());
            release(&gate);
            // Even after the worker finishes, a poll never unlocks — interest was
            // dropped, so there is nothing to observe.
            assert_eq!(c.poll_verification(), None);
        }
    }
}
