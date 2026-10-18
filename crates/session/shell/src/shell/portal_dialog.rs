//! Compositor-owned dialog state for desktop portals.
//!
//! The portal backend only enqueues authenticated requests.  This controller
//! is the UI-side owner: it never runs in the requesting process, keeps a
//! request editable until the user answers it, and resolves exactly one async
//! reply.  It intentionally has no D-Bus dependency, so shell tests can drive
//! the same flow without a bus.

#![allow(
    clippy::style,
    clippy::complexity,
    clippy::perf,
    clippy::pedantic,
    clippy::nursery,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "this bounded state machine mirrors several fixed portal dialog shapes; its keyboard, pointer, cancellation, paging, and conflict paths have direct controller tests"
)]
#![deny(clippy::too_many_arguments)]

pub(crate) mod authentication_actions;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use taffy::{Style, TaffyTree};
use tokio::sync::oneshot;
use ui::widget::{EventCtx, TextInput, Widget as _};
use vello::kurbo::Rect;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::PhysicalKey;

use crate::portal::desktop_services::{
    AppChooserService, ApplicationChoice, ApplicationChoiceRequest, PortalResult,
};
use crate::shell::app_action_consent::{AppActionConsent, AppActionConsentRequest};
use crate::shell::i18n::{t, t_args};

pub mod accessibility;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewRequest {
    pub handle: String,
    pub app_id: String,
    pub title: String,
    pub reason: String,
    pub resource: String,
    pub confirm_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Confirmed,
    Cancelled,
}

/// One physical USB device offered by the portal frontend.  `id` is never
/// rendered: it is an opaque identifier used only for the implementation
/// response.  The shell shows the human-readable label and the exact access
/// level the app requested instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDeviceRow {
    pub id: String,
    pub label: String,
    pub writable: bool,
}

/// A shell-owned, per-device USB consent sheet.  Devices begin selected so a
/// normal single-device request remains a one-click approval, while each row
/// can be toggled off before the user confirms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDeviceRequest {
    pub review: ReviewRequest,
    pub devices: Vec<UsbDeviceRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDeviceDecision {
    pub decision: ReviewDecision,
    /// Opaque identifiers of the individually approved devices.
    pub approved: Vec<String>,
}

/// A DynamicLauncher review with shell-owned editable fields.  The portal
/// caller never supplies UI objects: `icon_choices` are labels for safe Tilo
/// icon assets selected by the compositor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherReviewRequest {
    pub review: ReviewRequest,
    pub name: String,
    pub editable_name: bool,
    pub editable_icon: bool,
    pub icon_choices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherReviewDecision {
    pub decision: ReviewDecision,
    pub name: String,
    pub icon_choice: usize,
}

/// An editable physical-key binding in the shell-owned shortcut editor.
/// `accelerator` is canonical (for example `CTRL+SHIFT+KEYK`) so it remains
/// stable across keyboard-layout changes.
/// A polkit authentication request as the compositor sheet shows it. The
/// secret typed into it stays in compositor memory until it is handed to
/// `polkit-agent-helper-1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationRequest {
    pub handle: String,
    /// polkit's message for the action.
    pub message: String,
    pub action_id: String,
    /// User names that may authorise; more than one shows a picker.
    pub identities: Vec<String>,
    pub identity: usize,
    /// PAM's prompt, e.g. "Password:".
    pub prompt: String,
    /// Whether the answer is shown as typed.
    pub echo: bool,
    /// Why the last attempt failed, or PAM's informational text.
    pub notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthenticationDecision {
    Answered { identity: usize, secret: String },
    Cancelled,
}

/// How a network-secret field checks its value before the sheet may close;
/// the secret agent chooses it per field.
pub use tilo_network_secret_agent::SecretFieldRule;

/// One field of a network-secrets sheet, e.g. a Wi-Fi password or an
/// 802.1X user name. A masked field never carries a starting value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretFieldSpec {
    /// NetworkManager's key for the value, e.g. `psk` or `identity`.
    pub key: String,
    pub label: String,
    /// Drawn as bullets and never reported by `state-dump`.
    pub masked: bool,
    pub required: bool,
    /// What the field starts with; always empty for a masked field.
    pub initial: String,
    pub rule: SecretFieldRule,
}

impl std::fmt::Debug for SecretFieldSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretFieldSpec")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("masked", &self.masked)
            .field("required", &self.required)
            .field("rule", &self.rule)
            .finish_non_exhaustive()
    }
}

/// NetworkManager needs secrets to connect. Drawn only by the compositor,
/// so no application can imitate the sheet or read what is typed into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkSecretsRequest {
    pub handle: String,
    pub title: String,
    pub detail: String,
    pub fields: Vec<SecretFieldSpec>,
    /// `Some(default)` offers "Remember on this device", starting ticked
    /// or not; `None` when these secrets cannot be kept.
    pub remember: Option<bool>,
}

/// The answer to a [`NetworkSecretsRequest`]: one value per field, in order.
pub enum NetworkSecretsDecision {
    Answered {
        values: Vec<zeroize::Zeroizing<String>>,
        remember: bool,
    },
    Cancelled,
}

impl std::fmt::Debug for NetworkSecretsDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Answered { values, remember } => f
                .debug_struct("Answered")
                .field("values", &values.len())
                .field("remember", remember)
                .finish(),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

/// A field of a network-secrets sheet as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretFieldView {
    pub key: String,
    pub label: String,
    pub masked: bool,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutBindingRow {
    pub id: String,
    pub description: String,
    pub accelerator: Option<String>,
    /// Shell-generated explanation for an unavailable suggested binding.
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutBindingRequest {
    pub handle: String,
    pub app_id: String,
    pub rows: Vec<ShortcutBindingRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutBindingDecision {
    pub rows: Vec<ShortcutBindingRow>,
}

/// A step of a running shortcut that needs the person who started it: a
/// confirmation, or a question. The sheet shows `choices` as rows and
/// `field` as one text entry; a secret field is masked on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutStepRequest {
    pub handle: String,
    /// The shortcut's name.
    pub shortcut: String,
    pub title: String,
    pub detail: String,
    pub confirm_label: String,
    pub choices: Vec<String>,
    pub field: Option<ShortcutStepField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutStepField {
    pub masked: bool,
}

/// The person's answer to a [`ShortcutStepRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutStepDecision {
    /// Confirmed, with the chosen row (when there were choices) and the
    /// entered text (when there was a field).
    Confirmed {
        choice: Option<usize>,
        text: String,
    },
    Cancelled,
}

type ShortcutStepReply = Box<dyn FnOnce(ShortcutStepDecision) + Send>;
type FieldValidator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// A compositor-owned review with one empty text field. Uses the shared
/// question sheet and its accessibility actions; no application owns the
/// entered answer or can prefill it. Useful for independent peer verification.
pub struct FieldReviewRequest {
    pub handle: String,
    pub app_id: String,
    pub title: String,
    pub detail: String,
    pub confirm_label: String,
}

/// The person's answer to carrying a window into another workspace
/// ([`crate::shell::window_transfer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowTransferDecision {
    /// The document leaves its workspace for the destination.
    Move,
    /// The destination gets its own copy; the original stays.
    Copy,
    /// Nothing can travel: open a new, empty window in the destination.
    OpenFresh,
    Cancelled,
}

/// One answer the transfer sheet offers, with its sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTransferChoice {
    pub decision: WindowTransferDecision,
    pub label: String,
}

/// The sheet shown before anything crosses a workspace boundary: what
/// moves, what stays and who loses access, then the answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTransferRequest {
    pub handle: String,
    pub app_id: String,
    pub title: String,
    pub detail: String,
    pub choices: Vec<WindowTransferChoice>,
    pub confirm_label: String,
}

/// The owner's answer to a document someone proposed bringing into their
/// workspace ([`crate::shell::document_import`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentImportDecision {
    Accept,
    Decline,
    /// The sheet went away unanswered; the proposal stays unanswered.
    Dismissed,
}

/// The sheet asking a workspace's owner whether a proposed document may
/// join it. It is drawn as a choice sheet: accept, or decline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentImportRequest {
    pub handle: String,
    pub app_id: String,
    pub title: String,
    pub detail: String,
    pub accept_label: String,
    pub decline_label: String,
    pub confirm_label: String,
}

/// Whether, and how, to open an NTFS drive ([`crate::shell::drives`]).
/// Only the compositor draws this sheet, so no app can answer it for the
/// person; the mount service opens a drive only on an answer from here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveConsentRequest {
    pub handle: String,
    pub title: String,
    /// Where the drive is and how large, under the title.
    pub drive: String,
    pub detail: String,
    /// One sentence per way to open it.
    pub choices: Vec<String>,
    /// The choice the sheet starts on. A suggestion, never an answer.
    pub preselect: usize,
    /// The label of the "Remember" row, or `None` when nothing about this
    /// drive may be remembered.
    pub remember: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
}

/// The answer to a [`DriveConsentRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveConsentDecision {
    Chosen { choice: usize, remember: bool },
    NotNow,
}

enum PendingDialog {
    DriveConsent {
        request: DriveConsentRequest,
        reply: oneshot::Sender<DriveConsentDecision>,
    },
    ShortcutStep {
        request: ShortcutStepRequest,
        reply: ShortcutStepReply,
        validate: Option<FieldValidator>,
    },
    AppChoice {
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    },
    Review {
        request: ReviewRequest,
        reply: oneshot::Sender<ReviewDecision>,
    },
    ShortcutBindings {
        request: ShortcutBindingRequest,
        reply: oneshot::Sender<ShortcutBindingDecision>,
    },
    LauncherReview {
        request: LauncherReviewRequest,
        reply: oneshot::Sender<LauncherReviewDecision>,
    },
    UsbDevices {
        request: UsbDeviceRequest,
        selected: Vec<bool>,
        reply: oneshot::Sender<UsbDeviceDecision>,
    },
    Authentication {
        request: Arc<AuthenticationRequest>,
        generation: uuid::Uuid,
        owner_uid: libc::uid_t,
        reply: oneshot::Sender<AuthenticationDecision>,
    },
    NetworkSecrets {
        request: NetworkSecretsRequest,
        reply: oneshot::Sender<NetworkSecretsDecision>,
    },
    ActionGrant {
        request: AppActionConsentRequest,
        reply: oneshot::Sender<AppActionConsent>,
    },
    WindowTransfer {
        request: WindowTransferRequest,
        reply: oneshot::Sender<WindowTransferDecision>,
    },
    DocumentImport {
        request: DocumentImportRequest,
        reply: oneshot::Sender<DocumentImportDecision>,
    },
}

#[derive(Default)]
struct DialogQueueInner {
    order: VecDeque<String>,
    active: Option<String>,
    pending: HashMap<String, PendingDialog>,
}

/// Thread-safe, process-local handoff from the portal runtime to the
/// compositor frame loop.
#[derive(Clone, Default)]
pub struct PortalDialogQueue {
    inner: Arc<Mutex<DialogQueueInner>>,
}

impl PortalDialogQueue {
    pub fn enqueue_field_review(
        &self,
        request: FieldReviewRequest,
        validate: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
        reply: impl FnOnce(ShortcutStepDecision) + Send + 'static,
    ) -> Result<(), String> {
        self.enqueue_step(
            ShortcutStepRequest {
                handle: request.handle,
                shortcut: friendly_app_name(&request.app_id),
                title: request.title,
                detail: request.detail,
                confirm_label: request.confirm_label,
                choices: Vec::new(),
                field: Some(ShortcutStepField { masked: false }),
            },
            Some(Arc::new(validate)),
            reply,
        )
    }

