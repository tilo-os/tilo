//! Concrete test fixture over the real private file and cache phases.
use super::*;
use crate::collaboration::RouteSpec;
use crate::host_worker::private_actions::committed_test_fixture;
use crate::portable_storage::PortableStorage;
use std::time::Duration;

pub(crate) struct TransportFixture {
    sender: ManagedSender,
    ticket: Ticket,
    binding: PublishedInvocationBinding,
    _cache: PortableStorage,
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
fn finish(sender: &mut ManagedSender) -> Completion {
    for _ in 0..8 {
        if let Some(completion) = sender.advance().unwrap() {
            return completion;
        }
    }
    panic!("actual private file mutation did not complete");
}
impl TransportFixture {
    pub(crate) fn new() -> Self {
        let (root, current, cache) =
            committed_test_fixture(super::super::NativeSenderOutboxV1::empty_bytes());
        let mut sender = ManagedSender::open(current).unwrap();
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
            )
            .unwrap();
        assert!(matches!(
            sender.request(ticket, caller(), Operation::Invoke, "{}", Instant::now()),
            Err(Failure::Busy)
        ));
        assert_eq!(finish(&mut sender), Completion::Began(ticket));
        Self {
            sender,
            ticket,
            binding,
            _cache: cache,
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
            self.sender.observe_result(result, caller()).unwrap(),
            ReplyDisposition::Queued
        );
        assert!(
            self.sender.terminal_receipt(self.ticket, caller()).is_err(),
            "no terminal receipt before exact physical finish"
        );
    }
    pub(crate) fn finish_result(&mut self) {
        assert_eq!(finish(&mut self.sender), Completion::Observed(self.ticket));
    }
    pub(crate) fn assert_readback(&self, expected: &str) {
        assert_eq!(
            self.sender
                .read_result(self.ticket, caller())
                .unwrap()
                .json(),
            expected
        );
    }
    pub(crate) fn assert_consume_busy(&mut self) {
        let receipt = self.sender.terminal_receipt(self.ticket, caller()).unwrap();
        assert!(matches!(
            self.sender.consume(receipt, caller()),
            Err(Failure::Busy)
        ));
    }
    pub(crate) fn consume_and_finish(&mut self) {
        let receipt = self.sender.terminal_receipt(self.ticket, caller()).unwrap();
        self.sender.consume(receipt, caller()).unwrap();
        assert_eq!(finish(&mut self.sender), Completion::Consumed(self.ticket));
        assert!(matches!(
            self.sender.request(
                self.ticket,
                caller(),
                Operation::Invoke,
                "{}",
                Instant::now()
            ),
            Err(Failure::NotRetained)
        ));
    }
}
