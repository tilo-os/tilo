//! Shared private feedback presentation. Desktop hosts lend the system-owned
//! controller into a private component; no draft is a workspace record.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::feedback::{Composer, Consent, DeliveryState};
use crate::i18n::EmbeddedCatalog;
use crate::prelude::*;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("feedback_composer.ftl"))],
    "feedback-title",
);

/// Supplied by the desktop host, never resolved from a URL or draft content.
#[derive(Clone)]
pub struct FeedbackServices {
    controller: Rc<RefCell<Composer>>,
    presentation: Rc<RefCell<Option<LocalState<String>>>>,
    mounted: Rc<Cell<bool>>,
}

impl FeedbackServices {
    /// Private outbox writes need a wake independently of the ephemeral draft.
    /// Transport retries remain explicit; only pending local persistence polls.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        let controller = self.controller.try_borrow().ok()?;
        match controller.state().ok()? {
            Some(DeliveryState::Saving) => {
                crate::time::now().checked_add(crate::time::Duration::from_millis(100))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn new(controller: Rc<RefCell<Composer>>) -> Self {
        Self {
            controller,
            presentation: Rc::default(),
            mounted: Rc::default(),
        }
    }

    /// Pump after a host storage/transport wake, publishing progress to the
    /// current private presentation. Its `LocalState` retains the component's
    /// retirement scope, so a late completion cannot wake a closed component.
    ///
    /// # Errors
    /// Reports a busy controller or the actual storage/transport failure.
    pub fn pump(&self) -> Result<(), crate::feedback::Error> {
        let result = self
            .controller
            .try_borrow_mut()
            .map_err(|_| crate::feedback::Error::Invalid("composer busy"))
            .and_then(|mut controller| controller.pump());
        if let Ok(presentation) = self.presentation.try_borrow()
            && let Some(status) = presentation.as_ref()
        {
            if let Err(error) = &result {
                status.set(error_label(error).to_owned());
            } else {
                restore_progress(self, status);
            }
        }
        result
    }
}

/// The host retains the outbox, while exactly one private presentation owns
/// its raw review. Retirement drops this lease with the actual effect owner.
struct PresentationLease(FeedbackServices);

impl Drop for PresentationLease {
    fn drop(&mut self) {
        self.0.presentation.borrow_mut().take();
        self.0.mounted.set(false);
        if let Ok(mut controller) = self.0.controller.try_borrow_mut() {
            controller.abandon_review();
        }
    }
}

/// Ephemeral CRDT state, admitted only into a private component host.
#[derive(Clone, Debug, Default, PartialEq, Model)]
pub struct FeedbackDraft {
    pub issue: String,
    pub include_context: bool,
    pub include_screenshot: bool,
}

#[resource(kind = "tilo.feedback-draft", label = "Feedback")]
impl FeedbackDraft {
    /// Describe the issue.
    #[action]
    pub fn describe(&mut self, issue: String) -> Result<(), &'static str> {
        if issue.len() > 8_000 {
            return Err("feedback-issue-too-long");
        }
        self.issue = issue;
        Ok(())
    }

    /// Explicitly include or exclude contextual information.
    #[action]
    pub fn context(&mut self, include: bool) {
        self.include_context = include;
    }

    /// Explicitly request or exclude a single masked app screenshot.
    #[action]
    pub fn screenshot(&mut self, include: bool) {
        self.include_screenshot = include;
    }
}

#[derive(Clone, Debug, Effect)]
#[effect(operation = "feedback.compose", device_only)]
pub enum FeedbackEffect {
    /// Inspect exactly what will leave this device.
    Review,
    /// Approve the displayed review and durably queue it.
    Submit,
    /// Retry the immutable report and read its durable delivery status.
    Retry,
    /// Hide a span in the already-masked review.
    Redact {
        #[param("Reviewed field key")]
        key: String,
        #[param("UTF-8 byte start")]
        start: usize,
        #[param("UTF-8 byte end")]
        end: usize,
    },
    /// Remove the screenshot from the review.
    RemoveScreenshot,
}

/// Mounted inside desktop chrome, never as an overlapping application window.
pub struct FeedbackComposer;

/// The shared composer definition, instantiated by the SDK once.
/// Hosts use this entry point to avoid another copy of its generic resource,
/// history and effect machinery in each host crate.
#[must_use]
#[inline(never)]
pub fn definition() -> &'static crate::application::ApplicationDefinition {
    crate::framework::definition::<FeedbackComposer>()
}

