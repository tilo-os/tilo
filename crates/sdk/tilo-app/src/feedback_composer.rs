//! Shared private feedback presentation. Desktop hosts lend the system-owned
//! controller into a private component; no draft is a workspace record.

use std::{cell::RefCell, rc::Rc};

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
pub struct FeedbackServices(pub Rc<RefCell<Composer>>);

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
    let definition = crate::framework::definition::<FeedbackComposer>();
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
                controls = controls.push(Box::new(Text::new(&field.value)));
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
                Text::from_state(&shown, Clone::clone).with_id("feedback-preview"),
            ]
            .push(fields_view)
            .push(picture)
            .push(Box::new(crate::column![
                cx.effect_button(
                    "feedback-remove-screenshot",
                    &FeedbackEffect::RemoveScreenshot
                ),
                cx.effect_button("feedback-submit", &FeedbackEffect::Submit),
                Text::from_state(&status, Clone::clone).with_id("feedback-status"),
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
    let preview = shown.clone();
    let reviewed_fields = fields.clone();
    let reviewed_pixels = pixels.clone();
    let progress = status.clone();
    cx.effects(move |effect: FeedbackEffect, effect_cx| {
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
            .0
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
        let key = if result.is_err() {
            "feedback-retry-needed"
        } else {
            match composer
                .state()
                .map_err(|_| "feedback-retry-needed".to_owned())?
            {
                None => "feedback-reviewed",
                Some(DeliveryState::Saving) => "feedback-saving",
                Some(DeliveryState::StorageFailed) => "feedback-storage-failed",
                Some(DeliveryState::AwaitingReceipt) => "feedback-awaiting-receipt",
                Some(DeliveryState::Sent { .. }) => "feedback-sent",
            }
        };
        progress.set(key.to_owned());
        result.map_err(|_| key.to_owned())?;
        Ok(Effected::done())
    });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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
            Ok(Vec::new())
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
        let component = prepare(
            Some(FeedbackServices(controller.clone())),
            "device",
            "guest",
            42,
        )
        .expect("private composer");
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
        call(&mut frame, "retry", &json!({})).expect("confirm durable receipt");
        assert_eq!(
            controller.borrow().state().expect("state"),
            Some(DeliveryState::Sent {
                report: "chat.message:m_feedback_test".into()
            })
        );
    }
}
