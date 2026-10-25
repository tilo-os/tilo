//! Shared private feedback presentation. Desktop hosts lend the system-owned
//! controller into a private component; no draft is a workspace record.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::feedback::{Composer, Consent, DeliveryState, PickedReport, Subject};
use crate::i18n::EmbeddedCatalog;
use crate::prelude::*;

#[path = "feedback_composer_report.rs"]
mod report;

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", &[("en-US", include_str!("feedback_composer.ftl"))]);

/// Supplied by the desktop host, never resolved from a URL or draft content.
#[derive(Clone)]
pub struct FeedbackServices {
    controller: Rc<RefCell<Composer>>,
    presentation: Rc<RefCell<Option<Presentation>>>,
    mounted: Rc<Cell<bool>>,
    /// The model the host lends for reviewing reports, if any.
    models: Rc<RefCell<crate::models::Models>>,
    pass: report::Pass,
}

/// What the one mounted composer shows of the controller's state.
#[derive(Clone)]
struct Presentation {
    status: LocalState<String>,
    subject: LocalState<Option<Subject>>,
    shown: LocalState<String>,
    fields: LocalState<Vec<crate::feedback::ContextField>>,
    pixels: LocalState<Option<Vec<u8>>>,
    /// What each picked part is called, while reporting picks.
    picks: LocalState<Option<Vec<String>>>,
    review: LocalState<Option<report::ReviewShown>>,
    /// A note per picked part, by its place.
    notes: Rc<Vec<LocalState<String>>>,
}

impl Presentation {
    /// Show the controller's current review and subject: a review the
    /// controller discarded must not stay on screen as what would be sent.
    fn show(&self, composer: &Composer) {
        self.subject.set(composer.subject().cloned());
        self.picks.set(report::titles(composer));
        self.review.set(report::ReviewShown::of(composer));
        if let Some(review) = composer.preview() {
            let feedback = review.feedback();
            self.shown.set(feedback.text.clone());
            self.fields.set(feedback.context.clone());
            self.pixels
                .set(review.screenshot().map(|image| image.png().to_vec()));
        } else {
            self.shown.set(String::new());
            self.fields.set(Vec::new());
            self.pixels.set(None);
        }
    }
}

impl FeedbackServices {
    /// Private outbox writes need a wake independently of the ephemeral draft.
    /// Transport retries remain explicit; only pending local persistence polls.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        if self.pass.try_borrow().is_ok_and(|pass| pass.is_some()) {
            return crate::time::now().checked_add(crate::time::Duration::from_millis(50));
        }
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
            models: Rc::default(),
            pass: Rc::default(),
        }
    }

    /// Lend the model that looks over reports before they are sent, as the
    /// host's settings chose it.
    pub fn set_models(&self, models: crate::models::Models) {
        *self.models.borrow_mut() = models;
    }

    /// Start a report about what the person picked; the composer opens on
    /// it.
    ///
    /// # Errors
    /// Refuses while the controller is busy in an effect.
    pub fn begin_report(&self, picked: PickedReport) -> Result<(), crate::feedback::Error> {
        let mut controller = self
            .controller
            .try_borrow_mut()
            .map_err(|_| crate::feedback::Error::Invalid("composer busy"))?;
        controller.begin_report(picked);
        self.pass.borrow_mut().take();
        if let Ok(presentation) = self.presentation.try_borrow()
            && let Some(presentation) = presentation.as_ref()
        {
            for note in presentation.notes.iter() {
                note.set(String::new());
            }
            presentation.show(&controller);
        }
        Ok(())
    }

    /// Name the window the next report is about, as the desktop chose it
    /// when the person asked for Feedback there.
    ///
    /// # Errors
    /// Refuses while the controller is busy in an effect.
    pub fn about(&self, subject: Option<Subject>) -> Result<(), crate::feedback::Error> {
        let mut controller = self
            .controller
            .try_borrow_mut()
            .map_err(|_| crate::feedback::Error::Invalid("composer busy"))?;
        controller.about(subject);
        if let Ok(presentation) = self.presentation.try_borrow()
            && let Some(presentation) = presentation.as_ref()
        {
            presentation.show(&controller);
        }
        Ok(())
    }

    /// Pump after a host storage/transport wake, publishing progress to the
    /// current private presentation. Its `LocalState` retains the component's
    /// retirement scope, so a late completion cannot wake a closed component.
    ///
    /// # Errors
    /// Reports a busy controller or the actual storage/transport failure.
    pub fn pump(&self) -> Result<(), crate::feedback::Error> {
        self.poll_model();
        let result = self
            .controller
            .try_borrow_mut()
            .map_err(|_| crate::feedback::Error::Invalid("composer busy"))
            .and_then(|mut controller| controller.pump());
        if let Ok(presentation) = self.presentation.try_borrow()
            && let Some(presentation) = presentation.as_ref()
        {
            if let Err(error) = &result {
                presentation.status.set(error_label(error).to_owned());
            } else {
                restore_progress(self, &presentation.status);
            }
        }
        result
    }
}

