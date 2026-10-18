use super::*;
use crate::collab_broker_capnp::host_steps::signed_test_support::Fixture;
use crate::services::{HostServices, LaunchableApplication, LauncherServices};
use crate::test_support::WorkspaceId;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Launcher {
    calls: AtomicUsize,
    fail: bool,
}

fn app() -> LaunchableApplication {
    LaunchableApplication {
        package_id: "so.tilo.notes".into(),
        display_name: "Notes".into(),
        unconfined: false,
        offers: Vec::new(),
    }
}

impl LauncherServices for Launcher {
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }

    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![app()]
    }

    fn closes_applications(&self) -> bool {
        true
    }

    fn close(&self, _: &str, _: &Workspace) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn launch(
        &self,
        _: &str,
        _: &Workspace,
        _: Option<&str>,
        _: &LaunchPlace,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err("launcher failed".into())
        } else {
            Ok(())
        }
    }
}

fn destination(fixture: &Fixture) -> LaunchDestination {
    let mut workspace = Workspace::new(WorkspaceId(9), "Launch child");
    workspace.set_room(tilo_desktop_model::RoomId::parse(&fixture.room).unwrap());
    LaunchDestination {
        workspace,
        app_id: app().package_id,
        record: Some("record".into()),
    }
}

fn parent() -> InvokeRequest {
    let mut request = child_request(
        InvocationId::new(),
        "room",
        &app_open_descriptor(&app()),
        &app().package_id,
    );
    request.target = RemoteActionTarget {
        provider: "retained-provider".into(),
        address: ActionAddress::new("document", "append"),
    };
    request.argument = InvokeArgument::Text("not launch content".into());
    request
}

