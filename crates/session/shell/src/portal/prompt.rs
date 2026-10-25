//! One queue for every request the compositor puts to the person.
//!
//! A capability prompt, the file chooser, a review of what an app is about
//! to do, a password for polkit or for a network, a shortcut step, a grant
//! to use another app's action — they were built at different times and
//! reached the screen through separate pipelines, each with its own outcome
//! type and its own rule for what happens when the asker gives up. To the
//! person they are one thing: a sheet the compositor draws, which only a
//! human at this machine answers.
//!
//! So they share one queue, [`PromptQueue`], carrying one request type,
//! [`PromptRequest`]. The queue decides **order and nothing else**: it never
//! looks inside an ask, never answers one itself, and hands each ask on by
//! value, so the reply channel travels with the question.
//!
//! What an answer *means* stays where it was. A capability answer is
//! recorded by the grant store, a portal answer returns to its D-Bus
//! caller; each keeps its own tests and its own code. This module is the
//! seam every interface crosses — a queue, not a policy.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::file_picker_spawner::FilePickerRequest;
use crate::portal::desktop_services::{ApplicationChoice, ApplicationChoiceRequest, PortalResult};
use crate::shell::app_action_consent::{AppActionConsent, AppActionConsentRequest};
use crate::shell::portal_dialog::{
    AuthenticationDecision, AuthenticationRequest, DriveConsentDecision, DriveConsentRequest,
    LauncherReviewDecision, LauncherReviewRequest, NetworkSecretsDecision, NetworkSecretsRequest,
    ReviewDecision, ReviewRequest, ShortcutBindingDecision, ShortcutBindingRequest,
    ShortcutStepDecision, ShortcutStepRequest, UsbDeviceDecision, UsbDeviceRequest,
};

/// One request for the person, waiting in [`PromptQueue`].
///
/// Each variant pairs the request its interface already builds with the
/// channel that answer leaves on. The queue moves these values; it does not
/// re-describe them, and it does not decide anything about them.
pub enum PromptRequest {
    /// An app asks for a capability: microphone, capture, files, network.
    Capability { grant: super::PendingGrant },
    /// A file chooser, save dialog or folder picker.
    Picker { request: FilePickerRequest },
    /// A review of what an app is about to do, before it does it.
    Review {
        request: ReviewRequest,
        reply: oneshot::Sender<ReviewDecision>,
    },
    /// Which application should open something.
    AppChoice {
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    },
    /// The physical keys a shortcut binds to.
    ShortcutBindings {
        request: ShortcutBindingRequest,
        reply: oneshot::Sender<ShortcutBindingDecision>,
    },
    /// A step of a running shortcut that needs the person. Answered through
    /// a callback, because the run hands the answer straight back to
    /// whoever executes it.
    ShortcutStep {
        request: ShortcutStepRequest,
        reply: Box<dyn FnOnce(ShortcutStepDecision) + Send>,
    },
    /// A launcher entry an app wants to create.
    LauncherReview {
        request: LauncherReviewRequest,
        reply: oneshot::Sender<LauncherReviewDecision>,
    },
    /// Which of the connected devices an app may reach.
    UsbDevices {
        request: UsbDeviceRequest,
        reply: oneshot::Sender<UsbDeviceDecision>,
    },
    /// An administrator's credentials, for polkit.
    Authentication {
        request: Arc<AuthenticationRequest>,
        /// Which attempt this is: a retry after a wrong password replaces
        /// the sheet, and an answer to the replaced one must not count.
        generation: uuid::Uuid,
        /// The account that may authorise, as the agent heard it.
        owner_uid: libc::uid_t,
        reply: oneshot::Sender<AuthenticationDecision>,
    },
    /// Secrets `NetworkManager` needs to connect: a passphrase, a SIM's PIN.
    NetworkSecrets {
        request: NetworkSecretsRequest,
        reply: oneshot::Sender<NetworkSecretsDecision>,
    },
    /// Whether one application may use another's action.
    ActionGrant {
        request: AppActionConsentRequest,
        reply: oneshot::Sender<AppActionConsent>,
    },
    /// Whether a drive is opened, and how.
    DriveConsent {
        request: DriveConsentRequest,
        reply: oneshot::Sender<DriveConsentDecision>,
    },
}