impl FeedbackServices {
    /// Lay a model's answer over the review once it is in.
    fn poll_model(&self) {
        let (Ok(mut pass), Ok(mut controller)) =
            (self.pass.try_borrow_mut(), self.controller.try_borrow_mut())
        else {
            return;
        };
        let Some(running) = pass.as_mut() else {
            return;
        };
        if running.poll(&mut controller) {
            *pass = None;
            if let Ok(presentation) = self.presentation.try_borrow()
                && let Some(presentation) = presentation.as_ref()
            {
                presentation.show(&controller);
            }
        }
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

/// Ephemeral CRDT state, admitted only into a private component host. Its
/// fields are edited at their paths; the effects review and send it.
#[derive(Clone, Debug, Default, PartialEq, Model)]
#[model(check = FeedbackDraft::valid)]
pub struct FeedbackDraft {
    /// The issue, in the person's words.
    pub issue: String,
    /// Whether contextual information is included.
    pub include_context: bool,
    /// Whether a single masked app screenshot is included.
    pub include_screenshot: bool,
    /// "This was slow": a report about picked parts carries the last
    /// frame timings.
    pub slow: bool,
}

impl FeedbackDraft {
    fn valid(&self) -> Result<(), tilo_resource::Refusal> {
        if self.issue.len() > 8_000 {
            return Err(tilo_resource::Refusal::new("feedback-issue-too-long"));
        }
        Ok(())
    }
}

#[resource(kind = "tilo.feedback-draft", label = "Feedback")]
impl FeedbackDraft {}

#[derive(Clone, Debug, Effect)]
#[effect(operation = "feedback.compose")]
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
    /// Show a span the review hid, or hide it again.
    Toggle {
        #[param("Which hidden span, by its place in the review")]
        index: usize,
    },
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
    let permissions = Rc::new(RefCell::new(crate::permissions::PermissionContext::member(
        author,
        crate::permissions::Role::Owner,
    )));
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
        let presentation = Presentation {
            status: LocalState::new("feedback-review-first".to_owned()),
            subject: LocalState::new(None),
            shown: LocalState::new(String::new()),
            fields: LocalState::new(Vec::new()),
            pixels: LocalState::new(None),
            picks: LocalState::new(None),
            review: LocalState::new(None),
            notes: Rc::new(
                (0..crate::feedback::report::MAX_PICKS)
                    .map(|_| LocalState::new(String::new()))
                    .collect(),
            ),
        };
        let screenshots = install_effects(cx, &presentation);
        let shown = presentation.clone();
        cx.switch(&presentation.picks, move |part, picks| match picks {
            Some(titles) => report::sheet(part, &shown, titles),
            None => typed(part, &shown, screenshots),
        })
    }
}

/// The composer for a report typed out rather than picked.
fn typed(cx: &ViewCx<FeedbackDraft>, presentation: &Presentation, screenshots: bool) -> Element {
    let subject = about(cx, &presentation.subject);
    let fields_view = reviewed_fields(cx, &presentation.fields);
    let preview = cx.switch(&presentation.shown, |_, shown| {
        Box::new(
            Text::new("feedback-reviewed-text")
                .with_arg("value", shown.clone())
                .with_id("feedback-preview"),
        )
    });
    let picture = cx.switch(&presentation.pixels, |_, png| {
        png.as_ref()
            .and_then(|bytes| crate::widget::Image::from_encoded(bytes).ok())
            .map_or_else(
                || Box::new(group()) as Element,
                |image| Box::new(image.with_alt("feedback-screenshot-preview")) as Element,
            )
    });
    let mut choices = crate::column![
        Text::new("feedback-title"),
        cx.input(FeedbackDraft::paths().issue)
            .with_id("feedback-issue")
            .with_placeholder("feedback-describe")
            .multiline(true),
    ]
    .push(subject);
    // A host that cannot capture is never offered the attachment.
    if screenshots {
        choices = choices.push(Box::new(cx.checkbox(
            "feedback-screenshot",
            FeedbackDraft::paths().include_screenshot,
        )));
    }
    let mut sending = group().push(picture);
    if screenshots {
        sending = sending.push(Box::new(cx.effect_button(
            "feedback-remove-screenshot",
            &FeedbackEffect::RemoveScreenshot,
        )));
    }
    let sending = sending
        .push(Box::new(
            cx.effect_button("feedback-submit", &FeedbackEffect::Submit),
        ))
        .push(Box::new(
            Text::from_state(&presentation.status, Clone::clone)
                .with_id("feedback-status")
                .with_live(crate::a11y::Politeness::Polite),
        ))
        .push(Box::new(
            cx.effect_button("feedback-retry", &FeedbackEffect::Retry),
        ));
    Box::new(
        choices
            .push(Box::new(Text::new("feedback-privacy")))
            .push(Box::new(
                cx.effect_button("feedback-review", &FeedbackEffect::Review),
            ))
            .push(preview)
            .push(fields_view)
            .push(Box::new(sending)),
    )
}

