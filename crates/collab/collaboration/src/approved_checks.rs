//! Typed bounded jobs on an existing acknowledged record channel.
use runtime_collab::approved_checks::{Receipt, Report, ReportedProgressUpdate, Request};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

use crate::lane::{Lane, Registration};

const MAX_PENDING: usize = 2;
type Reply = Result<Receipt, String>;
type ReportedReply = (oneshot::Receiver<Reply>, watch::Receiver<Option<Report>>);
/// One check queued on a lane, as the broker worker takes it.
pub struct Job {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
    pub reports: Option<watch::Sender<Option<Report>>>,
}
pub(crate) struct Endpoint(mpsc::Sender<Job>);

/// The routes this kind of lane serves.
static LANE: Lane<Endpoint> = Lane::new("approved-check");

pub fn enqueue(route: &str, request: Request) -> Result<oneshot::Receiver<Reply>, String> {
    request.encode()?;
    let endpoint = LANE.endpoint(route)?;
    let (reply, receive) = oneshot::channel();
    endpoint
        .0
        .try_send(Job {
            request,
            reply,
            reports: None,
        })
        .map_err(|_| "approved-check bounded lane unavailable or full")?;
    Ok(receive)
}

pub fn enqueue_reported(route: &str, request: Request) -> Result<ReportedReply, String> {
    request.encode()?;
    let endpoint = LANE.endpoint(route)?;
    let (reply, receive) = oneshot::channel();
    let (reports, observe) = watch::channel(None);
    endpoint
        .0
        .try_send(Job {
            request,
            reply,
            reports: Some(reports),
        })
        .map_err(|_| "approved-check bounded lane unavailable or full")?;
    Ok((receive, observe))
}

/// Register only the capability returned by the existing authenticated join.
/// Duplicate live routes refuse ambiguity rather than selecting another instance.
pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration<Endpoint>, String> {
    let (send, mut receive) = mpsc::channel::<Job>(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    LANE.admit(route, &endpoint)?;
    let worker = tokio::task::spawn_local(async move {
        while let Some(mut job) = receive.recv().await {
            if job.reply.is_closed() {
                continue;
            }
            if job.reports.is_some() {
                let result = async {
                    let mut stream = channel
                        .approved_check_reported_progress(&job.request)
                        .await
                        .map_err(|error| error.to_string())?;
                    loop {
                        let update = tokio::select! {
                            biased;
                            () = job.reply.closed() => return Err("approved-check observer retired".into()),
                            update = stream.read() => update.map_err(|error| error.to_string())?,
                        };
                        match update {
                            ReportedProgressUpdate::Report(report) => {
                                if let Some(reports) = &job.reports {
                                    reports.send_replace(Some(report));
                                }
                            }
                            ReportedProgressUpdate::Receipt(receipt) => return Ok(*receipt),
                        }
                    }
                }
                .await;
                // Closing the sender after terminal delivery freezes the bounded latest report.
                let _ = job.reply.send(result);
                continue;
            }
            let result = channel
                .approved_check(&job.request)
                .await
                .map_err(|error| error.to_string());
            let _ = job.reply.send(result);
        }
    });
    Ok(Registration::new(endpoint, worker))
}

/// A lane admitted on `route` for the toolkit's tests, which take its jobs
/// from the returned receiver; dropping it retires the lane.
#[cfg(any(test, feature = "test-support"))]
pub struct LaneForTest(
    #[allow(dead_code, reason = "held to keep the lane admitted")] Arc<Endpoint>,
);

/// Admit a lane on `route` for a test, as the broker worker admits one on an
/// acknowledged record route.
///
/// # Errors
/// When `route` is invalid, already has a live lane, or too many are live.
#[cfg(any(test, feature = "test-support"))]
pub fn admit_for_test(route: String) -> Result<(LaneForTest, mpsc::Receiver<Job>), String> {
    let (send, receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    LANE.admit(route, &endpoint)?;
    Ok((LaneForTest(endpoint), receive))
}