impl PromptRequest {
    /// The kind of ask, for the state dump and for diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Capability { .. } => "capability",
            Self::Picker { .. } => "picker",
            Self::Review { .. } => "review",
            Self::AppChoice { .. } => "app-choice",
            Self::ShortcutBindings { .. } => "shortcut-bindings",
            Self::ShortcutStep { .. } => "shortcut-step",
            Self::LauncherReview { .. } => "launcher-review",
            Self::UsbDevices { .. } => "usb-devices",
            Self::Authentication { .. } => "authentication",
            Self::NetworkSecrets { .. } => "network-secrets",
            Self::ActionGrant { .. } => "action-grant",
            Self::DriveConsent { .. } => "drive-consent",
        }
    }

    /// The handle the asker knows this ask by, when it has one: the portal
    /// request it came from, the sheet the agent opened, the action the app
    /// was refused. Empty for asks nobody names.
    pub fn handle(&self) -> &str {
        match self {
            Self::Capability { grant } => grant.request_key().unwrap_or_default(),
            Self::Picker { .. } => "",
            Self::Review { request, .. } => &request.handle,
            Self::AppChoice { request, .. } => &request.handle,
            Self::ShortcutBindings { request, .. } => &request.handle,
            Self::ShortcutStep { request, .. } => &request.handle,
            Self::LauncherReview { request, .. } => &request.review.handle,
            Self::UsbDevices { request, .. } => &request.review.handle,
            Self::Authentication { request, .. } => &request.handle,
            Self::NetworkSecrets { request, .. } => &request.handle,
            Self::ActionGrant { request, .. } => &request.handle,
            Self::DriveConsent { request, .. } => &request.handle,
        }
    }

    /// Whether whoever asked has stopped waiting. A withdrawn ask leaves the
    /// queue without ever being shown, and the asker reads that as
    /// cancelled.
    pub fn is_withdrawn(&self) -> bool {
        match self {
            Self::Capability { grant } => grant.reply.is_closed(),
            Self::Picker { request } => request.reply.is_closed(),
            Self::Review { reply, .. } => reply.is_closed(),
            Self::AppChoice { reply, .. } => reply.is_closed(),
            Self::ShortcutBindings { reply, .. } => reply.is_closed(),
            // A shortcut step is answered through a callback that runs once
            // and is never withdrawn.
            Self::ShortcutStep { .. } => false,
            Self::LauncherReview { reply, .. } => reply.is_closed(),
            Self::UsbDevices { reply, .. } => reply.is_closed(),
            Self::Authentication { reply, .. } => reply.is_closed(),
            Self::NetworkSecrets { reply, .. } => reply.is_closed(),
            Self::ActionGrant { reply, .. } => reply.is_closed(),
            Self::DriveConsent { reply, .. } => reply.is_closed(),
        }
    }
}

impl std::fmt::Debug for PromptRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What is being asked is what a diagnostic may print; a reply channel
        // is not.
        write!(f, "{}(\"{}\")", self.name(), self.handle())
    }
}

/// Every request for the person, in the order they were raised.
///
/// Held by the compositor and drained once per frame, so a sheet raised on a
/// D-Bus worker thread and one raised on the frame thread itself queue in the
/// same line. Cloning shares the queue: one per session, never one per
/// interface.
#[derive(Clone, Default)]
pub struct PromptQueue {
    pending: Arc<Mutex<VecDeque<PromptRequest>>>,
}

