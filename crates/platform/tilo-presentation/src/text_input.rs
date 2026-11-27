//! Optional original-field acknowledgements beside standard native IME delivery:
//! no transcript, and no microphone or input ownership. A field's purpose, its
//! keyed fingerprint, its editing history's status, and the receipts of edits a
//! host defers to a field's original owner are named here, so a toolkit and an
//! engine share them.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_clock::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;
use winit::window::Window;

pub fn set_content_type(window: &Window, hint: u32, purpose: u32) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.set_ime_content_type(Some(winit::platform::wayland::ImeContentType {
            hint,
            purpose,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, hint, purpose);
}

pub fn publish(window: &Window, snapshot: Option<[u8; 32]>) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.private_text_input(snapshot)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, snapshot);
        false
    }
}

pub fn acknowledge(window: &Window, serial: u32, outcome: u32) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.acknowledge_private_text_input(serial, outcome);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, serial, outcome);
}

/// What a field holds, for an input method to pick its layout: an email field gets `@`, a
/// phone field the dial pad, a PIN the digit pad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InputPurpose {
    /// Free text.
    #[default]
    Text,
    /// A person's name.
    Name,
    Search,
    Email,
    Url,
    /// A number, with sign and decimal separator.
    Number,
    /// A number with a fractional part.
    Decimal,
    Phone,
    /// Digits only, such as a pairing passkey.
    Pin,
    /// A code sent to the person to confirm a sign-in.
    OneTimeCode,
    Password,
    Date,
    Time,
    /// A terminal, whose keyboard wants the extra keys.
    Terminal,
}

/// Keyed, ephemeral equality check for a field's text and selection. This is invalidation
/// metadata, never a persisted text hash or insertion authority.
#[must_use]
pub fn edit_fingerprint(text: &str, anchor: usize, focus: usize, key: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(&anchor.to_le_bytes());
    hash.update(&focus.to_le_bytes());
    hash.update(text.as_bytes());
    *hash.finalize().as_bytes()
}

/// What the live editing history of a page can do right now, as its engine reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoStatus {
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo_summary: Option<String>,
    pub redo_summary: Option<String>,
}

impl UndoStatus {
    /// The status of a history in which nothing has happened yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether the verb `which` is available.
    #[must_use]
    pub fn can(&self, which: UndoRequest) -> bool {
        match which {
            UndoRequest::Undo => self.can_undo,
            UndoRequest::Redo => self.can_redo,
        }
    }

    #[must_use]
    pub fn summary(&self, which: UndoRequest) -> Option<&str> {
        match which {
            UndoRequest::Undo => self.undo_summary.as_deref(),
            UndoRequest::Redo => self.redo_summary.as_deref(),
        }
    }

    /// The sentence an assistive technology should hear as the control's *description*:
    /// what the verb would do, or the reason it is unavailable. `"Nothing to undo"` when
    /// unavailable — never a description of an action that is not there.
    #[must_use]
    pub fn description(&self, which: UndoRequest) -> String {
        if !self.can(which) {
            return format!("Nothing to {}", which.verb());
        }
        match self.summary(which) {
            Some(s) => format!("{}: {s}", which.title()),
            None => which.title().to_string(),
        }
    }
}

/// A request that the host drive an editing history. The host performs it through the one
/// undo implementation; a control never touches the stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoRequest {
    Undo,
    Redo,
}

impl UndoRequest {
    /// Lower-case verb, for composed sentences (`"Nothing to undo"`).
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Undo => "undo",
            Self::Redo => "redo",
        }
    }

    /// Title-case label, for a control's accessible name.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
        }
    }

    /// The keyboard shortcut the host binds to this verb, for a tooltip — so the control
    /// teaches the shortcut rather than replacing it.
    #[must_use]
    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Undo => "Ctrl+Z",
            Self::Redo => "Ctrl+Shift+Z",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Lease(String, bool);

impl Lease {
    /// The lease a reply names: 32 lowercase hex digits, single-line unless its multiline
    /// flag says otherwise.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Self(value.into(), false))
    }

    /// The lease's own 32 hex digits, as a request names it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the field this lease names takes more than one line.
    #[must_use]
    pub fn multiline(&self) -> bool {
        self.1
    }

    pub fn set_multiline(&mut self, multiline: bool) {
        self.1 = multiline;
    }

    /// The keyed fingerprint of this lease, which changes when its field does.
    #[must_use]
    pub fn stamp(&self, key: &[u8; 32]) -> [u8; 32] {
        edit_fingerprint(&self.0, usize::from(self.1), 0, key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Busy,
    Retired,
    Rejected,
    Unknown,
}

/// How a deferred edit ended: the lease it was granted, if any, or why it failed.
pub type Outcome = Result<Option<Lease>, Failure>;

/// A local receipt shared with the original caller, independent of widget lifetime.
#[derive(Clone)]
pub struct Receipt {
    result: Arc<Mutex<Option<Outcome>>>,
    canceled: Arc<AtomicBool>,
    history: Arc<Mutex<Option<(UndoStatus, Instant)>>>,
    deadline: Instant,
}

/// The original host's one-shot completion end. Completing it grants no input authority;
/// the host must first bind delivery to its original field owner.
pub struct Resolver(Receipt);
impl Resolver {
    pub fn complete(self, result: Result<Option<Lease>, Failure>) {
        self.0.complete(result);
    }
}

impl Receipt {
    /// A bounded private host receipt and its one-shot completion end.
    pub fn channel() -> (Self, Resolver) {
        let receipt = Self::pending();
        (receipt.clone(), Resolver(receipt))
    }

    /// Observe cancellation of this exact scope, including after start ACK.
    pub fn was_canceled(&self) -> bool {
        self.is_canceled()
    }
    /// A receipt nothing has completed yet.
    #[must_use]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "ten seconds past a monotonic reading cannot overflow an Instant"
    )]
    pub fn pending() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            canceled: Arc::new(AtomicBool::new(false)),
            history: Arc::new(Mutex::new(None)),
            deadline: tilo_clock::now() + Duration::from_secs(10),
        }
    }

    /// What the field's editing history could do when the engine last said, until that
    /// observation expires or the receipt is canceled.
    #[must_use]
    pub fn history_status(&self) -> Option<UndoStatus> {
        let (status, expires) = self.history.lock().ok()?.clone()?;
        (tilo_clock::now() < expires && !self.is_canceled()).then_some(status)
    }

    pub fn failed(failure: Failure) -> Self {
        let receipt = Self::pending();
        receipt.complete(Err(failure));
        receipt
    }

    pub fn poll(&self) -> Option<Result<Option<Lease>, Failure>> {
        let mut result = self.result.lock_recover();
        if result.is_none() && tilo_clock::now() >= self.deadline {
            *result = Some(Err(Failure::Unknown));
        }
        result.clone()
    }

    /// Stop a final that has not been admitted. After admission its outcome is uncertain
    /// until the original native reply; never synthesize an undo.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
        self.complete(Err(Failure::Unknown));
    }

    /// Keep the engine's observation of the field's editing history until `expires`.
    pub fn record_history(&self, status: UndoStatus, expires: Instant) {
        *self.history.lock_recover() = Some((status, expires));
    }

    fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
    }

    /// Complete a receipt once; a later completion is ignored.
    pub fn complete(&self, value: Result<Option<Lease>, Failure>) {
        let mut result = self.result.lock_recover();
        if result.is_none() {
            *result = Some(value);
        }
    }
}
