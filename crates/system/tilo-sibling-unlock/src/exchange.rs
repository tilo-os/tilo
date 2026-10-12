//! The exchange with a sibling over the peer network
//! ([`tilo_unlock_protocol`], ALPN [`UNLOCK_ALPN`]).
//!
//! Early boot binds a fresh iroh endpoint of its own (its identity is the
//! boot key's certificate, not the endpoint), with iroh's relays, so a sibling
//! is reached directly on the local network or through a relay otherwise. All
//! siblings are asked at once: the first release wins, and the owner's denial
//! on any of them ends the attempt.
//!
//! Each exchange answers a fresh challenge from the sibling, signed with the
//! boot key and bound to the device, this boot, the slot and an ephemeral key
//! drawn for this request alone; the half comes back sealed to that key. The
//! frames early boot sent are kept in [`TRANSCRIPT`] (they carry no secret),
//! so a verification run can replay them and see them refused.

use crate::token::{Sibling, Token};
use crate::unlock::{Refusal, Siblings};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh_base::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tilo_unlock_protocol::{
    BootId, BootKeyCert, ClientFrame, Half, PROTOCOL_VERSION, SiblingFrame, UNLOCK_ALPN,
    UnlockHello, UnlockRequest, decode_frame, encode_frame, frame_len, open_release,
};

/// Where the frames of the last exchange are kept for this boot.
pub const TRANSCRIPT: &str = "/run/tilo-sibling-unlock/exchange.json";

/// How long early boot keeps trying to reach a sibling at all.
const REACH_FOR: Duration = Duration::from_secs(300);
/// One dial attempt.
const DIAL_FOR: Duration = Duration::from_secs(20);
/// Between dial attempts, while the network comes up.
const REDIAL_AFTER: Duration = Duration::from_secs(3);
/// How long an exchange may wait for the owner, past the sibling's own
/// approval timeout.
const DECIDE_FOR: Duration =
    Duration::from_secs(tilo_unlock_protocol::APPROVAL_TIMEOUT_NS / 1_000_000_000 + 60);

/// The frames early boot sent one sibling, as kept in [`TRANSCRIPT`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    /// The sibling asked, hex.
    pub sibling: String,
    /// Its address, as the token recorded it.
    pub endpoint: String,
    /// Each [`ClientFrame`], base64 of its encoded frame.
    pub sent: Vec<String>,
    /// How it ended.
    pub outcome: String,
}

/// Reports progress (the approval code) to the person at the boot screen.
pub trait Progress: Sync {
    fn pending(&self, sibling: &Sibling, approval_code: &str);
}

/// The real siblings, over iroh.
pub struct Peers<'a, P: Progress> {
    pub progress: &'a P,
    /// Where to keep the transcript; `None` keeps none.
    pub transcript: Option<&'a Path>,
}

/// Where to dial a sibling.
///
/// # Errors
/// When neither the recorded address nor the key names an endpoint.
pub fn address(sibling: &Sibling) -> Result<EndpointAddr> {
    if !sibling.endpoint.trim().is_empty() {
        return serde_json::from_str(&sibling.endpoint)
            .with_context(|| format!("sibling {} has a malformed address", sibling.device));
    }
    let mut key = [0_u8; 32];
    hex::decode_to_slice(&sibling.device, &mut key)
        .with_context(|| format!("sibling key {:?} is not 32 bytes of hex", sibling.device))?;
    let id = EndpointId::from_bytes(&key).map_err(|error| anyhow!("sibling key: {error}"))?;
    Ok(EndpointAddr::from(id))
}

async fn write_frame(send: &mut SendStream, frame: &ClientFrame) -> Result<Vec<u8>> {
    let bytes = encode_frame(frame).map_err(|error| anyhow!("{error}"))?;
    send.write_all(&bytes)
        .await
        .context("the sibling stopped reading")?;
    Ok(bytes)
}

async fn read_frame(recv: &mut RecvStream) -> Result<SiblingFrame> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix)
        .await
        .context("the sibling closed the stream")?;
    let len = frame_len(prefix).map_err(|error| anyhow!("{error}"))?;
    let mut body = vec![0_u8; len];
    recv.read_exact(&mut body)
        .await
        .context("the sibling closed the stream")?;
    decode_frame(&body).map_err(|error| anyhow!("{error}"))
}

