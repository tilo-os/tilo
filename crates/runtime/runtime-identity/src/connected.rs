use crate::{
    NotAvailable,
    wire::{Request, Response},
};
use std::os::unix::net::UnixStream;

/// Serve exactly one pre-bound connection. The handler must revalidate current
/// host authority on every request and finish within its host operation budget.
/// Shutdown by closing/shutting down this owned socket; no global identity is read.
pub fn serve(
    mut stream: UnixStream,
    mut handler: impl FnMut(Request) -> Response,
) -> Result<(), NotAvailable> {
    stream.set_nonblocking(false)?;
    loop {
        let request = match crate::wire::FRAMING.read_value(&mut stream) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                let error: std::io::Error = error.into();
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(());
                }
                return Err(NotAvailable::ConnectionFailed(error));
            }
        };
        crate::wire::FRAMING
            .write_value(&mut stream, &handler(request))
            .map_err(|error| NotAvailable::ConnectionFailed(error.into()))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrokerSigningIdentity, wire};
    use std::os::unix::net::UnixStream;
    #[test]
    fn connected_identity_serves_exact_host_metadata_and_denies_signing() {
        let (host, app) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve(host, |request| match request {
                wire::Request::Metadata => wire::Response::Metadata {
                    display_name: "Enrolled".into(),
                    avatar_color: [1, 2, 3],
                    group_public_key: [4; 32],
                    signing_public_key: [5; 32],
                    device_certificate: Some(vec![7; 64]),
                    threshold: 1,
                    total_shares: 1,
                    share_index: 1,
                },
                _ => wire::Response::Denied("revoked".into()),
            })
        });
        let identity = BrokerSigningIdentity::from_connected_io(app).unwrap();
        assert_eq!(identity.metadata().signing_public_key(), [5; 32]);
        assert_eq!(identity.metadata().device_certificate(), Some(&[7; 64][..]));
        assert!(identity.sign_collaboration_join("room").is_err());
        drop(identity);
        assert!(worker.join().unwrap().is_ok());
    }
}
