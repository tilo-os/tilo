//! A tilolang action runs a check its project's host approved as the device's work for the
//! invocation the broker served: it calls the device's approved checks through the one dispatch
//! as that invocation's work, naming only the check. The invocation stays open while the check
//! runs, the progress the device reports is the invocation's progress and its receipt the answer;
//! the invocation's cancel stops the check, and a press of the action goes through the dispatch.

use super::*;
use collaboration::approved_checks::{self as checks, Binding, Diagnostic, Outcome, Receipt};
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
    let compiled = tilolang::compile_package(CHECKED_SOURCE, &tilolang::IdsLock::new(), &[])
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
        .find(|published| published.address.action == collaboration::APPROVED_CHECK_ACTION)
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

/// How many times the instance asked the dispatch how its call `call` stands.
fn observed(sent: &Sent, call: tilo_actions::InvocationId) -> usize {
    sent.borrow()
        .iter()
        .filter(|request| {
            matches!(request, CollabRequest::RemoteAction {
                message: RemoteActionMessage::Observe { invocation, .. },
                ..
            } if *invocation == call)
        })
        .count()
}

/// The dispatch tells the instance `message` of its call.
fn dispatch_says(inbox: &Inbox, message: RemoteActionMessage) {
    inbox
        .borrow_mut()
        .push_back(CollabEvent::RemoteAction { message });
}

/// The dispatch's state of the call `call`, as it answers an ask.
fn stands(
    call: tilo_actions::InvocationId,
    update: tilo_actions::InvocationUpdate,
) -> RemoteActionMessage {
    RemoteActionMessage::Update {
        invocation: call,
        state: Some(InvocationState::new(call, update)),
        correlation: None,
    }
}

/// Every progress the broker was told of delivery `id`.
fn progress(sent: &Sent, id: u64) -> Vec<tilo_actions::InvocationUpdate> {
    sent.borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::UpdateInvocation {
                id: told,
                update: update @ tilo_actions::InvocationUpdate::Progress { .. },
            } if *told == id => Some(update.clone()),
            _ => None,
        })
        .collect()
}

/// What the host observed of a check that passed.
fn passed(invocation: tilo_actions::InvocationId, check_id: &str) -> Receipt {
    let digest = "a".repeat(64);
    Receipt {
        version: 1,
        invocation: invocation.0.to_string(),
        check_id: check_id.to_owned(),
        binding: Binding {
            app_id: CHECKED.to_owned(),
            workspace: uuid::Uuid::new_v4().to_string(),
            host: [7; 32],
            record: "record".to_owned(),
            resource: "project".to_owned(),
        },
        artifact_sha256: digest.clone(),
        config_sha256: digest.clone(),
        runtime_binary_sha256: digest.clone(),
        supervisor_source_sha256: digest.clone(),
        outcome: Outcome::Exited,
        exit_code: Some(0),
        signal: None,
        elapsed_ms: 12_340,
        cleanup_observed: true,
        stdout: Diagnostic {
            sha256: Some(digest),
            preview: "running 42 tests\nok".to_owned(),
            truncated: false,
        },
        stderr: Diagnostic {
            sha256: None,
            preview: String::new(),
            truncated: false,
        },
        failure: None,
    }
}

