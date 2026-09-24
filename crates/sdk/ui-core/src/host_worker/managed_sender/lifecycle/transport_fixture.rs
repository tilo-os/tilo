//! Concrete test fixture over the real private file and cache phases.
use super::*;
use crate::collaboration::RouteSpec;
use crate::portable_storage::PortableStorage;
use std::time::Duration;

pub(crate) struct TransportFixture {
    sender: ManagedSender,
    ticket: Ticket,
    binding: PublishedInvocationBinding,
    _cache: PortableStorage,
    work: NativeAllowance,
    _root: tempfile::TempDir,
}
fn caller() -> CallerView<'static> {
    CallerView {
        root: [1; 32],
        device: [2; 32],
        app: "caller-app",
        record: "caller-record",
        agent: None,
    }
}
fn finish(sender: &mut ManagedSender, work: &NativeAllowance) -> Completion {
    for _ in 0..8 {
        if let Some(completion) = sender.advance(work).unwrap() {
            return completion;
        }
    }
    panic!("actual private file mutation did not complete");
}
impl TransportFixture {
    pub(crate) fn new() -> Self {
        let work = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let (root, current, cache) =
            committed_test_fixture(super::super::NativeSenderOutboxV1::empty_bytes(), &work);
        let mut sender = ManagedSender::open(current, &work).unwrap();
        let binding = PublishedInvocationBinding {
            host: [3; 16],
            peer_id: 7,
            app_id: "target-app".into(),
            record_id: "target-record".into(),
            root: [4; 32],
            device: [5; 32],
            target_binding: [6; 32],
        };
        let ticket = sender
            .begin_new(
                caller(),
                Intent {
                    binding: &binding,
                    revision: "original",
                    resource: "record",
                    action: "edit",
                    argument: Some(""),
                },
                &work,
            )
            .unwrap();
        assert!(matches!(
            sender.request(
                ticket,
                caller(),
                Operation::Invoke,
                "{}",
                Instant::now(),
                &work
            ),
            Err(Failure::Busy)
        ));
        assert_eq!(finish(&mut sender, &work), Completion::Began(ticket));
        Self {
            sender,
            ticket,
            binding,
            _cache: cache,
            work,
            _root: root,
        }
    }
    pub(crate) fn route(&self) -> RouteSpec {
        RouteSpec::record("caller-app", self.sender.room, "caller-record")
    }
    pub(crate) fn invocation(&self) -> [u8; 16] {
        self.ticket.invocation
    }
    pub(crate) fn binding(&self) -> &PublishedInvocationBinding {
        &self.binding
    }
    pub(crate) fn revision(&self) -> &str {
        "original"
    }
    pub(crate) fn resource(&self) -> &str {
        "record"
    }
    pub(crate) fn action(&self) -> &str {
        "edit"
    }
    pub(crate) fn argument(&self) -> Option<&str> {
        Some("")
    }
    pub(crate) fn confirmation(&self) -> &str {
        r#"{"current":true}"#
    }
    fn request(&mut self, operation: Operation) -> ManagedAttempt {
        self.sender
            .request(
                self.ticket,
                caller(),
                operation,
                r#"{"current":true}"#,
                Instant::now() + Duration::from_secs(5),
                &self.work,
            )
            .unwrap()
    }
    pub(crate) fn invoke(&mut self) -> ManagedAttempt {
        self.request(Operation::Invoke)
    }
    pub(crate) fn observe(&mut self) -> ManagedAttempt {
        self.request(Operation::Observe)
    }
    pub(crate) fn cancel(&mut self) -> ManagedAttempt {
        self.request(Operation::Cancel)
    }
    pub(crate) fn queue_result(&mut self, result: ManagedAttemptResult) {
        assert_eq!(
            self.sender
                .observe_result(result, caller(), &self.work)
                .unwrap(),
            ReplyDisposition::Queued
        );
        assert!(
            self.sender
                .terminal_receipt(self.ticket, caller(), &self.work)
                .is_err(),
            "no terminal receipt before exact physical finish"
        );
    }
    pub(crate) fn finish_result(&mut self) {
        assert_eq!(
            finish(&mut self.sender, &self.work),
            Completion::Observed(self.ticket)
        );
    }
    pub(crate) fn assert_readback(&self, expected: &str) {
        assert_eq!(
            self.sender
                .read_result(self.ticket, caller(), &self.work)
                .unwrap()
                .json(),
            expected
        );
    }
    pub(crate) fn assert_consume_busy(&mut self) {
        let receipt = self
            .sender
            .terminal_receipt(self.ticket, caller(), &self.work)
            .unwrap();
        assert!(matches!(
            self.sender.consume(receipt, caller(), &self.work),
            Err(Failure::Busy)
        ));
    }
    pub(crate) fn consume_and_finish(&mut self) {
        let receipt = self
            .sender
            .terminal_receipt(self.ticket, caller(), &self.work)
            .unwrap();
        self.sender.consume(receipt, caller(), &self.work).unwrap();
        assert_eq!(
            finish(&mut self.sender, &self.work),
            Completion::Consumed(self.ticket)
        );
        assert!(matches!(
            self.sender.request(
                self.ticket,
                caller(),
                Operation::Invoke,
                "{}",
                Instant::now(),
                &self.work
            ),
            Err(Failure::NotRetained)
        ));
    }
}