impl PromptQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put `request` behind everything already waiting, and wake the loop
    /// that draws it. `false` when the queue is poisoned: the compositor is
    /// gone, and dropping the request is the answer.
    pub fn push(&self, request: PromptRequest) -> bool {
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.push_back(request);
                crate::shell::wake_compositor();
                true
            }
            Err(_) => false,
        }
    }

    /// Take everything raised so far, oldest first. Each request still owns
    /// its reply channel: whoever drains answers it.
    pub fn drain(&self) -> Vec<PromptRequest> {
        match self.pending.lock() {
            Ok(mut pending) => pending.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Take the file choosers waiting, in the order they were raised, and
    /// leave every other ask where it is. The chooser is drained by the
    /// overlay that draws it ([`crate::shell::file_picker_overlay`]), while
    /// the other sheets are drained by the sheet controller; one queue, read
    /// by the surface each kind belongs to.
    pub fn take_pickers(&self) -> Vec<super::file_picker_spawner::FilePickerRequest> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        let mut pickers = Vec::new();
        let mut rest = VecDeque::with_capacity(pending.len());
        for request in pending.drain(..) {
            match request {
                PromptRequest::Picker { request } => pickers.push(request),
                other => rest.push_back(other),
            }
        }
        *pending = rest;
        pickers
    }

    /// How many file choosers are waiting. The chooser service answers
    /// "how many of mine are queued" without taking anything.
    pub fn pickers_len(&self) -> usize {
        self.pending.lock().map_or(0, |pending| {
            pending
                .iter()
                .filter(|request| matches!(request, PromptRequest::Picker { .. }))
                .count()
        })
    }

    /// The oldest request still waiting, as a diagnostic prints it.
    pub fn front(&self) -> Option<String> {
        let pending = self.pending.lock().ok()?;
        pending.front().map(|request| format!("{request:?}"))
    }

    /// How many requests are waiting. For the state dump and for tests.
    pub fn len(&self) -> usize {
        self.pending.lock().map_or(0, |pending| pending.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{Capability, PendingGrant, PortalCaller, PromptOutcome};
    use crate::shell::state::WorkspaceId;

    fn capability_request() -> (PromptRequest, oneshot::Receiver<PromptOutcome>) {
        let (reply, receiver) = oneshot::channel();
        let grant = PendingGrant::for_test(
            PortalCaller::new(WorkspaceId::from(1), "org.tilo.notes"),
            Capability::Microphone {
                scope: crate::portal::GRANT_ONCE,
            },
            reply,
        );
        (PromptRequest::Capability { grant }, receiver)
    }

    /// Requests leave the queue in the order they were raised: the person
    /// answers the sheet raised first.
    #[test]
    fn the_queue_hands_requests_on_oldest_first() {
        let queue = PromptQueue::new();
        let (first, _first_reply) = capability_request();
        let (second, _second_reply) = capability_request();
        assert!(queue.push(first));
        assert!(queue.push(second));
        assert_eq!(queue.len(), 2);

        let drained = queue.drain();
        assert_eq!(
            drained.iter().map(PromptRequest::name).collect::<Vec<_>>(),
            vec!["capability", "capability"]
        );
        assert!(queue.is_empty(), "draining leaves nothing behind");
    }

    /// An asker that stops waiting is withdrawn: the sheet is never drawn.
    #[test]
    fn a_request_whose_asker_left_is_withdrawn() {
        let (request, receiver) = capability_request();
        assert!(!request.is_withdrawn());
        drop(receiver);
        assert!(request.is_withdrawn());
    }

    /// The queue is one line shared by every thread that asks, not one line
    /// per interface.
    #[test]
    fn a_clone_speaks_for_the_same_queue() {
        let queue = PromptQueue::new();
        let (request, _reply) = capability_request();
        assert!(queue.clone().push(request));
        assert_eq!(queue.len(), 1);
    }

    fn picker_request(
        title: &str,
    ) -> (
        super::super::file_picker_spawner::FilePickerRequest,
        oneshot::Receiver<Option<Vec<std::path::PathBuf>>>,
    ) {
        let (reply, answer) = oneshot::channel();
        (
            super::super::file_picker_spawner::FilePickerRequest {
                caller: PortalCaller::new(WorkspaceId::from(1), "org.tilo.notes"),
                parent_window: None,
                title: title.to_owned(),
                save: false,
                directory: false,
                writable_directory: false,
                multiple: false,
                accept_label: None,
                current_name: None,
                filters: Vec::new(),
                scope: None,
                recent: Vec::new(),
                reply,
            },
            answer,
        )
    }

    /// A chooser and a capability prompt raised back to back keep the order
    /// they were raised in, whichever surface reads them: one queue, not two
    /// that could interleave.
    #[test]
    fn a_chooser_and_a_capability_keep_one_order() {
        let queue = PromptQueue::new();
        let (capability, _capability_reply) = capability_request();
        assert!(queue.push(capability));
        let (picker, _picker_reply) = picker_request("Open a document");
        assert!(queue.push(PromptRequest::Picker { request: picker }));

        // The chooser's surface reads only its own kind…
        let pickers = queue.take_pickers();
        assert_eq!(pickers.len(), 1);
        assert_eq!(pickers[0].title, "Open a document");
        // …and the ask that was raised first is still in front of the rest.
        assert_eq!(
            queue.front().as_deref(),
            Some("capability(\"\")"),
            "the sheet raised first is the one the person answers first"
        );
    }

    /// The chooser service's own handle raises into the same queue the
    /// compositor reads, rather than a line only the chooser knows about.
    #[test]
    fn the_chooser_raises_into_the_session_queue() {
        use super::super::FilePickerQueue;
        use super::super::file_picker_spawner::FilePickerSpawner as _;

        let queue = PromptQueue::new();
        let choosers = FilePickerQueue::for_prompts(queue.clone());
        let (picker, _answer) = picker_request("Save a note");
        choosers.spawn(picker);
        assert_eq!(choosers.len(), 1);

        let (capability, _capability_reply) = capability_request();
        assert!(queue.push(capability));
        assert_eq!(
            queue.len(),
            2,
            "the chooser lives in the same queue as everything else, not one of its own"
        );
        // The queue the compositor holds is the very one the chooser wrote
        // into, and it holds both asks in the order they were raised.
        assert_eq!(
            choosers.drain().len(),
            1,
            "the chooser's own door reads back what it raised"
        );
        assert_eq!(
            queue.front().as_deref(),
            Some("capability(\"\")"),
            "the capability was raised first, so it is answered first"
        );
    }
}
