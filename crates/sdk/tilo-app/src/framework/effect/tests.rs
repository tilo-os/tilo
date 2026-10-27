// Test-only on its own, so the packaging scan of resource kinds, which
// reads each file alone, does not take its probe kind for the SDK's.
#![cfg(test)]

use super::*;
use tilo_resource::{Model, Reader, resource};

#[derive(Model, Default, Debug, Clone)]
pub struct Probe {
    pub count: i64,
}

#[resource(kind = "test.probe", label = "Probe")]
impl Probe {
    /// Set the count.
    #[action]
    pub fn set(&mut self, count: i64) {
        self.count = count;
    }
}

#[derive(Clone, Debug, tilo_resource::Effect)]
#[effect(operation = "test.probe.effects")]
enum Work {
    /// Count until cancelled.
    #[effect(cancellable)]
    Count,
    /// Answer at once.
    Ping,
}

fn runner() -> EffectRunner<Probe, (), Work> {
    runner_on(Dispatcher::new("test", Probe::KIND, false, None))
}

fn runner_on(dispatch: Dispatcher<Probe>) -> EffectRunner<Probe, (), Work> {
    let mut doc = tilo_resource::Doc::new();
    Probe::initialize(&mut doc).unwrap();
    EffectRunner::new(EffectEnv {
        dispatch,
        live: Live::new(Reader::<Probe>::read(&doc).unwrap()),
        wake: WakeHandle::detached(),
        scope: None,
    })
}

#[test]
fn blocking_cancellation_signal_tracks_actual_job_and_preserves_custody_checks() {
    let mut runner = runner();
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let (entered, entrance) = channel();
    let (stopped, observation) = channel();
    runner
        .start(&op, &mut (), None, |(), _, cx| {
            Ok(cx.thread(
                move |progress| {
                    assert!(!progress.is_cancelled());
                    let signal = progress.cancellation_signal();
                    entered.send(()).unwrap();
                    while !signal.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    assert!(progress.is_cancelled());
                    let mut work_ran = false;
                    assert!(
                        progress
                            .with_effect(|| {
                                work_ran = true;
                                Ok(())
                            })
                            .is_err()
                    );
                    assert!(!work_ran);
                    stopped.send(()).unwrap();
                },
                |(), (), _| Ok(Effected::done()),
            ))
        })
        .unwrap();
    entrance.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert!(matches!(
        runner.poll(invocation, true, &mut ()),
        Some(InvocationUpdate::Cancelled { .. })
    ));
    observation
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    assert!(runner.poll(invocation, false, &mut ()).is_none());
}

