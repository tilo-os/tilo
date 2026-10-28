//! One launcher-authenticated stream, owned by one host instance.
use super::{CallFuture, Request, Response, Transport};
use agent_provider_config::protocol::{ErrorKind, ModelError};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

/// Serialized RPC over a duplicated launcher-issued descriptor. No pathname or
/// caller-forged identity is used. Cancellation is negotiated before any work:
/// old gateways fail closed. A background owner cancels the provider and drains
/// through its acknowledgement before handing the connection to the next call.
/// Corruption, EOF or a missing cancellation barrier invalidate the capability.
/// Tokio's I/O and timer drivers must be enabled on the polling runtime.
#[derive(Clone)]
pub struct FdTransport {
    stream: Arc<tokio::sync::Mutex<Option<UnixStream>>>,
    next_id: Arc<AtomicU64>,
}

impl FdTransport {
    /// Duplicate an explicitly supplied launcher capability.
    ///
    /// # Errors
    /// Refuses descriptors that cannot be duplicated or are not connected sockets.
    pub fn new(descriptor: BorrowedFd<'_>) -> Result<Self, String> {
        let owned: OwnedFd = descriptor
            .try_clone_to_owned()
            .map_err(|_| "model gateway descriptor unavailable")?;
        let stream = UnixStream::from(owned);
        stream
            .peer_addr()
            .map_err(|_| "model gateway descriptor is not connected")?;
        Ok(Self {
            stream: Arc::new(tokio::sync::Mutex::new(Some(stream))),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Capture the launcher capability once; never fall back to a socket path.
    ///
    /// # Errors
    /// Refuses an absent, invalid or disconnected launcher capability.
    pub fn from_launcher() -> Result<Self, String> {
        let stream = crate::service_stream().map_err(|_| "model gateway unavailable")?;
        Self::new(stream.as_fd())
    }
}

/// Shutdown also invalidates the launcher's original duplicate on protocol loss.
struct ActiveConnection(Option<tokio::net::UnixStream>);
impl Drop for ActiveConnection {
    fn drop(&mut self) {
        if let Some(stream) = &self.0 {
            // SAFETY: the owned stream keeps this descriptor valid throughout.
            unsafe { libc::shutdown(stream.as_raw_fd(), libc::SHUT_RDWR) };
        }
    }
}

async fn write_request(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    request: Request,
) -> Result<(), ModelError> {
    let body = zeroize::Zeroizing::new(
        serde_json::to_vec(&crate::Frame {
            version: crate::RPC_VERSION,
            value: request,
        })
        .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?,
    );
    if body.len() > 2 * 1024 * 1024 {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    crate::FRAMING
        .write_async(stream, &body)
        .await
        .map_err(|_| ModelError::new(ErrorKind::Transport))
}

async fn read_response(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Response, ModelError> {
    let body = crate::FRAMING
        .read_async(stream)
        .await
        .map_err(|error| {
            ModelError::new(match error {
                wire_frame::FrameError::Io(_) => ErrorKind::Transport,
                wire_frame::FrameError::Truncated | wire_frame::FrameError::Closed => {
                    ErrorKind::Truncated
                }
                _ => ErrorKind::InvalidResponse,
            })
        })?
        .ok_or_else(|| ModelError::new(ErrorKind::Truncated))?;
    serde_json::from_slice(&body).map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
}

async fn exchange(
    stream: &mut tokio::net::UnixStream,
    request: Request,
    id: u64,
    cancelled: &mut tokio::sync::oneshot::Receiver<()>,
    replies: &tokio::sync::mpsc::Sender<Result<Response, ModelError>>,
) -> Result<(), ModelError> {
    // An older peer must never start paid work that cannot be stopped. A
    // rejection is not success and is not silently downgraded to legacy RPC.
    let supported = tokio::time::timeout(Duration::from_secs(2), async {
        write_request(stream, Request::CancellationSupported).await?;
        read_response(stream)
            .await
            .map_err(|_| ModelError::new(ErrorKind::Unsupported))
    })
    .await
    .map_err(|_| ModelError::new(ErrorKind::Unsupported))??;
    if !supported.ok
        || supported.event.is_some()
        || supported.output.as_deref() != Some("cancel-v1")
    {
        return Err(ModelError::new(ErrorKind::Unsupported));
    }
    if cancelled.try_recv() != Err(tokio::sync::oneshot::error::TryRecvError::Empty) {
        return Ok(());
    }
    tokio::time::timeout(
        Duration::from_secs(2),
        write_request(
            stream,
            Request::Cancellable {
                id,
                request: Box::new(request),
            },
        ),
    )
    .await
    .map_err(|_| ModelError::new(ErrorKind::TimedOut))??;
    let (mut reader, mut writer) = stream.split();
    let mut events = 0usize;
    let mut stopping = false;
    let barrier = format!("cancelled:{id}");
    let mut deadline = std::pin::pin!(tokio::time::sleep(Duration::from_secs(120)));
    loop {
        // Keep this exact read future alive when Stop arrives; dropping a
        // partial frame reader would lose framing and contaminate the retry.
        let reading = read_response(&mut reader);
        tokio::pin!(reading);
        let response = loop {
            tokio::select! {
                biased;
                _ = &mut *cancelled, if !stopping => {
                    stopping = true;
                    deadline.as_mut().reset(cancellation_deadline()?);
                    tokio::time::timeout(Duration::from_secs(2), write_request(&mut writer, Request::Cancel { id }))
                        .await.map_err(|_| ModelError::new(ErrorKind::TimedOut))??;
                }
                () = &mut deadline => return Err(ModelError::new(ErrorKind::TimedOut)),
                response = &mut reading => break response?,
            }
        };
        if stopping {
            if response.event.is_none()
                && response.ok
                && response.output.as_deref() == Some(&barrier)
            {
                return Ok(());
            }
        } else if response.event.is_none() {
            let _ = replies.send(Ok(response)).await;
            return Ok(());
        } else if replies.send(Ok(response)).await.is_err() {
            // The UI may disappear while delivery is backpressured. No frame
            // read is in flight here, but provider work still needs Stop.
            stopping = true;
            deadline.as_mut().reset(cancellation_deadline()?);
            tokio::time::timeout(
                Duration::from_secs(2),
                write_request(&mut writer, Request::Cancel { id }),
            )
            .await
            .map_err(|_| ModelError::new(ErrorKind::TimedOut))??;
        }
        events = events.saturating_add(1);
        if events > crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS + 1 {
            return Err(ModelError::new(ErrorKind::InvalidResponse));
        }
    }
}

fn cancellation_deadline() -> Result<tokio::time::Instant, ModelError> {
    tokio::time::Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(|| ModelError::new(ErrorKind::TimedOut))
}

impl Transport for FdTransport {
    fn call<'a>(
        &'a self,
        request: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            tokio::runtime::Handle::try_current()
                .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
            let millis = match &request {
                Request::Complete { timeout_ms, .. }
                | Request::CompleteDefault { timeout_ms, .. }
                | Request::Plan { timeout_ms, .. }
                | Request::Transform { timeout_ms, .. } => (*timeout_ms).clamp(1, 120_000),
                _ => 120_000,
            };
            let id = self
                .next_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
            let slot = self.stream.clone();
            let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
            let (replies, mut received) = tokio::sync::mpsc::channel(8);
            // The owner outlives a dropped UI future, but only through bounded
            // cleanup. Queued calls hold no stream and cannot cancel other work.
            tokio::spawn(async move {
                let mut slot = tokio::select! {
                    biased;
                    _ = &mut cancelled => return,
                    slot = slot.lock() => slot,
                };
                let result = async {
                    let stream = slot
                        .take()
                        .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))?;
                    stream
                        .set_nonblocking(true)
                        .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
                    let mut active = ActiveConnection(Some(
                        tokio::net::UnixStream::from_std(stream)
                            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?,
                    ));
                    // Bound negotiation and writes too, including unresponsive peers.
                    tokio::time::timeout(
                        Duration::from_secs(123),
                        exchange(
                            active
                                .0
                                .as_mut()
                                .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))?,
                            request,
                            id,
                            &mut cancelled,
                            &replies,
                        ),
                    )
                    .await
                    .map_err(|_| ModelError::new(ErrorKind::TimedOut))??;
                    *slot = Some(
                        active
                            .0
                            .take()
                            .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))?
                            .into_std()
                            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?,
                    );
                    Ok::<(), ModelError>(())
                }
                .await;
                if let Err(error) = result {
                    let _ = replies.send(Err(error)).await;
                }
            });
            let operation = async {
                while let Some(reply) = received.recv().await {
                    let response = reply?;
                    if response.event.is_none() {
                        return Ok(response);
                    }
                    if !event(response) {
                        return Err(ModelError::new(ErrorKind::Cancelled));
                    }
                }
                Err(ModelError::new(ErrorKind::Unavailable))
            };
            let result = tokio::time::timeout(Duration::from_millis(millis), operation)
                .await
                .map_err(|_| ModelError::new(ErrorKind::TimedOut))?;
            drop(cancel);
            result
        })
    }
}

#[cfg(test)]
#[path = "transport_fd_tests.rs"]
mod tests;