/// Prepare the chrome component over an isolated, ephemeral store. The host's
/// durable outbox is lent separately; raw drafts cannot reach profile records,
/// history or room synchronization through this preparation path.
///
/// # Errors
/// Reports invalid host identity or component admission failure.
pub fn prepare(
    services: Option<FeedbackServices>,
    device: &str,
    author: &str,
    instance: u64,
) -> Result<AppComponent, crate::launch::LaunchError> {
    let definition = definition();
    let store = Rc::new(crate::feedback::MemoryStore::new());
    let room = "feedback-private-draft";
    let records = crate::record::RecordOwners::with_store(store.clone())
        .open(FeedbackComposer::ID, room)
        .map_err(|error| crate::launch::LaunchError::Operation(error.to_string()))?;
    let views = Rc::new(
        InstanceViewStore::with_store(store, device, room)
            .map_err(|error| crate::launch::LaunchError::Operation(error.to_string()))?,
    );
    let target = LaunchTarget::New;
    let permissions = Rc::new(RefCell::new(
        crate::permissions::PermissionContext::from_capabilities(
            crate::permissions::Actor::person(Some(crate::permissions::Role::Owner), author),
            vec![],
        ),
    ));
    let mut context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: device.to_owned(),
            instance_id: instance,
            permissions,
            author: crate::permissions::UserId(author.to_owned()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )?;
    context.provide_private();
    if let Some(services) = services {
        if services.mounted.replace(true) {
            return Err(crate::launch::LaunchError::Operation(
                "feedback-busy".into(),
            ));
        }
        context.lend(PresentationLease(services.clone()));
        context.lend(services);
    }
    definition.instantiate(context, target, RestoreInput::None)
}

impl ResourceApp for FeedbackComposer {
    type Resource = FeedbackDraft;
    const ID: &'static str = "so.tilo.feedback-composer";
    const TITLE: &'static str = "Feedback";
    const DOCUMENT_HISTORY: bool = false;
    const PRIVATE_WINDOWS: bool = true;
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        capture_protected: true,
        ..ComponentBehaviour::DEFAULT
    };

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<FeedbackDraft>) -> Element {
        let shown = LocalState::new(String::new());
        let fields = LocalState::new(Vec::<crate::feedback::ContextField>::new());
        let pixels = LocalState::new(None::<Vec<u8>>);
        let status = LocalState::new("feedback-review-first".to_owned());
        install_effects(cx, &shown, &status, &fields, &pixels);
        let fields_view = cx.switch(&fields, |part, fields| {
            let mut controls = Column::new();
            for field in fields {
                controls = controls.push(Box::new(
                    Text::new("feedback-reviewed-text").with_arg("value", field.value.clone()),
                ));
                controls = controls.push(Box::new(part.effect_button(
                    "feedback-hide-context",
                    &FeedbackEffect::Redact {
                        key: field.key.clone(),
                        start: 0,
                        end: field.value.len(),
                    },
                )));
            }
            Box::new(controls)
        });
        let preview = cx.switch(&shown, |_, shown| {
            Box::new(
                Text::new("feedback-reviewed-text")
                    .with_arg("value", shown.clone())
                    .with_id("feedback-preview"),
            )
        });
        let picture = cx.switch(&pixels, |_, png| {
            png.as_ref()
                .and_then(|bytes| crate::widget::Image::from_encoded(bytes).ok())
                .map_or_else(
                    || Box::new(Column::new()) as Element,
                    |image| Box::new(image.with_alt("feedback-screenshot-preview")) as Element,
                )
        });
        Box::new(
            crate::column![
                Text::new("feedback-title"),
                cx.input(|draft| draft.issue.clone(), FeedbackDraft::DESCRIBE)
                    .with_id("feedback-issue")
                    .with_placeholder("feedback-describe")
                    .multiline(true),
                cx.checkbox(
                    "feedback-context",
                    |draft| draft.include_context,
                    FeedbackDraft::CONTEXT
                ),
                cx.checkbox(
                    "feedback-screenshot",
                    |draft| draft.include_screenshot,
                    FeedbackDraft::SCREENSHOT
                ),
                Text::new("feedback-privacy"),
                cx.effect_button("feedback-review", &FeedbackEffect::Review),
            ]
            .push(preview)
            .push(fields_view)
            .push(picture)
            .push(Box::new(crate::column![
                cx.effect_button(
                    "feedback-remove-screenshot",
                    &FeedbackEffect::RemoveScreenshot
                ),
                cx.effect_button("feedback-submit", &FeedbackEffect::Submit),
                Text::from_state(&status, Clone::clone)
                    .with_id("feedback-status")
                    .with_live(crate::a11y::Politeness::Polite),
                cx.effect_button("feedback-retry", &FeedbackEffect::Retry),
            ])),
        )
    }
}

