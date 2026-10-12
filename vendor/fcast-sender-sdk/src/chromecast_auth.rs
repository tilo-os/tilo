//! Protocol-only receiver authentication before any Cast control or media URL.
//! Anchors never enter ambient `WebPKI`, OS or Tilo collaborator trust stores.
use super::{auth_random, auth_rng, pki, protocol, tls_runtime, wire_proto};
use auth_rng::TryRng as _;
use pki::UnixTime;
use protocol::protos::CastMessage;
use std::{io, time::Duration};
use tls_runtime::client::TlsStream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use wire_proto::Message as _;
#[path = "chromecast_auth/proto.rs"]
mod proto;
#[path = "chromecast_auth/verify.rs"]
mod verify;

const NAMESPACE: &str = "urn:x-cast:com.google.cast.tp.deviceauth";
const MAX_FRAME: usize = 64 * 1024;
const MAX_CERT: usize = 16 * 1024;
const MAX_CHAIN: usize = 8;
include!("chromecast_auth/authorities.rs");
const AUTHORITIES: Authorities<'static> = Authorities {
    device: &[CAST_ROOT, EUREKA_ROOT],
    revocation: &[CRL_ROOT],
};
struct Authorities<'a> {
    device: &'a [&'a [u8]],
    revocation: &'a [&'a [u8]],
}
fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// Owned challenge state is consumed on every attempted reply. Reconnect creates
/// a new OS-random nonce and binds a different connection's exact TLS leaf.
struct Pending {
    nonce: [u8; 16],
    tls_leaf: Vec<u8>,
}
impl Pending {
    fn new(tls_leaf: &[u8]) -> io::Result<Self> {
        if tls_leaf.is_empty() || tls_leaf.len() > MAX_CERT {
            return Err(invalid("Cast TLS leaf size"));
        }
        let mut nonce = [0; 16];
        auth_random::SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(io::Error::other)?;
        Ok(Self {
            nonce,
            tls_leaf: tls_leaf.to_vec(),
        })
    }
    fn challenge(&self) -> CastMessage {
        let auth = proto::DeviceAuth {
            challenge: Some(proto::Challenge {
                signature_algorithm: Some(1),
                sender_nonce: Some(self.nonce.to_vec()),
                hash_algorithm: Some(1),
            }),
            response: None,
            error: None,
        };
        CastMessage {
            protocol_version: 0,
            source_id: "sender-0".into(),
            destination_id: "receiver-0".into(),
            namespace: NAMESPACE.into(),
            payload_type: 1,
            payload_utf8: None,
            payload_binary: Some(auth.encode_to_vec()),
        }
    }
    fn verify(
        self,
        message: CastMessage,
        authorities: &Authorities<'_>,
        now: UnixTime,
    ) -> io::Result<UnixTime> {
        if message.protocol_version != 0
            || message.namespace != NAMESPACE
            || message.source_id != "receiver-0"
            || message.destination_id != "sender-0"
            || message.payload_type != 1
            || message.payload_utf8.is_some()
        {
            return Err(invalid("Unexpected Cast DeviceAuth envelope"));
        }
        let payload = message
            .payload_binary
            .ok_or_else(|| invalid("Missing DeviceAuth binary payload"))?;
        let auth = proto::DeviceAuth::decode(payload.as_slice())
            .map_err(|_| invalid("Malformed Cast DeviceAuth payload"))?;
        if auth.challenge.is_some() || auth.error.is_some() {
            return Err(invalid(
                "Cast DeviceAuth receiver error or unsolicited challenge",
            ));
        }
        let response = auth
            .response
            .ok_or_else(|| invalid("Missing Cast DeviceAuth response"))?;
        if response.signature_algorithm.unwrap_or(1) != 1
            || response.hash_algorithm != Some(1)
            || response.sender_nonce.as_deref() != Some(self.nonce.as_slice())
        {
            return Err(invalid("Cast DeviceAuth nonce or algorithm mismatch"));
        }
        let tls_expiry = verify::tls_validity(&self.tls_leaf, now)?;
        let path = verify::chain(
            &response.client_auth_certificate,
            &response.intermediate_certificate,
            authorities.device,
            now,
        )?;
        verify::screen_capability(&path)?;
        let crl_expiry =
            verify::revocation(response.crl.as_deref(), authorities.revocation, &path, now)?;
        let mut signed = self.nonce.to_vec();
        signed.extend_from_slice(&self.tls_leaf);
        verify::signature(
            &response.client_auth_certificate,
            &signed,
            &response.signature,
        )?;
        Ok(tls_expiry.min(crl_expiry).min(verify::expiry(&path)?))
    }
}