async fn dial(endpoint: &Endpoint, sibling: &Sibling) -> Result<Connection> {
    let addr = address(sibling)?;
    let started = tokio::time::Instant::now();
    loop {
        match tokio::time::timeout(DIAL_FOR, endpoint.connect(addr.clone(), UNLOCK_ALPN)).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(error)) if started.elapsed() >= REACH_FOR => bail!("{error}"),
            Err(_) if started.elapsed() >= REACH_FOR => bail!("no answer"),
            _ => tokio::time::sleep(REDIAL_AFTER).await,
        }
    }
}

/// One exchange with one sibling.
async fn ask(
    endpoint: &Endpoint,
    sibling: &Sibling,
    cert: &BootKeyCert,
    boot_key: &SigningKey,
    boot_id: BootId,
    progress: &impl Progress,
    transcript: &mut Transcript,
) -> Result<Result<Half, Refusal>> {
    let connection = dial(endpoint, sibling).await?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .context("no stream to the sibling")?;
    let hello = ClientFrame::Hello(UnlockHello {
        version: PROTOCOL_VERSION,
        boot_cert: cert.clone(),
    });
    let engine = base64::engine::general_purpose::STANDARD;
    transcript
        .sent
        .push(engine.encode(write_frame(&mut send, &hello).await?));
    let challenge = match read_frame(&mut recv).await? {
        SiblingFrame::Challenge(challenge) => challenge,
        SiblingFrame::Refused(reason) => return Ok(Err(Refusal::Refused(reason))),
        SiblingFrame::Denied => return Ok(Err(Refusal::Denied)),
        _ => bail!("the sibling answered out of order"),
    };
    if hex::encode(challenge.sibling_public_key) != sibling.device.to_ascii_lowercase() {
        bail!("another device answered for {}", sibling.device);
    }
    let ephemeral = tilo_seal::SecretKey::generate().map_err(|error| anyhow!("{error}"))?;
    let ephemeral_public = ephemeral.public_key().map_err(|error| anyhow!("{error}"))?;
    let request = UnlockRequest::sign(boot_key, cert, &challenge, boot_id, &ephemeral_public, None)
        .map_err(|error| anyhow!("{error}"))?;
    let digest = request
        .digest(cert, &challenge.sibling_public_key)
        .map_err(|error| anyhow!("{error}"))?;
    transcript
        .sent
        .push(engine.encode(write_frame(&mut send, &ClientFrame::Request(request)).await?));
    let answer = tokio::time::timeout(DECIDE_FOR, async {
        loop {
            match read_frame(&mut recv).await? {
                SiblingFrame::Pending { approval_code } => {
                    progress.pending(sibling, &approval_code);
                }
                SiblingFrame::Released(release) => {
                    return open_release(&ephemeral, &digest, &release)
                        .map(Ok)
                        .map_err(|error| anyhow!("the released half does not open: {error}"));
                }
                SiblingFrame::Denied => return Ok(Err(Refusal::Denied)),
                SiblingFrame::Refused(reason) => return Ok(Err(Refusal::Refused(reason))),
                SiblingFrame::Challenge(_) => bail!("the sibling challenged twice"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("nobody decided in time"))?;
    let _ = send.finish();
    connection.close(0_u32.into(), b"done");
    answer
}

fn keep(path: Option<&Path>, transcripts: &[Transcript]) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(transcripts) {
        let _ = std::fs::write(path, json);
    }
}

impl<P: Progress> Siblings for Peers<'_, P> {
    async fn release(
        &self,
        token: &Token,
        cert: &BootKeyCert,
        boot_key: &SigningKey,
        boot_id: BootId,
    ) -> Result<Half, Refusal> {
        let endpoint = Endpoint::bind(presets::N0)
            .await
            .map_err(|error| Refusal::Unreachable(format!("no peer-network endpoint: {error}")))?;
        let mut asking = futures::stream::FuturesUnordered::new();
        for sibling in &token.siblings {
            let endpoint = &endpoint;
            asking.push(async move {
                let mut transcript = Transcript {
                    sibling: sibling.device.clone(),
                    endpoint: sibling.endpoint.clone(),
                    ..Transcript::default()
                };
                let answer = ask(
                    endpoint,
                    sibling,
                    cert,
                    boot_key,
                    boot_id,
                    self.progress,
                    &mut transcript,
                )
                .await;
                transcript.outcome = match &answer {
                    Ok(Ok(_)) => "released".into(),
                    Ok(Err(refusal)) => refusal.to_string(),
                    Err(error) => format!("{error:#}"),
                };
                (transcript, answer)
            });
        }
        let mut transcripts = Vec::new();
        let mut refused = None;
        let mut unreachable = Vec::new();
        let result = loop {
            let Some((transcript, answer)) = futures::StreamExt::next(&mut asking).await else {
                break Err(refused.unwrap_or_else(|| Refusal::Unreachable(unreachable.join("; "))));
            };
            let label = transcript.sibling.clone();
            transcripts.push(transcript);
            match answer {
                Ok(Ok(half)) => break Ok(half),
                Ok(Err(Refusal::Denied)) => break Err(Refusal::Denied),
                Ok(Err(refusal)) => refused = Some(refusal),
                Err(error) => unreachable.push(format!("{label}: {error:#}")),
            }
        };
        drop(asking);
        keep(self.transcript, &transcripts);
        endpoint.close().await;
        result
    }
}

/// Sends the frames of a kept transcript to its sibling again, as an
/// eavesdropper replaying a recorded exchange would, and returns the
/// sibling's answer. Verification only: it proves a sibling refuses a
/// replayed request, which can never open anything.
///
/// # Errors
/// When the transcript is unusable or the sibling cannot be reached.
pub async fn replay(transcript: &Transcript) -> Result<String> {
    let sibling = Sibling {
        device: transcript.sibling.clone(),
        label: String::new(),
        endpoint: transcript.endpoint.clone(),
    };
    let endpoint = Endpoint::bind(presets::N0)
        .await
        .context("no peer-network endpoint")?;
    let connection = dial(&endpoint, &sibling).await?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .context("no stream to the sibling")?;
    let engine = base64::engine::general_purpose::STANDARD;
    let mut answers = Vec::new();
    for frame in &transcript.sent {
        let bytes = engine.decode(frame).context("a kept frame is not base64")?;
        send.write_all(&bytes)
            .await
            .context("the sibling stopped reading")?;
        let answer =
            match tokio::time::timeout(Duration::from_secs(30), read_frame(&mut recv)).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(error)) => {
                    answers.push(format!("closed: {error:#}"));
                    break;
                }
                Err(_) => {
                    answers.push("no answer".into());
                    break;
                }
            };
        let done = !matches!(
            answer,
            SiblingFrame::Challenge(_) | SiblingFrame::Pending { .. }
        );
        answers.push(match answer {
            SiblingFrame::Challenge(_) => "challenge".into(),
            SiblingFrame::Pending { approval_code } => format!("pending {approval_code}"),
            SiblingFrame::Released(_) => "released".into(),
            SiblingFrame::Denied => "denied".into(),
            SiblingFrame::Refused(reason) => format!("refused: {reason}"),
        });
        if done {
            break;
        }
    }
    connection.close(0_u32.into(), b"done");
    endpoint.close().await;
    Ok(answers.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_is_dialled_at_its_recorded_address_or_by_its_key() {
        let key = SigningKey::from_bytes(&[5; 32]).verifying_key().to_bytes();
        let by_key = Sibling {
            device: hex::encode(key),
            label: String::new(),
            endpoint: String::new(),
        };
        let addr = address(&by_key).unwrap();
        assert_eq!(addr.id.as_bytes(), &key);
        let recorded = Sibling {
            endpoint: serde_json::to_string(&addr).unwrap(),
            device: "00".repeat(32),
            ..by_key
        };
        assert_eq!(address(&recorded).unwrap(), addr);
        let broken = Sibling {
            endpoint: "{".into(),
            ..recorded
        };
        assert!(address(&broken).is_err());
    }
}
