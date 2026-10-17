//! The system-owned feedback journey shared by native and browser hosts.

use crate::{
    DeclaredSensitivity, DeliveryState, Destination, Draft, Error, MaskedScreenshot, Outbox,
    Preview, Receipt, ReviewText, Submission, TextOrigin,
};

/// What a report is about: the window the person opened Feedback from, as
/// the desktop names it. Only the app's public identity becomes context;
/// window titles and app content are never read through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    /// The app's id, as grants and launchers key it.
    pub app: String,
    /// The app's display name.
    pub name: String,
}

impl Subject {
    fn context(&self) -> ReviewText {
        let text = if self.name.is_empty() || self.name == self.app {
            format!("App: {}", self.app)
        } else {
            format!("App: {} ({})", self.name, self.app)
        };
        ReviewText {
            node: "feedback.subject.app".into(),
            sensitivity: DeclaredSensitivity::Public,
            kind: None,
            origin: TextOrigin::Semantic,
            text,
        }
    }
}

/// Both attachments start off. Opening the composer collects neither.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Consent {
    pub context: bool,
    pub screenshot: bool,
}

/// An adapter to an already-admitted workspace and its existing transport.
/// Implementations must not connect accounts, invent destinations or capture
/// the desktop. They retain their authenticated route across retries.
pub trait FeedbackHost {
    /// Whether [`Self::capture`] can ever produce a screenshot. A host that
    /// cannot is never offered the choice, rather than refusing it later.
    fn offers_screenshots(&self) -> bool;
    /// Read opt-in context from the selected app, beyond its identity.
    ///
    /// # Errors
    /// Reports unavailable or invalid context.
    fn context(&mut self) -> Result<Vec<ReviewText>, Error>;
    /// Explicit one-shot request; masking must finish before this returns.
    ///
    /// # Errors
    /// Refuses protected, changed or unclassified capture geometry.
    fn capture(&mut self) -> Result<MaskedScreenshot, Error>;
    /// Persist masked pixels locally before queueing their reference. No upload
    /// is permitted until the approved submission is durably queued.
    ///
    /// # Errors
    /// Reports local storage that has not durably committed the masked bytes.
    fn persist_screenshot(&mut self, image: &MaskedScreenshot) -> Result<(), Error>;
    /// Enqueue on the existing authenticated transport with submission.id as
    /// its business retry identity. Acceptance is never a delivery receipt.
    ///
    /// # Errors
    /// Reports unavailable transport or refused authenticated admission.
    fn send(&mut self, submission: &Submission) -> Result<(), Error>;
    /// Only return a receipt from an authenticated, durably committed owner.
    ///
    /// # Errors
    /// Reports unavailable transport or an unobservable canonical commit.
    fn receipt(&mut self, submission: &Submission) -> Result<Option<Receipt>, Error>;
}

/// Private, local authoring state; raw drafts and previews never enter a shared
/// record. The host supplies durable storage and wakes its presentation when
/// local storage or the existing transport changes.
pub struct Composer {
    destination: Destination,
    outbox: Outbox,
    host: Box<dyn FeedbackHost>,
    reviewed: Option<(String, Consent, Preview)>,
    active: Option<String>,
    subject: Option<Subject>,
}

impl Composer {
    /// Bind the composer to the admitted destination and the device outbox.
    ///
    /// # Errors
    /// Refuses invalid destination identifiers.
    pub fn new(
        destination: Destination,
        outbox: Outbox,
        host: Box<dyn FeedbackHost>,
    ) -> Result<Self, Error> {
        destination.validate()?;
        let mut active = None;
        for id in outbox.ids() {
            if let Some(submission) = outbox.pending(id)?
                && submission.destination == destination
            {
                active = Some(id.to_owned());
                break;
            }
        }
        Ok(Self {
            destination,
            outbox,
            host,
            reviewed: None,
            active,
            subject: None,
        })
    }

    /// Name what the next report is about. Its context changes, so an
    /// earlier review no longer shows what would be sent.
    pub fn about(&mut self, subject: Option<Subject>) {
        if self.subject != subject {
            self.reviewed = None;
            self.subject = subject;
        }
    }

    pub fn subject(&self) -> Option<&Subject> {
        self.subject.as_ref()
    }

    pub fn offers_screenshots(&self) -> bool {
        self.host.offers_screenshots()
    }

