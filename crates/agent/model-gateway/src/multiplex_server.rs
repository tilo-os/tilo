//! Bounded concurrent requests on one launcher-authenticated capability.
//! Authorization is still exclusively `handle_streaming`'s actual handoff app.

use super::{handle_streaming, read_request_frame, rejected, send_body};
use std::sync::Arc;
use tilo_model_gateway::{MAX_CONCURRENT_REQUESTS, MAX_QUEUED_RESPONSE_BYTES};
use tilo_model_gateway::{RPC_VERSION, RequestFrame, Response, ResponseFrame};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use zeroize::Zeroizing;

struct Queued {
    body: Zeroizing<Vec<u8>>,
    _charge: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct Output {
    frames: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    failed: watch::Sender<bool>,
}

impl Output {
    fn send(&self, id: u64, response: Response) -> Result<(), ()> {
        let frame = ResponseFrame {
            request_id: id,
            value: response,
        };
        let result = self.enqueue(&frame);
        if result.is_err() {
            let _ = self.failed.send(true);
        }
        result
    }

    fn event(&self, id: u64, response: &Response) -> bool {
        #[derive(serde::Serialize)]
        struct Borrowed<'a> {
            request_id: u64,
            #[serde(flatten)]
            value: &'a Response,
        }
        let result = self.enqueue(&Borrowed {
            request_id: id,
            value: response,
        });
        if result.is_err() {
            let _ = self.failed.send(true);
        }
        result.is_ok()
    }

    fn enqueue(&self, frame: &impl serde::Serialize) -> Result<(), ()> {
        let body = Zeroizing::new(serde_json::to_vec(frame).map_err(|_| ())?);
        // Check encoded frame bounds too: the aggregate queue is not a license
        // to emit one frame the client cannot read.
        if body.len() > 2 * 1024 * 1024 {
            return Err(());
        }
        let amount = u32::try_from(body.len()).map_err(|_| ())?;
        let charge = Arc::clone(&self.bytes)
            .try_acquire_many_owned(amount)
            .map_err(|_| ())?;
        self.frames
            .try_send(Queued {
                body,
                _charge: charge,
            })
            .map_err(|_| ())
    }
}

