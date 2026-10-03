//! Transport-independent client for the workspace identity service.

use std::io::{self, Read, Write};
use std::sync::Mutex;

use wire_frame::FrameError;

use crate::{NotAvailable, wire};

trait BrokerIo: Read + Write + Send {}
impl<T: Read + Write + Send> BrokerIo for T {}

/// Public identity metadata plus a live capability to request host signatures.
///
/// This owns no private key and has no app-local key-store fallback. A successful
/// metadata exchange does not authenticate the stream's peer or validate its
/// certificate. Host handoff authentication and downstream certificate/route
/// admission remain mandatory. `Debug` never prints the stream or certificate.
pub struct BrokerSigningIdentity {
    display_name: String,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
    signing_public_key: [u8; 32],
    device_certificate: Option<Vec<u8>>,
    threshold: u16,
    total_shares: u16,
    share_index: u16,
    stream: Mutex<Box<dyn BrokerIo>>,
}

impl std::fmt::Debug for BrokerSigningIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerSigningIdentity")
            .finish_non_exhaustive()
    }
}

impl BrokerSigningIdentity {
    /// Exchange metadata over a caller-owned, connected broker stream.
    ///
    /// Works with synchronous Windows pipe/file adapters as well as Unix
    /// sockets. The stream must implement blocking `Read`/`Write` semantics;
    /// the caller configures any I/O deadlines before transferring ownership.
    /// Use on the SDK signing worker, not the UI thread.
    ///
    /// This performs no discovery, authentication, enrollment or authorization
    /// handshake. The host must already have authenticated and bound this
    /// connection to the app/workspace. The caller must authenticate the host.
    /// A raw endpoint name or same-account connection is not such evidence.
    ///
    /// # Errors
    /// Returns [`NotAvailable::ConnectionFailed`] for I/O, malformed metadata,
    /// or a broker refusal, preserving the existing service protocol errors.
    pub fn from_connected_io<S>(mut stream: S) -> Result<Self, NotAvailable>
    where
        S: Read + Write + Send + 'static,
    {
        write_frame(&mut stream, &wire::Request::Metadata)?;
        match read_frame::<wire::Response>(&mut stream)? {
            wire::Response::Metadata {
                display_name,
                avatar_color,
                group_public_key,
                signing_public_key,
                device_certificate,
                threshold,
                total_shares,
                share_index,
            } => Ok(Self {
                display_name,
                avatar_color,
                group_public_key,
                signing_public_key,
                device_certificate,
                threshold,
                total_shares,
                share_index,
                stream: Mutex::new(Box::new(stream)),
            }),
            wire::Response::Denied(reason) | wire::Response::Error(reason) => {
                Err(NotAvailable::ConnectionFailed(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    reason,
                )))
            }
            wire::Response::Signature(_) | wire::Response::ChannelKeys { .. } => {
                Err(NotAvailable::ConnectionFailed(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "identity broker returned an invalid metadata response",
                )))
            }
        }
    }

    /// Human-picked name supplied by the host.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    /// Avatar RGB supplied by the host.
    pub fn avatar_color(&self) -> [u8; 3] {
        self.avatar_color
    }
    /// Root/group public key supplied by the host.
    pub fn group_public_key(&self) -> [u8; 32] {
        self.group_public_key
    }
    /// Device public key supplied by the host.
    pub fn signing_public_key(&self) -> [u8; 32] {
        self.signing_public_key
    }
    /// Opaque certificate bytes; receiving them does not validate the chain.
    pub fn device_certificate(&self) -> Option<&[u8]> {
        self.device_certificate.as_deref()
    }
    /// Threshold in the host's t-of-n metadata.
    pub fn threshold(&self) -> u16 {
        self.threshold
    }
    /// Total shares in the host's t-of-n metadata.
    pub fn total_shares(&self) -> u16 {
        self.total_shares
    }
    /// Share index supplied by the host.
    pub fn share_index(&self) -> u16 {
        self.share_index
    }
    /// Whether the host reports a 1-of-1 identity.
    pub fn is_single_device(&self) -> bool {
        self.threshold == 1 && self.total_shares == 1
    }

    /// Request the existing generic action grant; no local signing fallback.
    ///
    /// # Errors
    /// Returns the same transport, response or authorization errors as `sign_for`.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
        self.sign_for("generic", message)
    }

    /// Request a signature under the host's live, exact action capability.
    ///
    /// # Errors
    /// Returns transport/framing errors or the broker's authorization refusal.
    pub fn sign_for(&self, action: &str, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
        self.signature(&wire::Request::Sign {
            action: action.to_owned(),
            message: message.to_vec(),
        })
    }

    /// Ask the host to sign its fixed collaboration-join domain for this route.
    ///
    /// The host checks the workspace against the connection's launch binding.
    /// # Errors
    /// Returns transport/framing errors or the broker's authorization refusal.
    pub fn sign_collaboration_join(&self, workspace_id: &str) -> Result<[u8; 64], NotAvailable> {
        self.signature(&wire::Request::CollaborationJoin {
            workspace_id: workspace_id.to_owned(),
        })
    }

    /// Request signing of a typed operation, retaining every route field.
    ///
    /// The host validates the envelope and live role; this client cannot grant it.
    /// # Errors
    /// Returns transport/framing errors or the broker's authorization refusal.
    pub fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], NotAvailable> {
        self.signature(&wire::Request::CollaborationOp {
            workspace_id: workspace_id.to_owned(),
            app_id: app_id.to_owned(),
            record_id: record_id.to_owned(),
            unsigned_op: unsigned_op.to_vec(),
        })
    }

    /// Ask for the content keys of one document route. The host checks the
    /// route against the connection's launch binding and the caller's live
    /// role; it never hands out keys of another app or workspace.
    ///
    /// # Errors
    /// Returns transport/framing errors or the broker's authorization refusal.
    pub fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<wire::ChannelKeysReply, NotAvailable> {
        let mut stream = self.stream.lock().map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other("identity broker lock poisoned"))
        })?;
        write_frame(
            &mut **stream,
            &wire::Request::ChannelKeys {
                workspace_id: workspace_id.to_owned(),
                app_id: app_id.to_owned(),
                record_id: record_id.to_owned(),
                known_generation,
            },
        )?;
        channel_keys_reply(read_frame::<wire::Response>(&mut **stream)?)
    }

    fn signature(&self, request: &wire::Request) -> Result<[u8; 64], NotAvailable> {
        let mut stream = self.stream.lock().map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other("identity broker lock poisoned"))
        })?;
        write_frame(&mut **stream, request)?;
        read_signature_response(&mut **stream)
    }
}

