//! A tilolang action runs a check its project's host approved: published as the action that
//! declares the device's run as its step, and stopped when the invocation that asked for it is
//! cancelled.

use super::*;
use tilo_action_services::approved_checks::{self as checks};
use tilo_actions::remote::{
    InvocationReceipt, InvocationState, InvokeRequest, ReceiptStatus, RemoteActionMessage,
};

const CHECKED: &str = "so.tilo.lang.checked";

/// A project whose one action runs a check its host approved, and keeps what it was told.
const CHECKED_SOURCE: &str = r#"app so.tilo.lang.checked "Checked" (icon: .hash, category: .utility)

/// A project checked by its host.
type Project {
    job: Text = ""
    said: Text = ""
}

root Project

default view project for Project

/// The project, and what its last check was told.
view project(p: Project) {
    ui.Text(p.said)
}

/// Reads how the last check stands, from the job's record on the device that runs it.
action read_check(p: Project) (label: "Read check") {
    if let j = engine.job(p.job) {
        p.said = j.detail
    } else {
        p.said = "This device no longer keeps the check"
    }
}

/// Runs a check the project's host approved.
action run_approved_check(p: Project, check_id: Text) uses check (label: "Run check") {
    match engine.check(check_id) {
        .err(why) => {
            p.said = why
        }
        .ok(job) => {
            p.job = job
        }
    }
}
"#;

fn checked() -> Bundle {
    let compiled = compile(CHECKED_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the checked package compiles: {diagnostics:?}"));
    Bundle::new(&compiled, [])
}

/// The descriptor the instance publishes the approved check under.
fn the_check(host: &TilolangProcess) -> tilo_actions::ActionDescriptor {
    host.instances()
        .next()
        .expect("the app runs")
        .published()
        .iter()
        .find(|published| {
            published.address.action == tilo_action_services::approved_checks::ACTION_ID
        })
        .unwrap_or_else(|| panic!("the action that runs a check is the app's run-approved-check"))
        .clone()
}

/// Deliver the check `check_id` to the app as ada's invocation `invocation`, the broker's
/// delivery `id`.
fn deliver_check(
    inbox: &Inbox,
    id: u64,
    invocation: tilo_actions::InvocationId,
    address: &tilo_actions::ActionAddress,
    check_id: &str,
) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation,
        resource: address.resource.clone(),
        action: address.action.clone(),
        argument: Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "check_id": check_id })
        )),
        actor: tilo_actions::MemberId::person("ada").into(),
        context: None,
    });
}

/// The call to the device's approved checks the instance sent.
fn check_call(sent: &Sent) -> Option<InvokeRequest> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(invoke),
                ..
            } if invoke.target.provider == checks::PROVIDER => Some(invoke.clone()),
            _ => None,
        })
}

/// The dispatch tells the instance `message` of its call.
fn dispatch_says(inbox: &Inbox, message: RemoteActionMessage) {
    inbox
        .borrow_mut()
        .push_back(CollabEvent::RemoteAction { message });
}

tilo_test_env::scenario! {
    fn a_tilolang_action_runs_its_projects_approved_check_as_the_devices_work;

    phase the_check_is_published_as_the_job_a_check_runs_as;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let check = the_check(&host);
        // It declares no cancellation of its own: the dispatch lets its invoker stop it while it
        // waits on the device's check, a step that can be stopped, as it does any action.
        assert_eq!(
            check.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(check.contract.kind, tilo_actions::ActionKind::Undoable);
        // A check answers the host's receipt of what the commands did: a document with the
        // outcome, the exit code and the output they printed.
        let tilo_actions::ResultSpec::Json { schema } = &check.contract.result else {
            panic!(
                "a check answers the receipt of what it observed, not {:?}",
                check.contract.result
            );
        };
        assert_eq!(
            schema,
            &serde_json::json!({ "type": "object" }),
            "the receipt's shape"
        );
        assert!(
            check
                .contract
                .tags
                .contains(&tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned()),
            "what the commands printed is untrusted: {:?}",
            check.contract.tags
        );
        assert!(
            check.contract.declares(
                checks::PROVIDER,
                &tilo_actions::ActionAddress::new(checks::RESOURCE, checks::RUN)
            ),
            "the device's run is the action's declared step, covered by its yes: {:?}",
            check.contract.effects
        );
    }

    phase the_devices_progress_is_the_invocations_progress;
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let address = the_check(&host).address;
        let invocation = tilo_actions::InvocationId::new();
        deliver_check(&inbox, 1, invocation, &address, "unit-tests");
        settle(&mut host, &mut window);
        let call = check_call(&sent).expect("the check went to the device");
        dispatch_says(
            &inbox,
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: call.invocation,
                status: ReceiptStatus::Accepted,
            }),
        );
        settle(&mut host, &mut window);
        dispatch_says(
            &inbox,
            RemoteActionMessage::Update {
                invocation: call.invocation,
                state: Some(InvocationState::new(
                    call.invocation,
                    tilo_actions::InvocationUpdate::Progress {
                        invocation: call.invocation,
                        sequence: 1,
                        phase: "Compiling".to_owned(),
                        completed: Some(3),
                        total: Some(10),
                        message: None,
                    },
                )),
                correlation: None,
            },
        );
        settle(&mut host, &mut window);
        let told: Vec<_> = sent
            .borrow()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::UpdateInvocation {
                    id: 1,
                    update: update @ tilo_actions::InvocationUpdate::Progress { .. },
                } => Some(update.clone()),
                _ => None,
            })
            .collect();
        match told.first() {
            Some(tilo_actions::InvocationUpdate::Progress {
                invocation: told,
                phase,
                completed,
                total,
                ..
            }) => {
                assert_eq!(*told, invocation);
                assert_eq!(phase, "Compiling");
                assert_eq!((*completed, *total), (Some(3), Some(10)));
            }
            other => panic!("the report is the invocation's progress: {other:?}"),
        }
    }

    phase the_invocations_cancel_stops_its_check;
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let address = the_check(&host).address;
        deliver_check(&inbox, 2, tilo_actions::InvocationId::new(), &address, "slow-suite");
        settle(&mut host, &mut window);
        let call = check_call(&sent).expect("the check went to the device");
        inbox
            .borrow_mut()
            .push_back(CollabEvent::InvocationCancellation { id: 2 });
        settle(&mut host, &mut window);
        assert!(
            sent.borrow().iter().any(|request| matches!(request, CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel { invocation },
                ..
            } if *invocation == call.invocation)),
            "the device is asked to stop the check"
        );
        assert_eq!(answered(&sent, 2), None, "answered once the check stopped");
        dispatch_says(
            &inbox,
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: call.invocation,
                status: ReceiptStatus::Cancelled { message: None },
            }),
        );
        settle(&mut host, &mut window);
        assert!(
            matches!(answered(&sent, 2), Some(tilo_actions::InvocationUpdate::Cancelled { .. })),
            "{:?}",
            answered(&sent, 2)
        );
    }

}
