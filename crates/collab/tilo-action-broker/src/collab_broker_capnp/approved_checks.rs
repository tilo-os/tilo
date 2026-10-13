//! Host-only check execution under the actual accepted target action's custody.
use super::{Arc, CapnpBrokerState, CapnpChannelHandle, OriginalInvocation};
use runtime_collab::approved_checks::{Receipt, Report, ReportObservation, Request};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, Weak};
use tokio::sync::watch;

/// No public constructor or deserializer: only this broker mints custody.
#[derive(Clone)]
pub struct ApprovedCheckContext {
    original: Arc<OriginalInvocation>,
    state: Weak<CapnpBrokerState>,
    check_id: String,
    reports: Option<Arc<ReportPublisher>>,
}
impl ApprovedCheckContext {
    pub fn app_id(&self) -> &str {
        &self.original.route().app_id
    }
    pub fn workspace(&self) -> &str {
        &self.original.route().cluster_id
    }
    pub fn record(&self) -> &str {
        &self.original.route().record_id
    }
    pub fn resource(&self) -> &str {
        &self.original.request().address.resource
    }
    /// The stable identity retained from the authenticated target publication.
    ///
    /// # Errors
    /// Refuses absent or malformed identity; never derives one from caller strings.
    pub fn resource_identity(&self) -> Result<&tilo_actions::ResourceSemanticIdentity, String> {
        self.original.resource_identity()
    }
    pub fn check_id(&self) -> &str {
        &self.check_id
    }
    pub fn invocation(&self) -> String {
        self.original.request().invocation.0.to_string()
    }
    /// Forward a bounded informational observation under this exact run's custody.
    /// It never completes an invocation or establishes command success.
    ///
    /// # Errors
    /// Refuses revoked custody, malformed observations and closed report lanes.
    pub fn report(&self, observation: ReportObservation) -> Result<(), String> {
        self.recheck()?;
        if let Some(reports) = &self.reports {
            reports.publish(self, observation)?;
        }
        Ok(())
    }
    /// Recheck original attribution, current target custody and active cancellation.
    ///
    /// # Errors
    /// Refuses retired workers, cancelled/terminal invocations or changed policy.
    pub fn recheck(&self) -> Result<(), String> {
        let state = self
            .state
            .upgrade()
            .ok_or("approved check broker retired")?;
        self.original.check_execution_frontier(&state)?;
        self.original.check_resource_publication(&state)?;
        self.original.check_lifecycle()?;
        self.original.check_execution_frontier(&state)
    }
}

/// Trusted host adapter; it resolves the exact resource binding, command and bounds.
/// Implementations run on the host effect worker, never the broker's `LocalSet`.
pub trait ApprovedCheckServices: Send + Sync {
    /// Execute this exact approved ID and recheck custody before and during work.
    ///
    /// # Errors
    /// Refuses unsupported registrations, changed authority or unavailable enforcement.
    fn run(&self, context: &ApprovedCheckContext) -> Result<Receipt, String>;
}

pub(super) enum Entry {
    Running,
    Complete(Box<Result<Receipt, String>>),
}
#[derive(Default)]
pub(super) struct Registry(HashMap<u64, Entry>);

fn matched_reply(context: &ApprovedCheckContext, reply: Receipt) -> Result<Receipt, String> {
    reply.encode()?;
    if reply.invocation != context.invocation()
        || reply.check_id != context.check_id()
        || reply.binding.app_id != context.app_id()
        || reply.binding.workspace != context.workspace()
        || reply.binding.record != context.record()
        || reply.binding.resource != context.resource()
    {
        return Err("approved check host reply changed its original resource binding".into());
    }
    context.recheck()?;
    Ok(reply)
}

pub(super) fn context(
    channel: &CapnpChannelHandle,
    request: Request,
) -> Result<ApprovedCheckContext, String> {
    Ok(ApprovedCheckContext {
        original: OriginalInvocation::for_approved_check(channel, &request)?,
        state: Arc::downgrade(&channel.state),
        check_id: request.check_id,
        reports: None,
    })
}

type Reply = Result<Receipt, String>;

pub(super) async fn execute(context: ApprovedCheckContext) -> Result<Vec<u8>, String> {
    let receive = start(&context)?;
    let reply = receive
        .await
        .map_err(|_| "approved-check worker ended without an observed result")??;
    matched_reply(&context, reply)?.encode()
}