fn install_effects(
    cx: &ViewCx<FeedbackDraft>,
    shown: &LocalState<String>,
    status: &LocalState<String>,
    fields: &LocalState<Vec<crate::feedback::ContextField>>,
    pixels: &LocalState<Option<Vec<u8>>>,
) {
    // Refuse a host accidentally mounting the raw draft in a shared room.
    let services = cx
        .is_private()
        .then(|| cx.take_lent::<FeedbackServices>())
        .flatten();
    let presentation = cx.take_lent::<PresentationLease>();
    if let Some(services) = &services {
        *services.presentation.borrow_mut() = Some(status.clone());
        restore_progress(services, status);
    }
    let preview = shown.clone();
    let reviewed_fields = fields.clone();
    let reviewed_pixels = pixels.clone();
    let progress = status.clone();
    cx.effects(move |effect: FeedbackEffect, effect_cx| {
        let _presentation = &presentation;
        let Some(services) = &services else {
            progress.set("feedback-unconfigured".to_owned());
            return Err("feedback-unconfigured".to_owned());
        };
        let draft = effect_cx.live().with(Clone::clone);
        let consent = Consent {
            context: draft.include_context,
            screenshot: draft.include_screenshot,
        };
        let mut composer = services
            .controller
            .try_borrow_mut()
            .map_err(|_| "feedback-busy".to_owned())?;
        let result = match effect {
            FeedbackEffect::Review => composer.review(&draft.issue, consent),
            FeedbackEffect::Submit => {
                let id = effect_cx
                    .invocation()
                    .ok_or_else(|| "feedback-busy".to_owned())?
                    .0
                    .simple()
                    .to_string();
                composer.submit(id, &draft.issue, consent)
            }
            FeedbackEffect::Retry => composer.pump(),
            FeedbackEffect::Redact { key, start, end } => composer.hide(&key, start, end),
            FeedbackEffect::RemoveScreenshot => composer.remove_screenshot(),
        };
        if let Some(review) = composer.preview() {
            let feedback = review.feedback();
            preview.set(feedback.text.clone());
            reviewed_fields.set(feedback.context.clone());
            reviewed_pixels.set(review.screenshot().map(|image| image.png().to_vec()));
        } else {
            preview.set(String::new());
            reviewed_fields.set(Vec::new());
            reviewed_pixels.set(None);
        }
        let key = if let Err(error) = &result {
            error_label(error)
        } else {
            match composer
                .state()
                .map_err(|_| "feedback-retry-needed".to_owned())?
            {
                None => "feedback-reviewed",
                Some(state) => delivery_label(&state),
            }
        };
        progress.set(key.to_owned());
        result.map_err(|_| key.to_owned())?;
        Ok(Effected::done())
    });
}

fn error_label(error: &crate::feedback::Error) -> &'static str {
    match error {
        crate::feedback::Error::CaptureProtected => "feedback-capture-unavailable",
        crate::feedback::Error::CaptureChanged => "feedback-capture-changed",
        crate::feedback::Error::NotApproved => "feedback-review-first",
        crate::feedback::Error::Store(_) => "feedback-storage-failed",
        _ => "feedback-retry-needed",
    }
}

fn restore_progress(services: &FeedbackServices, status: &LocalState<String>) {
    match services.controller.try_borrow() {
        Ok(composer) => match composer.state() {
            Ok(Some(state)) => status.set(delivery_label(&state).to_owned()),
            Ok(None) => {}
            Err(_) => status.set("feedback-retry-needed".to_owned()),
        },
        Err(_) => status.set("feedback-busy".to_owned()),
    }
}