/// What the report is about, and the choice to include it; nothing while
/// Feedback was opened with no window in front.
fn about(cx: &ViewCx<FeedbackDraft>, subject: &LocalState<Option<Subject>>) -> Element {
    cx.switch(subject, |part, subject| {
        let Some(subject) = subject else {
            return Box::new(group()) as Element;
        };
        let name = if subject.name.is_empty() {
            subject.app.clone()
        } else {
            subject.name.clone()
        };
        Box::new(
            group()
                .push(Box::new(
                    Text::new("feedback-about")
                        .with_arg("app", name)
                        .with_id("feedback-about"),
                ))
                .push(Box::new(part.checkbox(
                    "feedback-context",
                    FeedbackDraft::paths().include_context,
                ))),
        ) as Element
    })
}

/// Each reviewed context field with its control to hide it.
fn reviewed_fields(
    cx: &ViewCx<FeedbackDraft>,
    fields: &LocalState<Vec<crate::feedback::ContextField>>,
) -> Element {
    cx.switch(fields, |part, fields| {
        let mut controls = group();
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
    })
}

/// A run of the composer's controls inside its one column: as tall as its
/// contents and aligned with the rest, not a second full-height column.
fn group() -> Column {
    Column::new().with_intrinsic_height().with_no_padding()
}

