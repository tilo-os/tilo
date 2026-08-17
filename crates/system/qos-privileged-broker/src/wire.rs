//! The mediated channel — a length-prefixed JSON framing over a Unix socket.
//!
//! The launcher opens a socket pair, keeps one end, and passes the other into
//! the sandboxed app as a connected file descriptor (the same shape as every
//! other qOS system service, see `runtime-portal`). The app writes one
//! [`crate::BrokerRequest`] frame and reads one [`crate::BrokerResponse`]
//! frame back. Deliberately not Cap'n Proto: a hand-audited JSON framing is
//! small enough to read in full, and the broker's whole job is to be
//! auditable.
//!
//! Wire frame: a 4-byte big-endian length, then that many bytes of JSON. The
//! length is capped so a malicious or corrupt peer cannot make the broker
//! allocate an unbounded buffer.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Broker, BrokerRequest, BrokerResponse, ConsentOracle, FirewallApplier};
use pkg::capabilities::Capabilities;

/// The largest frame the broker will read — a firewall plan is a few hundred
/// short command lines, so 1 MiB is generous and still bounds the allocation.
pub const MAX_FRAME: u32 = 1024 * 1024;

/// Transport / protocol errors on the broker channel.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("broker channel I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("broker frame too large: {0} bytes (max {MAX_FRAME})")]
    FrameTooLarge(u32),
    #[error("broker message encode/decode: {0}")]
    Codec(#[from] serde_json::Error),
}

/// Write one length-prefixed JSON frame.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), WireError> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| WireError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(WireError::FrameTooLarge(len));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// Read one length-prefixed JSON frame.
pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T, WireError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(WireError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

/// The app-side client. Wraps the connected socket the launcher handed in and
/// speaks one request → one response.
pub struct BrokerClient {
    stream: UnixStream,
}

impl BrokerClient {
    /// Wrap an already-connected stream (the fd the launcher passed in).
    pub fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Connect to a broker listening on `path`. Used in dev / tests where the
    /// broker runs as a plain process rather than being plumbed in by the
    /// launcher.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, WireError> {
        Ok(Self::from_stream(UnixStream::connect(path)?))
    }

    /// Send `request` and read the broker's single response.
    pub fn request(&mut self, request: &BrokerRequest) -> Result<BrokerResponse, WireError> {
        write_message(&mut self.stream, request)?;
        read_message(&mut self.stream)
    }
}

/// Handle exactly one request on an accepted connection: read the request,
/// run it through the broker's three gates and applier for `app_id` /
/// `declared` / `consent`, and write the response.
///
/// The compositor calls this per accepted connection, having resolved
/// `app_id` and the app's `declared` capabilities from its per-workspace
/// PID→app map and the installed manifest, and having built a `consent`
/// oracle over the workspace's capability list. Reading the *identity* from
/// the connection rather than from the request is what keeps an app from
/// speaking for another: it cannot claim a different `app_id`.
pub fn serve_connection<F: FirewallApplier>(
    stream: &mut UnixStream,
    broker: &mut Broker<F>,
    app_id: &str,
    declared: &Capabilities,
    consent: &dyn ConsentOracle,
) -> Result<(), WireError> {
    let request: BrokerRequest = read_message(stream)?;
    let response = broker.handle(app_id, declared, consent, &request);
    write_message(stream, &response)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DenyReason, FirewallOp, FixedConsent, RecordingFirewall};
    use pkg::capabilities::{PrivilegedCapability, PrivilegedRequest};
    use std::thread;

    fn declares_firewall() -> Capabilities {
        Capabilities {
            privileged: vec![PrivilegedRequest::new(PrivilegedCapability::FirewallAdmin)],
        }
    }

    fn plan() -> BrokerRequest {
        BrokerRequest::Firewall(FirewallOp {
            plan: vec!["firewall-cmd --reload".to_string()],
        })
    }

    #[test]
    fn round_trip_over_a_socket_pair_applies() {
        // The full wire path: a client on one end of a socketpair, the broker
        // serving the other, over the real framing. Only the nft boundary is
        // mocked.
        let (app_end, broker_end) = UnixStream::pair().unwrap();

        let server = thread::spawn(move || {
            let mut broker_end = broker_end;
            let mut broker = Broker::new(RecordingFirewall::default());
            serve_connection(
                &mut broker_end,
                &mut broker,
                "dev.qos.firewall",
                &declares_firewall(),
                &FixedConsent(true),
            )
            .unwrap();
            // Hand the recorder back so the test can assert the applier ran.
            broker.into_firewall_for_test()
        });

        let mut client = BrokerClient::from_stream(app_end);
        let resp = client.request(&plan()).unwrap();
        assert_eq!(resp, BrokerResponse::Applied { commands: 1 });

        let recorder = server.join().unwrap();
        assert_eq!(recorder.applied.len(), 1);
    }

    #[test]
    fn round_trip_denies_without_consent() {
        let (app_end, broker_end) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut broker_end = broker_end;
            let mut broker = Broker::new(RecordingFirewall::default());
            serve_connection(
                &mut broker_end,
                &mut broker,
                "dev.qos.firewall",
                &declares_firewall(),
                &FixedConsent(false),
            )
            .unwrap();
            broker.into_firewall_for_test()
        });

        let mut client = BrokerClient::from_stream(app_end);
        let resp = client.request(&plan()).unwrap();
        assert_eq!(
            resp,
            BrokerResponse::Denied(DenyReason::NoConsent(PrivilegedCapability::FirewallAdmin))
        );
        let recorder = server.join().unwrap();
        assert!(recorder.applied.is_empty());
    }
}
