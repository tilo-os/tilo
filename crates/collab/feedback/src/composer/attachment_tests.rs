use super::*;
use std::{cell::Cell, cell::RefCell, rc::Rc, task::Poll};

struct AsyncHost {
    gate: Rc<Cell<u8>>,
    admitted: Rc<Cell<bool>>,
    persisted: Rc<RefCell<Vec<String>>>,
    sent: Rc<RefCell<Vec<Submission>>>,
}

impl FeedbackHost for AsyncHost {
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Ok(Vec::new())
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        crate::CapturePermit::requested("synthetic:approved-app".into(), 1)?.mask(
            crate::SurfaceCapture {
                surface: "synthetic:approved-app".into(),
                revision: 1,
                width: 1,
                height: 1,
                rgba: vec![70, 80, 90, 255],
                capture_protected: false,
                sensitive_regions: Some(vec![]),
            },
        )
    }
    fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
        Err(Error::NotDurable)
    }
    fn persist_screenshot_async(&mut self, image: MaskedScreenshot) -> AttachmentFuture<'static> {
        self.persisted
            .borrow_mut()
            .push(image.reference().hash.clone());
        let gate = Rc::clone(&self.gate);
        Box::pin(std::future::poll_fn(move |_| match gate.get() {
            0 => Poll::Pending,
            1 => Poll::Ready(Err(Error::NotDurable)),
            _ => Poll::Ready(Ok(())),
        }))
    }
    fn validate_submission(&mut self, submission: &Submission) -> Result<(), Error> {
        if !self.admitted.get() {
            return Err(Error::ReceiptMismatch);
        }
        submission.validate()
    }
    fn send(&mut self, submission: &Submission) -> Result<(), Error> {
        self.sent.borrow_mut().push(submission.clone());
        Ok(())
    }
    fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
}

#[test]
fn async_custody_precedes_queue_and_rechecks_authority_without_changing_approval() {
    for revoke_after_write in [false, true] {
        let store = Rc::new(crate::MemoryStore::queued());
        let gate = Rc::new(Cell::new(0));
        let admitted = Rc::new(Cell::new(true));
        let persisted = Rc::new(RefCell::new(Vec::new()));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut composer = Composer::new(
            Destination {
                room: "synthetic-room".into(),
                provider: "record:chat:feedback".into(),
                channel: "feedback".into(),
            },
            Outbox::open(store.clone(), crate::StorageKey::package("async-feedback")).unwrap(),
            Box::new(AsyncHost {
                gate: Rc::clone(&gate),
                admitted: Rc::clone(&admitted),
                persisted: Rc::clone(&persisted),
                sent: Rc::clone(&sent),
            }),
        )
        .unwrap();
        let consent = Consent {
            context: false,
            screenshot: true,
        };
        let id = "0123456789abcdef0123456789abcdef";
        composer.review("Approved issue", consent).unwrap();
        let reference = composer
            .preview()
            .unwrap()
            .screenshot()
            .unwrap()
            .reference()
            .clone();
        composer
            .submit(id.into(), "Approved issue", consent)
            .unwrap();
        assert_eq!(composer.state().unwrap(), Some(DeliveryState::Saving));
        assert!(composer.pending_ids().next().is_none());
        assert!(
            store.drain().is_empty(),
            "pending custody must not queue a report"
        );
        assert!(sent.borrow().is_empty());
        assert!(matches!(
            composer.review("Edited issue", consent),
            Err(Error::Conflict)
        ));
        assert!(matches!(composer.remove_screenshot(), Err(Error::Conflict)));
        assert!(matches!(composer.hide("issue", 0, 1), Err(Error::Conflict)));
        // A closed presentation discards its raw review, but explicit approval
        // remains immutable and private until both custody boundaries complete.
        composer.abandon_review();
        assert!(composer.preview().is_none());
        if revoke_after_write {
            gate.set(2);
            admitted.set(false);
            assert!(matches!(composer.pump(), Err(Error::ReceiptMismatch)));
        } else {
            gate.set(1);
            assert!(matches!(composer.pump(), Err(Error::NotDurable)));
        }
        assert_eq!(
            composer.state().unwrap(),
            Some(DeliveryState::StorageFailed)
        );
        assert!(store.drain().is_empty());
        assert!(sent.borrow().is_empty());
        admitted.set(true);
        gate.set(2);
        composer.pump().unwrap();
        assert!(sent.borrow().is_empty(), "image custody alone cannot send");
        assert_eq!(composer.pending_ids().collect::<Vec<_>>(), vec![id]);
        let writes = store.drain();
        assert_eq!(writes.len(), 1);
        for write in writes {
            store.acknowledge(write.revision, Ok(())).unwrap();
        }
        composer.pump().unwrap();
        let sent = sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent.first().unwrap().id, id);
        assert_eq!(sent.first().unwrap().feedback.text, "Approved issue");
        assert_eq!(
            sent.first().unwrap().feedback.screenshot.as_ref(),
            Some(&reference)
        );
        assert_eq!(
            persisted.borrow().as_slice(),
            [reference.hash.clone(), reference.hash]
        );
    }
}