fn start(context: &ApprovedCheckContext) -> Result<tokio::sync::oneshot::Receiver<Reply>, String> {
    let state = context
        .state
        .upgrade()
        .ok_or("approved check broker retired")?;
    let id = context.original.request().wire_id;
    context.recheck()?;
    let service = state
        .services
        .approved_checks
        .clone()
        .ok_or("approved project checks unsupported by this host")?;
    // Bound the replay cache to the same retained invocation frontier. Never
    // evict active work or execute a retry after uncertain response delivery.
    let retained: HashSet<_> = state
        .pending_invocations
        .lock()
        .map_err(|_| "invocation registry unavailable")?
        .keys()
        .copied()
        .collect();
    let cached = {
        let mut registry = state
            .approved_check_jobs
            .lock()
            .map_err(|_| "check registry unavailable")?;
        registry
            .0
            .retain(|id, entry| retained.contains(id) || matches!(entry, Entry::Running));
        match registry.0.get(&id) {
            Some(Entry::Running) => {
                return Err(
                    "approved check is already executing; observe the original action".into(),
                );
            }
            Some(Entry::Complete(reply)) => Some(reply.as_ref().clone()),
            None => {
                if registry.0.len() >= 256 {
                    return Err("approved check retained execution limit reached".into());
                }
                registry.0.insert(id, Entry::Running);
                None
            }
        }
    };
    if let Some(reply) = cached {
        let reply = matched_reply(context, reply?);
        let (send, receive) = tokio::sync::oneshot::channel();
        if let Some(reports) = &context.reports {
            reports.finish(reply.clone());
        }
        let _ = send.send(reply);
        return Ok(receive);
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = ApprovedCheckContext::clone(context);
    let weak_state = Arc::downgrade(&state);
    let spawned = std::thread::Builder::new()
        .name("tilo-approved-check".into())
        .spawn(move || {
            // A host adapter panic must retain a terminal refusal, never leave
            // an uncertain execution retryable or permanently marked Running.
            let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker
                    .recheck()
                    .and_then(|()| service.run(&worker))
                    .and_then(|reply| matched_reply(&worker, reply))
            }))
            .unwrap_or_else(|_| {
                Err("approved-check host worker panicked; outcome unobserved".into())
            });
            if let Some(state) = weak_state.upgrade()
                && let Ok(mut registry) = state.approved_check_jobs.lock()
            {
                registry
                    .0
                    .insert(id, Entry::Complete(Box::new(reply.clone())));
            }
            if let Some(reports) = &worker.reports {
                reports.finish(reply.clone());
            }
            let _ = send.send(reply);
        });
    if let Err(error) = spawned {
        if let Ok(mut registry) = state.approved_check_jobs.lock() {
            registry
                .0
                .insert(id, Entry::Complete(Box::new(Err(error.to_string()))));
        }
        return Err(error.to_string());
    }
    drop(state);
    Ok(receive)
}

/// Latest informational report, bounded independently of captured command bytes.
/// The final receipt replaces it and permanently freezes this lane.
#[derive(Clone)]
enum Frame {
    Report(Report),
    Final(Arc<Reply>),
}
struct ReportPublisher {
    state: Mutex<ReportFrontier>,
}
struct ReportFrontier {
    sequence: u64,
    finished: bool,
    disabled: bool,
    send: watch::Sender<Option<Frame>>,
}
impl ReportPublisher {
    fn publish(
        &self,
        context: &ApprovedCheckContext,
        observation: ReportObservation,
    ) -> Result<(), String> {
        let mut frontier = self
            .state
            .lock()
            .map_err(|_| "approved-check report lane unavailable")?;
        if frontier.finished || frontier.disabled {
            return Err("approved-check report lane already finished or disabled".into());
        }
        let sequence = frontier
            .sequence
            .checked_add(1)
            .ok_or("approved-check report sequence exhausted")?;
        let report = Report {
            invocation: context.invocation(),
            check_id: context.check_id().into(),
            sequence,
            observation,
        };
        report.encode()?;
        context.recheck()?;
        frontier.sequence = sequence;
        frontier.disabled = matches!(report.observation, ReportObservation::Disabled { .. });
        // Replacement stays under the same frontier lock as finality.
        frontier.send.send_replace(Some(Frame::Report(report)));
        drop(frontier);
        Ok(())
    }
    fn finish(&self, reply: Reply) {
        if let Ok(mut frontier) = self.state.lock() {
            frontier.finished = true;
            frontier
                .send
                .send_replace(Some(Frame::Final(Arc::new(reply))));
        }
    }
}