#[test]
fn what_an_effect_submits_continues_its_invocation() {
    let dispatch = Dispatcher::new("test", Probe::KIND, false, None);
    let mut runner = runner_on(dispatch.clone());
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    runner
        .start(&op, &mut (), None, |(), _, cx| {
            cx.dispatcher().effect(&Work::Ping);
            Ok(cx.thread(
                |_| (),
                |(), (), cx| {
                    cx.dispatcher().effect(&Work::Ping);
                    Ok(Effected::done())
                },
            ))
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !runner
        .poll(invocation, false, &mut ())
        .is_some_and(|update| update.is_terminal())
    {
        assert!(std::time::Instant::now() < deadline, "the job finishes");
        std::thread::yield_now();
    }
    dispatch.effect(&Work::Ping);
    let continues = std::iter::from_fn(|| dispatch.core().take_attributed())
        .map(|(_, on_behalf_of)| on_behalf_of)
        .collect::<Vec<_>>();
    assert_eq!(
        continues,
        [Some(invocation), Some(invocation), None],
        "the handler's and the completion's continue the invocation; \
         the app's own continues nothing"
    );
}

fn payload(effect: &Work) -> Vec<u8> {
    Work::specs()
        .into_iter()
        .find(|spec| spec.id == effect.id())
        .unwrap()
        .build_json(&effect.arguments())
        .unwrap()
        .payload
}

#[test]
fn approved_check_reports_use_original_effect_progress_and_freeze_after_cancel() {
    let mut runner = runner();
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let (forward, reports) = channel();
    let checked: ui_core::invocation_scope::ApprovedCheckReportedWork = Box::new(move |report| {
        forward.send(report).unwrap();
        Box::pin(std::future::pending())
    });
    let started = runner
        .start(&op, &mut (), None, |(), _, cx| {
            Ok(cx.spawn(
                move |progress| attach_approved_check_reporter(checked, progress),
                |(), _receipt, _| Ok(Effected::Done(ActionOutput::default())),
            ))
        })
        .unwrap();
    assert!(matches!(started, Some(ResourceOperationStart::Accepted)));
    let report = reports.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    for (sequence, phase) in [(1, "Synthesis"), (2, "Routing")] {
        report(
            ui_core::invocation_scope::ApprovedCheckReportObservation::Reported {
                phase: phase.into(),
                completed: Some(sequence),
                total: Some(2),
            },
        )
        .unwrap();
        assert!(matches!(runner.poll(invocation, false, &mut ()),
            Some(InvocationUpdate::Progress { invocation: actual, sequence: observed,
                phase: actual_phase, completed: Some(done), total: Some(2), message: None })
            if actual == invocation && observed == sequence && actual_phase == phase && done == sequence));
        assert!(runner.poll(invocation, false, &mut ()).is_none());
    }
    assert!(matches!(runner.poll(invocation, true, &mut ()),
        Some(InvocationUpdate::Cancelled { invocation: actual, .. }) if actual == invocation));
    assert!(
        report(
            ui_core::invocation_scope::ApprovedCheckReportObservation::Reported {
                phase: "Late".into(),
                completed: None,
                total: None,
            }
        )
        .is_err()
    );
    assert!(runner.poll(invocation, false, &mut ()).is_none());
    drop(report);
    drop(reports);
    drop(runner);
}

#[test]
fn cancelling_a_job_stops_its_worker_and_drops_its_result() {
    let mut runner = runner();
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let (stopped, observed) = channel();
    let started = runner
        .start(&op, &mut (), None, |(), _, cx| {
            Ok(cx.thread(
                move |progress| {
                    while !progress.is_cancelled() {
                        std::thread::yield_now();
                    }
                    let _ = stopped.send(());
                    7
                },
                |(), count: i32, _| Ok(Effected::value(count)),
            ))
        })
        .unwrap();
    assert!(matches!(started, Some(ResourceOperationStart::Accepted)));
    assert!(runner.poll(invocation, false, &mut ()).is_none());
    assert!(matches!(
        runner.poll(invocation, true, &mut ()),
        Some(InvocationUpdate::Cancelled { .. })
    ));
    // The worker stops, or never starts when cancelled first.
    assert!(!matches!(
        observed.recv_timeout(tilo_test_env::hang_bound()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(
        runner.poll(invocation, false, &mut ()).is_none(),
        "a cancelled job's result is dropped"
    );
}

#[test]
fn a_failed_handler_cancels_the_job_it_already_started() {
    let mut runner = runner();
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let mut progress = None;
    let result = runner.start(&op, &mut (), None, |(), _, cx| {
        let reply = cx.reply();
        progress = Some(reply.progress().clone());
        Err("handler refused after starting work".into())
    });
    assert!(result.is_err());
    assert!(progress.unwrap().is_cancelled());
    assert!(runner.poll(invocation, false, &mut ()).is_none());
}

#[test]
fn dropping_the_handler_cancels_its_retained_jobs() {
    let mut runner = runner();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        InvocationId::new(),
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let mut progress = None;
    assert!(matches!(
        runner
            .start(&op, &mut (), None, |(), _, cx| {
                let reply = cx.reply();
                progress = Some(reply.progress().clone());
                Ok(Effected::Pending)
            })
            .unwrap(),
        Some(ResourceOperationStart::Accepted)
    ));
    let progress = progress.unwrap();
    assert!(!progress.is_cancelled());
    drop(runner);
    assert!(progress.is_cancelled());
    let mut work_ran = false;
    assert!(
        progress
            .with_effect(|| {
                work_ran = true;
                Ok(())
            })
            .is_err()
    );
    assert!(!work_ran, "retired work must not enter its effect callback");
}

/// A handler reads the document it answers about, now and at a past
/// version, and the document is back with its owner once it answered.
#[test]
fn a_handler_reads_the_document_now_and_at_a_version() {
    use ui_core::automerge::{ReadDoc, transaction::Transactable};
    let mut runner = runner();
    let mut doc = tilo_resource::Doc::new();
    doc.put(ui_core::automerge::ROOT, "count", 1_i64).unwrap();
    let first = tilo_resource::history::commit_stamped(&mut doc).unwrap();
    doc.put(ui_core::automerge::ROOT, "count", 2_i64).unwrap();
    tilo_resource::history::commit_stamped(&mut doc);
    let payload = payload(&Work::Ping);
    let op = ResourceOperation::local(
        InvocationId::new(),
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let count = |doc: &tilo_resource::Doc| {
        doc.get(ui_core::automerge::ROOT, "count")
            .unwrap()
            .and_then(|(value, _)| value.as_i64())
    };
    let before = (doc.base_heads(), doc.pending_ops());
    let answered = runner
        .start(&op, &mut (), Some(&doc), |(), _, cx| {
            let now = count(&cx.document().expect("lent to the handler"));
            let past = cx.version(&first.to_string())?;
            assert_eq!(past.heads, vec![first]);
            Ok(Effected::value([now, count(&past.doc)]))
        })
        .unwrap();
    assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    assert!(matches!(
        answered,
        Some(ResourceOperationStart::Completed(output))
            if output.value == Some(serde_json::json!([2, 1]))
    ));
    assert_eq!(count(&doc), Some(2), "the document is back with its owner");
    let refused = runner
        .start(&op, &mut (), None, |(), _, cx| {
            cx.version(&first.to_string()).map(|_| Effected::done())
        })
        .unwrap_err();
    assert!(refused.contains("no document"), "{refused}");
}

/// An effect waiting to show another record here: the opener it asked,
/// and the requests it made.
fn opener() -> (
    super::super::OpenHere,
    Rc<RefCell<Vec<ui_core::application::ReplacementOutcome>>>,
) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let requests = Rc::clone(&asked);
    let opener = super::super::OpenHere::new(move |_| {
        let outcome = ui_core::application::ReplacementOutcome::default();
        requests.borrow_mut().push(outcome.clone());
        outcome
    });
    (opener, asked)
}

fn open_here_job(
    runner: &mut EffectRunner<Probe, (), Work>,
    opener: &super::super::OpenHere,
) -> InvocationId {
    let invocation = InvocationId::new();
    let payload = payload(&Work::Count);
    let op = ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let opener = opener.clone();
    runner
        .start(&op, &mut (), None, move |(), _, cx| {
            Ok(cx.open_here(&opener, &["b.pdf".into()], |(), answer, _| {
                answer.map(|()| Effected::value("shown"))
            }))
        })
        .unwrap();
    invocation
}

#[test]
fn opening_here_answers_with_the_hosts_answer_or_withdraws_when_cancelled() {
    let (opener, asked) = opener();
    let mut runner = runner();

    let shown = open_here_job(&mut runner, &opener);
    assert!(
        runner.poll(shown, false, &mut ()).is_none(),
        "waits for the host"
    );
    asked.borrow()[0].answer(Ok(()));
    assert!(matches!(
        runner.poll(shown, false, &mut ()),
        Some(InvocationUpdate::Succeeded { output, .. })
            if output.value == Some(serde_json::json!("shown"))
    ));

    let refused = open_here_job(&mut runner, &opener);
    asked.borrow()[1].answer(Err("the file is gone".into()));
    assert!(matches!(
        runner.poll(refused, false, &mut ()),
        Some(InvocationUpdate::Failed { error, .. }) if error.message == "the file is gone"
    ));

    let cancelled = open_here_job(&mut runner, &opener);
    assert!(matches!(
        runner.poll(cancelled, true, &mut ()),
        Some(InvocationUpdate::Cancelled { .. })
    ));
    assert!(
        !asked.borrow()[2].withdraw(),
        "the cancelled effect withdrew its request before the host took it"
    );
}

#[test]
fn an_effect_declared_immediate_cannot_answer_later() {
    let mut runner = runner();
    let payload = payload(&Work::Ping);
    let op = ResourceOperation::local(
        InvocationId::new(),
        tilo_actions::OperationKind::Effect,
        Work::OPERATION,
        &payload,
    );
    let refused = runner
        .start(&op, &mut (), None, |(), _, cx| {
            let _reply = cx.reply();
            Ok(Effected::Pending)
        })
        .unwrap_err();
    assert!(refused.contains("#[effect(job)]"), "{refused}");
    let answered = runner
        .start(&op, &mut (), None, |(), _, _| Ok(Effected::value("pong")))
        .unwrap();
    assert!(matches!(
        answered,
        Some(ResourceOperationStart::Completed(output))
            if output.value == Some(serde_json::json!("pong"))
    ));
}