    /// A new review invalidates the old approval before any host call. Failed
    /// capture cannot silently leave a stale preview ready for submission.
    ///
    /// # Errors
    /// Refuses a queued report, invalid review or unavailable attachment.
    pub fn review(&mut self, text: &str, consent: Consent) -> Result<(), Error> {
        self.reviewed = None;
        if self.active.is_some() {
            if !matches!(self.state()?, Some(DeliveryState::Sent { .. })) {
                return Err(Error::Conflict);
            }
            self.active = None;
        }
        if text.trim().is_empty() || text.len() > 8_000 {
            return Err(Error::Invalid("feedback"));
        }
        let mut preview = Draft {
            text: text.to_owned(),
            context: if consent.context {
                // Context describes the window the report is about; without
                // one there is nothing truthful to attach.
                let subject = self.subject.as_ref().ok_or(Error::Invalid("context"))?;
                let mut context = vec![subject.context()];
                context.extend(self.host.context()?);
                context
            } else {
                Vec::new()
            },
        }
        .preview()?;
        if consent.screenshot {
            preview.attach_screenshot(self.host.capture()?);
        }
        self.reviewed = Some((text.to_owned(), consent, preview));
        Ok(())
    }

    pub fn preview(&self) -> Option<&Preview> {
        self.reviewed.as_ref().map(|(_, _, preview)| preview)
    }

    /// Closing a private presentation discards its unapproved text and pixels.
    /// An already queued immutable report keeps its existing retry identity.
    pub fn abandon_review(&mut self) {
        self.reviewed = None;
    }

    /// Hide a reviewed span without restoring the original text.
    ///
    /// # Errors
    /// Refuses a missing preview or invalid field/span.
    pub fn hide(&mut self, key: &str, start: usize, end: usize) -> Result<(), Error> {
        self.reviewed
            .as_mut()
            .ok_or(Error::NotApproved)?
            .2
            .hide(key, start, end)
    }

    /// Remove an optional attachment before approval.
    ///
    /// # Errors
    /// Refuses a missing preview.
    pub fn remove_screenshot(&mut self) -> Result<(), Error> {
        self.reviewed
            .as_mut()
            .ok_or(Error::NotApproved)?
            .2
            .remove_screenshot();
        Ok(())
    }

    /// Approve exactly the displayed snapshot. Edited issue text or consent
    /// requires fresh review. An accepted queue freezes this id and payload.
    ///
    /// # Errors
    /// Refuses changed drafts, invalid reports, storage or transport failures.
    pub fn submit(&mut self, id: String, text: &str, consent: Consent) -> Result<(), Error> {
        if self.active.is_some() {
            return Err(Error::Conflict);
        }
        let (reviewed_text, reviewed_consent, preview) =
            self.reviewed.as_mut().ok_or(Error::NotApproved)?;
        if text != reviewed_text || consent != *reviewed_consent {
            return Err(Error::NotApproved);
        }
        preview.approve();
        let submission = preview.submission(id.clone(), self.destination.clone())?;
        if let Some(image) = preview.screenshot() {
            self.host.persist_screenshot(image)?;
        }
        if let Err(error) = self.outbox.queue(submission) {
            if matches!(error, Error::Store(_)) {
                self.active = Some(id);
            }
            return Err(error);
        }
        self.active = Some(id);
        self.pump()
    }

    /// Resume a pending report after a host restart; no draft or screenshot
    /// consent is inferred, and the original approved payload is retained.
    ///
    /// # Errors
    /// Refuses an unknown id.
    pub fn resume(&mut self, id: &str) -> Result<(), Error> {
        self.outbox.state(id)?;
        self.active = Some(id.to_owned());
        self.reviewed = None;
        Ok(())
    }

    pub fn pending_ids(&self) -> impl Iterator<Item = &str> {
        self.outbox.ids()
    }

    /// Delivery progress, including both local and destination durability.
    ///
    /// # Errors
    /// Reports an invalid active id.
    pub fn state(&self) -> Result<Option<DeliveryState>, Error> {
        self.active
            .as_deref()
            .map(|id| self.outbox.state(id))
            .transpose()
    }