pub(super) fn reported_progress(
    mut context: ApprovedCheckContext,
) -> Result<runtime_collab::collab_capnp::approved_check_reported_progress_stream::Client, String> {
    let (send, receive) = watch::channel(None);
    let publisher = Arc::new(ReportPublisher {
        state: Mutex::new(ReportFrontier {
            sequence: 0,
            finished: false,
            disabled: false,
            send,
        }),
    });
    context.reports = Some(publisher);
    // Admission remains exactly the original execution/replay registry.
    // Retain the worker's exact terminal receiver independently of report delivery.
    let completion = start(&context)?;
    Ok(capnp_rpc::new_client(ReportedProgressStream {
        context,
        reader: super::Rc::new(super::AsyncMutex::new(ReportReader {
            receive,
            completion: Some(completion),
            terminal: None,
            initial: true,
        })),
    }))
}
struct ReportedProgressStream {
    context: ApprovedCheckContext,
    reader: super::Rc<super::AsyncMutex<ReportReader>>,
}
struct ReportReader {
    receive: watch::Receiver<Option<Frame>>,
    completion: Option<tokio::sync::oneshot::Receiver<Reply>>,
    terminal: Option<Reply>,
    initial: bool,
}
impl ReportReader {
    async fn next(&mut self) -> Result<Frame, String> {
        if let Some(reply) = &self.terminal {
            return Ok(Frame::Final(Arc::new(reply.clone())));
        }
        loop {
            if self.initial {
                self.initial = false;
            } else if let Some(completion) = &mut self.completion {
                tokio::select! {
                    reply = completion => {
                        let reply = reply.unwrap_or_else(|_| Err("approved-check worker ended without an observed result".into()));
                        self.terminal = Some(reply.clone());
                        self.completion.take();
                        return Ok(Frame::Final(Arc::new(reply)));
                    }
                    changed = self.receive.changed() => {
                        changed.map_err(|_| "approved-check report lane retired")?;
                    }
                }
            } else {
                return Err("approved-check observation ended without a terminal receipt".into());
            }
            let frame = self.receive.borrow_and_update().clone();
            if let Some(frame) = frame {
                if let Frame::Final(reply) = &frame {
                    self.terminal = Some(reply.as_ref().clone());
                    self.completion.take();
                }
                return Ok(frame);
            }
        }
    }
}
impl runtime_collab::collab_capnp::approved_check_reported_progress_stream::Server
    for ReportedProgressStream
{
    fn read(
        self: super::Rc<Self>,
        _params: runtime_collab::collab_capnp::approved_check_reported_progress_stream::ReadParams,
        mut results: runtime_collab::collab_capnp::approved_check_reported_progress_stream::ReadResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        capnp::capability::Promise::from_future(async move {
            self.context.recheck().map_err(capnp::Error::failed)?;
            let frame = self
                .reader
                .lock()
                .await
                .next()
                .await
                .map_err(capnp::Error::failed)?;
            self.context.recheck().map_err(capnp::Error::failed)?;
            match frame {
                Frame::Report(report) => results
                    .get()
                    .set_report(&report.encode().map_err(capnp::Error::failed)?),
                Frame::Final(reply) => match reply.as_ref() {
                    Ok(receipt) => results.get().set_receipt(
                        &matched_reply(&self.context, receipt.clone())
                            .and_then(|receipt| receipt.encode())
                            .map_err(capnp::Error::failed)?,
                    ),
                    Err(error) => {
                        let bounded: String = error.chars().take(512).collect();
                        results.get().set_error(bounded.as_str());
                    }
                },
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod report_delivery_tests {
    use super::*;

    #[tokio::test]
    async fn completion_observation_remains_available_when_informational_lane_cannot_publish() {
        let (_informational, receive) = watch::channel(None);
        let (send, completion) = tokio::sync::oneshot::channel();
        let mut reader = ReportReader {
            receive,
            completion: Some(completion),
            terminal: None,
            initial: true,
        };
        send.send(Err("exact observed worker refusal".into()))
            .unwrap();
        let Frame::Final(reply) = tilo_test_env::within(
            "completion must not depend on informational publication",
            reader.next(),
        )
        .await
        .unwrap() else {
            panic!("expected final observed refusal");
        };
        assert_eq!(
            reply.as_ref().as_ref().unwrap_err(),
            "exact observed worker refusal"
        );
    }

    #[tokio::test]
    async fn lost_completion_reports_unobserved_outcome_instead_of_waiting_forever() {
        let (_informational, receive) = watch::channel(None);
        let (send, completion) = tokio::sync::oneshot::channel::<Reply>();
        let mut reader = ReportReader {
            receive,
            completion: Some(completion),
            terminal: None,
            initial: true,
        };
        drop(send);
        let Frame::Final(reply) =
            tilo_test_env::within("lost worker completion must refuse", reader.next())
                .await
                .unwrap()
        else {
            panic!("expected outcome-unobserved refusal");
        };
        assert!(
            reply
                .as_ref()
                .as_ref()
                .unwrap_err()
                .contains("without an observed result")
        );
    }
}