pub(super) async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    stream: TlsStream<S>,
    cancelled: impl std::future::Future<Output = ()>,
    deadline: Duration,
) -> io::Result<Option<(TlsStream<S>, Window)>> {
    authenticate_with(stream, cancelled, deadline, &AUTHORITIES).await
}
async fn authenticate_with<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: TlsStream<S>,
    cancelled: impl std::future::Future<Output = ()>,
    deadline: Duration,
    authorities: &Authorities<'_>,
) -> io::Result<Option<(TlsStream<S>, Window)>> {
    let leaf = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or_else(|| invalid("Cast TLS peer has no certificate"))?;
    let pending = Pending::new(leaf.as_ref())?;
    let auth = async {
        let challenge = pending.challenge().encode_to_vec();
        stream
            .write_all(
                &u32::try_from(challenge.len())
                    .map_err(|_| invalid("Cast challenge size"))?
                    .to_be_bytes(),
            )
            .await?;
        stream.write_all(&challenge).await?;
        stream.flush().await?;
        let size = stream.read_u32().await? as usize;
        if size == 0 || size > MAX_FRAME {
            return Err(invalid("Cast DeviceAuth frame size"));
        }
        let mut frame = vec![0; size];
        stream.read_exact(&mut frame).await?;
        let message = CastMessage::decode(frame.as_slice())
            .map_err(|_| invalid("Malformed Cast DeviceAuth envelope"))?;
        let expiry = pending.verify(message, authorities, UnixTime::now())?;
        Ok::<_, io::Error>((stream, Window::new(expiry)?))
    };
    tokio::pin!(cancelled);
    tokio::select! {
        biased;
        () = &mut cancelled => Ok(None),
        result = tokio::time::timeout(deadline, auth) => result
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Cast DeviceAuth timed out"))?.map(Some),
    }
}
#[cfg(test)]
#[path = "chromecast_auth/tests.rs"]
mod tests;

/// A verified session cannot outlive its TLS leaf, identity path or signed CRL.
/// This is ephemeral per-connection state, not a pairing or persistent grant.
#[derive(Clone, Copy, Debug)]
pub(super) struct Window {
    expiry: UnixTime,
    deadline: tokio::time::Instant,
}
impl Window {
    fn new(expiry: UnixTime) -> io::Result<Self> {
        let remaining = expiry
            .as_secs()
            .checked_sub(UnixTime::now().as_secs())
            .filter(|seconds| *seconds > 0)
            .ok_or_else(|| invalid("Expired Cast authentication window"))?;
        Ok(Self {
            expiry,
            deadline: tokio::time::Instant::now()
                .checked_add(Duration::from_secs(remaining))
                .ok_or_else(|| invalid("Cast authentication deadline range"))?,
        })
    }
    fn check(self) -> io::Result<()> {
        if UnixTime::now().as_secs() >= self.expiry.as_secs()
            || tokio::time::Instant::now() >= self.deadline
        {
            return Err(invalid("Cast authentication expired"));
        }
        Ok(())
    }
    pub(super) async fn write_frame<W: AsyncWrite + Unpin>(
        self,
        writer: &mut W,
        prefix: &[u8],
        body: &[u8],
    ) -> io::Result<()> {
        self.check()?;
        tokio::select! {
            biased;
            ()=self.expired()=>Err(invalid("Cast authentication expired during write")),
            result=async {writer.write_all(prefix).await?;writer.write_all(body).await}=>result,
        }
    }
    pub(super) async fn expired(self) {
        use std::{future::Future as _, task::Poll};
        let mut deadline = Box::pin(tokio::time::sleep_until(self.deadline));
        let mut clock = tokio::time::interval(Duration::from_secs(1));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        std::future::poll_fn(|cx| {
            if self.check().is_err() || deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(());
            }
            while clock.poll_tick(cx).is_ready() {
                if self.check().is_err() {
                    return Poll::Ready(());
                }
            }
            Poll::Pending
        })
        .await;
    }
}