    /// Called on host storage/network wakes and by the semantic Retry action.
    /// Polling or a successful dispatch does not change `AwaitingReceipt`.
    ///
    /// # Errors
    /// Reports storage, transport or mismatched receipt failures.
    pub fn pump(&mut self) -> Result<(), Error> {
        let Some(id) = self.active.as_deref() else {
            return Ok(());
        };
        if self.outbox.state(id)? == DeliveryState::StorageFailed {
            self.outbox.retry_storage()?;
        }
        if self.outbox.state(id)? != DeliveryState::AwaitingReceipt {
            return Ok(());
        }
        let Some(submission) = self.outbox.pending(id)?.cloned() else {
            return Ok(());
        };
        if submission.destination != self.destination {
            return Err(Error::ReceiptMismatch);
        }
        if let Some(receipt) = self.host.receipt(&submission)? {
            self.outbox
                .confirm_durable_receipt(&self.destination, receipt)
        } else {
            self.host.send(&submission)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use durable_store::{MemoryStore, StorageKey};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Calls {
        context: usize,
        captures: usize,
        sent: Vec<Submission>,
        receipt: Option<Receipt>,
    }
    struct Host(Rc<RefCell<Calls>>);
    impl FeedbackHost for Host {
        fn offers_screenshots(&self) -> bool {
            false
        }
        fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
            let mut calls = self.0.borrow_mut();
            calls.context = calls.context.saturating_add(1);
            Ok(Vec::new())
        }
        fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
            let mut calls = self.0.borrow_mut();
            calls.captures = calls.captures.saturating_add(1);
            Err(Error::CaptureProtected)
        }
        fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
            Err(Error::CaptureProtected)
        }
        fn send(&mut self, submission: &Submission) -> Result<(), Error> {
            self.0.borrow_mut().sent.push(submission.clone());
            Ok(())
        }
        fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
            Ok(self.0.borrow().receipt.clone())
        }
    }
    fn destination() -> Destination {
        Destination {
            room: "demo".into(),
            provider: "record:chat:feedback".into(),
            channel: "feedback".into(),
        }
    }
    fn composer(store: Rc<MemoryStore>, calls: Rc<RefCell<Calls>>) -> Composer {
        Composer::new(
            destination(),
            Outbox::open(store, StorageKey::package("feedback")).unwrap(),
            Box::new(Host(calls)),
        )
        .unwrap()
    }
    fn commit(store: &MemoryStore) {
        for write in store.drain() {
            store.acknowledge(write.revision, Ok(())).unwrap();
        }
    }

    #[test]
    fn consent_changes_and_failed_capture_require_a_new_review() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut composer = composer(Rc::new(MemoryStore::new()), calls.clone());
        assert!(composer.preview().is_none());
        assert_eq!((calls.borrow().context, calls.borrow().captures), (0, 0));
        composer
            .review("The book did not open", Consent::default())
            .unwrap();
        assert_eq!((calls.borrow().context, calls.borrow().captures), (0, 0));
        let id = "0123456789abcdef0123456789abcdef".to_owned();
        assert!(matches!(
            composer.submit(id.clone(), "An edited issue", Consent::default()),
            Err(Error::NotApproved)
        ));
        let attached = Consent {
            context: true,
            screenshot: true,
        };
        assert!(matches!(
            composer.submit(id, "The book did not open", attached),
            Err(Error::NotApproved)
        ));
        assert!(
            matches!(
                composer.review("The book did not open", attached),
                Err(Error::Invalid("context"))
            ),
            "no window was named, so there is no context to attach"
        );
        composer.about(Some(notes()));
        assert!(matches!(
            composer.review("The book did not open", attached),
            Err(Error::CaptureProtected)
        ));
        assert!(composer.preview().is_none());
        assert!(calls.borrow().sent.is_empty());
    }

    fn notes() -> Subject {
        Subject {
            app: "so.tilo.notes".into(),
            name: "Notes".into(),
        }
    }

    #[test]
    fn context_names_the_subject_only_with_consent_and_a_new_subject_needs_review() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut composer = composer(Rc::new(MemoryStore::new()), calls.clone());
        composer.about(Some(notes()));
        composer
            .review("Saving lost my edit", Consent::default())
            .unwrap();
        assert!(composer.preview().unwrap().feedback().context.is_empty());
        assert_eq!(calls.borrow().context, 0, "nothing is read without consent");
        let context = Consent {
            context: true,
            screenshot: false,
        };
        composer.review("Saving lost my edit", context).unwrap();
        assert_eq!(calls.borrow().context, 1);
        let reviewed = composer.preview().unwrap().feedback().clone();
        assert_eq!(reviewed.context.len(), 1);
        assert_eq!(reviewed.context[0].value, "App: Notes (so.tilo.notes)");
        assert!(
            !reviewed.context[0].key.contains("subject"),
            "local semantic ids never travel as report keys"
        );
        composer.about(Some(notes()));
        assert!(
            composer.preview().is_some(),
            "the same subject keeps the review"
        );
        composer.about(Some(Subject {
            app: "so.tilo.files".into(),
            name: "Files".into(),
        }));
        assert!(
            composer.preview().is_none(),
            "another window's context needs a fresh review"
        );
        let id = "0123456789abcdef0123456789abcdef".to_owned();
        assert!(matches!(
            composer.submit(id, "Saving lost my edit", context),
            Err(Error::NotApproved)
        ));
        assert!(calls.borrow().sent.is_empty());
    }

    #[test]
    fn restart_and_transport_acceptance_never_claim_durable_delivery() {
        let store = Rc::new(MemoryStore::queued());
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut composer = composer(store.clone(), calls.clone());
        let id = "0123456789abcdef0123456789abcdef".to_owned();
        composer
            .review("The book did not open", Consent::default())
            .unwrap();
        composer
            .submit(id.clone(), "The book did not open", Consent::default())
            .unwrap();
        assert_eq!(composer.state().unwrap(), Some(DeliveryState::Saving));
        assert!(calls.borrow().sent.is_empty());
        commit(&store);
        composer.pump().unwrap();
        assert_eq!(
            composer.state().unwrap(),
            Some(DeliveryState::AwaitingReceipt)
        );
        let submitted = calls.borrow().sent.first().unwrap().clone();
        drop(composer);
        let mut restored = self::composer(store.clone(), calls.clone());
        restored.resume(&id).unwrap();
        restored.pump().unwrap();
        assert!(calls.borrow().sent.iter().all(|item| item == &submitted));
        calls.borrow_mut().receipt = Some(Receipt {
            id,
            digest: submitted.digest().unwrap(),
            destination: submitted.destination,
            report: "chat.message:m_feedback_42".into(),
        });
        restored.pump().unwrap();
        assert_eq!(restored.state().unwrap(), Some(DeliveryState::Saving));
        assert!(matches!(
            restored.review("Another issue", Consent::default()),
            Err(Error::Conflict)
        ));
        commit(&store);
        assert_eq!(
            restored.state().unwrap(),
            Some(DeliveryState::Sent {
                report: "chat.message:m_feedback_42".into()
            })
        );
        calls.borrow_mut().receipt = None;
        restored
            .review("Another issue", Consent::default())
            .unwrap();
        let next_id = "1123456789abcdef0123456789abcdef".to_owned();
        restored
            .submit(next_id.clone(), "Another issue", Consent::default())
            .unwrap();
        assert_eq!(restored.state().unwrap(), Some(DeliveryState::Saving));
        commit(&store);
        restored.pump().unwrap();
        assert_eq!(calls.borrow().sent.last().unwrap().id, next_id);
        assert_eq!(
            calls.borrow().sent.last().unwrap().feedback.text,
            "Another issue"
        );
    }

    #[test]
    fn rejecting_the_first_local_write_preserves_the_approved_retry_identity() {
        let store = Rc::new(crate::test_store::RejectOnceStore::new());
        let calls = Rc::new(RefCell::new(Calls::default()));
        let outbox = Outbox::open(store.clone(), StorageKey::package("feedback")).unwrap();
        let mut composer =
            Composer::new(destination(), outbox, Box::new(Host(calls.clone()))).unwrap();
        let id = "0123456789abcdef0123456789abcdef".to_owned();
        composer
            .review("The book did not open", Consent::default())
            .unwrap();
        assert!(matches!(
            composer.submit(id.clone(), "The book did not open", Consent::default()),
            Err(Error::Store(_))
        ));
        assert_eq!(
            composer.state().unwrap(),
            Some(DeliveryState::StorageFailed)
        );
        assert_eq!(
            composer.pending_ids().collect::<Vec<_>>(),
            vec![id.as_str()]
        );
        assert!(calls.borrow().sent.is_empty());
        composer.pump().unwrap();
        let submitted = calls.borrow().sent.first().unwrap().clone();
        assert_eq!(submitted.id, id);
        drop(composer);
        let outbox = Outbox::open(store, StorageKey::package("feedback")).unwrap();
        let mut restored =
            Composer::new(destination(), outbox, Box::new(Host(calls.clone()))).unwrap();
        restored.pump().unwrap();
        assert!(
            calls
                .borrow()
                .sent
                .iter()
                .all(|report| report == &submitted)
        );
    }
}