#[test]
fn failed_child_barrier_never_calls_launcher() {
    let launcher = Arc::new(Launcher {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let fixture = Fixture::new(HostServices {
        launcher: Some(launcher.clone()),
        ..Default::default()
    });
    fixture.fail_next_barrier();
    assert!(
        fixture
            .state
            .launch_retained_child(
                &fixture.caller,
                Some(&fixture.origin),
                &parent(),
                &destination(&fixture),
                &LaunchPlace::Focus,
            )
            .is_err()
    );
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn launcher_outcome_completes_signed_child_not_parent() {
    for fail in [false, true] {
        let launcher = Arc::new(Launcher {
            calls: AtomicUsize::new(0),
            fail,
        });
        let fixture = Fixture::new(HostServices {
            launcher: Some(launcher.clone()),
            ..Default::default()
        });
        let parent = parent();
        let child = child_request(
            parent.invocation,
            &fixture.room,
            &app_open_descriptor(&app()),
            &app().package_id,
        );
        let result = fixture.state.launch_retained_child(
            &fixture.caller,
            Some(&fixture.origin),
            &parent,
            &destination(&fixture),
            &LaunchPlace::Focus,
        );
        assert_eq!(result.is_err(), fail);
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
        assert_ne!(parent.invocation, child.invocation);
        let record = fixture.assert_signed(&child, true);
        assert_eq!(
            record.intent["host_start"]["parent_invocation"],
            serde_json::to_value(parent.invocation).unwrap()
        );
        assert_eq!(record.intent["host_start"]["record"], "record");
        assert_eq!(
            matches!(record.update, InvocationUpdate::Failed { .. }),
            fail
        );
        assert!(
            fixture
                .state
                .remote_job(&fixture.caller, &parent.invocation)
                .is_none()
        );
    }
}

struct NativePerson;

impl crate::services::LocalPersonServices for NativePerson {
    fn certificate(
        &self,
        room: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        let (_, certificate) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| error.to_string())?;
        if crate::collab_broker_capnp::principal_id_for_root(certificate.user_public_key) != person
        {
            return Err("different person".into());
        }
        Ok(certificate)
    }
}

#[test]
fn protected_native_parent_is_checked_before_child_rebind() {
    let launcher = Arc::new(Launcher {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let fixture = Fixture::new(HostServices {
        launcher: Some(launcher.clone()),
        local_person: Some(Arc::new(NativePerson)),
        ..Default::default()
    });
    let InvocationCaller::Local { admitted, .. } = &fixture.origin else {
        panic!("local fixture")
    };
    let parent = parent();
    let mut origin = InvocationCaller::NativePerson {
        invocation: InvocationId::new(),
        room: fixture.room.clone(),
        certificate: admitted.certificate.clone().unwrap(),
        time: fixture.state.registered_policy_time().unwrap().unwrap(),
    };
    assert!(
        fixture
            .state
            .launch_retained_child(
                &fixture.caller,
                Some(&origin),
                &parent,
                &destination(&fixture),
                &LaunchPlace::Focus,
            )
            .is_err()
    );
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    let InvocationCaller::NativePerson { invocation, .. } = &mut origin else {
        unreachable!()
    };
    *invocation = parent.invocation;
    fixture
        .state
        .launch_retained_child(
            &fixture.caller,
            Some(&origin),
            &parent,
            &destination(&fixture),
            &LaunchPlace::Focus,
        )
        .unwrap();
    let child = child_request(
        parent.invocation,
        &fixture.room,
        &app_open_descriptor(&app()),
        &app().package_id,
    );
    fixture.assert_signed(&child, true);
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn launcher_observes_signed_admission_before_its_first_effect() {
    let launcher = Arc::new(Launcher {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let fixture = Fixture::new(HostServices {
        launcher: Some(launcher),
        ..Default::default()
    });
    let parent = parent();
    let child = child_request(
        parent.invocation,
        &fixture.room,
        &app_open_descriptor(&app()),
        &app().package_id,
    );
    let InvocationCaller::Local { admitted, .. } = &fixture.origin else {
        panic!("local fixture")
    };
    let certificate = admitted.certificate.as_ref().unwrap();
    let owner = InvocationOwner::AuthenticatedLocal {
        root: certificate.user_public_key,
        device: certificate.device_public_key,
        agent: None,
    };
    let state = Arc::downgrade(&fixture.state);
    let room = fixture.room.clone();
    let invocation = child.invocation;
    let parent_id = parent.invocation;
    fixture.state.launchable.replace_launcher_for_test(
        Arc::new(move |_, _, _, _| {
            let state = state.upgrade().unwrap();
            let guard = state.journal.lock().unwrap();
            let journal = guard.as_ref().unwrap().as_ref().unwrap();
            let record = journal
                .lookup(&crate::collab_broker_capnp::InvocationKey {
                    room: room.clone(),
                    host: journal.host(),
                    owner: owner.clone(),
                    invocation,
                })
                .unwrap()
                .unwrap();
            let receipt = record.admission.as_ref().unwrap().public_receipt();
            crate::action_evidence::verify(receipt, &receipt.facts).unwrap();
            assert_eq!(
                record.intent["host_start"]["parent_invocation"],
                serde_json::to_value(parent_id).unwrap()
            );
            assert!(matches!(record.update, InvocationUpdate::Accepted { .. }));
            assert!(record.terminal.is_none());
            Ok(())
        }),
        std::time::Duration::from_secs(1),
    );
    fixture
        .state
        .launch_retained_child(
            &fixture.caller,
            Some(&fixture.origin),
            &parent,
            &destination(&fixture),
            &LaunchPlace::Focus,
        )
        .unwrap();
    fixture.assert_signed(&child, true);
}

/// Stop at the actual durable admission boundary, then withdraw the original
/// certificate before continuing through the production launch/close effect.
#[test]
fn revoked_original_after_admission_never_launches_or_closes() {
    for certified in [false, true] {
        for close in [false, true] {
            let launcher = Arc::new(Launcher {
                calls: AtomicUsize::new(0),
                fail: false,
            });
            let services = HostServices {
                launcher: Some(launcher.clone()),
                ..Default::default()
            };
            let fixture = if certified {
                Fixture::new_certified(services)
            } else {
                Fixture::new(services)
            };
            let descriptor = if close {
                app_close_descriptor(&app().package_id, "Notes")
            } else {
                app_open_descriptor(&app())
            };
            let request = child_request(
                InvocationId::new(),
                &fixture.room,
                &descriptor,
                &app().package_id,
            );
            let admitted = fixture
                .state
                .admit_host_action(
                    &fixture.caller,
                    Some(&fixture.origin),
                    HostActionAdmission {
                        room: &fixture.room,
                        app_id: &app().package_id,
                        descriptor: &descriptor,
                        request: &request,
                    },
                )
                .unwrap();
            fixture.assert_signed(&request, false);
            fixture.authority.revoked.store(true, Ordering::SeqCst);
            let custody = (
                &fixture.caller,
                Some(&fixture.origin),
                fixture.room.as_str(),
            );
            let effect = || {
                if close {
                    fixture
                        .state
                        .close_application(custody, admitted, &app().package_id)
                } else {
                    fixture.state.launch_application(
                        custody,
                        admitted,
                        &app().package_id,
                        crate::services::LaunchContent {
                            hash: "",
                            caller: None,
                            author: None,
                        },
                        &LaunchPlace::Focus,
                    )
                }
            };
            let result = if let InvocationCaller::Certified { caller, .. } = &fixture.origin {
                // The original synchronous dispatch can still have its
                // pre-barrier verification memo installed at this boundary.
                crate::collab_broker_capnp::admission_memo::with_admitted_now(
                    caller,
                    caller.admitted.clone(),
                    effect,
                )
            } else {
                effect()
            };
            assert!(result.is_err());
            assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
            assert!(matches!(
                fixture.record(&request).update,
                InvocationUpdate::Failed { .. }
            ));
        }
    }
}

#[test]
fn child_identity_is_stable_and_framed() {
    let parent = InvocationId::new();
    assert_eq!(
        child_id(parent, "room", "app"),
        child_id(parent, "room", "app")
    );
    assert_ne!(child_id(parent, "a", "bc"), child_id(parent, "ab", "c"));
    assert_ne!(
        child_id(parent, "room", "app"),
        child_id(InvocationId::new(), "room", "app")
    );
}