pub(super) async fn serve(
    stream: tokio::net::UnixStream,
    app_id: String,
    first: RequestFrame,
    http: reqwest::Client,
) {
    let (mut reader, mut writer) = stream.into_split();
    let (frames, mut queued) = mpsc::channel::<Queued>(128);
    let (failed, mut failure) = watch::channel(false);
    let output = Output {
        frames,
        bytes: Arc::new(Semaphore::new(MAX_QUEUED_RESPONSE_BYTES)),
        failed,
    };
    let mut writing = tokio::spawn(async move {
        while let Some(frame) = queued.recv().await {
            send_body(&mut writer, &frame.body).await?;
        }
        Ok::<(), ()>(())
    });
    let mut jobs = tokio::task::JoinSet::new();
    let mut last_id = 0;
    let mut next = Some(first);
    let mut writer_finished = false;
    'connection: loop {
        if let Some(frame) = next.take() {
            let Some(id) = frame.request_id.filter(|id| *id > last_id) else {
                break;
            };
            if jobs.len() >= MAX_CONCURRENT_REQUESTS || *failure.borrow() {
                break;
            }
            last_id = id;
            let output = output.clone();
            let app_id = app_id.clone();
            let http = http.clone();
            jobs.spawn(async move {
                let response = if frame.version == RPC_VERSION {
                    let mut emit = |response: &Response| output.event(id, response);
                    handle_streaming(&app_id, frame.value, &http, &mut emit).await
                } else {
                    rejected("unsupported gateway RPC version")
                };
                output.send(id, response)
            });
        }
        // Keep the read alive when a job finishes: recreating this future
        // after consuming half a prefix would corrupt the next request.
        let reading = read_request_frame(&mut reader);
        tokio::pin!(reading);
        loop {
            tokio::select! {
                biased;
                _ = failure.changed() => break 'connection,
                _ = &mut writing => {
                    writer_finished = true;
                    break 'connection;
                },
                result = jobs.join_next(), if !jobs.is_empty() => {
                    if !matches!(result, Some(Ok(Ok(())))) { break 'connection; }
                }
                result = &mut reading => {
                    let Ok(body) = result else { break 'connection };
                    let Ok(frame) = serde_json::from_slice::<RequestFrame>(&body) else { break 'connection };
                    next = Some(frame);
                    break;
                }
            }
        }
    }
    drop(output);
    // EOF, protocol failure or backpressure cancels provider work rather than
    // retaining detached jobs after the capability/process has gone away.
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
    if !writer_finished {
        writing.abort();
        let _ = writing.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use tilo_model_gateway::{Request, read_frame, write_frame};

    fn authenticated_client() -> (UnixStream, std::thread::JoinHandle<()>) {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_frame(
            &mut client,
            &serde_json::json!({"protocol":1,"app_id":super::super::AGENT_APP}),
        )
        .unwrap();
        server.set_nonblocking(true).unwrap();
        let running = std::thread::spawn(move || {
            let runtime = tilo_rt::daemon("gateway-tagged-test")
                .workers(2)
                .build()
                .unwrap();
            runtime.block_on(async move {
                let stream = tokio::net::UnixStream::from_std(server).unwrap();
                super::super::serve_connection(stream, &super::super::provider_client().unwrap())
                    .await;
            });
        });
        (client, running)
    }

    fn forbidden(id: u64) -> RequestFrame {
        RequestFrame {
            version: RPC_VERSION,
            request_id: Some(id),
            value: Request::SecretStatus {
                profile_id: "private-profile".into(),
            },
        }
    }

    #[test]
    fn tagged_requests_keep_actual_handoff_authorization_and_reject_id_replay() {
        let (mut client, running) = authenticated_client();
        write_frame(&mut client, &forbidden(1)).unwrap();
        let response: ResponseFrame = read_frame(&mut client).unwrap();
        assert_eq!(response.request_id, 1);
        assert!(!response.value.ok);
        assert_eq!(
            response.value.error.as_deref(),
            Some("caller is not authorized for this gateway operation")
        );
        assert!(response.value.output.is_none());
        write_frame(&mut client, &forbidden(1)).unwrap();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        running.join().unwrap();
    }

    #[test]
    fn job_completion_does_not_cancel_a_partially_received_frame() {
        let (mut client, running) = authenticated_client();
        write_frame(&mut client, &forbidden(1)).unwrap();
        let body = serde_json::to_vec(&forbidden(2)).unwrap();
        let prefix = u32::try_from(body.len()).unwrap().to_le_bytes();
        client.write_all(&prefix[..1]).unwrap();
        let first: ResponseFrame = read_frame(&mut client).unwrap();
        assert_eq!(first.request_id, 1);
        // Let the handler's completion be observed while the reader is paused
        // in the rest of the prefix. This must not restart framing at byte 2.
        std::thread::sleep(Duration::from_millis(20));
        client.write_all(&prefix[1..]).unwrap();
        client.write_all(&body).unwrap();
        let second: ResponseFrame = read_frame(&mut client).unwrap();
        assert_eq!(second.request_id, 2);
        assert!(!second.value.ok);
        client.shutdown(std::net::Shutdown::Both).unwrap();
        running.join().unwrap();
    }

    #[test]
    fn response_budget_is_shared_and_released_only_with_queued_frame() {
        let (frames, mut queued) = mpsc::channel(2);
        let (failed, _failure) = watch::channel(false);
        let bytes = Arc::new(Semaphore::new(1024));
        let output = Output {
            frames,
            bytes: Arc::clone(&bytes),
            failed,
        };
        assert!(
            output
                .send(1, super::super::succeeded(Some("small".into()), None))
                .is_ok()
        );
        assert!(bytes.available_permits() < 1024);
        assert!(
            output
                .send(2, super::super::succeeded(Some("x".repeat(1024)), None))
                .is_err()
        );
        drop(output);
        drop(queued.try_recv().unwrap());
        assert_eq!(bytes.available_permits(), 1024);
    }
}
