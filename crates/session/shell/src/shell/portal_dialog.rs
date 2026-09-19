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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::oneshot;
use vello::kurbo::Rect;
use winit::event::Modifiers;
use winit::keyboard::PhysicalKey;

use crate::portal::desktop_services::{
    AppChooserService, ApplicationChoice, ApplicationChoiceRequest, PortalResult,
};

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

enum PendingDialog {
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
}

#[derive(Default)]
struct DialogQueueInner {
    order: VecDeque<String>,
    pending: HashMap<String, PendingDialog>,
}

/// Thread-safe, process-local handoff from the portal runtime to the
/// compositor frame loop.
#[derive(Clone, Default)]
pub struct PortalDialogQueue {
    inner: Arc<Mutex<DialogQueueInner>>,
}

impl PortalDialogQueue {
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

    pub fn enqueue_review(
        &self,
        request: ReviewRequest,
    ) -> Result<oneshot::Receiver<ReviewDecision>, String> {
        let (reply, receiver) = oneshot::channel();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&request.handle) {
            return Err("duplicate live portal request".into());
        }
        inner.order.push_back(request.handle.clone());
        inner.pending.insert(
            request.handle.clone(),
            PendingDialog::Review { request, reply },
        );
        Ok(receiver)
    }

    pub fn enqueue_launcher_review(
        &self,
        request: LauncherReviewRequest,
    ) -> Result<oneshot::Receiver<LauncherReviewDecision>, String> {
        if request.name.trim().is_empty() || request.icon_choices.is_empty() {
            return Err("invalid launcher review request".into());
        }
        let (reply, receiver) = oneshot::channel();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&request.review.handle) {
            return Err("duplicate live portal request".into());
        }
        inner.order.push_back(request.review.handle.clone());
        inner.pending.insert(
            request.review.handle.clone(),
            PendingDialog::LauncherReview { request, reply },
        );
        Ok(receiver)
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
        let (reply, receiver) = oneshot::channel();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&request.review.handle) {
            return Err("duplicate live portal request".into());
        }
        inner.order.push_back(request.review.handle.clone());
        let selected = vec![true; request.devices.len()];
        inner.pending.insert(
            request.review.handle.clone(),
            PendingDialog::UsbDevices {
                request,
                selected,
                reply,
            },
        );
        Ok(receiver)
    }

    /// Queue the trusted shell editor used by the GlobalShortcuts portal.
    /// It captures physical keys from the compositor event loop; requesting
    /// apps only receive the eventual, reviewed selection.
    pub fn enqueue_shortcut_bindings(
        &self,
        request: ShortcutBindingRequest,
    ) -> Result<oneshot::Receiver<ShortcutBindingDecision>, String> {
        let (reply, receiver) = oneshot::channel();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "portal dialog controller is unavailable".to_string())?;
        if inner.pending.contains_key(&request.handle) {
            return Err("duplicate live portal request".into());
        }
        inner.order.push_back(request.handle.clone());
        inner.pending.insert(
            request.handle.clone(),
            PendingDialog::ShortcutBindings { request, reply },
        );
        Ok(receiver)
    }

    fn next_handle(&self) -> Option<String> {
        let mut inner = self.inner.lock().ok()?;
        while let Some(handle) = inner.order.pop_front() {
            if inner.pending.contains_key(&handle) {
                return Some(handle);
            }
        }
        None
    }

    fn view(&self, handle: &str) -> Option<DialogView> {
        let inner = self.inner.lock().ok()?;
        match inner.pending.get(handle)? {
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
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::AppChoice { reply, .. }) => {
                let result = choice.map_or(PortalResult::Cancelled, |choice| {
                    PortalResult::Success(ApplicationChoice {
                        choice,
                        activation_token: None,
                    })
                });
                let _ = reply.send(result);
                true
            }
            Some(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
            None => false,
        }
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
            Some(PendingDialog::AppChoice { reply, .. }) => {
                let _ = reply.send(PortalResult::Cancelled);
                true
            }
            Some(PendingDialog::Review { reply, .. }) => {
                let _ = reply.send(ReviewDecision::Cancelled);
                true
            }
            Some(PendingDialog::ShortcutBindings { .. }) => true,
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
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::Review { reply, .. }) => {
                let _ = reply.send(decision);
                true
            }
            Some(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
            None => false,
        }
    }

    fn finish_shortcut_bindings(&self, handle: &str) -> bool {
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::ShortcutBindings { request, reply }) => {
                let _ = reply.send(ShortcutBindingDecision { rows: request.rows });
                true
            }
            Some(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
            None => false,
        }
    }

    fn finish_launcher_review(
        &self,
        handle: &str,
        decision: ReviewDecision,
        name: String,
        icon_choice: usize,
    ) -> bool {
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::LauncherReview { request, reply }) => {
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
                true
            }
            Some(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
            None => false,
        }
    }

    fn finish_usb_devices(&self, handle: &str, decision: ReviewDecision) -> bool {
        let pending = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending.remove(handle));
        match pending {
            Some(PendingDialog::UsbDevices {
                request,
                selected,
                reply,
            }) => {
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
                true
            }
            Some(other) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.pending.insert(handle.into(), other);
                }
                false
            }
            None => false,
        }
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
            request.rows[selected].status =
                Some("Hold Ctrl, Alt, Shift, or Super, then press a key.".into());
            return true;
        };
        if crate::portal::global_shortcuts::is_reserved_accelerator(&accelerator) {
            request.rows[selected].status =
                Some("Reserved by Tilo. Choose another shortcut.".into());
            return true;
        }
        if request.rows.iter().enumerate().any(|(index, other)| {
            index != selected && other.accelerator.as_deref() == Some(accelerator.as_str())
        }) {
            request.rows[selected].status =
                Some("Already assigned in this request. Choose another shortcut.".into());
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
            Self::AppChoice { handle, .. }
            | Self::Review { handle, .. }
            | Self::ShortcutBindings { handle, .. }
            | Self::LauncherReview { handle, .. }
            | Self::UsbDevices { handle, .. } => handle,
        }
    }
    pub fn title(&self) -> String {
        match self {
            Self::AppChoice { .. } => "Choose an application".into(),
            Self::Review { title, .. } => title.clone(),
            Self::ShortcutBindings { .. } => "Set global shortcuts".into(),
            Self::LauncherReview { title, .. } => title.clone(),
            Self::UsbDevices { title, .. } => title.clone(),
        }
    }
    pub fn app_name(&self) -> &str {
        match self {
            Self::AppChoice { app_name, .. }
            | Self::Review { app_name, .. }
            | Self::ShortcutBindings { app_name, .. }
            | Self::LauncherReview { app_name, .. }
            | Self::UsbDevices { app_name, .. } => app_name,
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Self::AppChoice { detail, .. } | Self::Review { detail, .. } => detail,
            Self::ShortcutBindings { .. } => {
                "Select a row, then press the shortcut. Tilo and active app shortcuts cannot be claimed. Backspace clears a row."
            }
            Self::LauncherReview { detail, .. } => detail,
            Self::UsbDevices { detail, .. } => detail,
        }
    }
    pub fn choices(&self) -> &[String] {
        match self {
            Self::AppChoice { choices, .. } => choices,
            Self::Review { .. }
            | Self::ShortcutBindings { .. }
            | Self::LauncherReview { .. }
            | Self::UsbDevices { .. } => &[],
        }
    }
    pub fn shortcut_rows(&self) -> &[ShortcutBindingRow] {
        match self {
            Self::ShortcutBindings { rows, .. } => rows,
            Self::AppChoice { .. }
            | Self::Review { .. }
            | Self::LauncherReview { .. }
            | Self::UsbDevices { .. } => &[],
        }
    }
    pub fn selectable_count(&self) -> usize {
        match self {
            Self::ShortcutBindings { rows, .. } => rows.len(),
            Self::AppChoice { choices, .. } => choices.len(),
            Self::UsbDevices { devices, .. } => devices.len(),
            Self::Review { .. } | Self::LauncherReview { .. } => 0,
        }
    }
    pub fn confirm_label(&self) -> &str {
        match self {
            Self::AppChoice { .. } => "Open",
            Self::Review { confirm_label, .. } => confirm_label,
            Self::ShortcutBindings { .. } => "Save shortcuts",
            Self::LauncherReview { confirm_label, .. } => confirm_label,
            Self::UsbDevices { confirm_label, .. } => confirm_label,
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
}

pub struct PortalDialogController {
    queue: PortalDialogQueue,
    active: Option<String>,
    selected: usize,
    launcher_name: String,
    launcher_icon: usize,
    launcher_field: usize,
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
            selected: 0,
            launcher_name: String::new(),
            launcher_icon: 0,
            launcher_field: 0,
        }
    }
    pub fn pump(&mut self) -> bool {
        if self.active.is_none() {
            self.active = self.queue.next_handle();
            self.selected = 0;
            self.launcher_icon = 0;
            self.launcher_field = 0;
            self.launcher_name = self
                .view()
                .and_then(|view| view.launcher_editor().map(|editor| editor.name.to_owned()))
                .unwrap_or_default();
            self.active.is_some()
        } else {
            false
        }
    }
    pub fn view(&self) -> Option<DialogView> {
        self.active
            .as_deref()
            .and_then(|handle| self.queue.view(handle))
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn select_next(&mut self, reverse: bool) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
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
    pub fn confirm(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        let done = match &view {
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
        };
        if done {
            self.active = None;
        }
        done
    }
    pub fn layout(&self, bounds: Rect) -> Option<DialogLayout> {
        let view = self.view()?;
        let width = bounds.width().min(560.0);
        let row_count = view.selectable_count().min(6);
        let launcher_editor = view.launcher_editor();
        let launcher_rows = launcher_editor.map_or(0, |editor| {
            usize::from(editor.editable_name) + usize::from(editor.editable_icon)
        });
        let height = 190.0 + small_count_as_f64(row_count + launcher_rows) * 48.0;
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
                let y = sheet.y0 + 118.0 + small_count_as_f64(i) * 48.0;
                Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
            })
            .collect();
        let launcher_name = launcher_editor
            .filter(|editor| editor.editable_name)
            .map(|_| {
                let y = sheet.y0 + 118.0 + small_count_as_f64(row_count) * 48.0;
                Rect::new(sheet.x0 + 24.0, y, sheet.x1 - 24.0, y + row_h)
            });
        let launcher_icons = launcher_editor
            .filter(|editor| editor.editable_icon)
            .map(|editor| {
                let y = sheet.y0
                    + 118.0
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
        Some(DialogLayout {
            sheet,
            choices,
            cancel,
            confirm,
            launcher_name,
            launcher_icons,
        })
    }
    pub fn click(&mut self, point: vello::kurbo::Point, layout: &DialogLayout) -> bool {
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
                if let Some((index, _)) = self.launcher_name.char_indices().next_back() {
                    self.launcher_name.truncate(index);
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

fn chooser_detail(request: &ApplicationChoiceRequest) -> String {
    let subject = request
        .filename
        .as_deref()
        .or(request.uri.as_deref())
        .or(request.content_type.as_deref())
        .unwrap_or("this item");
    format!(
        "{} wants to open {}",
        friendly_app_name(&request.app_id),
        subject
    )
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
    #[test]
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
    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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
            &PhysicalKey::Code(KeyCode::KeyK),
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
}