    pub fn global() -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("global portal dialog queue");
        static QUEUE: LazyLock<PortalDialogQueue> = LazyLock::new(PortalDialogQueue::default);
        QUEUE.clone()
    }

    fn enqueue_choice(
        &self,
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    ) -> PortalResult<()> {
        let Ok(mut inner) = self.inner.lock() else {
            return PortalResult::Unavailable("portal dialog controller is unavailable".into());
        };
        if inner.pending.contains_key(&request.handle) {
            return PortalResult::Failed("duplicate live portal request".into());
        }
        inner.order.push_back(request.handle.clone());
        inner.pending.insert(
            request.handle.clone(),
            PendingDialog::AppChoice { request, reply },
        );
        PortalResult::Success(())
    }

    fn update_choice(&self, handle: &str, choices: &[String]) -> PortalResult<()> {
        let Ok(mut inner) = self.inner.lock() else {
            return PortalResult::Unavailable("portal dialog controller is unavailable".into());
        };
        let Some(PendingDialog::AppChoice { request, .. }) = inner.pending.get_mut(handle) else {
            return PortalResult::Failed("chooser request is no longer live".into());
        };
        request.choices = choices.to_vec();
        PortalResult::Success(())
    }

    /// Queue the sheet `dialog` builds around a fresh reply channel, unless
    /// a sheet with the same handle is already live.
    fn enqueue_reply<T>(
        &self,
        handle: String,
        dialog: impl FnOnce(oneshot::Sender<T>) -> PendingDialog,
    ) -> Result<oneshot::Receiver<T>, String> {
        let (reply, receiver) = oneshot::channel();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&handle) {
            return Err("duplicate live portal request".into());
        }
        inner.order.push_back(handle.clone());
        inner.pending.insert(handle, dialog(reply));
        Ok(receiver)
    }

    /// Take the live sheet under `handle` and let `answer` reply to it. A
    /// sheet of another kind comes back as `Err` and is put back untouched.
    fn finish_pending(
        &self,
        handle: &str,
        answer: impl FnOnce(PendingDialog) -> Result<(), PendingDialog>,
    ) -> bool {
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        let Some(pending) = pending else {
            return false;
        };
        match answer(pending) {
            Ok(()) => true,
            Err(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
        }
    }

    pub fn enqueue_review(
        &self,
        request: ReviewRequest,
    ) -> Result<oneshot::Receiver<ReviewDecision>, String> {
        self.enqueue_reply(request.handle.clone(), |reply| PendingDialog::Review {
            request,
            reply,
        })
    }

    pub fn enqueue_launcher_review(
        &self,
        request: LauncherReviewRequest,
    ) -> Result<oneshot::Receiver<LauncherReviewDecision>, String> {
        if request.name.trim().is_empty() || request.icon_choices.is_empty() {
            return Err("invalid launcher review request".into());
        }
        self.enqueue_reply(request.review.handle.clone(), |reply| {
            PendingDialog::LauncherReview { request, reply }
        })
    }

    /// Queue an administrator-authentication sheet. A request with the
    /// same handle replaces a stale one (a retry after a wrong password).
    pub fn enqueue_authentication(
        &self,
        request: AuthenticationRequest,
    ) -> Result<oneshot::Receiver<AuthenticationDecision>, String> {
        if request.identities.is_empty() {
            return Err("no identity can authorise this request".into());
        }
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::Authentication {
                request: Arc::new(request),
                generation: uuid::Uuid::new_v4(),
                owner_uid: unsafe { libc::geteuid() },
                reply,
            }
        })
    }

    /// Queue a network-secrets sheet. A masked field's starting value is
    /// dropped: a secret is never shown back to the person.
    pub fn enqueue_network_secrets(
        &self,
        mut request: NetworkSecretsRequest,
    ) -> Result<oneshot::Receiver<NetworkSecretsDecision>, String> {
        if request.fields.is_empty() {
            return Err("a network-secrets sheet needs at least one field".into());
        }
        for field in &mut request.fields {
            if field.masked {
                field.initial.clear();
            }
        }
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::NetworkSecrets { request, reply }
        })
    }

    fn finish_network_secrets(&self, handle: &str, decision: NetworkSecretsDecision) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::NetworkSecrets { reply, .. } => {
                let _ = reply.send(decision);
                Ok(())
            }
            other => Err(other),
        })
    }

    /// A live network-secrets sheet's fields as queued.
    fn network_fields(&self, handle: &str) -> (Vec<SecretFieldSpec>, Option<bool>) {
        let Ok(inner) = self.inner.lock() else {
            return (Vec::new(), None);
        };
        match inner.pending.get(handle) {
            Some(PendingDialog::NetworkSecrets { request, .. }) => {
                (request.fields.clone(), request.remember)
            }
            _ => (Vec::new(), None),
        }
    }

    /// Queue a USB sheet with independent device toggles.  The selection
    /// lives in compositor memory, never in the requesting process.
    pub fn enqueue_usb_devices(
        &self,
        request: UsbDeviceRequest,
    ) -> Result<oneshot::Receiver<UsbDeviceDecision>, String> {
        if request.devices.is_empty() {
            return Err("USB selection needs at least one device".into());
        }
        let selected = vec![true; request.devices.len()];
        self.enqueue_reply(request.review.handle.clone(), |reply| {
            PendingDialog::UsbDevices {
                request,
                selected,
                reply,
            }
        })
    }

    /// Queue the trusted shell editor used by the GlobalShortcuts portal.
    /// It captures physical keys from the compositor event loop; requesting
    /// apps only receive the eventual, reviewed selection.
    pub fn enqueue_shortcut_bindings(
        &self,
        request: ShortcutBindingRequest,
    ) -> Result<oneshot::Receiver<ShortcutBindingDecision>, String> {
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::ShortcutBindings { request, reply }
        })
    }

    /// Queue the sheet asking whether one application may use another's
    /// action. The answer is the person's alone: the asking application
    /// never sees the sheet, only whether its call went ahead.
    pub fn enqueue_action_grant(
        &self,
        request: AppActionConsentRequest,
    ) -> Result<oneshot::Receiver<AppActionConsent>, String> {
        self.enqueue_reply(request.handle.clone(), |reply| PendingDialog::ActionGrant {
            request,
            reply,
        })
    }

    /// Queue the sheet asking whether, and how, a window crosses into
    /// another workspace. Dismissing it answers
    /// [`WindowTransferDecision::Cancelled`].
    pub fn enqueue_window_transfer(
        &self,
        request: WindowTransferRequest,
    ) -> Result<oneshot::Receiver<WindowTransferDecision>, String> {
        if request.choices.is_empty() {
            return Err("a transfer sheet needs an answer to offer".into());
        }
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::WindowTransfer { request, reply }
        })
    }

    /// Queue the sheet asking a workspace's owner to accept or decline a
    /// proposed document. Dismissing it answers
    /// [`DocumentImportDecision::Dismissed`].
    pub fn enqueue_document_import(
        &self,
        request: DocumentImportRequest,
    ) -> Result<oneshot::Receiver<DocumentImportDecision>, String> {
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::DocumentImport { request, reply }
        })
    }

    /// Queue the sheet asking whether, and how, to open a drive.
    /// Dismissing it answers [`DriveConsentDecision::NotNow`].
    ///
    /// # Errors
    ///
    /// When the request offers nothing to choose, or a sheet with the same
    /// handle is already live.
    pub fn enqueue_drive_consent(
        &self,
        request: DriveConsentRequest,
    ) -> Result<oneshot::Receiver<DriveConsentDecision>, String> {
        if request.choices.is_empty() || request.preselect >= request.choices.len() {
            return Err("a drive sheet needs its choices and a valid starting one".into());
        }
        self.enqueue_reply(request.handle.clone(), |reply| {
            PendingDialog::DriveConsent { request, reply }
        })
    }

    fn finish_drive_consent(&self, handle: &str, decision: DriveConsentDecision) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::DriveConsent { reply, .. } => {
                let _ = reply.send(decision);
                Ok(())
            }
            other => Err(other),
        })
    }

    fn finish_window_transfer(&self, handle: &str, choice: Option<usize>) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::WindowTransfer { request, reply } => {
                let decision = choice.map_or(WindowTransferDecision::Cancelled, |index| {
                    request
                        .choices
                        .get(index)
                        .map_or(WindowTransferDecision::Cancelled, |choice| choice.decision)
                });
                let _ = reply.send(decision);
                Ok(())
            }
            PendingDialog::DocumentImport { reply, .. } => {
                let _ = reply.send(match choice {
                    Some(0) => DocumentImportDecision::Accept,
                    Some(1) => DocumentImportDecision::Decline,
                    _ => DocumentImportDecision::Dismissed,
                });
                Ok(())
            }
            other => Err(other),
        })
    }

    fn finish_action_grant(&self, handle: &str, answer: AppActionConsent) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::ActionGrant { reply, .. } => {
                let _ = reply.send(answer);
                Ok(())
            }
            other => Err(other),
        })
    }

    /// Queue a sheet for a running shortcut's step; `reply` receives the
    /// answer exactly once, [`ShortcutStepDecision::Cancelled`] when the
    /// sheet is withdrawn.
    ///
    /// # Errors
    ///
    /// When a sheet with the same handle is already live.
    pub fn enqueue_shortcut_step(
        &self,
        request: ShortcutStepRequest,
        reply: impl FnOnce(ShortcutStepDecision) + Send + 'static,
    ) -> Result<(), String> {
        self.enqueue_step(request, None, reply)
    }

    fn enqueue_step(
        &self,
        request: ShortcutStepRequest,
        validate: Option<FieldValidator>,
        reply: impl FnOnce(ShortcutStepDecision) + Send + 'static,
    ) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&request.handle) {
            return Err("duplicate live shortcut step".into());
        }
        inner.order.push_back(request.handle.clone());
        inner.pending.insert(
            request.handle.clone(),
            PendingDialog::ShortcutStep {
                request,
                reply: Box::new(reply),
                validate,
            },
        );
        Ok(())
    }

    fn validate_field(&self, handle: &str, value: &str) -> Result<(), String> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| "portal controller unavailable")?;
        match inner.pending.get(handle) {
            Some(PendingDialog::ShortcutStep {
                validate: Some(validate),
                ..
            }) => validate(value),
            Some(PendingDialog::ShortcutStep { .. }) => Ok(()),
            _ => Err("portal request is no longer live".into()),
        }
    }

    fn finish_shortcut_step(&self, handle: &str, decision: ShortcutStepDecision) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::ShortcutStep { reply, .. } => {
                reply(decision);
                Ok(())
            }
            other => Err(other),
        })
    }

    fn next_handle(&self) -> Option<String> {
        let mut inner = self.inner.lock().ok()?;
        while let Some(handle) = inner.order.pop_front() {
            if inner.pending.contains_key(&handle) {
                inner.active = Some(handle.clone());
                return Some(handle);
            }
        }
        None
    }

    fn view(&self, handle: &str) -> Option<DialogView> {
        let inner = self.inner.lock().ok()?;
        match inner.pending.get(handle)? {
            PendingDialog::DriveConsent { request, .. } => Some(DialogView::DriveConsent {
                handle: request.handle.clone(),
                title: request.title.clone(),
                drive: request.drive.clone(),
                detail: request.detail.clone(),
                choices: request.choices.clone(),
                preselect: request.preselect,
                remember_label: request.remember.clone(),
                confirm_label: request.confirm_label.clone(),
                cancel_label: request.cancel_label.clone(),
            }),
            PendingDialog::ShortcutStep {
                request, validate, ..
            } => Some(DialogView::ShortcutStep {
                handle: request.handle.clone(),
                shortcut: request.shortcut.clone(),
                title: request.title.clone(),
                detail: request.detail.clone(),
                confirm_label: request.confirm_label.clone(),
                choices: request.choices.clone(),
                field: request.field.clone(),
                field_review: validate.is_some(),
            }),
            PendingDialog::AppChoice { request, .. } => Some(DialogView::AppChoice {
                handle: request.handle.clone(),
                app_name: friendly_app_name(&request.app_id),
                detail: chooser_detail(request),
                choices: request.choices.clone(),
            }),
            PendingDialog::Review { request, .. } => Some(DialogView::Review {
                handle: request.handle.clone(),
                app_name: friendly_app_name(&request.app_id),
                title: request.title.clone(),
                detail: if request.reason.is_empty() {
                    request.resource.clone()
                } else {
                    format!("{}\n{}", request.reason, request.resource)
                },
                confirm_label: request.confirm_label.clone(),
            }),
            PendingDialog::ShortcutBindings { request, .. } => Some(DialogView::ShortcutBindings {
                handle: request.handle.clone(),
                app_name: friendly_app_name(&request.app_id),
                rows: request.rows.clone(),
            }),
            PendingDialog::LauncherReview { request, .. } => Some(DialogView::LauncherReview {
                handle: request.review.handle.clone(),
                app_name: friendly_app_name(&request.review.app_id),
                title: request.review.title.clone(),
                detail: if request.review.reason.is_empty() {
                    request.review.resource.clone()
                } else {
                    format!("{}\n{}", request.review.reason, request.review.resource)
                },
                confirm_label: request.review.confirm_label.clone(),
                name: request.name.clone(),
                editable_name: request.editable_name,
                icon_choices: request.icon_choices.clone(),
                editable_icon: request.editable_icon,
            }),
            PendingDialog::Authentication { request, .. } => Some(DialogView::Authentication {
                handle: request.handle.clone(),
                title: request.message.clone(),
                detail: authentication_detail(request),
                identities: if request.identities.len() > 1 {
                    request.identities.clone()
                } else {
                    Vec::new()
                },
                identity: request.identity,
                echo: request.echo,
            }),
            PendingDialog::NetworkSecrets { request, .. } => Some(DialogView::NetworkSecrets {
                handle: request.handle.clone(),
                title: request.title.clone(),
                detail: request.detail.clone(),
                fields: request
                    .fields
                    .iter()
                    .map(|field| SecretFieldView {
                        key: field.key.clone(),
                        label: field.label.clone(),
                        masked: field.masked,
                        required: field.required,
                    })
                    .collect(),
                remember_offered: request.remember.is_some(),
            }),
            PendingDialog::ActionGrant { request, .. } => Some(DialogView::ActionGrant {
                handle: request.handle.clone(),
                title: if request.confirming {
                    t_args(
                        "portal-confirm-title",
                        &[("who", friendly_app_name(&request.app_id).into())],
                    )
                } else {
                    t_args(
                        "portal-grant-title",
                        &[
                            ("app", friendly_app_name(&request.app_id).into()),
                            ("target", friendly_app_name(&request.target_app).into()),
                        ],
                    )
                },
                // A grant covers this action on this resource only, so the
                // sheet names both.
                action: format!("{} · {}", request.resource_label, request.action_label),
                detail: action_grant_detail(request),
                // Confirming one invocation grants nothing: it is allowed
                // this once or not at all.
                choices: action_grant_answers(request)
                    .iter()
                    .map(|answer| crate::shell::app_action_consent::consent_label(*answer))
                    .collect(),
            }),
            PendingDialog::WindowTransfer { request, .. } => Some(DialogView::WindowTransfer {
                handle: request.handle.clone(),
                app_name: friendly_app_name(&request.app_id),
                title: request.title.clone(),
                detail: request.detail.clone(),
                choices: request
                    .choices
                    .iter()
                    .map(|choice| choice.label.clone())
                    .collect(),
                confirm_label: request.confirm_label.clone(),
            }),
            PendingDialog::DocumentImport { request, .. } => Some(DialogView::WindowTransfer {
                handle: request.handle.clone(),
                app_name: friendly_app_name(&request.app_id),
                title: request.title.clone(),
                detail: request.detail.clone(),
                choices: vec![request.accept_label.clone(), request.decline_label.clone()],
                confirm_label: request.confirm_label.clone(),
            }),
            PendingDialog::UsbDevices {
                request, selected, ..
            } => Some(DialogView::UsbDevices {
                handle: request.review.handle.clone(),
                app_name: friendly_app_name(&request.review.app_id),
                title: request.review.title.clone(),
                detail: if request.review.reason.is_empty() {
                    request.review.resource.clone()
                } else {
                    format!("{}\n{}", request.review.reason, request.review.resource)
                },
                confirm_label: request.review.confirm_label.clone(),
                devices: request
                    .devices
                    .iter()
                    .zip(selected)
                    .map(|(device, selected)| UsbDeviceViewRow {
                        label: device.label.clone(),
                        writable: device.writable,
                        selected: *selected,
                    })
                    .collect(),
            }),
        }
    }

    fn finish_choice(&self, handle: &str, choice: Option<String>) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::AppChoice { reply, .. } => {
                let result = choice.map_or(PortalResult::Cancelled, |choice| {
                    PortalResult::Success(ApplicationChoice {
                        choice,
                        activation_token: None,
                    })
                });
                let _ = reply.send(result);
                Ok(())
            }
            other => Err(other),
        })
    }

    /// Withdraw any portal sheet by its implementation-side Request path.
    /// The dynamic Request lifecycle uses this when the portal frontend sends
    /// `Close`; dropping a receiver would leave a visible sheet behind.
    pub fn cancel(&self, handle: &str) -> bool {
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::DriveConsent { reply, .. }) => {
                let _ = reply.send(DriveConsentDecision::NotNow);
                true
            }
            Some(PendingDialog::ShortcutStep { reply, .. }) => {
                reply(ShortcutStepDecision::Cancelled);
                true
            }
            Some(PendingDialog::AppChoice { reply, .. }) => {
                let _ = reply.send(PortalResult::Cancelled);
                true
            }
            Some(PendingDialog::Review { reply, .. }) => {
                let _ = reply.send(ReviewDecision::Cancelled);
                true
            }
            Some(PendingDialog::ShortcutBindings { .. }) => true,
            Some(PendingDialog::ActionGrant { reply, .. }) => {
                let _ = reply.send(AppActionConsent::Deny);
                true
            }
            Some(PendingDialog::WindowTransfer { reply, .. }) => {
                let _ = reply.send(WindowTransferDecision::Cancelled);
                true
            }
            Some(PendingDialog::DocumentImport { reply, .. }) => {
                let _ = reply.send(DocumentImportDecision::Dismissed);
                true
            }
            Some(PendingDialog::Authentication { reply, .. }) => {
                let _ = reply.send(AuthenticationDecision::Cancelled);
                true
            }
            Some(PendingDialog::NetworkSecrets { reply, .. }) => {
                let _ = reply.send(NetworkSecretsDecision::Cancelled);
                true
            }
            Some(PendingDialog::LauncherReview { request, reply }) => {
                let _ = reply.send(LauncherReviewDecision {
                    decision: ReviewDecision::Cancelled,
                    name: request.name,
                    icon_choice: 0,
                });
                true
            }
            Some(PendingDialog::UsbDevices { request, reply, .. }) => {
                let _ = reply.send(UsbDeviceDecision {
                    decision: ReviewDecision::Cancelled,
                    approved: Vec::new(),
                });
                // Keep `request` consumed so no untrusted caller can retain a
                // live selection after the shell withdrew it.
                drop(request);
                true
            }
            None => false,
        }
    }

    fn finish_review(&self, handle: &str, decision: ReviewDecision) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::Review { reply, .. } => {
                let _ = reply.send(decision);
                Ok(())
            }
            other => Err(other),
        })
    }

    fn finish_shortcut_bindings(&self, handle: &str) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::ShortcutBindings { request, reply } => {
                let _ = reply.send(ShortcutBindingDecision { rows: request.rows });
                Ok(())
            }
            other => Err(other),
        })
    }

    fn finish_launcher_review(
        &self,
        handle: &str,
        decision: ReviewDecision,
        name: String,
        icon_choice: usize,
    ) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::LauncherReview { request, reply } => {
                let _ = reply.send(LauncherReviewDecision {
                    decision,
                    name: if request.editable_name {
                        name
                    } else {
                        request.name
                    },
                    icon_choice: if request.editable_icon {
                        icon_choice
                    } else {
                        0
                    },
                });
                Ok(())
            }
            other => Err(other),
        })
    }

    fn finish_usb_devices(&self, handle: &str, decision: ReviewDecision) -> bool {
        self.finish_pending(handle, |pending| match pending {
            PendingDialog::UsbDevices {
                request,
                selected,
                reply,
            } => {
                let approved = if decision == ReviewDecision::Confirmed {
                    request
                        .devices
                        .iter()
                        .zip(selected)
                        .filter_map(|(device, selected)| selected.then(|| device.id.clone()))
                        .collect()
                } else {
                    Vec::new()
                };
                let _ = reply.send(UsbDeviceDecision { decision, approved });
                Ok(())
            }
            other => Err(other),
        })
    }

    fn toggle_usb_device(&self, handle: &str, index: usize) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let Some(PendingDialog::UsbDevices { selected, .. }) = inner.pending.get_mut(handle) else {
            return false;
        };
        let Some(approved) = selected.get_mut(index) else {
            return false;
        };
        *approved = !*approved;
        true
    }

    fn edit_shortcut_binding(
        &self,
        handle: &str,
        selected: usize,
        physical: &PhysicalKey,
        modifiers: Modifiers,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let Some(PendingDialog::ShortcutBindings { request, .. }) = inner.pending.get_mut(handle)
        else {
            return false;
        };
        if selected >= request.rows.len() {
            return false;
        }
        let Some(accelerator) =
            crate::portal::global_shortcuts::physical_accelerator(physical, modifiers)
        else {
            request.rows[selected].status = Some(t("portal-shortcut-needs-modifier"));
            return true;
        };
        if crate::portal::global_shortcuts::ShellGlobalShortcutsService::global()
            .is_reserved(&accelerator)
        {
            request.rows[selected].status = Some(t("portal-shortcut-reserved"));
            return true;
        }
        if request.rows.iter().enumerate().any(|(index, other)| {
            index != selected && other.accelerator.as_deref() == Some(accelerator.as_str())
        }) {
            request.rows[selected].status = Some(t("portal-shortcut-duplicate"));
            return true;
        }
        request.rows[selected].accelerator = Some(accelerator);
        request.rows[selected].status = None;
        true
    }

    fn clear_shortcut_binding(&self, handle: &str, selected: usize) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let Some(PendingDialog::ShortcutBindings { request, .. }) = inner.pending.get_mut(handle)
        else {
            return false;
        };
        let Some(row) = request.rows.get_mut(selected) else {
            return false;
        };
        row.accelerator = None;
        row.status = None;
        true
    }
}

