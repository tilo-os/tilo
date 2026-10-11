//! The system-owned feedback journey shared by native and browser hosts.

use crate::{
    AttachmentFuture, DeliveryState, Destination, Draft, Error, MaskedScreenshot, Outbox, Preview,
    Receipt, ReviewText, Submission,
};

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
    /// Read opt-in context from the selected app.
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
    /// Own the approved masked image while its exact storage write completes.
    /// The default retains synchronous hosts; browser hosts supply a future
    /// that awaits both this write and fresh per-address custody.
    fn persist_screenshot_async(&mut self, image: MaskedScreenshot) -> AttachmentFuture<'static> {
        Box::pin(std::future::ready(self.persist_screenshot(&image)))
    }
    /// Recheck current admission after asynchronous custody and before queueing.
    /// # Errors
    /// Refuses a changed destination, revoked authority or invalid submission.
    fn validate_submission(&mut self, submission: &Submission) -> Result<(), Error> {
        submission.validate()
    }
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
    pending_attachment: Option<PendingAttachment>,
}

/// Approval is immutable while storage completes. Only masked bytes are held;
/// no transport sees this payload until the existing outbox is durable.
struct PendingAttachment {
    submission: Submission,
    image: MaskedScreenshot,
    future: Option<AttachmentFuture<'static>>,
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
            pending_attachment: None,
        })
    }

    /// A new review invalidates the old approval before any host call. Failed
    /// capture cannot silently leave a stale preview ready for submission.
    ///
    /// # Errors
    /// Refuses a queued report, invalid review or unavailable attachment.
    pub fn review(&mut self, text: &str, consent: Consent) -> Result<(), Error> {
        if self.pending_attachment.is_some() {
            return Err(Error::Conflict);
        }
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
                self.host.context()?
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
        if self.pending_attachment.is_some() {
            return Err(Error::Conflict);
        }
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
        if self.pending_attachment.is_some() {
            return Err(Error::Conflict);
        }
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
        if self.active.is_some() || self.pending_attachment.is_some() {
            return Err(Error::Conflict);
        }
        let (reviewed_text, reviewed_consent, preview) =
            self.reviewed.as_mut().ok_or(Error::NotApproved)?;
        if text != reviewed_text || consent != *reviewed_consent {
            return Err(Error::NotApproved);
        }
        preview.approve();
        let submission = preview.submission(id, self.destination.clone())?;
        if let Some(image) = preview.screenshot() {
            let image = image.clone();
            let future = self.host.persist_screenshot_async(image.clone());
            self.pending_attachment = Some(PendingAttachment {
                submission,
                image,
                future: Some(future),
            });
            return self.pump();
        }
        self.queue_approved(submission)
    }

    fn queue_approved(&mut self, submission: Submission) -> Result<(), Error> {
        self.host.validate_submission(&submission)?;
        let id = submission.id.clone();
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
        if self.pending_attachment.is_some() {
            return Err(Error::Conflict);
        }
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
        if let Some(pending) = &self.pending_attachment {
            return Ok(Some(if pending.future.is_some() {
                DeliveryState::Saving
            } else {
                DeliveryState::StorageFailed
            }));
        }
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
        if let Some(mut pending) = self.pending_attachment.take() {
            let future = pending
                .future
                .get_or_insert_with(|| self.host.persist_screenshot_async(pending.image.clone()));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Pending => {
                    self.pending_attachment = Some(pending);
                    return Ok(());
                }
                std::task::Poll::Ready(Err(error)) => {
                    pending.future = None;
                    self.pending_attachment = Some(pending);
                    return Err(error);
                }
                std::task::Poll::Ready(Ok(())) => {
                    let result = self.queue_approved(pending.submission.clone());
                    if result.is_err() && self.active.is_none() {
                        pending.future = None;
                        self.pending_attachment = Some(pending);
                    }
                    return result;
                }
            }
        }
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
mod attachment_tests;

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
        assert!(matches!(
            composer.review("The book did not open", attached),
            Err(Error::CaptureProtected)
        ));
        assert!(composer.preview().is_none());
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