pub(super) fn channel_keys_reply(
    response: wire::Response,
) -> Result<wire::ChannelKeysReply, NotAvailable> {
    match response {
        wire::Response::ChannelKeys {
            access,
            generation,
            keys,
        } => Ok(wire::ChannelKeysReply {
            access,
            generation,
            keys: (!keys.is_empty()).then_some(keys),
        }),
        wire::Response::Denied(reason) => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            reason,
        ))),
        wire::Response::Error(reason) => {
            Err(NotAvailable::ConnectionFailed(io::Error::other(reason)))
        }
        _ => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity broker returned an invalid channel-keys response",
        ))),
    }
}

fn read_signature_response(
    stream: &mut (impl Read + Write + ?Sized),
) -> Result<[u8; 64], NotAvailable> {
    match read_frame::<wire::Response>(stream)? {
        wire::Response::Signature(bytes) if bytes.len() == 64 => {
            let mut signature = [0_u8; 64];
            signature.copy_from_slice(&bytes);
            Ok(signature)
        }
        wire::Response::Denied(reason) => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            reason,
        ))),
        wire::Response::Error(reason) => {
            Err(NotAvailable::ConnectionFailed(io::Error::other(reason)))
        }
        _ => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity broker returned an invalid signature response",
        ))),
    }
}

pub(super) fn write_frame<T: serde::Serialize>(
    stream: &mut (impl Read + Write + ?Sized),
    value: &T,
) -> Result<(), NotAvailable> {
    wire::FRAMING
        .write_value(stream, value)
        .map_err(connection_failed)?;
    stream.flush()?;
    Ok(())
}