/// Adapter installed in [`DesktopPortalServices`](crate::portal::desktop_services::DesktopPortalServices).
#[derive(Clone, Default)]
pub struct ShellAppChooserService {
    queue: PortalDialogQueue,
}

impl ShellAppChooserService {
    pub fn global() -> Self {
        Self {
            queue: PortalDialogQueue::global(),
        }
    }
    pub fn with_queue(queue: PortalDialogQueue) -> Self {
        Self { queue }
    }
}

impl AppChooserService for ShellAppChooserService {
    fn enqueue(
        &self,
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    ) -> PortalResult<()> {
        self.queue.enqueue_choice(request, reply)
    }
    fn update_choices(&self, handle: &str, choices: &[String]) -> PortalResult<()> {
        self.queue.update_choice(handle, choices)
    }
    fn cancel(&self, handle: &str) {
        let _ = self.queue.cancel(handle);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogView {
    /// Whether, and how, to open a drive. The ways to open it are the
    /// choices; Cancel leaves it closed.
    DriveConsent {
        handle: String,
        title: String,
        drive: String,
        detail: String,
        choices: Vec<String>,
        preselect: usize,
        remember_label: Option<String>,
        confirm_label: String,
        cancel_label: String,
    },
    ShortcutStep {
        handle: String,
        shortcut: String,
        title: String,
        detail: String,
        confirm_label: String,
        choices: Vec<String>,
        field: Option<ShortcutStepField>,
        field_review: bool,
    },
    AppChoice {
        handle: String,
        app_name: String,
        detail: String,
        choices: Vec<String>,
    },
    Review {
        handle: String,
        app_name: String,
        title: String,
        detail: String,
        confirm_label: String,
    },
    ShortcutBindings {
        handle: String,
        app_name: String,
        rows: Vec<ShortcutBindingRow>,
    },
    LauncherReview {
        handle: String,
        app_name: String,
        title: String,
        detail: String,
        confirm_label: String,
        name: String,
        editable_name: bool,
        icon_choices: Vec<String>,
        editable_icon: bool,
    },
    UsbDevices {
        handle: String,
        app_name: String,
        title: String,
        detail: String,
        confirm_label: String,
        devices: Vec<UsbDeviceViewRow>,
    },
    /// Whether one application may use another's action, and for how
    /// long. The allowing answers are the choices; Cancel denies.
    ActionGrant {
        handle: String,
        title: String,
        /// The action, as its publisher labels it.
        action: String,
        detail: String,
        choices: Vec<String>,
    },
    /// Whether, and how, a window crosses into another workspace. The
    /// answers are the choices; Cancel leaves everything as it was.
    WindowTransfer {
        handle: String,
        app_name: String,
        title: String,
        detail: String,
        choices: Vec<String>,
        confirm_label: String,
    },
    /// Administrator authentication for polkit. Drawn only by the
    /// compositor, so no application can imitate it or read the secret.
    Authentication {
        handle: String,
        title: String,
        detail: String,
        /// Empty unless there is a choice of identity.
        identities: Vec<String>,
        identity: usize,
        echo: bool,
    },
    /// Secrets NetworkManager needs to connect. Drawn only by the
    /// compositor; the typed values live in the controller, not here.
    NetworkSecrets {
        handle: String,
        title: String,
        detail: String,
        fields: Vec<SecretFieldView>,
        remember_offered: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDeviceViewRow {
    pub label: String,
    pub writable: bool,
    pub selected: bool,
}

impl DialogView {
    pub fn handle(&self) -> &str {
        match self {
            Self::ShortcutStep { handle, .. }
            | Self::AppChoice { handle, .. }
            | Self::Review { handle, .. }
            | Self::ShortcutBindings { handle, .. }
            | Self::LauncherReview { handle, .. }
            | Self::UsbDevices { handle, .. }
            | Self::ActionGrant { handle, .. }
            | Self::WindowTransfer { handle, .. }
            | Self::Authentication { handle, .. }
            | Self::NetworkSecrets { handle, .. }
            | Self::DriveConsent { handle, .. } => handle,
        }
    }
    pub fn title(&self) -> String {
        match self {
            Self::ShortcutStep { title, .. }
            | Self::WindowTransfer { title, .. }
            | Self::DriveConsent { title, .. } => title.clone(),
            Self::AppChoice { .. } => t("portal-choose-app-title"),
            Self::Review { title, .. } => title.clone(),
            Self::ShortcutBindings { .. } => t("portal-shortcuts-title"),
            Self::LauncherReview { title, .. } => title.clone(),
            Self::UsbDevices { title, .. } => title.clone(),
            Self::ActionGrant { title, .. } => title.clone(),
            Self::Authentication { title, .. } => title.clone(),
            Self::NetworkSecrets { title, .. } => title.clone(),
        }
    }
    pub fn app_name(&self) -> String {
        match self {
            Self::ShortcutStep {
                shortcut: app_name, ..
            }
            | Self::AppChoice { app_name, .. }
            | Self::Review { app_name, .. }
            | Self::ShortcutBindings { app_name, .. }
            | Self::LauncherReview { app_name, .. }
            | Self::UsbDevices { app_name, .. }
            | Self::WindowTransfer { app_name, .. } => app_name.clone(),
            Self::DriveConsent { drive, .. } => drive.clone(),
            Self::ActionGrant { action, .. } => action.clone(),
            Self::Authentication { .. } => t("portal-auth-heading"),
            Self::NetworkSecrets { .. } => t("portal-network-heading"),
        }
    }
    pub fn detail(&self) -> String {
        match self {
            Self::ShortcutStep { detail, .. }
            | Self::AppChoice { detail, .. }
            | Self::Review { detail, .. }
            | Self::WindowTransfer { detail, .. }
            | Self::DriveConsent { detail, .. } => detail.clone(),
            Self::ShortcutBindings { .. } => t("portal-shortcuts-detail"),
            Self::LauncherReview { detail, .. } => detail.clone(),
            Self::UsbDevices { detail, .. } => detail.clone(),
            Self::ActionGrant { detail, .. } => detail.clone(),
            Self::Authentication { detail, .. } => detail.clone(),
            Self::NetworkSecrets { detail, .. } => detail.clone(),
        }
    }
    pub fn choices(&self) -> &[String] {
        match self {
            Self::AppChoice { choices, .. }
            | Self::ActionGrant { choices, .. }
            | Self::WindowTransfer { choices, .. }
            | Self::ShortcutStep { choices, .. }
            | Self::DriveConsent { choices, .. } => choices,
            Self::Authentication { identities, .. } => identities,
            Self::Review { .. }
            | Self::ShortcutBindings { .. }
            | Self::LauncherReview { .. }
            | Self::UsbDevices { .. }
            | Self::NetworkSecrets { .. } => &[],
        }
    }
    pub fn shortcut_rows(&self) -> &[ShortcutBindingRow] {
        match self {
            Self::ShortcutBindings { rows, .. } => rows,
            Self::ShortcutStep { .. }
            | Self::AppChoice { .. }
            | Self::Review { .. }
            | Self::LauncherReview { .. }
            | Self::UsbDevices { .. }
            | Self::ActionGrant { .. }
            | Self::WindowTransfer { .. }
            | Self::Authentication { .. }
            | Self::NetworkSecrets { .. }
            | Self::DriveConsent { .. } => &[],
        }
    }
    /// The fields of a network-secrets sheet.
    pub fn secret_fields(&self) -> &[SecretFieldView] {
        match self {
            Self::NetworkSecrets { fields, .. } => fields,
            _ => &[],
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Self::AppChoice { .. } => "app_choice",
            Self::Review { .. } => "review",
            Self::ShortcutBindings { .. } => "shortcut_bindings",
            Self::LauncherReview { .. } => "launcher_review",
            Self::UsbDevices { .. } => "usb_devices",
            Self::Authentication { .. } => "authentication",
            Self::NetworkSecrets { .. } => "network_secrets",
            Self::ActionGrant { .. } => "action_grant",
            Self::WindowTransfer { .. } => "window_transfer",
            Self::DriveConsent { .. } => "drive_consent",
            Self::ShortcutStep { .. } => "shortcut_step",
        }
    }
    pub fn selectable_count(&self) -> usize {
        match self {
            Self::ShortcutBindings { rows, .. } => rows.len(),
            Self::AppChoice { choices, .. }
            | Self::ActionGrant { choices, .. }
            | Self::WindowTransfer { choices, .. }
            | Self::ShortcutStep { choices, .. }
            | Self::DriveConsent { choices, .. } => choices.len(),
            Self::UsbDevices { devices, .. } => devices.len(),
            Self::Authentication { identities, .. } => identities.len(),
            Self::Review { .. } | Self::LauncherReview { .. } | Self::NetworkSecrets { .. } => 0,
        }
    }
    pub fn confirm_label(&self) -> String {
        match self {
            Self::AppChoice { .. } => t("portal-open"),
            Self::ShortcutStep { confirm_label, .. }
            | Self::WindowTransfer { confirm_label, .. }
            | Self::DriveConsent { confirm_label, .. } => confirm_label.clone(),
            Self::Review { confirm_label, .. } => confirm_label.clone(),
            Self::ShortcutBindings { .. } => t("portal-save-shortcuts"),
            Self::LauncherReview { confirm_label, .. } => confirm_label.clone(),
            Self::UsbDevices { confirm_label, .. } => confirm_label.clone(),
            Self::ActionGrant { .. } => t("portal-allow"),
            Self::Authentication { .. } => t("portal-authenticate"),
            Self::NetworkSecrets { .. } => t("portal-connect"),
        }
    }

    /// The label of the button that answers no.
    pub fn cancel_label(&self) -> String {
        match self {
            Self::ActionGrant { .. } => {
                crate::shell::app_action_consent::consent_label(AppActionConsent::Deny)
            }
            Self::DriveConsent { cancel_label, .. } => cancel_label.clone(),
            _ => t("portal-cancel"),
        }
    }

    pub fn launcher_editor(&self) -> Option<LauncherEditorView<'_>> {
        match self {
            Self::LauncherReview {
                name,
                editable_name,
                icon_choices,
                editable_icon,
                ..
            } => Some(LauncherEditorView {
                name,
                editable_name: *editable_name,
                icon_choices,
                editable_icon: *editable_icon,
            }),
            // A shortcut's text answer is edited in the same field.
            Self::ShortcutStep { field: Some(_), .. } => Some(LauncherEditorView {
                name: "",
                editable_name: true,
                icon_choices: &[],
                editable_icon: false,
            }),
            _ => None,
        }
    }

    pub fn usb_devices(&self) -> &[UsbDeviceViewRow] {
        match self {
            Self::UsbDevices { devices, .. } => devices,
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LauncherEditorView<'a> {
    pub name: &'a str,
    pub editable_name: bool,
    pub icon_choices: &'a [String],
    pub editable_icon: bool,
}

/// Geometry and hit regions are controller-owned so compositor rendering and
/// test drivers cannot disagree about which click confirms an action.
#[derive(Clone, Debug)]
pub struct DialogLayout {
    pub sheet: Rect,
    pub choices: Vec<Rect>,
    pub cancel: Rect,
    pub confirm: Rect,
    pub launcher_name: Option<Rect>,
    pub launcher_icons: Vec<Rect>,
    /// The secret field of an authentication sheet.
    pub secret: Option<Rect>,
    /// The fields of a network-secrets sheet, in order.
    pub secret_fields: Vec<Rect>,
    /// The "Remember on this device" row of a network-secrets sheet.
    pub remember: Option<Rect>,
    /// Trusted disclosure navigation; never resolves the consent reply.
    pub review_back: Option<Rect>,
}

pub struct PortalDialogController {
    queue: PortalDialogQueue,
    active: Option<String>,
    active_authentication_generation: Option<uuid::Uuid>,
    selected: usize,
    review_page: usize,
    launcher_name: String,
    launcher_icon: usize,
    launcher_field: usize,
    /// The authentication sheet's answer field; secret unless PAM echoes.
    /// Wiped whenever the sheet closes, whichever way.
    secret: TextInput,
    /// One field per network-secrets field, masked ones secret; wiped like
    /// `secret`.
    entries: Vec<TextInput>,
    /// A shortcut step's typed answer; secret when the step asks for one.
    /// Wiped like `secret`.
    step_field: TextInput,
    /// The focused network-secrets field; one past the last is the
    /// "Remember" row.
    entry_focus: usize,
    remember: bool,
    /// Why the network-secrets sheet refused to close.
    entry_error: Option<String>,
}

fn small_count_as_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}
impl Default for PortalDialogController {
    fn default() -> Self {
        Self::new(PortalDialogQueue::global())
    }
}
impl PortalDialogController {
    pub fn new(queue: PortalDialogQueue) -> Self {
        Self {
            queue,
            active: None,
            active_authentication_generation: None,
            selected: 0,
            review_page: 0,
            launcher_name: String::new(),
            launcher_icon: 0,
            launcher_field: 0,
            secret: TextInput::local(),
            entries: Vec::new(),
            step_field: TextInput::local(),
            entry_focus: 0,
            remember: false,
            entry_error: None,
        }
    }
    /// The queue this controller shows sheets from.
    pub fn queue(&self) -> &PortalDialogQueue {
        &self.queue
    }

    pub fn pump(&mut self) -> bool {
        // A sheet its requester withdrew (a portal `Close`, polkit's or
        // NetworkManager's cancel) is gone from the queue: let go of it and
        // of anything typed into it, so the next sheet can show.
        let withdrawn = self.active.is_some() && self.view().is_none();
        if withdrawn {
            self.active = None;
            self.forget_secret();
        }
        if self.active.is_none() {
            self.active = self.queue.next_handle();
            self.active_authentication_generation = self
                .active
                .as_deref()
                .and_then(|handle| self.queue.authentication_generation(handle));
            self.selected = 0;
            self.review_page = 0;
            self.launcher_icon = 0;
            self.launcher_field = 0;
            self.forget_secret();
            match self.view() {
                Some(DialogView::DriveConsent { preselect, .. }) => {
                    self.selected = preselect;
                }
                Some(DialogView::Authentication { identity, echo, .. }) => {
                    self.selected = identity;
                    self.secret = TextInput::local()
                        .with_id("portal-authentication-secret")
                        .with_secret(!echo);
                }
                Some(DialogView::ShortcutStep {
                    field: Some(ShortcutStepField { masked }),
                    ..
                }) => {
                    self.step_field = TextInput::local()
                        .with_id("portal-shortcut-step-field")
                        .with_secret(masked);
                }
                _ => {}
            }
            if let Some(handle) = self.active.clone() {
                let (fields, remember) = self.queue.network_fields(&handle);
                // Start on the first field still to fill in.
                self.entry_focus = fields
                    .iter()
                    .position(|field| field.required && field.initial.is_empty())
                    .or_else(|| fields.iter().position(|field| field.initial.is_empty()))
                    .unwrap_or(0);
                self.entries = fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, field)| {
                        TextInput::local()
                            .with_id(&format!("portal-network-secret-{index}"))
                            .with_placeholder(&field.label)
                            .with_accessible_name(field.label)
                            .with_secret(field.masked)
                            .with_initial_value(&field.initial)
                    })
                    .collect();
                self.remember = remember.unwrap_or(false);
            }
            self.launcher_name = self
                .view()
                .and_then(|view| view.launcher_editor().map(|editor| editor.name.to_owned()))
                .unwrap_or_default();
            self.active.is_some() || withdrawn
        } else {
            false
        }
    }
    pub fn view(&self) -> Option<DialogView> {
        let handle = self.active.as_deref()?;
        let view = self.queue.view(handle)?;
        if matches!(view, DialogView::Authentication { .. })
            && self.queue.authentication_generation(handle) != self.active_authentication_generation
        {
            return None;
        }
        Some(view)
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn select_next(&mut self, reverse: bool) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        if let DialogView::NetworkSecrets {
            fields,
            remember_offered,
            ..
        } = &view
        {
            let len = fields.len() + usize::from(*remember_offered);
            self.entry_focus = if reverse {
                (self.entry_focus + len - 1) % len
            } else {
                (self.entry_focus + 1) % len
            };
            return true;
        }
        if let Some(editor) = view.launcher_editor()
            && editor.editable_icon
        {
            let len = editor.icon_choices.len();
            self.launcher_icon = if reverse {
                (self.launcher_icon + len - 1) % len
            } else {
                (self.launcher_icon + 1) % len
            };
            self.launcher_field = 1;
            return true;
        }
        let len = view.selectable_count();
        if len == 0 {
            return false;
        };
        self.selected = if reverse {
            (self.selected + len - 1) % len
        } else {
            (self.selected + 1) % len
        };
        true
    }
    /// The displayed disclosure uses bounded pages, not an unbounded sheet.
    /// All pages must be traversed before a separate final approval.
    pub fn review_pages(&self) -> Option<Vec<String>> {
        let view = self.view()?;
        if !matches!(&view, DialogView::ActionGrant { detail, .. } if detail.contains('\n')) {
            return None;
        }
        // At 14 px even full-width glyphs fit 24 columns inside the
        // narrowest supported sheet's gutters. Break long identifiers too:
        // word wrapping alone would hide endpoint or signing-key suffixes.
        let mut lines = Vec::new();
        for line in view.detail().split('\n') {
            let chars: Vec<_> = line.chars().collect();
            if chars.is_empty() {
                lines.push(String::new());
            } else {
                lines.extend(chars.chunks(24).map(|part| part.iter().collect::<String>()));
            }
        }
        Some(lines.chunks(6).map(|page| page.join("\n")).collect())
    }
    pub fn displayed_detail(&self) -> String {
        self.review_pages()
            .and_then(|pages| pages.get(self.review_page).cloned())
            .unwrap_or_else(|| self.view().map_or_else(String::new, |view| view.detail()))
    }
    pub fn reviewing_more(&self) -> bool {
        self.review_pages()
            .is_some_and(|pages| self.review_page + 1 < pages.len())
    }
    pub fn review_back(&mut self) -> bool {
        if self.review_page == 0 {
            return false;
        }
        self.review_page -= 1;
        true
    }
    pub fn confirm(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        if self.reviewing_more() {
            self.review_page += 1;
            return true;
        }
        let done = match &view {
            DialogView::DriveConsent { .. } => self.queue.finish_drive_consent(
                view.handle(),
                DriveConsentDecision::Chosen {
                    choice: self.selected,
                    remember: self.remember,
                },
            ),
            DialogView::ShortcutStep { choices, .. } => {
                if let Err(reason) = self
                    .queue
                    .validate_field(view.handle(), self.step_field.value())
                {
                    self.entry_error = Some(reason);
                    return false;
                }
                self.queue.finish_shortcut_step(
                    view.handle(),
                    ShortcutStepDecision::Confirmed {
                        choice: (!choices.is_empty()).then_some(self.selected),
                        text: take_text(&mut self.step_field),
                    },
                )
            }
            DialogView::AppChoice { choices, .. } => choices
                .get(self.selected)
                .cloned()
                .is_some_and(|choice| self.queue.finish_choice(view.handle(), Some(choice))),
            DialogView::Review { .. } => self
                .queue
                .finish_review(view.handle(), ReviewDecision::Confirmed),
            DialogView::ShortcutBindings { .. } => {
                self.queue.finish_shortcut_bindings(view.handle())
            }
            DialogView::LauncherReview { .. } => self.queue.finish_launcher_review(
                view.handle(),
                ReviewDecision::Confirmed,
                self.launcher_name.clone(),
                self.launcher_icon,
            ),
            DialogView::UsbDevices { .. } => self
                .queue
                .finish_usb_devices(view.handle(), ReviewDecision::Confirmed),
            DialogView::ActionGrant { .. } => AppActionConsent::ALLOWING
                .get(self.selected)
                .is_some_and(|answer| self.queue.finish_action_grant(view.handle(), *answer)),
            DialogView::WindowTransfer { .. } => self
                .queue
                .finish_window_transfer(view.handle(), Some(self.selected)),
            DialogView::Authentication {
                identities,
                identity,
                ..
            } => {
                let identity = if identities.is_empty() {
                    *identity
                } else {
                    self.selected
                };
                let Some(ticket) = self.queue.current_authentication().filter(|ticket| {
                    Some(ticket.generation) == self.active_authentication_generation
                        && self.active.as_deref() == Some(ticket.request().handle.as_str())
                }) else {
                    return false;
                };
                let Some(offered) = ticket.request().identities.get(identity) else {
                    return false;
                };
                let secret = take_text(&mut self.secret);
                self.queue.answer_authentication(&ticket, offered, secret)
            }
            DialogView::NetworkSecrets { .. } => {
                let (fields, _) = self.queue.network_fields(view.handle());
                let refusal = fields.iter().zip(&self.entries).enumerate().find_map(
                    |(index, (field, entry))| {
                        let value = entry.value();
                        if value.is_empty() {
                            field.required.then(|| (index, t("portal-secret-required")))
                        } else if !field.rule.accepts(value) {
                            Some((index, t("portal-secret-wifi-length")))
                        } else {
                            None
                        }
                    },
                );
                if let Some((index, reason)) = refusal {
                    self.entry_focus = index;
                    self.entry_error = Some(reason);
                    return false;
                }
                let values = self
                    .entries
                    .iter()
                    .map(|entry| zeroize::Zeroizing::new(entry.value().to_owned()))
                    .collect();
                let remember = self.remember;
                self.forget_secret();
                self.queue.finish_network_secrets(
                    view.handle(),
                    NetworkSecretsDecision::Answered { values, remember },
                )
            }
        };
        if done {
            self.active = None;
        }
        done
    }
    pub fn cancel(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        let done = match &view {
            DialogView::DriveConsent { .. } => self
                .queue
                .finish_drive_consent(view.handle(), DriveConsentDecision::NotNow),
            DialogView::ShortcutStep { .. } => self
                .queue
                .finish_shortcut_step(view.handle(), ShortcutStepDecision::Cancelled),
            DialogView::AppChoice { .. } => self.queue.finish_choice(view.handle(), None),
            DialogView::Review { .. } => self
                .queue
                .finish_review(view.handle(), ReviewDecision::Cancelled),
            DialogView::ShortcutBindings { .. } => self.queue.cancel(view.handle()),
            DialogView::LauncherReview { .. } => self.queue.finish_launcher_review(
                view.handle(),
                ReviewDecision::Cancelled,
                self.launcher_name.clone(),
                self.launcher_icon,
            ),
            DialogView::UsbDevices { .. } => self
                .queue
                .finish_usb_devices(view.handle(), ReviewDecision::Cancelled),
            DialogView::ActionGrant { .. } => self
                .queue
                .finish_action_grant(view.handle(), AppActionConsent::Deny),
            DialogView::WindowTransfer { .. } => {
                self.queue.finish_window_transfer(view.handle(), None)
            }
            DialogView::Authentication { .. } => {
                let Some(ticket) = self.queue.current_authentication().filter(|ticket| {
                    Some(ticket.generation) == self.active_authentication_generation
                        && self.active.as_deref() == Some(ticket.request().handle.as_str())
                }) else {
                    return false;
                };
                self.forget_secret();
                self.queue.cancel_authentication(&ticket)
            }
            DialogView::NetworkSecrets { .. } => {
                self.forget_secret();
                self.queue
                    .finish_network_secrets(view.handle(), NetworkSecretsDecision::Cancelled)
            }
        };
        if done {
            self.active = None;
        }
        done
    }
    pub fn layout(&self, bounds: Rect) -> Option<DialogLayout> {
        let view = self.view()?;
        // Verified field reviews contain exact file/endpoint/certificate text,
        // rather than a one-line caption. Never overlap it with the entry.
        let moderation_review = matches!(&view, DialogView::ActionGrant { detail, .. }
            if detail.lines().count() > 1);
        let field_review = moderation_review
            || matches!(
                view,
                DialogView::ShortcutStep {
                    field_review: true,
                    ..
                }
            );
        let extra_detail = if moderation_review {
            // Six disclosure lines on every page keep navigation and the
            // final consent controls stationary, including on laptops.
            5.0 * 24.0
        } else if field_review {
            small_count_as_f64(view.detail().lines().count().saturating_sub(1)) * 24.0
        } else {
            0.0
        };
        let review_width = 560.0;
        let width = bounds.width().min(review_width);
        let row_count = view.selectable_count().min(6);
        let launcher_editor = view.launcher_editor();
        let launcher_rows = launcher_editor.map_or(0, |editor| {
            usize::from(editor.editable_name) + usize::from(editor.editable_icon)
        });
        let secret_rows = usize::from(matches!(view, DialogView::Authentication { .. }));
        let (network_fields, remember_row) = match &view {
            DialogView::NetworkSecrets {
                fields,
                remember_offered,
                ..
            } => (fields.len().min(6), usize::from(*remember_offered)),
            DialogView::DriveConsent { remember_label, .. } => {
                (0, usize::from(remember_label.is_some()))
            }
            _ => (0, 0),
        };
        let height = 190.0
            + extra_detail
            + small_count_as_f64(
                row_count + launcher_rows + secret_rows + network_fields + remember_row,
            ) * 48.0;
        let minimum_width = if moderation_review {
            400.0
        } else {
            review_width
        };
        if field_review && (height > bounds.height() || width < minimum_width) {
            // A clipped security review cannot approve anything invisibly.
            return None;
        }
        let sheet = Rect::new(
            bounds.center().x - width / 2.0,
            bounds.center().y - height / 2.0,
            bounds.center().x + width / 2.0,
            bounds.center().y + height / 2.0,
        );
        // Choice rows are Control rows (HIG §9.6: 44) on the 48 px pitch —
        // the 38 they were left the row under the density scale.
        let row_h = f64::from(ui::tokens::Metrics::BASE.panel_row_height);
        let choices = (0..row_count)
            .map(|i| {
                let y = sheet.y0 + (118.0 + extra_detail) + small_count_as_f64(i) * 48.0;
                Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
            })
            .collect();
        let launcher_name = launcher_editor
            .filter(|editor| editor.editable_name)
            .map(|_| {
                let y = sheet.y0 + (118.0 + extra_detail) + small_count_as_f64(row_count) * 48.0;
                Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
            });
        let launcher_icons = launcher_editor
            .filter(|editor| editor.editable_icon)
            .map(|editor| {
                let y = sheet.y0
                    + (118.0 + extra_detail)
                    + small_count_as_f64(row_count + usize::from(editor.editable_name)) * 48.0;
                let count = editor.icon_choices.len().min(3);
                let gap = 8.0;
                let width =
                    (sheet.width() - 48.0 - gap * small_count_as_f64(count.saturating_sub(1)))
                        / small_count_as_f64(count);
                (0..count)
                    .map(|index| {
                        let x = sheet.x0 + 24.0 + small_count_as_f64(index) * (width + gap);
                        Rect::new(x, y, x + width, y + row_h)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The action row shares the choice rows' 24 px gutter, so Confirm's
        // right edge lines up with the rows above it instead of overhanging
        // them by 4 px. 92 / 84 wide, 34 tall, 12 apart — unchanged.
        let cancel = Rect::new(
            sheet.x1 - 212.0,
            sheet.y1 - 54.0,
            sheet.x1 - 120.0,
            sheet.y1 - 20.0,
        );
        let confirm = Rect::new(
            sheet.x1 - 108.0,
            sheet.y1 - 54.0,
            sheet.x1 - 24.0,
            sheet.y1 - 20.0,
        );
        let secret = (secret_rows > 0).then(|| {
            let y = sheet.y0 + (118.0 + extra_detail) + small_count_as_f64(row_count) * 48.0;
            Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
        });
        let field_row = |index: usize| {
            let y =
                sheet.y0 + (118.0 + extra_detail) + small_count_as_f64(row_count + index) * 48.0;
            Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
        };
        let secret_fields = (0..network_fields).map(field_row).collect();
        let remember = (remember_row > 0).then(|| field_row(network_fields));
        Some(DialogLayout {
            sheet,
            choices,
            cancel,
            confirm,
            launcher_name,
            launcher_icons,
            secret,
            secret_fields,
            remember,
            review_back: (moderation_review && self.review_page > 0)
                .then(|| Rect::new(sheet.x0 + 24.0, cancel.y0, sheet.x0 + 108.0, cancel.y1)),
        })
    }
    pub fn click(&mut self, point: vello::kurbo::Point, layout: &DialogLayout) -> bool {
        if layout.review_back.is_some_and(|rect| rect.contains(point)) {
            return self.review_back();
        }
        if let Some(index) = layout
            .secret_fields
            .iter()
            .position(|rect| rect.contains(point))
        {
            self.entry_focus = index;
            return true;
        }
        if layout.remember.is_some_and(|rect| rect.contains(point)) {
            self.entry_focus = self.entries.len();
            self.remember = !self.remember;
            return true;
        }
        if let Some(index) = layout.choices.iter().position(|rect| rect.contains(point)) {
            let visible_start = self.visible_selection_start();
            self.selected = visible_start + index;
            if matches!(self.view(), Some(DialogView::UsbDevices { .. })) {
                let _ = self.toggle_selected_usb_device();
            }
            return true;
        }
        if layout
            .launcher_name
            .is_some_and(|rect| rect.contains(point))
        {
            self.launcher_field = 0;
            return true;
        }
        if let Some(index) = layout
            .launcher_icons
            .iter()
            .position(|rect| rect.contains(point))
        {
            self.launcher_field = 1;
            self.launcher_icon = index;
            return true;
        }
        if layout.cancel.contains(point) {
            return self.cancel();
        }
        if layout.confirm.contains(point) {
            return self.confirm();
        }
        layout.sheet.contains(point)
    }

    /// Handles a key that arrived while the modal sheet owns focus. Returns
    /// true whenever the key is consumed, including rejected captures, so a
    /// proposed global shortcut can never leak into the requesting app.
    pub fn key(&mut self, physical: &PhysicalKey, modifiers: Modifiers) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        if !matches!(view, DialogView::ShortcutBindings { .. }) {
            return false;
        }
        self.queue
            .edit_shortcut_binding(view.handle(), self.selected, physical, modifiers)
    }

    pub fn clear_selected_shortcut(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        if !matches!(view, DialogView::ShortcutBindings { .. }) {
            return false;
        }
        self.queue
            .clear_shortcut_binding(view.handle(), self.selected)
    }

    pub fn is_shortcut_editor(&self) -> bool {
        matches!(self.view(), Some(DialogView::ShortcutBindings { .. }))
    }

    /// Toggle the focused USB row.  This is intentionally distinct from
    /// confirmation: Enter commits the reviewed subset, while Space (or a
    /// row click) changes only that physical device.
    pub fn toggle_selected_usb_device(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        matches!(view, DialogView::UsbDevices { .. })
            && self.queue.toggle_usb_device(view.handle(), self.selected)
    }

    pub fn is_usb_device_selector(&self) -> bool {
        matches!(self.view(), Some(DialogView::UsbDevices { .. }))
    }

    pub fn visible_selection_start(&self) -> usize {
        const VISIBLE_ROWS: usize = 6;
        self.selected / VISIBLE_ROWS * VISIBLE_ROWS
    }

    /// Edits the launcher title only while the portal sheet is active.  This
    /// keeps text entry out of the requester and gives the user the exact
    /// title that will be installed.
    pub fn edit_launcher_name(&mut self, logical_key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};

        let Some(view) = self.view() else {
            return false;
        };
        let Some(editor) = view.launcher_editor() else {
            return false;
        };
        if !editor.editable_name || self.launcher_field != 0 {
            return false;
        }
        match logical_key {
            Key::Named(NamedKey::Backspace) => {
                ui::delete_last_grapheme(&mut self.launcher_name);
                true
            }
            Key::Named(NamedKey::Space) => {
                if self.launcher_name.len() < 255 {
                    self.launcher_name.push(' ');
                }
                true
            }
            Key::Character(text) => {
                let accepted: String = text.chars().filter(|ch| !ch.is_control()).collect();
                if !accepted.is_empty()
                    && self.launcher_name.len().saturating_add(accepted.len()) <= 255
                {
                    self.launcher_name.push_str(&accepted);
                }
                true
            }
            _ => false,
        }
    }

    /// Hand a key or IME event to the sheet's focused text field. The
    /// "Remember" row takes only Space, which ticks it. Consumes every
    /// event while a sheet with text entry is up.
    pub fn edit_secret(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::keyboard::{Key, NamedKey};

        if self.is_network_secrets() {
            if self.entry_focus >= self.entries.len() {
                // The "Remember" row: Space toggles it, nothing else types.
                if matches!(
                    event,
                    WindowEvent::KeyboardInput {
                        event: KeyEvent {
                            logical_key: Key::Named(NamedKey::Space),
                            state: ElementState::Pressed,
                            ..
                        },
                        ..
                    }
                ) {
                    self.remember = !self.remember;
                }
                return true;
            }
            self.entry_error = None;
            let focus = self.entry_focus;
            for (index, entry) in self.entries.iter_mut().enumerate() {
                entry.set_focus(index == focus);
            }
            if let Some(entry) = self.entries.get_mut(focus) {
                forward_to_field(entry, event, ctx);
            }
            return true;
        }
        self.entry_error = None;
        let Some(field) = self.text_field_mut() else {
            return false;
        };
        field.set_focus(true);
        forward_to_field(field, event, ctx);
        true
    }

    /// The single text field of an authentication sheet or a shortcut
    /// step, when one is up.
    fn text_field_mut(&mut self) -> Option<&mut TextInput> {
        match self.view()? {
            DialogView::Authentication { .. } => Some(&mut self.secret),
            DialogView::ShortcutStep { field: Some(_), .. } => Some(&mut self.step_field),
            _ => None,
        }
    }

    /// Lay the sheet's text fields out in `layout` and mark the focused
    /// one; returns them for painting and pointer routing.
    pub fn place_fields(&mut self, layout: &DialogLayout) -> Vec<&mut TextInput> {
        let Some(view) = self.view() else {
            return Vec::new();
        };
        let mut placed = Vec::new();
        match view {
            DialogView::Authentication { .. } => {
                if let Some(rect) = layout.secret {
                    self.secret.apply_layout_external(rect);
                    self.secret.set_focus(true);
                    placed.push(&mut self.secret);
                }
            }
            DialogView::ShortcutStep { field: Some(_), .. } => {
                if let Some(rect) = layout.launcher_name {
                    self.step_field.apply_layout_external(rect);
                    self.step_field.set_focus(true);
                    placed.push(&mut self.step_field);
                }
            }
            DialogView::NetworkSecrets { .. } => {
                let focus = self.entry_focus;
                for (index, (entry, rect)) in self
                    .entries
                    .iter_mut()
                    .zip(&layout.secret_fields)
                    .enumerate()
                {
                    entry.apply_layout_external(*rect);
                    entry.set_focus(index == focus);
                    placed.push(entry);
                }
            }
            _ => {}
        }
        placed
    }

    /// Route a pointer event to the sheet's text fields, for hover, caret
    /// placement and their show/hide controls; a press on a field focuses
    /// it first. `true` when a field used the event.
    pub fn field_pointer_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        layout: &DialogLayout,
        point: Option<vello::kurbo::Point>,
    ) -> bool {
        if let (
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            },
            Some(point),
        ) = (event, point)
            && let Some(index) = layout
                .secret_fields
                .iter()
                .position(|rect| rect.contains(point))
        {
            self.entry_focus = index;
        }
        let mut used = false;
        for field in self.place_fields(layout) {
            used |= forward_to_field(field, event, ctx);
        }
        used
    }

    pub fn is_authentication(&self) -> bool {
        matches!(self.view(), Some(DialogView::Authentication { .. }))
    }

    pub fn is_network_secrets(&self) -> bool {
        matches!(self.view(), Some(DialogView::NetworkSecrets { .. }))
    }

    /// Whether the sheet up takes typed text — a secret, or a shortcut
    /// step's answer — so every key belongs to it.
    pub fn is_text_entry(&self) -> bool {
        matches!(
            self.view(),
            Some(
                DialogView::Authentication { .. }
                    | DialogView::NetworkSecrets { .. }
                    | DialogView::ShortcutStep { field: Some(_), .. }
            )
        )
    }

    /// Whether the sheet's "Remember" row is ticked and focused, when the
    /// sheet offers one.
    pub fn remember_display(&self) -> Option<(bool, bool)> {
        match self.view()? {
            DialogView::NetworkSecrets {
                remember_offered: true,
                ..
            } => Some((self.remember, self.entry_focus >= self.entries.len())),
            // Its choices take the focus; Space ticks the row.
            DialogView::DriveConsent {
                remember_label: Some(_),
                ..
            } => Some((self.remember, false)),
            _ => None,
        }
    }

    /// What the sheet's "Remember" row says, when it offers one.
    pub fn remember_label(&self) -> Option<String> {
        match self.view()? {
            DialogView::NetworkSecrets {
                remember_offered: true,
                ..
            } => Some(t("portal-remember-device")),
            DialogView::DriveConsent { remember_label, .. } => remember_label,
            _ => None,
        }
    }

    /// Tick or untick the "Remember" row of a sheet whose keys otherwise
    /// move between choices. `false` when the sheet up has no such row.
    pub fn toggle_remember(&mut self) -> bool {
        let offered = matches!(
            self.view(),
            Some(DialogView::DriveConsent {
                remember_label: Some(_),
                ..
            })
        );
        if offered {
            self.remember = !self.remember;
        }
        offered
    }

    /// Why the network-secrets sheet did not close, when it refused.
    pub fn entry_error(&self) -> Option<&str> {
        self.entry_error.as_deref()
    }

    /// The sheet up, for `state-dump`: what the person sees, with every
    /// masked value reduced to its length.
    pub fn state_json(&self) -> serde_json::Value {
        let Some(view) = self.view() else {
            return serde_json::Value::Null;
        };
        let mut state = serde_json::json!({
            "kind": view.kind(),
            "handle": view.handle(),
            "title": view.title(),
            "app_name": view.app_name(),
            "detail": view.detail(),
            "confirm_label": view.confirm_label(),
            "choices": view.choices(),
            "selected": self.selected,
        });
        match &view {
            DialogView::Authentication { echo, .. } => {
                state["secret_len"] = self.secret.value().chars().count().into();
                if *echo {
                    state["secret"] = self.secret.value().into();
                }
            }
            DialogView::NetworkSecrets {
                fields,
                remember_offered,
                ..
            } => {
                state["fields"] = fields
                    .iter()
                    .zip(&self.entries)
                    .enumerate()
                    .map(|(index, (field, input))| {
                        let value = input.value();
                        let mut entry = serde_json::json!({
                            "key": field.key,
                            "label": field.label,
                            "masked": field.masked,
                            "required": field.required,
                            "length": value.chars().count(),
                            "focused": index == self.entry_focus,
                        });
                        if !field.masked {
                            entry["value"] = value.into();
                        }
                        entry
                    })
                    .collect();
                state["remember"] = if *remember_offered {
                    self.remember.into()
                } else {
                    serde_json::Value::Null
                };
                state["error"] = self.entry_error.clone().into();
            }
            DialogView::ShortcutStep {
                field_review: true, ..
            } => {
                state["error"] = self.entry_error.clone().into();
                state["value"] = self.step_field.value().into();
            }
            DialogView::DriveConsent { remember_label, .. } => {
                state["cancel_label"] = view.cancel_label().into();
                state["remember"] = if remember_label.is_some() {
                    self.remember.into()
                } else {
                    serde_json::Value::Null
                };
            }
            _ => {}
        }
        state
    }

    /// The sheet's single text field — an authentication answer or a
    /// shortcut step's — when one is up.
    pub fn text_field(&self) -> Option<&TextInput> {
        match self.view()? {
            DialogView::Authentication { .. } => Some(&self.secret),
            DialogView::ShortcutStep { field: Some(_), .. } => Some(&self.step_field),
            _ => None,
        }
    }

    /// The network-secrets sheet's fields, in order.
    pub fn secret_fields(&self) -> &[TextInput] {
        &self.entries
    }

    /// Whether the sheet up now asks for a secret: administrator
    /// authentication, network secrets, or any sheet whose field is a
    /// secret, such as a shortcut step's masked answer.
    pub fn holds_secret(&self) -> bool {
        matches!(
            self.view(),
            Some(DialogView::Authentication { .. } | DialogView::NetworkSecrets { .. })
        ) || self.text_field().is_some_and(TextInput::is_secret)
            || self.entries.iter().any(TextInput::is_secret)
    }

    fn forget_secret(&mut self) {
        // Overwrite before release so the freed allocation does not keep
        // the password.
        wipe(&mut self.secret);
        wipe(&mut self.step_field);
        for mut entry in std::mem::take(&mut self.entries) {
            wipe(&mut entry);
        }
        self.entry_focus = 0;
        self.remember = false;
        self.entry_error = None;
    }

    pub fn launcher_name(&self) -> Option<&str> {
        self.view()
            .is_some_and(|view| view.launcher_editor().is_some())
            .then_some(self.launcher_name.as_str())
    }

    pub fn launcher_icon(&self) -> Option<usize> {
        self.view()
            .is_some_and(|view| view.launcher_editor().is_some())
            .then_some(self.launcher_icon)
    }

    pub fn launcher_name_focused(&self) -> bool {
        self.view().is_some_and(|view| {
            view.launcher_editor()
                .is_some_and(|editor| editor.editable_name && self.launcher_field == 0)
        })
    }
}

/// Give `event` to an embedded field; `true` when the field used it. The
/// fields are local and never write the document.
fn forward_to_field(field: &mut TextInput, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut used = false;
    // The sheet owns focus between its fields; the node is a stand-in that
    // must never become the frame's focus target.
    let mut focus_target = None;
    field.handle_event(
        event,
        &mut EventCtx {
            taffy: &taffy,
            handled: &mut used,
            focus_target: &mut focus_target,
            ..ctx.reborrow(ctx.path, node)
        },
    );
    // The value is read back on confirm; drop the copy this hands out.
    if let Some(value) = field.take_dirty() {
        drop(zeroize::Zeroizing::new(value));
    }
    used
}

/// The field's value, leaving it wiped.
fn take_text(field: &mut TextInput) -> String {
    let value = field.value().to_owned();
    wipe(field);
    value
}

/// Empty `field`, overwriting what it held: a field marked secret zeroes
/// its text as it lets go of it, so an echoed answer is marked too.
fn wipe(field: &mut TextInput) {
    field.set_secret(true);
    field.set_text("");
}

fn authentication_detail(request: &AuthenticationRequest) -> String {
    let mut lines = Vec::new();
    if let Some(notice) = request
        .notice
        .as_deref()
        .filter(|notice| !notice.is_empty())
    {
        lines.push(notice.to_string());
    }
    let who = request
        .identities
        .get(request.identity)
        .map_or_else(String::new, |user| format!(" ({user})"));
    lines.push(format!("{}{who}", request.prompt.trim()));
    lines.push(request.action_id.clone());
    lines.join("\n")
}

fn chooser_detail(request: &ApplicationChoiceRequest) -> String {
    let subject = request
        .filename
        .as_deref()
        .or(request.uri.as_deref())
        .or(request.content_type.as_deref())
        .map_or_else(|| t("portal-chooser-this-item"), str::to_string);
    t_args(
        "portal-chooser-detail",
        &[
            ("app", friendly_app_name(&request.app_id).into()),
            ("subject", subject.into()),
        ],
    )
}
/// The allowing answers the grant sheet offers for `request`, in order.
fn action_grant_answers(request: &AppActionConsentRequest) -> &'static [AppActionConsent] {
    if request.confirming {
        &AppActionConsent::ALLOWING[..1]
    } else {
        &AppActionConsent::ALLOWING
    }
}

/// The action's consequences, as the grant sheet states them.
fn action_grant_detail(request: &AppActionConsentRequest) -> String {
    let risk = crate::shell::app_action_consent::risk_label(request.risk);
    if let Some(preview) = &request.moderation {
        return moderation_detail(preview);
    }
    if request.confirming {
        return t_args("portal-confirm-detail", &[("risk", risk.into())]);
    }
    let key = if request.needs_confirmation {
        "portal-grant-detail-confirms"
    } else {
        "portal-grant-detail-unattended"
    };
    t_args(key, &[("risk", risk.into())])
}

/// Keep every disclosure field and make control characters visible. The
/// controller paginates this complete text into a bounded trusted sheet;
/// neither long values nor short displays may hide content behind Allow.
fn moderation_detail(
    preview: &tilo_action_broker::app_actions::ModerationConsentPreview,
) -> String {
    use tilo_action_broker::app_actions::ModerationConsentPurpose;
    let purpose = match preview.purpose {
        ModerationConsentPurpose::Report => "portal-moderation-report",
        ModerationConsentPurpose::Appeal => "portal-moderation-appeal",
    };
    let mut detail = t(purpose);
    for (key, value) in [
        (
            "portal-moderation-destination",
            Some(preview.destination.as_str()),
        ),
        ("portal-moderation-owner", Some(preview.owner_root.as_str())),
        (
            "portal-moderation-identity",
            Some(preview.signing_root.as_str()),
        ),
        ("portal-moderation-target", Some(preview.target.as_str())),
        ("portal-moderation-category", preview.category.as_deref()),
        ("portal-moderation-content", Some(preview.content.as_str())),
        (
            "portal-moderation-evidence",
            preview.evidence_digest.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            detail.push('\n');
            detail.push_str(&t(key));
            detail.push('\n');
            // Escape controls and directional overrides: payload text cannot
            // impersonate a trusted field label by rearranging the sheet.
            let visible: String = value.chars().flat_map(|c| {
                if c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            }).collect();
            for (index, c) in visible.chars().enumerate() {
                if index > 0 && index % 56 == 0 {
                    detail.push('\n');
                }
                detail.push(c);
            }
        }
    }
    detail
}

fn friendly_app_name(app_id: &str) -> String {
    crate::shell::launcher::display_name_for_app_id(app_id).unwrap_or_else(|| {
        app_id
            .rsplit('.')
            .next()
            .unwrap_or(app_id)
            .replace(['-', '_'], " ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, ModifiersState};
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn portal_choice_dialog_updates_the_controller_and_completes_on_cancel_or_review() {
        fn action_buttons_share_the_choice_rows_gutter() {
            let q = PortalDialogQueue::default();
            let service = ShellAppChooserService::with_queue(q.clone());
            let (tx, _rx) = oneshot::channel();
            assert!(matches!(
                service.enqueue(
                    ApplicationChoiceRequest {
                        handle: "/r/layout".into(),
                        app_id: "org.example.Reader".into(),
                        choices: vec!["one".into(), "two".into()],
                        last_choice: None,
                        content_type: None,
                        uri: None,
                        filename: None,
                        activation_token: None
                    },
                    tx
                ),
                PortalResult::Success(())
            ));
            let mut c = PortalDialogController::new(q);
            assert!(c.pump());
            let layout = c
                .layout(Rect::new(0.0, 0.0, 1280.0, 800.0))
                .expect("an active sheet");
            let row = layout.choices[0];
            // Confirm's right edge is the rows' right edge; the row above used
            // to end 4 px short of the buttons below it.
            assert_eq!(layout.confirm.x1, row.x1);
            assert_eq!(layout.confirm.x1, layout.sheet.x1 - 24.0);
            assert_eq!(
                layout.cancel.x1 + 12.0,
                layout.confirm.x0,
                "buttons keep their 12 px gap"
            );
            assert_eq!(layout.cancel.width(), 92.0);
            assert_eq!(layout.confirm.width(), 84.0);
            assert!(layout.cancel.height() >= 32.0 && layout.confirm.height() >= 32.0);
            assert_eq!(layout.cancel.y0, layout.confirm.y0);
            assert_eq!(layout.cancel.y1, layout.confirm.y1);
        }

        fn choice_updates_reach_the_live_controller() {
            let q = PortalDialogQueue::default();
            let service = ShellAppChooserService::with_queue(q.clone());
            let (tx, rx) = oneshot::channel();
            assert!(matches!(
                service.enqueue(
                    ApplicationChoiceRequest {
                        handle: "/r/1".into(),
                        app_id: "org.example.Reader".into(),
                        choices: vec!["one".into()],
                        last_choice: None,
                        content_type: None,
                        uri: None,
                        filename: None,
                        activation_token: None
                    },
                    tx
                ),
                PortalResult::Success(())
            ));
            let mut c = PortalDialogController::new(q.clone());
            assert!(c.pump());
            assert!(matches!(
                service.update_choices("/r/1", &["two".into()]),
                PortalResult::Success(())
            ));
            assert_eq!(c.view().unwrap().choices(), ["two"]);
            assert!(c.confirm());
            assert!(matches!(
                rx.blocking_recv(),
                Ok(PortalResult::Success(ApplicationChoice { choice, .. })) if choice == "two"
            ));
        }

        fn cancel_completes_the_async_request() {
            let q = PortalDialogQueue::default();
            let (tx, rx) = oneshot::channel();
            assert!(matches!(
                q.enqueue_choice(
                    ApplicationChoiceRequest {
                        handle: "/r/2".into(),
                        app_id: "x".into(),
                        choices: vec!["one".into()],
                        last_choice: None,
                        content_type: None,
                        uri: None,
                        filename: None,
                        activation_token: None
                    },
                    tx
                ),
                PortalResult::Success(())
            ));
            let mut c = PortalDialogController::new(q);
            c.pump();
            assert!(c.cancel());
            assert!(matches!(rx.blocking_recv(), Ok(PortalResult::Cancelled)));
        }

        fn review_keeps_the_exact_reason_and_needs_confirmation() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_review(ReviewRequest {
                    handle: "/r/review".into(),
                    app_id: "org.example.Backup".into(),
                    title: "Share backup".into(),
                    reason: "Send an encrypted archive to your provider.".into(),
                    resource: "backup-2026.zip".into(),
                    confirm_label: "Share".into(),
                })
                .expect("a unique request queues");
            let mut controller = PortalDialogController::new(queue);
            assert!(controller.pump());
            let view = controller.view().expect("visible review");
            assert!(view.detail().contains("encrypted archive"));
            assert!(view.detail().contains("backup-2026.zip"));
            assert!(controller.confirm());
            assert_eq!(receiver.blocking_recv(), Ok(ReviewDecision::Confirmed));
        }

        action_buttons_share_the_choice_rows_gutter();
        choice_updates_reach_the_live_controller();
        cancel_completes_the_async_request();
        review_keeps_the_exact_reason_and_needs_confirmation();
    }

    fn authentication_request(identities: &[&str]) -> AuthenticationRequest {
        AuthenticationRequest {
            handle: "polkit:cookie".into(),
            message: "Authentication is required to install software".into(),
            action_id: "org.freedesktop.Flatpak.app-install".into(),
            identities: identities.iter().map(ToString::to_string).collect(),
            identity: 0,
            prompt: "Password:".into(),
            echo: false,
            notice: Some("Wrong password".into()),
        }
    }

    fn drive_request(remember: Option<&str>) -> DriveConsentRequest {
        DriveConsentRequest {
            handle: "tilo-drive-1-nvme0n1p3:1".into(),
            title: "Open Windows?".into(),
            drive: "Inside this computer · 512 GB".into(),
            detail: "Choose how to open this drive.".into(),
            choices: vec!["Open safely".into(), "Open".into()],
            preselect: 1,
            remember: remember.map(str::to_owned),
            confirm_label: "Open".into(),
            cancel_label: "Not now".into(),
        }
    }

    #[test]
    fn the_drive_sheet_starts_on_the_suggestion_and_answers_only_what_was_chosen() {
        let bounds = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let queue = PortalDialogQueue::default();
        let answer = queue
            .enqueue_drive_consent(drive_request(Some("Remember this drive")))
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue.clone());
        assert!(sheet.pump());
        assert_eq!(
            sheet.selected(),
            1,
            "the suggestion is selected, not answered"
        );
        let view = sheet.view().expect("visible");
        assert_eq!(view.app_name(), "Inside this computer · 512 GB");
        assert_eq!(view.cancel_label(), "Not now");
        assert_eq!(
            sheet.remember_label().as_deref(),
            Some("Remember this drive")
        );
        assert_eq!(sheet.remember_display(), Some((false, false)));
        let layout = sheet.layout(bounds).expect("layout");
        let remember = layout.remember.expect("a remember row");
        assert_eq!(layout.choices.len(), 2);
        assert!(remember.y0 >= layout.choices[1].y1, "below the choices");
        assert!(remember.y1 <= layout.cancel.y0, "above the buttons");
        assert!(sheet.toggle_remember());
        assert!(sheet.select_next(true));
        assert_eq!(sheet.state_json()["remember"], true);
        assert!(sheet.confirm());
        assert_eq!(
            answer.blocking_recv(),
            Ok(DriveConsentDecision::Chosen {
                choice: 0,
                remember: true
            })
        );

        let dismissed = queue
            .enqueue_drive_consent(drive_request(None))
            .expect("queued");
        assert!(sheet.pump());
        assert!(
            !sheet.toggle_remember(),
            "nothing to remember for this drive"
        );
        assert!(sheet.remember_display().is_none());
        assert!(sheet.layout(bounds).expect("layout").remember.is_none());
        assert!(sheet.cancel());
        assert_eq!(dismissed.blocking_recv(), Ok(DriveConsentDecision::NotNow));

        let withdrawn = queue
            .enqueue_drive_consent(drive_request(None))
            .expect("queued");
        assert!(queue.cancel("tilo-drive-1-nvme0n1p3:1"));
        assert_eq!(withdrawn.blocking_recv(), Ok(DriveConsentDecision::NotNow));
        let mut invalid = drive_request(None);
        invalid.preselect = 2;
        assert!(queue.enqueue_drive_consent(invalid).is_err());
    }

    #[test]
    fn authentication_sheet_hides_the_secret_and_hands_it_over_once() {
        use winit::keyboard::{Key, NamedKey};

        let queue = PortalDialogQueue::default();
        let receiver = queue
            .enqueue_authentication(authentication_request(&["quinn"]))
            .expect("queued");
        let mut controller = PortalDialogController::new(queue);
        assert!(controller.pump());
        let view = controller.view().expect("visible sheet");
        assert_eq!(
            view.title(),
            "Authentication is required to install software"
        );
        assert!(view.detail().contains("Wrong password"));
        assert!(
            view.detail()
                .contains("org.freedesktop.Flatpak.app-install")
        );
        assert!(view.choices().is_empty(), "one identity needs no picker");
        let layout = controller
            .layout(Rect::new(0.0, 0.0, 1280.0, 800.0))
            .expect("layout");
        assert!(layout.secret.is_some());
        for key in ["h", "u", "n", "t"] {
            assert!(type_key(&mut controller, Key::Character(key.into())));
        }
        assert!(type_key(&mut controller, Key::Named(NamedKey::Backspace)));
        let field = controller.text_field().expect("the answer field");
        assert!(field.is_secret(), "the answer is a secret field");
        assert!(!field.is_revealed(), "and starts hidden");
        assert_eq!(field.value(), "hun");
        assert!(controller.confirm());
        assert_eq!(
            receiver.blocking_recv(),
            Ok(AuthenticationDecision::Answered {
                identity: 0,
                secret: "hun".into()
            })
        );
        assert!(controller.text_field().is_none());
        assert!(controller.secret.value().is_empty(), "nothing is kept");
    }

    #[test]
    fn every_secret_field_of_a_sheet_is_hidden_with_its_own_reveal() {
        let layout_bounds = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let queue = PortalDialogQueue::default();
        let _auth = queue
            .enqueue_authentication(authentication_request(&["quinn"]))
            .expect("queued");
        let mut controller = PortalDialogController::new(queue.clone());
        assert!(controller.pump());
        let layout = controller.layout(layout_bounds).expect("layout");
        let fields = controller.place_fields(&layout);
        assert_eq!(fields.len(), 1);
        assert!(fields[0].is_secret() && !fields[0].is_revealed());
        assert!(fields[0].has_reveal_control());
        assert!(controller.cancel());

        let mut echo = authentication_request(&["quinn"]);
        echo.echo = true;
        let _echo = queue.enqueue_authentication(echo).expect("queued");
        assert!(controller.pump());
        assert!(
            !controller.text_field().expect("field").is_secret(),
            "an echoed answer is shown as typed"
        );
        assert!(controller.cancel());

        let _secrets = queue
            .enqueue_network_secrets(NetworkSecretsRequest {
                handle: "nm:1".into(),
                title: "Network sign-in".into(),
                detail: String::new(),
                fields: vec![
                    SecretFieldSpec {
                        key: "identity".into(),
                        label: "User name".into(),
                        masked: false,
                        required: true,
                        initial: "quinn".into(),
                        rule: SecretFieldRule::Any,
                    },
                    SecretFieldSpec {
                        key: "password".into(),
                        label: "Password".into(),
                        masked: true,
                        required: true,
                        initial: String::new(),
                        rule: SecretFieldRule::Any,
                    },
                ],
                remember: None,
            })
            .expect("queued");
        assert!(controller.pump());
        let [identity, password] = controller.secret_fields() else {
            panic!("two fields");
        };
        assert!(!identity.is_secret());
        assert_eq!(identity.value(), "quinn");
        assert!(password.is_secret() && !password.is_revealed());
        assert!(controller.cancel());

        queue_secret_step(&queue);
        assert!(controller.pump());
        let field = controller.text_field().expect("the step's field");
        assert!(field.is_secret() && !field.is_revealed());
    }

    fn queue_secret_step(queue: &PortalDialogQueue) {
        queue
            .enqueue_shortcut_step(
                ShortcutStepRequest {
                    handle: "shortcut:token".into(),
                    shortcut: "Publish".into(),
                    title: "Publishing token".into(),
                    detail: String::new(),
                    confirm_label: "Continue".into(),
                    choices: Vec::new(),
                    field: Some(ShortcutStepField { masked: true }),
                },
                |_| {},
            )
            .expect("queued");
    }

    fn type_key(controller: &mut PortalDialogController, key: winit::keyboard::Key) -> bool {
        let event = ui::synthetic_input::key_input(&key, winit::event::ElementState::Pressed);
        crate::shell::test_support::with_scratch_event_ctx(|ctx| {
            controller.edit_secret(&event, ctx)
        })
    }

    #[test]
    fn authentication_sheet_offers_a_choice_of_identity_and_can_be_dismissed() {
        use winit::keyboard::Key;

        let queue = PortalDialogQueue::default();
        let receiver = queue
            .enqueue_authentication(authentication_request(&["quinn", "root"]))
            .expect("queued");
        let mut controller = PortalDialogController::new(queue.clone());
        assert!(controller.pump());
        assert_eq!(
            controller.view().expect("sheet").choices(),
            ["quinn".to_string(), "root".to_string()]
        );
        assert!(controller.select_next(false));
        assert!(type_key(&mut controller, Key::Character("x".into())));
        assert!(controller.cancel());
        assert_eq!(
            receiver.blocking_recv(),
            Ok(AuthenticationDecision::Cancelled)
        );

        let receiver = queue
            .enqueue_authentication(authentication_request(&["quinn", "root"]))
            .expect("queued again");
        assert!(controller.pump());
        assert_eq!(
            controller.text_field().map(TextInput::value),
            Some(""),
            "nothing carried over"
        );
        assert!(controller.select_next(false));
        assert!(controller.confirm());
        assert_eq!(
            receiver.blocking_recv(),
            Ok(AuthenticationDecision::Answered {
                identity: 1,
                secret: String::new()
            })
        );
    }

    #[test]
    fn usb_sheet_returns_only_individually_selected_devices_on_every_page() {
        fn usb_sheet_returns_only_the_individually_selected_subset() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_usb_devices(UsbDeviceRequest {
                    review: ReviewRequest {
                        handle: "/r/usb".into(),
                        app_id: "org.example.Camera".into(),
                        title: "Choose USB devices".into(),
                        reason: "Review each device.".into(),
                        resource: "Access is temporary.".into(),
                        confirm_label: "Allow selected".into(),
                    },
                    devices: vec![
                        UsbDeviceRow {
                            id: "camera".into(),
                            label: "Conference camera".into(),
                            writable: false,
                        },
                        UsbDeviceRow {
                            id: "flash".into(),
                            label: "Firmware drive".into(),
                            writable: true,
                        },
                    ],
                })
                .unwrap();
            let mut controller = PortalDialogController::new(queue);
            assert!(controller.pump());
            let view = controller.view().unwrap();
            assert_eq!(view.usb_devices().len(), 2);
            assert!(view.usb_devices()[0].selected);
            assert!(view.usb_devices()[1].writable);
            assert!(controller.select_next(false));
            assert!(controller.toggle_selected_usb_device());
            assert!(controller.confirm());
            assert_eq!(
                receiver.blocking_recv(),
                Ok(UsbDeviceDecision {
                    decision: ReviewDecision::Confirmed,
                    approved: vec!["camera".into()],
                })
            );
        }

        fn usb_rows_after_the_first_page_remain_selectable() {
            let queue = PortalDialogQueue::default();
            let devices = (0..7)
                .map(|index| UsbDeviceRow {
                    id: format!("device-{index}"),
                    label: format!("Device {index}"),
                    writable: false,
                })
                .collect();
            let receiver = queue
                .enqueue_usb_devices(UsbDeviceRequest {
                    review: ReviewRequest {
                        handle: "/r/usb-pages".into(),
                        app_id: "org.example.Usb".into(),
                        title: "Choose USB devices".into(),
                        reason: String::new(),
                        resource: String::new(),
                        confirm_label: "Allow selected".into(),
                    },
                    devices,
                })
                .unwrap();
            let mut controller = PortalDialogController::new(queue);
            controller.pump();
            for _ in 0..6 {
                assert!(controller.select_next(false));
            }
            assert_eq!(controller.visible_selection_start(), 6);
            assert!(controller.toggle_selected_usb_device());
            assert!(controller.confirm());
            let decision = receiver.blocking_recv().unwrap();
            assert_eq!(decision.approved.len(), 6);
            assert!(!decision.approved.contains(&"device-6".into()));
        }

        usb_sheet_returns_only_the_individually_selected_subset();
        usb_rows_after_the_first_page_remain_selectable();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn launcher_and_shortcut_editors_return_only_reviewed_choices() {
        fn launcher_editor_returns_only_the_shell_reviewed_name_and_icon_choice() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_launcher_review(LauncherReviewRequest {
                    review: ReviewRequest {
                        handle: "/r/launcher".into(),
                        app_id: "org.example.Web".into(),
                        title: "Install this web app?".into(),
                        reason: "Reviewed by Tilo.".into(),
                        resource: "https://example.test".into(),
                        confirm_label: "Add launcher".into(),
                    },
                    name: "Example".into(),
                    editable_name: true,
                    editable_icon: true,
                    icon_choices: vec!["supplied".into(), "monogram".into()],
                })
                .expect("a unique launcher request queues");
            let mut controller = PortalDialogController::new(queue);
            assert!(controller.pump());
            assert!(controller.edit_launcher_name(&winit::keyboard::Key::Character("!".into())));
            assert!(controller.select_next(false));
            assert_eq!(controller.launcher_icon(), Some(1));
            assert!(controller.confirm());
            assert_eq!(
                receiver.blocking_recv(),
                Ok(LauncherReviewDecision {
                    decision: ReviewDecision::Confirmed,
                    name: "Example!".into(),
                    icon_choice: 1,
                })
            );
        }

        fn launcher_close_cancels_and_never_leaves_a_waiter() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_launcher_review(LauncherReviewRequest {
                    review: ReviewRequest {
                        handle: "/r/launcher-close".into(),
                        app_id: "org.example.Web".into(),
                        title: "Add launcher?".into(),
                        reason: String::new(),
                        resource: "Example".into(),
                        confirm_label: "Add".into(),
                    },
                    name: "Example".into(),
                    editable_name: false,
                    editable_icon: false,
                    icon_choices: vec!["supplied".into()],
                })
                .expect("a unique launcher request queues");
            assert!(queue.cancel("/r/launcher-close"));
            assert_eq!(
                receiver.blocking_recv(),
                Ok(LauncherReviewDecision {
                    decision: ReviewDecision::Cancelled,
                    name: "Example".into(),
                    icon_choice: 0,
                })
            );
        }

        fn shortcut_editor_records_a_physical_chord_and_can_clear_it() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_shortcut_bindings(ShortcutBindingRequest {
                    handle: "/r/shortcuts".into(),
                    app_id: "org.example.Recorder".into(),
                    rows: vec![ShortcutBindingRow {
                        id: "toggle".into(),
                        description: "Toggle recording".into(),
                        accelerator: None,
                        status: None,
                    }],
                })
                .expect("a unique request queues");
            let mut controller = PortalDialogController::new(queue);
            assert!(controller.pump());
            assert!(controller.key(
                &PhysicalKey::Code(KeyCode::KeyK),
                Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT),
            ));
            assert_eq!(
                controller.view().unwrap().shortcut_rows()[0]
                    .accelerator
                    .as_deref(),
                Some("CTRL+SHIFT+KEYK")
            );
            assert!(controller.clear_selected_shortcut());
            assert!(controller.confirm());
            assert_eq!(
                receiver.blocking_recv().unwrap().rows[0].accelerator,
                None,
                "clearing a row means it is not globally registered"
            );
        }

        fn shortcut_editor_explains_reserved_chords_without_accepting_them() {
            let queue = PortalDialogQueue::default();
            let receiver = queue
                .enqueue_shortcut_bindings(ShortcutBindingRequest {
                    handle: "/r/reserved".into(),
                    app_id: "org.example.Recorder".into(),
                    rows: vec![ShortcutBindingRow {
                        id: "toggle".into(),
                        description: String::new(),
                        accelerator: None,
                        status: None,
                    }],
                })
                .unwrap();
            let mut controller = PortalDialogController::new(queue);
            controller.pump();
            assert!(controller.key(
                &PhysicalKey::Code(KeyCode::Space),
                Modifiers::from(ModifiersState::SUPER),
            ));
            let view = controller.view().unwrap();
            let row = &view.shortcut_rows()[0];
            assert!(row.accelerator.is_none());
            assert!(
                row.status
                    .as_deref()
                    .is_some_and(|text| text.contains("Reserved"))
            );
            assert!(controller.cancel());
            assert!(
                receiver.blocking_recv().is_err(),
                "cancel drops the binding reply"
            );
        }

        launcher_editor_returns_only_the_shell_reviewed_name_and_icon_choice();
        launcher_close_cancels_and_never_leaves_a_waiter();
        shortcut_editor_records_a_physical_chord_and_can_clear_it();
        shortcut_editor_explains_reserved_chords_without_accepting_them();
    }
}
