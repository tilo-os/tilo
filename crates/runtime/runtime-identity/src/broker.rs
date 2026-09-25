//! A small, per-launch signing broker protocol.
//!
//! The launcher owns the private share and passes the sandbox exactly one end
//! of a connected Unix socket.  The app can read public identity metadata and
//! request signatures, but cannot export the share or enumerate identities.

use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::{NotAvailable, SigningIdentity, SigningSource};

pub const BROKER_ENV_VAR: &str = "TILO_IDENTITY_BROKER_FD";
pub const BROKER_CHILD_FD: RawFd = 15;
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const OP_METADATA: u8 = 1;
const OP_SIGN: u8 = 2;
static BROKER_FD_CLAIMED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize)]
struct PublicMetadata {
    display_name: String,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
    threshold: u16,
    total_shares: u16,
    share_index: u16,
    single_device: bool,
}

fn write_frame(stream: &mut UnixStream, status: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker frame too large",
        ));
    }
    stream.write_all(&[status])?;
    // Bounded by the MAX_MESSAGE check just above.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "payload.len() is checked <= MAX_MESSAGE just above"
    )]
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(payload)
}

fn read_frame(stream: &mut UnixStream) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header)?;
    // `header` is a fixed 5-byte array, so this 4-byte slice always converts.
    let Ok(len_bytes) = header[1..5].try_into() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker frame header malformed",
        ));
    };
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker frame too large",
        ));
    }
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

/// Serve one application's inherited connection until it exits or closes it.
///
/// # Errors
///
/// Returns an error if a frame can't be read or written, other than the
/// peer closing the connection (which ends the loop with `Ok(())`).
#[allow(
    clippy::needless_pass_by_value,
    reason = "public API also called with an owned SigningIdentity from crates/packaging/launcher, outside this pass's scope to update"
)]
pub fn serve(mut stream: UnixStream, identity: SigningIdentity) -> io::Result<()> {
    loop {
        let (op, payload) = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        match op {
            OP_METADATA if payload.is_empty() => {
                let metadata = PublicMetadata {
                    display_name: identity.display_name.clone(),
                    avatar_color: identity.avatar_color,
                    group_public_key: identity.group_public_key,
                    threshold: identity.threshold,
                    total_shares: identity.total_shares,
                    share_index: identity.share_index,
                    single_device: identity.is_single_device(),
                };
                let encoded = serde_json::to_vec(&metadata)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                write_frame(&mut stream, 0, &encoded)?;
            }
            OP_SIGN => match identity.sign_local(&payload) {
                Ok(signature) => write_frame(&mut stream, 0, &signature)?,
                Err(error) => write_frame(&mut stream, 1, error.to_string().as_bytes())?,
            },
            _ => write_frame(&mut stream, 1, b"unsupported identity broker operation")?,
        }
    }
}

pub(crate) fn connect_from_env() -> Result<SigningIdentity, NotAvailable> {
    let raw_value = std::env::var(BROKER_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
    let raw = raw_value.parse::<RawFd>().map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity broker fd",
        ))
    })?;
    if raw < 3 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity broker fd",
        )));
    }
    // Atomically claim the inherited authority before constructing an owner.
    // The environment is process-global and may be read concurrently, so it is
    // intentionally left untouched. A second load() fails before it can create
    // another UnixStream owner for the same raw descriptor.
    BROKER_FD_CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "identity broker fd has already been claimed by this process",
            ))
        })?;
    // SAFETY: Tilo launcher transfers sole ownership of this inherited fd to
    // the app. load() consumes it once, matching runtime-portal's contract.
    let mut stream = unsafe { UnixStream::from_raw_fd(raw) };
    write_frame(&mut stream, OP_METADATA, &[]).map_err(NotAvailable::ConnectionFailed)?;
    let (status, payload) = read_frame(&mut stream).map_err(NotAvailable::ConnectionFailed)?;
    if status != 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity broker refused metadata",
        )));
    }
    let metadata: PublicMetadata = serde_json::from_slice(&payload).map_err(|e| {
        NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    Ok(SigningIdentity {
        display_name: metadata.display_name,
        avatar_color: metadata.avatar_color,
        group_public_key: metadata.group_public_key,
        signing_public_key: metadata.group_public_key,
        device_certificate: None,
        threshold: metadata.threshold,
        total_shares: metadata.total_shares,
        share_index: metadata.share_index,
        source: SigningSource::Broker(Mutex::new(stream)),
        single_device: metadata.single_device,
    })
}

pub(crate) fn sign(stream: &Mutex<UnixStream>, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
    let mut stream = stream.lock().map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::other("identity broker lock poisoned"))
    })?;
    write_frame(&mut stream, OP_SIGN, message).map_err(NotAvailable::ConnectionFailed)?;
    let (status, payload) = read_frame(&mut stream).map_err(NotAvailable::ConnectionFailed)?;
    if status != 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            String::from_utf8_lossy(&payload).into_owned(),
        )));
    }
    payload.try_into().map_err(|payload: Vec<u8>| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("identity broker returned {} signature bytes", payload.len()),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{SigningKey, Verifier};
    use std::os::fd::IntoRawFd;

    #[test]
    fn broker_signs_without_exposing_the_key() {
        let key = SigningKey::from_bytes(&[42; 32]);
        let verifier = key.verifying_key();
        let identity = SigningIdentity::from_local_parts(
            "Ada".into(),
            [1, 2, 3],
            verifier.to_bytes(),
            1,
            1,
            1,
            Some(key),
        );
        let (host, mut app) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || serve(host, identity).unwrap());

        write_frame(&mut app, OP_METADATA, &[]).unwrap();
        let (status, metadata) = read_frame(&mut app).unwrap();
        assert_eq!(status, 0);
        assert!(!String::from_utf8_lossy(&metadata).contains("secret"));

        let message = b"brokered operation";
        write_frame(&mut app, OP_SIGN, message).unwrap();
        let (status, signature) = read_frame(&mut app).unwrap();
        assert_eq!(status, 0);
        verifier
            .verify(
                message,
                &ed25519_dalek::Signature::from_slice(&signature).unwrap(),
            )
            .unwrap();
        drop(app);
        thread.join().unwrap();
    }

    #[test]
    fn oversized_request_is_rejected_before_allocation() {
        let (mut left, mut right) = UnixStream::pair().unwrap();
        left.write_all(&[OP_SIGN]).unwrap();
        left.write_all(
            &(u32::try_from(MAX_MESSAGE).expect("MAX_MESSAGE fits in u32") + 1).to_be_bytes(),
        )
        .unwrap();
        assert_eq!(
            read_frame(&mut right).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn inherited_broker_fd_is_consumed_exactly_once() {
        let key = SigningKey::from_bytes(&[43; 32]);
        let identity = SigningIdentity::from_local_parts(
            "Ada".into(),
            [1, 2, 3],
            key.verifying_key().to_bytes(),
            1,
            1,
            1,
            Some(key),
        );
        let (host, app) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || serve(host, identity).unwrap());
        let raw = app.into_raw_fd();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(BROKER_ENV_VAR, raw.to_string());
        let connected = connect_from_env().unwrap();
        match connect_from_env() {
            Err(NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            }
            other => panic!("second claim did not fail safely: {other:?}"),
        }
        drop(env);
        drop(connected);
        thread.join().unwrap();
    }
}