fn delivery_label(state: &DeliveryState) -> &'static str {
    match state {
        DeliveryState::Saving => "feedback-saving",
        DeliveryState::StorageFailed => "feedback-storage-failed",
        DeliveryState::AwaitingReceipt => "feedback-awaiting-receipt",
        DeliveryState::Sent { .. } => "feedback-sent",
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    reason = "assertions prove the production component journey"
)]
mod tests {
    use super::*;
    use crate::feedback::{
        Destination, Error, FeedbackHost, MaskedScreenshot, MemoryStore, Outbox, Receipt,
        ReviewText, StorageKey, Submission,
    };
    use crate::headless::FrameHarness;
    use serde_json::json;

    #[derive(Default)]
    struct Transport {
        submissions: Vec<Submission>,
        receipt: Option<Receipt>,
    }
    struct Host(Rc<RefCell<Transport>>);
    impl FeedbackHost for Host {
        fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
            Ok(vec![ReviewText {
                node: "focused.public.label".into(),
                sensitivity: crate::feedback::DeclaredSensitivity::Public,
                kind: None,
                origin: crate::feedback::TextOrigin::Semantic,
                text: "feedback-privacy".into(),
            }])
        }
        fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
            Err(Error::CaptureProtected)
        }
        fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
            Err(Error::CaptureProtected)
        }
        fn send(&mut self, submission: &Submission) -> Result<(), Error> {
            self.0.borrow_mut().submissions.push(submission.clone());
            Ok(())
        }
        fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
            Ok(self.0.borrow().receipt.clone())
        }
    }

    fn call(
        frame: &mut FrameHarness,
        action: &str,
        arguments: &serde_json::Value,
    ) -> Result<(), tilo_actions::DoorError> {
        frame.relayout();
        let resource = frame
            .resources()
            .into_iter()
            .find(|resource| resource.kind == FeedbackDraft::KIND)
            .expect("the real composer resource is published");
        frame
            .invoke_bound_action(
                &crate::bound_dispatch::BoundActionAddress {
                    instance_id: 42,
                    address: tilo_actions::ActionAddress::new(resource.id.0, action),
                },
                arguments.as_object().expect("typed arguments"),
            )
            .map(|_| ())
    }

    #[test]
    fn private_storage_wake_sends_only_after_host_acknowledges_approval() {
        let transport = Rc::new(RefCell::new(Transport::default()));
        let storage = Rc::new(MemoryStore::queued());
        let controller = Composer::new(
            Destination {
                room: "private-fixture".into(),
                provider: "record:chat:feedback".into(),
                channel: "feedback".into(),
            },
            Outbox::open(storage.clone(), StorageKey::package("feedback-outbox"))
                .expect("private outbox"),
            Box::new(Host(transport.clone())),
        )
        .expect("admitted destination");
        let services = FeedbackServices::new(Rc::new(RefCell::new(controller)));
        let component =
            prepare(Some(services.clone()), "device", "guest", 42).expect("private composer");
        let mut frame = FrameHarness::from_component(
            component,
            (720, 720),
            ComponentRunnerInputs::from_native_process(),
        )
        .expect("production component");
        call(
            &mut frame,
            "describe",
            &json!({"issue":"Opening the book failed"}),
        )
        .expect("describe");
        call(&mut frame, "review", &json!({})).expect("review");
        call(&mut frame, "submit", &json!({})).expect("queue approval");
        frame.relayout();
        assert!(
            frame
                .inspection_json()
                .expect("inspection")
                .contains("Saving on this device")
        );
        assert!(services.next_wake_deadline().is_some());
        services.pump().expect("storage still pending");
        assert!(transport.borrow().submissions.is_empty());
        let pending = storage.drain();
        assert_eq!(pending.len(), 1);
        storage
            .acknowledge(pending[0].revision, Ok(()))
            .expect("host commit");
        services.pump().expect("durable approval sends");
        frame.relayout();
        assert_eq!(transport.borrow().submissions.len(), 1);
        assert!(services.next_wake_deadline().is_none());
        assert!(
            frame
                .inspection_json()
                .expect("inspection")
                .contains("Waiting for destination confirmation")
        );
        assert!(
            !frame
                .inspection_json()
                .expect("inspection")
                .contains("Feedback received")
        );
    }

    struct PendingImageHost {
        transport: Rc<RefCell<Transport>>,
        ready: Rc<Cell<bool>>,
    }

    impl FeedbackHost for PendingImageHost {
        fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
            Ok(Vec::new())
        }
        fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
            crate::feedback::CapturePermit::requested("synthetic:focused-app".into(), 1)?.mask(
                crate::feedback::SurfaceCapture {
                    surface: "synthetic:focused-app".into(),
                    revision: 1,
                    width: 1,
                    height: 1,
                    rgba: vec![70, 80, 90, 255],
                    capture_protected: false,
                    sensitive_regions: Some(vec![crate::feedback::PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    }]),
                },
            )
        }
        fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
            Err(Error::NotDurable)
        }
        fn persist_screenshot_async(
            &mut self,
            image: MaskedScreenshot,
        ) -> crate::feedback::AttachmentFuture<'static> {
            let ready = Rc::clone(&self.ready);
            Box::pin(async move {
                std::future::poll_fn(move |_| {
                    if ready.get() {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                })
                .await;
                drop(image);
                Ok(())
            })
        }
        fn send(&mut self, submission: &Submission) -> Result<(), Error> {
            self.transport
                .borrow_mut()
                .submissions
                .push(submission.clone());
            Ok(())
        }
        fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
            Ok(None)
        }
    }

    #[test]
    fn semantic_image_approval_survives_closed_presentation_and_waits_for_both_writes() {
        let transport = Rc::new(RefCell::new(Transport::default()));
        let ready = Rc::new(Cell::new(false));
        let storage = Rc::new(MemoryStore::queued());
        let controller = Rc::new(RefCell::new(
            Composer::new(
                Destination {
                    room: "private-fixture".into(),
                    provider: "record:chat:feedback".into(),
                    channel: "feedback".into(),
                },
                Outbox::open(
                    storage.clone(),
                    StorageKey::package("feedback-image-outbox"),
                )
                .expect("outbox"),
                Box::new(PendingImageHost {
                    transport: Rc::clone(&transport),
                    ready: Rc::clone(&ready),
                }),
            )
            .expect("controller"),
        ));
        let services = FeedbackServices::new(Rc::clone(&controller));
        let component = prepare(Some(services.clone()), "device", "guest", 42).expect("composer");
        let mut frame = FrameHarness::from_component(
            component,
            (720, 720),
            ComponentRunnerInputs::from_native_process(),
        )
        .expect("production component");
        call(
            &mut frame,
            "describe",
            &json!({"issue":"Approved image report"}),
        )
        .expect("describe");
        call(&mut frame, "screenshot", &json!({"include":true})).expect("explicit image consent");
        call(&mut frame, "review", &json!({})).expect("review masked image");
        let reference = controller
            .borrow()
            .preview()
            .expect("review")
            .screenshot()
            .expect("image")
            .reference()
            .clone();
        call(&mut frame, "submit", &json!({})).expect("approve through resource action");
        assert!(services.next_wake_deadline().is_some());
        assert!(storage.drain().is_empty());
        assert!(transport.borrow().submissions.is_empty());
        frame.relayout();
        assert!(
            frame
                .inspection_json()
                .expect("inspection")
                .contains("Saving on this device")
        );
        frame.close_components();
        assert!(controller.borrow().preview().is_none());
        assert!(
            services.next_wake_deadline().is_some(),
            "image storage is owned independently of the closed presentation"
        );
        ready.set(true);
        services.pump().expect("image custody completed");
        assert!(
            transport.borrow().submissions.is_empty(),
            "outbox commit is still pending"
        );
        let pending = storage.drain();
        assert_eq!(pending.len(), 1);
        storage
            .acknowledge(pending[0].revision, Ok(()))
            .expect("outbox committed");
        services.pump().expect("both writes completed");
        let submissions = &transport.borrow().submissions;
        assert_eq!(submissions.len(), 1);
        assert_eq!(submissions[0].feedback.text, "Approved image report");
        assert_eq!(
            submissions[0].feedback.screenshot.as_ref(),
            Some(&reference)
        );
        assert_eq!(
            controller.borrow().state().expect("state"),
            Some(DeliveryState::AwaitingReceipt)
        );
        assert!(services.next_wake_deadline().is_none());
    }

    #[test]
    fn semantic_composer_reviews_queues_and_waits_for_destination_receipt() {
        let transport = Rc::new(RefCell::new(Transport::default()));
        let controller = Rc::new(RefCell::new(
            Composer::new(
                Destination {
                    room: "demo".into(),
                    provider: "record:chat:feedback".into(),
                    channel: "feedback".into(),
                },
                Outbox::open(
                    Rc::new(MemoryStore::new()),
                    StorageKey::package("feedback-outbox"),
                )
                .expect("private outbox"),
                Box::new(Host(transport.clone())),
            )
            .expect("admitted destination"),
        ));
        let services = FeedbackServices::new(controller.clone());
        let component =
            prepare(Some(services.clone()), "device", "guest", 42).expect("private composer");
        assert!(
            prepare(Some(services.clone()), "device", "guest", 43).is_err(),
            "one approved outbox must not have competing private editors"
        );
        let mut frame = FrameHarness::from_component(
            component,
            (720, 720),
            ComponentRunnerInputs::from_native_process(),
        )
        .expect("production component");
        assert!(transport.borrow().submissions.is_empty());
        call(
            &mut frame,
            "describe",
            &json!({"issue": "Opening the book failed; token=secret12345"}),
        )
        .expect("describe through public action");
        frame.relayout();
        assert!(
            frame.capture_protected(),
            "the actual private composer paint must protect its enclosing frame"
        );
        assert!(
            call(&mut frame, "submit", &json!({})).is_err(),
            "sending before review must fail"
        );
        call(&mut frame, "review", &json!({})).expect("review");
        call(&mut frame, "submit", &json!({})).expect("approve exact preview");
        let submission = transport
            .borrow()
            .submissions
            .first()
            .expect("existing transport request")
            .clone();
        assert!(!submission.feedback.text.contains("secret12345"));
        assert!(submission.feedback.context.is_empty());
        assert!(submission.feedback.screenshot.is_none());
        assert_eq!(
            controller.borrow().state().expect("state"),
            Some(DeliveryState::AwaitingReceipt)
        );
        call(&mut frame, "retry", &json!({})).expect("stable retry");
        assert!(
            transport
                .borrow()
                .submissions
                .iter()
                .all(|retry| retry == &submission)
        );
        transport.borrow_mut().receipt = Some(Receipt {
            id: submission.id.clone(),
            digest: submission.digest().expect("digest"),
            destination: submission.destination,
            report: "chat.message:m_feedback_test".into(),
        });
        services.pump().expect("host receipt wake");
        frame.relayout();
        assert!(
            serde_json::to_string(&frame.inspection())
                .expect("real component inspection")
                .contains("Feedback received"),
            "the private UI must show receipt progress without another click"
        );
        assert_eq!(
            controller.borrow().state().expect("state"),
            Some(DeliveryState::Sent {
                report: "chat.message:m_feedback_test".into()
            })
        );
        assert_literal_second_review(&mut frame, &transport);
        frame.close_components();
        assert!(
            controller.borrow().preview().is_none(),
            "closing discards the unapproved review"
        );
        let reopened = prepare(Some(services), "device", "guest", 42)
            .expect("closed editor releases its private lease");
        drop(reopened);
    }

    fn find(node: &crate::headless::InspectionNode) -> Option<&crate::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("feedback-preview") {
            Some(node)
        } else {
            node.children.iter().find_map(find)
        }
    }
    fn contains_literal_context(node: &crate::headless::InspectionNode) -> bool {
        node.semantics
            .as_ref()
            .and_then(|node| node.name.as_deref())
            == Some("feedback-privacy")
            || node.children.iter().any(contains_literal_context)
    }
    fn assert_literal_second_review(frame: &mut FrameHarness, transport: &Rc<RefCell<Transport>>) {
        transport.borrow_mut().receipt = None;
        call(frame, "describe", &json!({"issue": "feedback-title"})).expect("second issue");
        call(frame, "context", &json!({"include": true})).expect("explicit context opt-in");
        call(frame, "review", &json!({})).expect("new review after durable receipt");
        frame.relayout();
        let inspection = frame.inspection();
        let preview = find(&inspection.root).expect("displayed approved preview");
        assert_eq!(
            preview
                .semantics
                .as_ref()
                .and_then(|node| node.name.as_deref()),
            Some("feedback-title"),
            "user text matching a Fluent key must remain literal"
        );
        assert!(contains_literal_context(&inspection.root));
        call(frame, "submit", &json!({})).expect("second approved submission");
        let sent = transport.borrow();
        let first = sent.submissions.first().expect("first report");
        let second = sent.submissions.last().expect("second report");
        assert_ne!(first.id, second.id);
        assert_eq!(second.feedback.text, "feedback-title");
        assert_eq!(
            second
                .feedback
                .context
                .first()
                .expect("approved context")
                .value,
            "feedback-privacy"
        );
    }
}