/// Wire the effects to the lent controller; true when its host can offer a
/// screenshot at all.
fn install_effects(cx: &ViewCx<FeedbackDraft>, presentation: &Presentation) -> bool {
    // Refuse a host accidentally mounting the raw draft in a shared room.
    let services = cx
        .is_private()
        .then(|| cx.take_lent::<FeedbackServices>())
        .flatten();
    let lease = cx.take_lent::<PresentationLease>();
    let mut screenshots = false;
    if let Some(services) = &services {
        *services.presentation.borrow_mut() = Some(presentation.clone());
        if let Ok(composer) = services.controller.try_borrow() {
            screenshots = composer.offers_screenshots();
            presentation.show(&composer);
        }
        restore_progress(services, &presentation.status);
    }
    let shown = presentation.clone();
    cx.effects(move |effect: FeedbackEffect, effect_cx| {
        let _lease = &lease;
        let Some(services) = &services else {
            shown.status.set("feedback-unconfigured".to_owned());
            return Err("feedback-unconfigured".to_owned());
        };
        let draft = effect_cx.live().with(Clone::clone);
        let mut composer = services
            .controller
            .try_borrow_mut()
            .map_err(|_| "feedback-busy".to_owned())?;
        // Context is only offered while there is a window to describe; the
        // review shows exactly what is attached either way. A screenshot the
        // host cannot take is refused rather than dropped.
        let consent = Consent {
            context: draft.include_context && composer.subject().is_some(),
            screenshot: draft.include_screenshot,
        };
        // A report about picked parts reads the notes beside each part.
        let picked = composer.picked().map(|draft| draft.picks.len());
        let words = picked.map(|picks| report::words(&draft, &shown, picks));
        let result = match (effect, &words) {
            (FeedbackEffect::Review, Some(words)) => {
                let reviewed = composer.review_report(words);
                if reviewed.is_ok() {
                    let models = services.models.borrow().clone();
                    *services.pass.borrow_mut() =
                        report::ModelPass::start(&models, &mut composer, words);
                }
                reviewed
            }
            (FeedbackEffect::Review, None) => composer.review(&draft.issue, consent),
            (FeedbackEffect::Submit, _) => {
                let id = effect_cx
                    .invocation()
                    .ok_or_else(|| "feedback-busy".to_owned())?
                    .0
                    .simple()
                    .to_string();
                match &words {
                    Some(words) => composer.submit_report(id, words),
                    None => composer.submit(id, &draft.issue, consent),
                }
            }
            (FeedbackEffect::Retry, _) => composer.pump(),
            (FeedbackEffect::Redact { key, start, end }, _) => composer.hide(&key, start, end),
            (FeedbackEffect::RemoveScreenshot, _) => composer.remove_screenshot(),
            (FeedbackEffect::Toggle { index }, _) => composer.toggle_redaction(index),
        };
        shown.show(&composer);
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
        shown.status.set(key.to_owned());
        result.map_err(|_| key.to_owned())?;
        Ok(Effected::done())
    });
    screenshots
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
        fn offers_screenshots(&self) -> bool {
            false
        }
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

    /// The arguments of the field edit `edit`, as the door takes them.
    fn edit(edit: &tilo_resource::Edit<FeedbackDraft>) -> serde_json::Value {
        json!({ tilo_resource::EDIT_PARAM: edit })
    }

    /// Type the issue, as the composer's text field does.
    fn describe(frame: &mut FrameHarness, issue: &str) -> Result<(), tilo_actions::DoorError> {
        call(
            frame,
            tilo_resource::FIELD_EDIT,
            &edit(&FeedbackDraft::paths().issue.replace(issue)),
        )
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
            tilo_resource::FIELD_EDIT,
            &edit(
                &FeedbackDraft::paths()
                    .issue
                    .replace("Opening the book failed"),
            ),
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
        describe(&mut frame, "Opening the book failed; token=secret12345")
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
        services
            .about(Some(Subject::of_app("so.tilo.books", "Books")))
            .expect("the desktop names the window feedback is about");
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
        describe(frame, "feedback-title").expect("second issue");
        call(
            frame,
            tilo_resource::FIELD_EDIT,
            &edit(&FeedbackDraft::paths().include_context.set(true)),
        )
        .expect("explicit context opt-in");
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
        let context: Vec<_> = second
            .feedback
            .context
            .iter()
            .map(|field| field.value.as_str())
            .collect();
        assert_eq!(context, ["App: Books (so.tilo.books)", "feedback-privacy"]);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one journey: pick, review, show a span, send"
    )]
    fn a_picked_report_shows_exactly_what_is_sent_and_toggles_hidden_spans() {
        use crate::feedback::report::{Area, Pick, PickedText, ReportDraft, TextKind};
        let transport = Rc::new(RefCell::new(Transport::default()));
        let controller = Rc::new(RefCell::new(
            Composer::new(
                Destination {
                    room: "lounge".into(),
                    provider: "record:so.tilo.chat:".into(),
                    channel: "c_feedback".into(),
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
        let services = FeedbackServices::new(controller);
        let pick = Pick {
            app: "so.tilo.chat".into(),
            app_name: "Chat".into(),
            build: Some("abc123".into()),
            path: "Chat[0]/Row[1]".into(),
            role: "ListItem".into(),
            label: None,
            record: None,
            area: Area {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
            texts: vec![
                PickedText {
                    kind: TextKind::Content,
                    text: "Thanks Ada Lovelace".into(),
                    area: None,
                },
                PickedText {
                    kind: TextKind::Interface,
                    text: "Reply".into(),
                    area: None,
                },
            ],
        };
        services
            .begin_report(PickedReport {
                draft: ReportDraft {
                    picks: vec![pick],
                    names: vec!["Ada Lovelace".into()],
                    ..ReportDraft::default()
                },
                picture: None,
            })
            .expect("report begins");
        let component = prepare(Some(services), "device", "guest", 42).expect("private composer");
        let mut frame = FrameHarness::from_component(
            component,
            (720, 900),
            ComponentRunnerInputs::from_native_process(),
        )
        .expect("production component");
        frame.relayout();
        let sheet = frame.inspection_json().expect("inspection");
        assert!(sheet.contains("Report a problem"), "the report sheet shows");
        assert!(sheet.contains("1. Chat \u{b7} List item"), "{sheet}");
        describe(&mut frame, "The row is cut off").expect("summary");
        call(&mut frame, "review", &json!({})).expect("review");
        frame.relayout();
        let preview = frame.inspection();
        let shown = find(&preview.root)
            .and_then(|node| node.semantics.as_ref())
            .and_then(|node| node.name.clone())
            .expect("the outgoing report is shown");
        assert!(shown.contains("The row is cut off"), "{shown}");
        assert!(shown.contains("Reply"), "{shown}");
        assert!(!shown.contains("Ada"), "content is hidden: {shown}");
        call(&mut frame, "toggle", &json!({"index": 0})).expect("show the message");
        frame.relayout();
        let preview = frame.inspection();
        let shown = find(&preview.root)
            .and_then(|node| node.semantics.as_ref())
            .and_then(|node| node.name.clone())
            .expect("the outgoing report is shown");
        assert!(shown.contains("Thanks \u{2039}name\u{203A}"), "{shown}");
        assert!(
            frame
                .inspection_json()
                .expect("inspection")
                .contains("Checked on this device by rules"),
            "with no model, the review says the rules checked it"
        );
        call(&mut frame, "submit", &json!({})).expect("send");
        let sent = transport.borrow();
        let submission = sent.submissions.first().expect("sent");
        assert_eq!(submission.feedback.selections.len(), 1);
        assert!(
            !serde_json::to_string(submission)
                .unwrap()
                .contains("Lovelace")
        );
    }
}
