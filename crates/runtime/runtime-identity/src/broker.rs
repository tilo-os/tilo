use std::io;
use std::os::unix::net::UnixStream;

use super::{IdentityMetadata, NotAvailable, SigningIdentity, SigningSource};
pub use tilo_identity_client::legacy::{BROKER_CHILD_FD, BROKER_ENV_VAR};
use tilo_identity_client::legacy::{OP_METADATA, OP_SIGN, PublicMetadata, read_frame, write_frame};

/// What `sign` always answers. The broker holds no key, and the checked path
/// for application signatures is the identity service.
const SIGN_REFUSAL: &[u8] = b"the root signing key is never brokered to an application; \
                              request signatures through the identity service";

/// Serve one application's inherited connection until it exits or closes it:
/// `metadata` answers the metadata operation, and every `sign` is refused.
///
/// # Errors
///
/// Returns an error if a frame can't be read or written, other than the
/// peer closing the connection (which ends the loop with `Ok(())`).
pub fn serve_metadata(mut stream: UnixStream, metadata: &IdentityMetadata) -> io::Result<()> {
    let encoded = serde_json::to_vec(&PublicMetadata {
        profile_photo: metadata.profile_photo.clone(),
        display_name: metadata.display_name.clone(),
        avatar_color: metadata.avatar_color,
        group_public_key: metadata.group_public_key,
        threshold: metadata.threshold,
        total_shares: metadata.total_shares,
        share_index: metadata.share_index,
        single_device: metadata.single_device,
    })
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    loop {
        let (op, payload) = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        match op {
            OP_METADATA if payload.is_empty() => write_frame(&mut stream, 0, &encoded)?,
            OP_SIGN => write_frame(&mut stream, 1, SIGN_REFUSAL)?,
            _ => write_frame(&mut stream, 1, b"unsupported identity broker operation")?,
        }
    }
}

pub(crate) fn connect_from_env() -> Result<SigningIdentity, NotAvailable> {
    let connection = tilo_identity_client::legacy::connect_from_env()?;
    let mut identity = super::from_broker(std::sync::Arc::clone(&connection));
    identity.source = SigningSource::Broker(connection);
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::IntoRawFd;
    use tilo_identity_client::legacy::FRAMING;

    fn ada() -> IdentityMetadata {
        IdentityMetadata {
            profile_photo: None,
            display_name: "Ada".into(),
            avatar_color: [1, 2, 3],
            group_public_key: [42; 32],
            threshold: 1,
            total_shares: 1,
            share_index: 1,
            single_device: true,
        }
    }

    #[test]
    fn broker_answers_metadata_and_refuses_every_sign_over_pinned_bounded_frames() {
        {
            let (host, mut app) = UnixStream::pair().unwrap();
            let thread = std::thread::spawn(move || serve_metadata(host, &ada()).unwrap());

            write_frame(&mut app, OP_METADATA, &[]).unwrap();
            let (status, metadata) = read_frame(&mut app).unwrap();
            assert_eq!(status, 0);
            let served: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
            let group_public_key = [42_u8; 32];
            assert_eq!(
                served,
                serde_json::json!({
                    "display_name": "Ada",
                    "avatar_color": [1, 2, 3],
                    "group_public_key": group_public_key,
                    "threshold": 1,
                    "total_shares": 1,
                    "share_index": 1,
                    "single_device": true,
                })
            );

            write_frame(&mut app, OP_SIGN, b"brokered operation").unwrap();
            let (status, reply) = read_frame(&mut app).unwrap();
            assert_eq!(status, 1, "the broker must never sign for an app");
            assert_eq!(
                reply,
                b"the root signing key is never brokered to an application; \
                  request signatures through the identity service"
            );
            write_frame(&mut app, OP_METADATA, &[]).unwrap();
            assert_eq!(read_frame(&mut app).unwrap(), (0, metadata));
            write_frame(&mut app, 9, &[]).unwrap();
            assert_eq!(read_frame(&mut app).unwrap().0, 1);
            drop(app);
            thread.join().unwrap();
        }
        {
            let (mut left, mut right) = UnixStream::pair().unwrap();
            write_frame(&mut left, OP_SIGN, b"hi").unwrap();
            let mut sent = [0_u8; 7];
            right.read_exact(&mut sent).unwrap();
            assert_eq!(sent, [OP_SIGN, 0, 0, 0, 2, b'h', b'i']);
            left.write_all(&[1, 0, 0, 0, 3, 7, 8, 9]).unwrap();
            assert_eq!(read_frame(&mut right).unwrap(), (1, vec![7, 8, 9]));
            left.write_all(&[OP_METADATA, 0, 0, 0, 0]).unwrap();
            assert_eq!(read_frame(&mut right).unwrap(), (OP_METADATA, Vec::new()));
        }
        {
            let (mut left, mut right) = UnixStream::pair().unwrap();
            left.write_all(&[OP_SIGN]).unwrap();
            left.write_all(
                &(u32::try_from(FRAMING.max_len()).expect("limit fits in u32") + 1).to_be_bytes(),
            )
            .unwrap();
            assert_eq!(
                read_frame(&mut right).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn inherited_broker_fd_is_consumed_exactly_once() {
        let (host, app) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || serve_metadata(host, &ada()).unwrap());
        let raw = app.into_raw_fd();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(BROKER_ENV_VAR, raw.to_string());
        let connected = connect_from_env().unwrap();
        assert_eq!(connected.display_name(), "Ada");
        assert_eq!(connected.group_public_key(), [42; 32]);
        assert!(connected.is_single_device());
        assert!(
            connected.sign(b"anything").is_err(),
            "an app's broker connection never yields a signature"
        );
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