pub(super) fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl Read + Write + ?Sized),
) -> Result<T, NotAvailable> {
    wire::FRAMING
        .read_value(stream)
        .map_err(connection_failed)?
        .ok_or_else(|| connection_failed(FrameError::Closed))
}

pub(super) fn connection_failed(error: FrameError) -> NotAvailable {
    NotAvailable::ConnectionFailed(error.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    // Intentionally not a Unix socket: exercises the public constructor with
    // the same short-read/write contract as a synchronous Windows pipe adapter.
    struct FragmentedIo {
        input: Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for FragmentedIo {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = bytes.len().min(3);
            self.input.read(&mut bytes[..count])
        }
    }

    impl Write for FragmentedIo {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = bytes.len().min(2);
            self.output
                .lock()
                .expect("output")
                .extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn metadata() -> wire::Response {
        wire::Response::Metadata {
            display_name: "Fixture".into(),
            avatar_color: [1, 2, 3],
            group_public_key: [4; 32],
            signing_public_key: [5; 32],
            device_certificate: Some(b"opaque fixture certificate".to_vec()),
            threshold: 1,
            total_shares: 1,
            share_index: 1,
        }
    }

    fn channel(responses: &[wire::Response]) -> (FragmentedIo, Arc<Mutex<Vec<u8>>>) {
        let mut input = Cursor::new(Vec::new());
        for response in responses {
            write_frame(&mut input, response).expect("fixture frame");
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        (
            FragmentedIo {
                input: Cursor::new(input.into_inner()),
                output: output.clone(),
            },
            output,
        )
    }

    fn error_kind<T>(result: Result<T, NotAvailable>) -> io::ErrorKind {
        match result {
            Err(NotAvailable::ConnectionFailed(error)) => error.kind(),
            _ => panic!("expected connection failure"),
        }
    }

    #[test]
    fn channel_keys_carry_the_route_and_report_every_access_state() {
        let (stream, output) = channel(&[
            metadata(),
            wire::Response::ChannelKeys {
                access: wire::ChannelAccess::Ready,
                generation: 4,
                keys: vec![1, 2, 3],
            },
            wire::Response::ChannelKeys {
                access: wire::ChannelAccess::Ready,
                generation: 4,
                keys: Vec::new(),
            },
            wire::Response::ChannelKeys {
                access: wire::ChannelAccess::Waiting,
                generation: 5,
                keys: Vec::new(),
            },
            wire::Response::Denied("not a member".into()),
            wire::Response::Signature(vec![1; 64]),
        ]);
        let identity = BrokerSigningIdentity::from_connected_io(stream).expect("metadata");
        let ready = identity
            .channel_keys("workspace", "so.tilo.notes", "record", 0)
            .unwrap();
        assert_eq!(ready.access, wire::ChannelAccess::Ready);
        assert_eq!(ready.keys.as_deref(), Some([1, 2, 3].as_slice()));
        let unchanged = identity
            .channel_keys("workspace", "so.tilo.notes", "record", 4)
            .unwrap();
        assert_eq!(unchanged.keys, None);
        let waiting = identity
            .channel_keys("workspace", "so.tilo.notes", "record", 4)
            .unwrap();
        assert_eq!(waiting.access, wire::ChannelAccess::Waiting);
        assert_eq!(
            error_kind(identity.channel_keys("workspace", "so.tilo.notes", "record", 5)),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            error_kind(identity.channel_keys("workspace", "so.tilo.notes", "record", 5)),
            io::ErrorKind::InvalidData
        );
        let captured = output.lock().expect("output").clone();
        let mut requests = Cursor::new(captured);
        let _ = read_frame::<wire::Request>(&mut requests).expect("metadata request");
        match read_frame::<wire::Request>(&mut requests).expect("keys request") {
            wire::Request::ChannelKeys {
                workspace_id,
                app_id,
                record_id,
                known_generation,
            } => {
                assert_eq!(
                    (
                        workspace_id.as_str(),
                        app_id.as_str(),
                        record_id.as_str(),
                        known_generation
                    ),
                    ("workspace", "so.tilo.notes", "record", 0)
                );
            }
            _ => panic!("wrong channel-keys request"),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn the_client_preserves_routes_and_maps_every_broker_error() {
        // phase: connected_io_metadata_and_signatures_preserve_all_route_fields
        {
            let (stream, output) = channel(&[
                metadata(),
                wire::Response::Signature(vec![11; 64]),
                wire::Response::Signature(vec![12; 64]),
                wire::Response::Signature(vec![13; 64]),
            ]);
            let identity = BrokerSigningIdentity::from_connected_io(stream).expect("metadata");
            assert_eq!(identity.display_name(), "Fixture");
            assert_eq!(identity.avatar_color(), [1, 2, 3]);
            assert_eq!(identity.group_public_key(), [4; 32]);
            assert_eq!(identity.signing_public_key(), [5; 32]);
            assert_eq!(
                identity.device_certificate(),
                Some(b"opaque fixture certificate".as_slice())
            );
            assert_eq!(
                (
                    identity.threshold(),
                    identity.total_shares(),
                    identity.share_index()
                ),
                (1, 1, 1)
            );
            assert!(identity.is_single_device());
            assert_eq!(
                identity
                    .sign_for("document.save", b"message")
                    .expect("action"),
                [11; 64]
            );
            assert_eq!(
                identity.sign_collaboration_join("workspace").expect("join"),
                [12; 64]
            );
            assert_eq!(
                identity
                    .sign_collaboration_op("workspace", "so.tilo.notes", "record", b"op")
                    .expect("op"),
                [13; 64]
            );
            let captured = output.lock().expect("output").clone();
            let mut requests = Cursor::new(captured);
            assert!(matches!(
                read_frame::<wire::Request>(&mut requests).expect("metadata request"),
                wire::Request::Metadata
            ));
            match read_frame::<wire::Request>(&mut requests).expect("sign request") {
                wire::Request::Sign { action, message } => {
                    assert_eq!(action, "document.save");
                    assert_eq!(message, b"message");
                }
                _ => panic!("wrong action request"),
            }
            match read_frame::<wire::Request>(&mut requests).expect("join request") {
                wire::Request::CollaborationJoin { workspace_id } => {
                    assert_eq!(workspace_id, "workspace");
                }
                _ => panic!("wrong join request"),
            }
            match read_frame::<wire::Request>(&mut requests).expect("operation request") {
                wire::Request::CollaborationOp {
                    workspace_id,
                    app_id,
                    record_id,
                    unsigned_op,
                } => {
                    assert_eq!(workspace_id, "workspace");
                    assert_eq!(app_id, "so.tilo.notes");
                    assert_eq!(record_id, "record");
                    assert_eq!(unsigned_op, b"op");
                }
                _ => panic!("wrong operation request"),
            }
            assert_eq!(requests.position(), requests.get_ref().len() as u64);
            assert!(!format!("{identity:?}").contains("opaque fixture certificate"));
        }
        // phase: metadata_errors_keep_existing_permission_denied_mapping
        {
            for response in [
                wire::Response::Denied("denied".into()),
                wire::Response::Error("error".into()),
            ] {
                let (stream, _) = channel(&[response]);
                assert_eq!(
                    error_kind(BrokerSigningIdentity::from_connected_io(stream)),
                    io::ErrorKind::PermissionDenied
                );
            }
            let (stream, _) = channel(&[wire::Response::Signature(vec![0; 64])]);
            assert_eq!(
                error_kind(BrokerSigningIdentity::from_connected_io(stream)),
                io::ErrorKind::InvalidData
            );
        }
        // phase: signing_errors_remain_distinct_and_never_fall_back_to_local_keys
        {
            for (response, expected) in [
                (
                    wire::Response::Denied("role revoked".into()),
                    io::ErrorKind::PermissionDenied,
                ),
                (
                    wire::Response::Error("host unavailable".into()),
                    io::ErrorKind::Other,
                ),
                (
                    wire::Response::Signature(vec![1; 63]),
                    io::ErrorKind::InvalidData,
                ),
                (
                    wire::Response::Signature(vec![1; 65]),
                    io::ErrorKind::InvalidData,
                ),
                (metadata(), io::ErrorKind::InvalidData),
            ] {
                let (stream, _) = channel(&[metadata(), response]);
                let identity = BrokerSigningIdentity::from_connected_io(stream).expect("metadata");
                assert_eq!(error_kind(identity.sign(b"generic")), expected);
            }
        }
        // phase: peer_close_is_an_error_at_bootstrap_and_after_metadata
        {
            let (stream, _) = channel(&[]);
            assert_eq!(
                error_kind(BrokerSigningIdentity::from_connected_io(stream)),
                io::ErrorKind::UnexpectedEof
            );
            let (stream, _) = channel(&[metadata()]);
            let identity = BrokerSigningIdentity::from_connected_io(stream).expect("metadata");
            assert_eq!(
                error_kind(identity.sign_collaboration_join("workspace")),
                io::ErrorKind::UnexpectedEof
            );
        }
    }

    #[test]
    fn the_wire_is_pinned_bounded_and_fails_closed() {
        // phase: wire_bytes_are_pinned
        // The identity service's bytes: a little-endian `u32` length, then
        // postcard. The shell's broker and every packaged app must agree.
        {
            let mut output = Cursor::new(Vec::new());
            write_frame(
                &mut output,
                &wire::Request::Sign {
                    action: "a".into(),
                    message: vec![1, 2],
                },
            )
            .unwrap();
            assert_eq!(output.into_inner(), [6, 0, 0, 0, 1, 1, b'a', 2, 1, 2]);
            let mut input = Cursor::new(vec![4, 0, 0, 0, 2, 2, b'n', b'o']);
            assert!(matches!(
                read_frame::<wire::Response>(&mut input).unwrap(),
                wire::Response::Denied(reason) if reason == "no"
            ));
        }
        // phase: join_signing_domain_is_unchanged
        {
            assert_eq!(
                crate::collaboration_join_signing_bytes("workspace"),
                b"tilo-collaboration-join-v1\0workspace"
            );
        }
        // phase: oversized_frame_is_rejected_before_reading_its_body
        {
            let len = u32::try_from(wire::FRAMING.max_len() + 1).expect("length");
            let mut bytes = Cursor::new(len.to_le_bytes().to_vec());
            assert_eq!(
                error_kind(read_frame::<wire::Response>(&mut bytes)),
                io::ErrorKind::InvalidData
            );
            assert_eq!(bytes.position(), 4);
        }
        // phase: malformed_and_truncated_frames_preserve_error_kinds
        {
            let mut malformed = Cursor::new(vec![1, 0, 0, 0, 255]);
            assert_eq!(
                error_kind(read_frame::<wire::Response>(&mut malformed)),
                io::ErrorKind::InvalidData
            );
            let mut truncated = Cursor::new(vec![2, 0, 0, 0, 1]);
            assert_eq!(
                error_kind(read_frame::<wire::Response>(&mut truncated)),
                io::ErrorKind::UnexpectedEof
            );
        }
        // phase: outbound_limit_rejects_without_writing_partial_frame
        {
            let request = wire::Request::Sign {
                action: "oversized".into(),
                message: vec![0; wire::FRAMING.max_len()],
            };
            let mut output = Cursor::new(Vec::new());
            assert_eq!(
                error_kind(write_frame(&mut output, &request)),
                io::ErrorKind::InvalidInput
            );
            assert!(output.into_inner().is_empty());
        }
        // phase: poisoned_stream_mutex_fails_closed
        {
            let (stream, _) = channel(&[metadata()]);
            let identity =
                Arc::new(BrokerSigningIdentity::from_connected_io(stream).expect("metadata"));
            let child = identity.clone();
            assert!(
                std::thread::spawn(move || {
                    let _guard = child.stream.lock().expect("lock");
                    panic!("fixture poison");
                })
                .join()
                .is_err()
            );
            assert_eq!(error_kind(identity.sign(b"message")), io::ErrorKind::Other);
        }
        // phase: client_can_cross_the_sdk_worker_boundary
        {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<BrokerSigningIdentity>();
        }
    }
}
