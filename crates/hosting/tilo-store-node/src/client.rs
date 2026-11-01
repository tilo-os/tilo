//! A member's side of the store protocol: dial a store device, prove which
//! device is calling, and pull what it lacks or push what it published.

use crate::serve::ALPN;
use crate::store::{Cursor, Floor, Replay};
use crate::wire::{PROTOCOL_VERSION, StoreReply, StoreRequest};
use iroh::endpoint::{RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// Why a member's session with a store could not continue.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The connection could not be opened or broke.
    #[error("connection: {0}")]
    Connection(String),
    /// The store refused the device, the workspace or the request.
    #[error("refused: {0}")]
    Refused(String),
    /// A message did not encode or decode.
    #[error("frame: {0}")]
    Frame(String),
    /// The store answered something else than the request asked for.
    #[error("the store answered a {0} to the wrong request")]
    Unexpected(&'static str),
}

/// A member's open session with a store device.
pub struct Link {
    send: SendStream,
    recv: RecvStream,
}

impl Link {
    /// Dial `store` from `endpoint` and introduce this device with `cert`;
    /// its iroh endpoint must be keyed by the same device key.
    ///
    /// # Errors
    ///
    /// [`LinkError::Connection`] when the store cannot be reached,
    /// [`LinkError::Refused`] when it does not admit this device.
    pub async fn connect(
        endpoint: &Endpoint,
        store: impl Into<EndpointAddr>,
        cert: DeviceCert,
        revocations: Vec<RevocationEntry>,
    ) -> Result<Self, LinkError> {
        let connection = endpoint
            .connect(store, ALPN)
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        let mut link = Self { send, recv };
        match link
            .ask(&StoreRequest::Hello {
                protocol: PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations,
            })
            .await?
        {
            StoreReply::Welcome { protocol } if protocol == PROTOCOL_VERSION => Ok(link),
            StoreReply::Welcome { protocol } => Err(LinkError::Refused(format!(
                "the store speaks version {protocol}, not {PROTOCOL_VERSION}"
            ))),
            other => Err(unexpected(&other)),
        }
    }

    /// Ask `workspace`'s store for the frames after `cursors`.
    ///
    /// # Errors
    ///
    /// As [`Self::ask`].
    pub async fn want(&mut self, workspace: u64, cursors: &[Cursor]) -> Result<Replay, LinkError> {
        match self
            .ask(&StoreRequest::Want {
                workspace,
                cursors: cursors.to_vec(),
            })
            .await?
        {
            StoreReply::Frames {
                frames,
                floors,
                more,
            } => {
                let mut decoded = Vec::with_capacity(frames.len());
                for bytes in &frames {
                    decoded.push(
                        postcard::from_bytes(bytes)
                            .map_err(|error| LinkError::Frame(error.to_string()))?,
                    );
                }
                Ok(Replay {
                    frames: decoded,
                    floors,
                    more,
                })
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Offer `frames` of `workspace` to the store. Returns how many it now
    /// holds.
    ///
    /// # Errors
    ///
    /// As [`Self::ask`].
    pub async fn push(
        &mut self,
        workspace: u64,
        frames: &[NeighbourMsg],
    ) -> Result<u32, LinkError> {
        let mut encoded = Vec::with_capacity(frames.len());
        for frame in frames {
            encoded.push(
                postcard::to_allocvec(frame)
                    .map_err(|error| LinkError::Frame(error.to_string()))?,
            );
        }
        match self
            .ask(&StoreRequest::Push {
                workspace,
                frames: encoded,
            })
            .await?
        {
            StoreReply::Acked { accepted, refused } => {
                if !refused.is_empty() {
                    tracing::debug!(?refused, "the store refused some frames");
                }
                Ok(accepted)
            }
            other => Err(unexpected(&other)),
        }
    }

    /// One request, one reply.
    ///
    /// # Errors
    ///
    /// [`LinkError::Connection`] when the stream broke,
    /// [`LinkError::Frame`] when a message does not encode or decode, and
    /// [`LinkError::Refused`] when the store refused.
    pub async fn ask(&mut self, request: &StoreRequest) -> Result<StoreReply, LinkError> {
        let bytes =
            postcard::to_allocvec(request).map_err(|error| LinkError::Frame(error.to_string()))?;
        let len =
            u32::try_from(bytes.len()).map_err(|_| LinkError::Frame("request too large".into()))?;
        self.send
            .write_all(&len.to_le_bytes())
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        self.send
            .write_all(&bytes)
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        let mut len = [0_u8; 4];
        self.recv
            .read_exact(&mut len)
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        let len = usize::try_from(u32::from_le_bytes(len))
            .map_err(|_| LinkError::Frame("reply too large".into()))?;
        let mut reply = vec![0_u8; len];
        self.recv
            .read_exact(&mut reply)
            .await
            .map_err(|error| LinkError::Connection(error.to_string()))?;
        match postcard::from_bytes(&reply).map_err(|error| LinkError::Frame(error.to_string()))? {
            StoreReply::Refused { reason } => Err(LinkError::Refused(reason)),
            reply => Ok(reply),
        }
    }
}

fn unexpected(reply: &StoreReply) -> LinkError {
    LinkError::Unexpected(match reply {
        StoreReply::Welcome { .. } => "welcome",
        StoreReply::Refused { .. } => "refusal",
        StoreReply::Frames { .. } => "reply with frames",
        StoreReply::Acked { .. } => "acknowledgement",
    })
}

/// The cursors that ask a store for every frame it holds, and where a
/// member's own cursors stand after `replay`.
pub fn advance(cursors: &mut Vec<Cursor>, replay: &Replay) {
    for frame in &replay.frames {
        let Ok(frame) = crate::Frame::of(frame) else {
            continue;
        };
        match cursors
            .iter_mut()
            .find(|cursor| cursor.lineage == frame.lineage)
        {
            Some(cursor) => cursor.after_seq = cursor.after_seq.max(frame.seq),
            None => cursors.push(Cursor {
                lineage: frame.lineage,
                after_seq: frame.seq,
            }),
        }
    }
}

/// The floors of `replay` that a member with `cursors` cannot bridge: the
/// store has no frame the member's cursor leads into, so another device of
/// the workspace has to answer for the gap.
#[must_use]
pub fn gaps(cursors: &[Cursor], replay: &Replay) -> Vec<Floor> {
    replay
        .floors
        .iter()
        .filter(|floor| {
            let after = cursors
                .iter()
                .filter(|cursor| cursor.lineage == floor.lineage)
                .map(|cursor| cursor.after_seq)
                .max()
                .unwrap_or(0);
            floor.earliest_seq > after.saturating_add(1)
        })
        .copied()
        .collect()
}
