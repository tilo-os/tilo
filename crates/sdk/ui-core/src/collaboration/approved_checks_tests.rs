//! The approved-check lane as a dispatched operation target uses it: one
//! lane per acknowledged record route, bounded, retired with its transport.

use crate::collaboration::approved_checks::{admit_for_test, enqueue};
use runtime_collab::approved_checks::{Report, Request};
use tokio::sync::mpsc;

#[tokio::test]
async fn approved_check_reported_bridge_forwards_before_terminal_and_freezes_late_reports() {
    use runtime_collab::approved_checks::{ACTION_ID, ReportObservation};
    let route = crate::collaboration::RouteSpec::record(
        "so.tilo.git-client",
        uuid::Uuid::new_v4(),
        "reported-record",
    );
    let (endpoint, mut receive) = admit_for_test(route.channel_id()).unwrap();
    let target = crate::host_operation::OperationTarget::new(
        route,
        tilo_actions::ActionAddress::new("repository", ACTION_ID),
    );
    let invocation = tilo_actions::InvocationId::new();
    let work = target
        .approved_check_reported(invocation, "regression")
        .unwrap();
    let (forward, mut forwarded) = mpsc::unbounded_channel();
    let running = tokio::spawn(work(Box::new(move |observation| {
        forward
            .send(observation)
            .map_err(|_| "observer retired".into())
    })));
    let job = receive.recv().await.unwrap();
    assert_eq!(job.request.invocation, invocation.0.to_string());
    let reports = job.reports.unwrap();
    for (sequence, phase) in [(1, "Synthesis"), (2, "Routing")] {
        let observation = ReportObservation::Reported {
            phase: phase.into(),
            completed: Some(sequence),
            total: Some(2),
        };
        reports.send_replace(Some(Report {
            invocation: invocation.0.to_string(),
            check_id: "regression".into(),
            sequence,
            observation: observation.clone(),
        }));
        assert_eq!(forwarded.recv().await.unwrap(), observation);
        assert!(
            !running.is_finished(),
            "reported phases cannot finish a check"
        );
    }
    job.reply.send(Err("terminal refusal".into())).unwrap();
    assert_eq!(running.await.unwrap(), Err("terminal refusal".into()));
    reports.send_replace(Some(Report {
        invocation: invocation.0.to_string(),
        check_id: "regression".into(),
        sequence: 3,
        observation: ReportObservation::Reported {
            phase: "Late".into(),
            completed: None,
            total: None,
        },
    }));
    assert!(
        forwarded.recv().await.is_none(),
        "terminal completion closes forwarding"
    );
    drop(reports);
    drop(target);
    drop(endpoint);
    drop(receive);
}

#[test]
fn approved_check_bridge_routes_two_live_records_of_one_app_by_actual_dispatch_target() {
    let room = uuid::Uuid::new_v4();
    let first_route =
        crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "first-record");
    let second_route =
        crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "second-record");
    let (first, mut first_receive) = admit_for_test(first_route.channel_id()).unwrap();
    let (_second, mut second_receive) = admit_for_test(second_route.channel_id()).unwrap();
    let address =
        tilo_actions::ActionAddress::new("repository", runtime_collab::approved_checks::ACTION_ID);
    let first_target = crate::host_operation::OperationTarget::new(first_route, address.clone());
    let second_target = crate::host_operation::OperationTarget::new(second_route, address.clone());
    let first_id = tilo_actions::InvocationId::new();
    let second_id = tilo_actions::InvocationId::new();
    let first_reply = first_target.approved_check(first_id, "regression").unwrap();
    assert_eq!(
        first_receive.try_recv().unwrap().request.invocation,
        first_id.0.to_string()
    );
    assert!(
        second_receive.try_recv().is_err(),
        "same app on a second record receives no request"
    );
    let second_reply = second_target
        .approved_check(second_id, "regression")
        .unwrap();
    assert_eq!(
        second_receive.try_recv().unwrap().request.invocation,
        second_id.0.to_string()
    );
    assert!(first_receive.try_recv().is_err());
    let foreign = crate::host_operation::OperationTarget::new(
        crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "foreign-record"),
        address,
    );
    assert!(foreign.approved_check(first_id, "regression").is_err());
    drop(first_reply);
    drop(second_reply);
    drop(first);
    assert!(first_target.approved_check(first_id, "regression").is_err());
}

#[test]
fn approved_check_bridge_refuses_ambiguous_foreign_full_and_retired_routes() {
    let route = format!("so.tilo.git#{}#record:project", uuid::Uuid::new_v4());
    let (endpoint, mut receive) = admit_for_test(route.clone()).unwrap();
    let request = Request {
        version: 1,
        invocation: uuid::Uuid::new_v4().to_string(),
        check_id: "regression".into(),
    };
    assert!(
        admit_for_test(route.clone()).is_err(),
        "never select a second live endpoint for the same record"
    );
    assert!(enqueue(&format!("{route}-foreign"), request.clone()).is_err());
    let first = enqueue(&route, request.clone()).unwrap();
    let second = enqueue(&route, request.clone()).unwrap();
    assert!(
        enqueue(&route, request.clone()).is_err(),
        "actual typed lane refuses a third queued job"
    );
    assert_eq!(receive.try_recv().unwrap().request, request);
    assert!(
        enqueue(
            &route,
            Request {
                check_id: "../raw-path".into(),
                ..request.clone()
            }
        )
        .is_err()
    );
    drop(first);
    drop(second);
    drop(receive);
    assert!(
        enqueue(&route, request.clone()).is_err(),
        "closed receiver is not silently successful"
    );
    drop(endpoint);
    assert!(
        enqueue(&route, request).is_err(),
        "a weak registry entry cannot retain retired transport"
    );
}
