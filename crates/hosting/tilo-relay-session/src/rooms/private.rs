//! Closed governor RPCs over authenticated encrypted transport.
//! Private payloads never enter the room log; the trusted host reads plaintext.
//! Variant order is append-only across independently updated devices.
use serde::{Deserialize, Serialize};
use tilo_actions::task_messaging::{TaskMessage, TaskMessageSend};
use tilo_user_identity::DeviceCert;

/// Dedicated authenticated Iroh protocol, not a broadcast action envelope.
pub const PRIVATE_ALPN: &[u8] = b"tilo-governor-private/1";
/// Maximum encoded request, including the bounded private message body.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// Maximum encoded answer, including retained exact-run report bodies.
/// Requests remain independently bounded; exceeding this limit fails, never truncates.
pub const MAX_REPLY_BYTES: usize = 40 * 1024 * 1024;

/// Authenticated private governor operations; variant ordering is stable on the wire.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateOperation {
    /// List contacts permitted by current membership.
    Contacts,
    /// Open the exact private conversation.
    Open {
        /// Exact other participant identifier.
        peer: String,
    },
    /// Read a bounded private message page.
    Read {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
        /// Maximum returned messages.
        limit: u16,
    },
    /// Append an idempotent private message.
    Send {
        /// Exact other participant identifier.
        peer: String,
        /// Idempotency identifier.
        invocation: uuid::Uuid,
        /// Owner-private body text.
        text: String,
    },
    /// Wait for the conversation sequence to change.
    Watch {
        /// Exact other participant identifier.
        peer: String,
        /// Exclusive sequence cursor.
        after_sequence: u64,
    },
    /// Read private instructions for the exact task epoch.
    TaskInstructions {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
    },
    /// Read the authorized task inbox.
    TaskMessages {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
    },
    /// Acknowledge messages incorporated in a model request.
    TaskAcknowledge {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
        /// Stable message identifiers.
        ids: Vec<String>,
    },
    /// Send authenticated task-to-task context.
    TaskSend {
        /// Exact recipient task identifier.
        recipient: String,
        /// Idempotency identifier.
        invocation: uuid::Uuid,
        /// Authenticated task message contents.
        send: TaskMessageSend,
    },
    /// Private native runtime custody; decoded only by the governor.
    NativeTask {
        /// Opaque private command or receipt bytes.
        payload: Vec<u8>,
    },
    /// Host-authenticated exact run; never a person claiming another sender.
    NativeTaskRead {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
        /// Certified executing agent identifier.
        agent: String,
        /// Host-certified exact-run evidence.
        evidence: Vec<u8>,
        /// Requested private operation.
        operation: Box<PrivateOperation>,
    },
    /// Read retained exact-run output receipts.
    TaskOutputs {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
    },
    /// Retain exact-run output receipts.
    TaskRecordOutputs {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
        /// Retained exact-run output references.
        outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
    },
    /// Read original automatic report prose for one authorized run.
    TaskReportTranscript {
        /// Exact task identifier.
        task: String,
        /// Exact run epoch.
        epoch: u64,
    },
    /// Read the full owner-private confirmation for an authorized exact run.
    TaskConfirmation {
        /// Exact task identifier.
        task: String,
        /// Exact task-run epoch.
        epoch: u64,
    },
    /// Digest the authenticated exact run from governor-private custody.
    TaskRunDigest {
        /// Exact task identifier.
        task: String,
        /// Exact run epoch.
        epoch: u64,
        /// Applied direction frontier.
        revision: u64,
    },
    /// Recover original child identity while its exact review is held.
    TaskConfirmationInvocation {
        /// Exact task identifier.
        task: String,
        /// Exact run epoch.
        epoch: u64,
        /// Applied direction frontier.
        revision: u64,
        /// Unique owner review identity.
        asking: uuid::Uuid,
    },
    /// Read the full approved nonsecret MODEL contract under exact native run custody.
    TaskModelContract {
        /// Exact task identifier.
        task: String,
        /// Exact run epoch.
        epoch: u64,
        /// Applied direction frontier.
        revision: u64,
    },
    /// Wait for accepted mail for an exact physical-host-authorized native run.
    /// Must be wrapped in `NativeTaskRead`; never grants read authority.
    TaskMailboxWatch {
        /// Exact recipient task identifier.
        task: String,
        /// Exact recipient run epoch.
        epoch: u64,
        /// Last observed accepted-message sequence, zero for initial observation.
        after_sequence: u64,
    },
}
impl std::fmt::Debug for PrivateOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (operation, len) = match self {
            Self::Contacts => ("contacts", 0),
            Self::Open { .. } => ("open", 0),
            Self::Read { .. } => ("read", 0),
            Self::Send { text, .. } => ("send", text.len()),
            Self::Watch { .. } => ("watch", 0),
            Self::TaskInstructions { .. } => ("task-instructions", 0),
            Self::TaskMessages { .. } => ("task-messages", 0),
            Self::TaskAcknowledge { ids, .. } => ("task-acknowledge", ids.len()),
            Self::TaskSend { .. } => ("task-send", 0),
            Self::NativeTask { payload } => ("native-task", payload.len()),
            Self::NativeTaskRead { .. } => ("native-task-read", 0),
            Self::TaskOutputs { .. } => ("task-outputs", 0),
            Self::TaskRecordOutputs { .. } => ("task-record-outputs", 0),
            Self::TaskReportTranscript { .. } => ("task-report-transcript", 0),
            Self::TaskConfirmation { .. } => ("task-confirmation", 0),
            Self::TaskRunDigest { .. } => ("task-run-digest", 0),
            Self::TaskConfirmationInvocation { .. } => ("task-confirmation-invocation", 0),
            Self::TaskModelContract { .. } => ("task-model-contract", 0),
            Self::TaskMailboxWatch { .. } => ("task-mailbox-watch", 0),
        };
        f.debug_struct("PrivateOperation")
            .field("operation", &operation)
            .field("len", &len)
            .finish()
    }
}
/// An authorized direct-conversation contact.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateContact {
    /// Canonical participant public key.
    pub public_key: String,
    /// Optional participant display name.
    pub display_name: Option<String>,
}
/// A retained owner-private direct message.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateMessage {
    /// Stable record identifier.
    pub id: String,
    /// Authenticated sender identifier.
    pub sender: String,
    /// Monotonic conversation sequence.
    pub sequence: u64,
    /// Receipt timestamp in seconds since the Unix epoch.
    pub received_at: i64,
    /// Owner-private body text.
    pub text: String,
}
/// One bounded page of a private conversation.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateConversation {
    /// Stable record identifier.
    pub id: String,
    /// Exact other participant identifier.
    pub peer: String,
    /// Bounded private message page.
    pub messages: Vec<PrivateMessage>,
    /// Cursor for the next page.
    pub next_sequence: u64,
    /// Whether another page is available.
    pub has_more: bool,
}
/// Private governor reply; debug formatting redacts contents.
#[derive(Clone, Serialize, Deserialize)]
pub enum PrivateReply {
    /// List contacts permitted by current membership.
    Contacts(Vec<PrivateContact>),
    /// A private conversation page.
    Conversation(PrivateConversation),
    /// The direct message was accepted.
    Sent,
    /// The conversation sequence changed.
    Changed(u64),
    /// The operation was refused without exposing private data.
    Refused,
    /// Read private instructions for the exact task epoch.
    TaskInstructions(String),
    /// Read the authorized task inbox.
    TaskMessages(Vec<TaskMessage>),
    /// The accepted task message.
    TaskSent(TaskMessage),
    /// The requested mutation completed.
    Done,
    /// Carry an opaque native owner command.
    NativeTask {
        /// Opaque private owner receipt bytes.
        payload: Vec<u8>,
    },
    /// Read retained exact-run output receipts.
    TaskOutputs(Vec<tilo_actions::task_messaging::TaskOutputRef>),
    /// Original owner-private automatic reports, in acceptance order.
    TaskReportTranscript(Vec<tilo_actions::task_messaging::TaskReport>),
    /// JSON-encoded full private confirmation, absent when no request is current.
    TaskConfirmation(Option<String>),
    /// Digest of the exact authenticated private execution envelope.
    TaskRunDigest([u8; 32]),
    /// Original reviewed child identity, not dispatch authority.
    TaskConfirmationInvocation(uuid::Uuid),
    /// JSON-encoded approved nonsecret contract; never included in room sync.
    TaskModelContract(String),
}
impl std::fmt::Debug for PrivateReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (op, len) = match self {
            Self::Contacts(v) => ("contacts", v.len()),
            Self::Conversation(v) => ("conversation", v.messages.len()),
            Self::Sent => ("sent", 0),
            Self::Changed(_) => ("changed", 0),
            Self::Refused => ("refused", 0),
            Self::TaskInstructions(v) => ("task-instructions", v.len()),
            Self::TaskMessages(v) => ("task-messages", v.len()),
            Self::TaskSent(_) => ("task-sent", 1),
            Self::Done => ("done", 0),
            Self::NativeTask { payload } => ("native-task", payload.len()),
            Self::TaskOutputs(outputs) => ("task-outputs", outputs.len()),
            Self::TaskReportTranscript(reports) => ("task-report-transcript", reports.len()),
            Self::TaskConfirmation(value) => ("task-confirmation", usize::from(value.is_some())),
            Self::TaskRunDigest(_) => ("task-run-digest", 0),
            Self::TaskConfirmationInvocation(_) => ("task-confirmation-invocation", 0),
            Self::TaskModelContract(value) => ("task-model-contract", value.len()),
        };
        f.debug_struct("PrivateReply")
            .field("operation", &op)
            .field("len", &len)
            .finish()
    }
}
/// Certificate-bound private request.
#[derive(Serialize, Deserialize)]
pub struct PrivateRequest {
    /// Current device certificate.
    pub device_cert: DeviceCert,
    /// Exact workspace identifier.
    pub room: String,
    /// Requested private operation.
    pub operation: PrivateOperation,
}
impl std::fmt::Debug for PrivateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.operation.fmt(f)
    }
}
/// Uses the actual endpoint's QUIC certificate, checked against `device_cert`
/// by the governor. No asserted sender or on-behalf-of principal crosses this door.
#[derive(Clone)]
pub struct PrivateClient {
    endpoint: iroh::Endpoint,
    device_cert: DeviceCert,
}
impl PrivateClient {
    /// Bind private exchanges to the endpoint's certified device identity.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint, device_cert: DeviceCert) -> Self {
        Self {
            endpoint,
            device_cert,
        }
    }
    /// One exchange. Watches remain pending until a real pair or authority change.
    /// # Errors
    /// Returns a redacted transport or codec failure.
    pub async fn send(
        &self,
        relay: iroh::EndpointAddr,
        room: String,
        operation: PrivateOperation,
    ) -> Result<PrivateReply, String> {
        if self.endpoint.id().as_bytes() != &self.device_cert.device_public_key {
            return Err("private endpoint certificate mismatch".into());
        }
        let bytes = postcard::to_stdvec(&PrivateRequest {
            device_cert: self.device_cert.clone(),
            room,
            operation,
        })
        .map_err(|_| "private request encoding failed")?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("private request exceeds bound".into());
        }
        let connection = self
            .endpoint
            .connect(relay, PRIVATE_ALPN)
            .await
            .map_err(|_| "private connection failed")?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| "private stream failed")?;
        tilo_relay_client::frame::write_frame(&mut send, &bytes)
            .await
            .map_err(|_| "private write failed")?;
        send.finish().map_err(|_| "private stream finish failed")?;
        let reply = tilo_relay_client::frame::read_frame(&mut recv, MAX_REPLY_BYTES)
            .await
            .map_err(|_| "private read failed")?;
        connection.close(0u32.into(), b"done");
        postcard::from_bytes(&reply).map_err(|_| "private reply decoding failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_task_variants_append_after_person_chat() {
        let instructions = PrivateOperation::TaskInstructions {
            task: "task:one".into(),
            epoch: 1,
        };
        assert_eq!(
            postcard::to_stdvec(&PrivateOperation::Contacts).unwrap()[0],
            0
        );
        assert_eq!(postcard::to_stdvec(&instructions).unwrap()[0], 5);
        let bytes = postcard::to_stdvec(&PrivateOperation::NativeTaskRead {
            task: "task:one".into(),
            epoch: 1,
            agent: "agent-one".into(),
            evidence: vec![1, 2, 3],
            operation: Box::new(instructions),
        })
        .unwrap();
        assert_eq!(bytes[0], 10);
        let decoded: PrivateOperation = postcard::from_bytes(&bytes).unwrap();
        assert!(
            matches!(decoded, PrivateOperation::NativeTaskRead { operation, .. }
            if matches!(*operation, PrivateOperation::TaskInstructions { epoch: 1, .. }))
        );
        let watch = PrivateOperation::TaskMailboxWatch {
            task: "task:one".into(),
            epoch: 1,
            after_sequence: 9,
        };
        let bytes = postcard::to_stdvec(&watch).unwrap();
        assert_eq!(bytes[0], 18);
        assert!(matches!(
            postcard::from_bytes::<PrivateOperation>(&bytes).unwrap(),
            PrivateOperation::TaskMailboxWatch {
                epoch: 1,
                after_sequence: 9,
                ..
            }
        ));
        assert!(!format!("{watch:?}").contains("task:one"));
        assert_eq!(postcard::to_stdvec(&PrivateReply::Refused).unwrap()[0], 4);
        assert_eq!(
            postcard::to_stdvec(&PrivateReply::TaskInstructions("text".into())).unwrap()[0],
            5
        );
    }

    #[test]
    fn debug_never_formats_private_payload() {
        let marker = "private-wire-marker";
        let send = PrivateOperation::Send {
            peer: marker.into(),
            invocation: uuid::Uuid::new_v4(),
            text: marker.into(),
        };
        assert!(!format!("{send:?}").contains(marker));
        let reply = PrivateReply::Conversation(PrivateConversation {
            id: marker.into(),
            peer: marker.into(),
            messages: vec![PrivateMessage {
                id: marker.into(),
                sender: marker.into(),
                sequence: 1,
                received_at: 1,
                text: marker.into(),
            }],
            next_sequence: 1,
            has_more: false,
        });
        assert!(!format!("{reply:?}").contains(marker));
        let read = PrivateOperation::NativeTaskRead {
            task: marker.into(),
            epoch: 1,
            agent: marker.into(),
            evidence: marker.as_bytes().to_vec(),
            operation: Box::new(PrivateOperation::TaskSend {
                recipient: marker.into(),
                invocation: uuid::Uuid::new_v4(),
                send: TaskMessageSend {
                    run_epoch: 1,
                    text: marker.into(),
                },
            }),
        };
        assert!(!format!("{read:?}").contains(marker));
        let native = PrivateOperation::NativeTask {
            payload: marker.as_bytes().to_vec(),
        };
        assert!(!format!("{native:?}").contains(marker));
        assert!(!format!("{:?}", PrivateReply::TaskInstructions(marker.into())).contains(marker));
    }
}