tilo_test_env::scenario! {
    fn a_tilolang_action_runs_its_projects_approved_check_as_the_devices_work;

    phase the_check_is_published_as_the_cancellable_job_a_check_runs_as;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let check = the_check(&host);
        assert_eq!(
            check.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );
        assert_eq!(check.contract.kind, tilo_actions::ActionKind::Undoable);
        assert_eq!(check.contract.result, tilo_actions::ResultSpec::Job);
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

    phase the_served_invocation_calls_the_device_and_ends_with_its_receipt;
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let address = the_check(&host).address;
        let invocation = tilo_actions::InvocationId::new();
        deliver_check(&inbox, 1, invocation, &address, "unit-tests");
        settle(&mut host, &mut window);
        let call = check_call(&sent).expect("the check went to the device through the dispatch");
        assert_eq!(call.on_behalf_of, Some(invocation), "as the served invocation's work");
        assert_eq!(
            call.target.address,
            tilo_actions::ActionAddress::new(checks::RESOURCE, checks::RUN)
        );
        match &call.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => {
                assert_eq!(
                    arguments,
                    serde_json::json!({ "check_id": "unit-tests" }).as_object().unwrap(),
                    "the call names the check and nothing else"
                );
            }
            other => panic!("the check's id is the argument: {other:?}"),
        }
        assert_eq!(
            answered(&sent, 1),
            None,
            "the invocation stays open while its check runs"
        );

        // The device took it: the instance follows it.
        dispatch_says(
            &inbox,
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: call.invocation,
                status: ReceiptStatus::Accepted,
            }),
        );
        settle(&mut host, &mut window);
        assert_eq!(observed(&sent, call.invocation), 1, "the running check is followed");

        // How far the device says it got is the invocation's progress.
        dispatch_says(
            &inbox,
            stands(
                call.invocation,
                tilo_actions::InvocationUpdate::Progress {
                    invocation: call.invocation,
                    sequence: 1,
                    phase: "Compiling".to_owned(),
                    completed: Some(3),
                    total: Some(10),
                    message: None,
                },
            ),
        );
        settle(&mut host, &mut window);
        match progress(&sent, 1).first() {
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
        assert_eq!(observed(&sent, call.invocation), 2, "and followed on");

        // The device's receipt is the invocation's answer.
        dispatch_says(
            &inbox,
            stands(
                call.invocation,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: call.invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(
                            serde_json::to_value(passed(invocation, "unit-tests")).unwrap(),
                        ),
                        ..Default::default()
                    },
                },
            ),
        );
        settle(&mut host, &mut window);
        let Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) = answered(&sent, 1)
        else {
            panic!("the receipt answers the invocation: {:?}", answered(&sent, 1))
        };
        let receipt = output.value.expect("the answer is the receipt");
        assert_eq!(receipt["check_id"], "unit-tests");
        assert_eq!(receipt["outcome"], "exited");
        assert_eq!(receipt["exit_code"], 0);

        // The app follows the check by the job its action kept: the device says what the host
        // observed.
        window.send(&[ToApp::Invoke {
            address: published_as(&host, "Read check"),
            argument: None,
        }]);
        settle(&mut host, &mut window);
        let said = shown(&window).replace("\\\"", "\"");
        for observed in [
            r#""state":"done""#,
            r#""outcome":"exited""#,
            r#""elapsed_ms":12340"#,
            r#""exit_code":0"#,
        ] {
            assert!(said.contains(observed), "{observed} in {said}");
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

    phase the_devices_refusal_answers_the_invocation_in_its_words;
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let address = the_check(&host).address;
        deliver_check(&inbox, 3, tilo_actions::InvocationId::new(), &address, "unknown");
        settle(&mut host, &mut window);
        let call = check_call(&sent).expect("the check went to the device");
        dispatch_says(
            &inbox,
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: call.invocation,
                status: ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::Refused {
                        reason: "This project's host approved no check `unknown`".to_owned(),
                    },
                },
            }),
        );
        settle(&mut host, &mut window);
        match answered(&sent, 3) {
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                assert!(error.message.contains("approved no check `unknown`"), "{error:?}");
            }
            other => panic!("the refusal answers the invocation: {other:?}"),
        }
    }

    phase a_press_of_the_check_goes_through_the_dispatch;
    {
        let (mut host, mut window, sent, _inbox, _route) = running_app("ada", CHECKED, checked());
        settle(&mut host, &mut window);
        let address = the_check(&host).address;
        window.send(&[ToApp::Invoke {
            address: address.clone(),
            argument: Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "check_id": "unit-tests" })
            )),
        }]);
        settle(&mut host, &mut window);
        let dispatched = sent.borrow().iter().find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
                ..
            } => Some(invoke.clone()),
            _ => None,
        });
        let invoke = dispatched.expect("the press went to the dispatch");
        assert_eq!(invoke.target.address.action, collaboration::APPROVED_CHECK_ACTION);
        assert_eq!(invoke.target.address.resource, address.resource);
        match invoke.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => {
                assert_eq!(arguments["check_id"], "unit-tests");
            }
            other => panic!("the check's id is the argument: {other:?}"),
        }
    }
}
