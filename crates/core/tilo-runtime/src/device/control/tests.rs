//! The device's steps through the provider a caller reaches: admitted, started as their unit,
//! followed, settled, cancelled.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, Confirmation, InvokeArgument,
    InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::{Declared, DeviceControl, PROVIDER, Start, Stop};

const DEVICE: [u8; 32] = [7; 32];

/// The steps these tests' device declares, as its step program would.
fn declared() -> Vec<Declared> {
    use tilo_actions::ActionParam;
    use tilo_actions::ParamType::{Array, Boolean, Integer, Object, Secret, Text};
    let step = |id: &str, params: Vec<ActionParam>| Declared {
        id: id.to_owned(),
        label: id.to_owned(),
        params,
        confirm: id == "device-unlock-enrol",
    };
    let (r, o) = (ActionParam::required, ActionParam::optional);
    let mut status = o("account", Text, "");
    status.nullable = true;
    vec![
        step("status", vec![status]),
        step("account-show", vec![r("name", Text, "")]),
        step("account-rm", vec![r("name", Text, "")]),
        step(
            "gc",
            vec![r("keep", Integer, ""), o("dry_run", Boolean, "")],
        ),
        step(
            "build",
            vec![
                r("worktree", Text, ""),
                r("target", Object, ""),
                r("provenance", Object, ""),
            ],
        ),
        step(
            "exec",
            vec![r("account", Text, ""), r("command", Array, "")],
        ),
        step(
            "device-unlock-enrol",
            vec![r("passphrase", Secret, ""), o("networks", Array, "")],
        ),
    ]
}

fn resource() -> String {
    format!("devices/{}/vm", hex::encode(DEVICE))
}

fn caller() -> Caller {
    Caller::authenticated([1; 32], [2; 32])
}

fn request(action: &str, args: Value) -> InvokeRequest {
    let Value::Object(args) = args else {
        panic!("arguments are an object")
    };
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(resource(), action),
        },
        argument: InvokeArgument::Json(args),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A device whose units run `unit` on the invocation's directory, and which records the units
/// it was asked to stop.
struct Device {
    _dirs: tempfile::TempDir,
    steps: PathBuf,
    control: DeviceControl,
    started: Arc<Mutex<Vec<String>>>,
    stopped: mpsc::Receiver<String>,
}

fn device(
    unit: impl Fn(&str, &Path, &mpsc::Receiver<()>) -> Result<(), String> + Send + Sync + 'static,
) -> Device {
    let dirs = tempfile::tempdir().expect("scratch");
    let steps = dirs.path().join("steps");
    let started = Arc::new(Mutex::new(Vec::new()));
    let (stop_tx, stopped) = mpsc::channel();
    let halts: Arc<Mutex<Vec<mpsc::Sender<()>>>> = Arc::default();
    let start: Start = {
        let (steps, started, halts) = (steps.clone(), Arc::clone(&started), Arc::clone(&halts));
        Arc::new(move |action: &str, id: &str| {
            started
                .lock()
                .expect("started")
                .push(super::unit(action, id));
            let (halt, halted) = mpsc::channel();
            halts.lock().expect("halts").push(halt);
            unit(action, &steps.join(id), &halted)
        })
    };
    let stop: Stop = Arc::new(move |action: &str, id: &str| {
        for halt in halts.lock().expect("halts").drain(..) {
            let _ = halt.send(());
        }
        let _ = stop_tx.send(super::unit(action, id));
    });
    let control = DeviceControl::new(
        DEVICE,
        &declared(),
        &steps,
        &dirs.path().join("content"),
        start,
        stop,
    );
    Device {
        _dirs: dirs,
        steps,
        control,
        started,
        stopped,
    }
}

fn settle(control: &DeviceControl, id: &InvocationId) -> InvocationUpdate {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .expect("a deadline");
    loop {
        let state = control
            .observe(&caller(), id)
            .expect("the invocation is known");
        if state.is_terminal() {
            return state.update;
        }
        assert!(Instant::now() < deadline, "the step settles");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn invoke(control: &DeviceControl, request: InvokeRequest) -> Result<(), ActionCallError> {
    control
        .invoke(&caller(), request)
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("invoke answers")
        .map(drop)
}

#[test]
fn the_catalogue_offers_every_declared_step_on_the_devices_vm() {
    let device = device(|_, _, _| Ok(()));
    let page = device
        .control
        .query(&caller(), CatalogueQuery::default())
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("query answers")
        .expect("the catalogue is published");
    let mut offered: Vec<String> = page
        .rows
        .iter()
        .filter_map(|row| {
            row.action
                .as_ref()
                .map(|action| action.address.action.clone())
        })
        .collect();
    let mut steps: Vec<String> = declared().into_iter().map(|step| step.id).collect();
    offered.sort();
    steps.sort();
    assert_eq!(offered, steps);
    assert!(page.rows.iter().all(|row| row.provider == PROVIDER));
    assert!(
        device.control.content_puts().is_some(),
        "steps read content put to the device"
    );
}

#[test]
fn a_root_step_runs_as_its_unit_and_settles_with_its_result() {
    let device = device(|action, dir, _| {
        assert_eq!(action, "account-show");
        let args: Value =
            serde_json::from_slice(&std::fs::read(dir.join("args.json")).expect("staged"))
                .expect("arguments");
        assert_eq!(args, json!({"name": "dev"}));
        std::fs::write(dir.join("progress"), "reading the account\n").expect("progress");
        std::fs::write(dir.join("result.json"), r#"{"ok":{"name":"dev"}}"#).expect("result");
        Ok(())
    });
    let request = request("account-show", json!({"name": "dev"}));
    let id = request.invocation;
    invoke(&device.control, request).expect("admitted");
    let InvocationUpdate::Succeeded { output, .. } = settle(&device.control, &id) else {
        panic!("the step succeeded")
    };
    assert_eq!(output.value, Some(json!({"name": "dev"})));
    assert_eq!(
        *device.started.lock().expect("started"),
        [format!(
            "tilo-device-account-show@{}.service",
            id.0.simple()
        )]
    );
    assert!(
        !device.steps.join(id.0.simple().to_string()).exists(),
        "a settled step leaves nothing staged"
    );
}

#[test]
fn a_refused_step_fails_with_the_steps_own_error() {
    let device = device(|_, dir, _| {
        let error = json!({"err": {"code": 3, "message": "no account dev", "hint": "tilo-vm account list", "see": null}});
        std::fs::write(dir.join("result.json"), error.to_string()).expect("result");
        Ok(())
    });
    let request = request("account-rm", json!({"name": "dev"}));
    let id = request.invocation;
    invoke(&device.control, request).expect("admitted");
    let InvocationUpdate::Failed { error, .. } = settle(&device.control, &id) else {
        panic!("the step failed")
    };
    assert_eq!(error.code, "step_failed");
    assert_eq!(error.message, "no account dev");
    let details = error.details.expect("the step's error");
    assert_eq!(details["hint"], "tilo-vm account list");
}

#[test]
fn a_unit_that_never_answers_fails_legibly() {
    let device = device(|_, _, _| Err("tilo-device-gc@x.service did not run".into()));
    let request = request("gc", json!({"keep": 1, "dry_run": true}));
    let id = request.invocation;
    invoke(&device.control, request).expect("admitted");
    let InvocationUpdate::Failed { error, .. } = settle(&device.control, &id) else {
        panic!("the step failed")
    };
    assert!(error.message.contains("did not run"), "{}", error.message);
}

#[test]
fn progress_is_followed_while_the_step_runs() {
    let (go, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let device = device(move |_, dir, _| {
        std::fs::write(dir.join("progress"), "compiling tilo\n").expect("progress");
        wait.lock().expect("wait").recv().expect("released");
        std::fs::write(dir.join("result.json"), r#"{"ok":{}}"#).expect("result");
        Ok(())
    });
    let request = request(
        "build",
        json!({"worktree": "w", "target": {}, "provenance": {}}),
    );
    let id = request.invocation;
    invoke(&device.control, request).expect("admitted");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .expect("a deadline");
    loop {
        let state = device.control.observe(&caller(), &id).expect("known");
        if let InvocationUpdate::Progress {
            message, completed, ..
        } = &state.update
        {
            assert_eq!(message.as_deref(), Some("compiling tilo\n"));
            assert_eq!(*completed, Some(15));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "progress arrives while the step runs"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    go.send(()).expect("release");
    assert!(matches!(
        settle(&device.control, &id),
        InvocationUpdate::Succeeded { .. }
    ));
}

#[test]
fn cancel_stops_the_unit_and_the_invocation_says_so() {
    let device = device(|_, _, halted| {
        halted.recv().expect("stopped");
        Err("stopped".into())
    });
    let request = request(
        "exec",
        json!({"account": "dev", "command": ["sleep", "1000"]}),
    );
    let id = request.invocation;
    invoke(&device.control, request).expect("admitted");
    device
        .control
        .cancel(&caller(), &id)
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("cancel answers")
        .expect("a running step cancels");
    assert_eq!(
        device
            .stopped
            .recv_timeout(Duration::from_secs(5))
            .expect("stopped"),
        format!("tilo-device-exec@{}.service", id.0.simple())
    );
    assert!(matches!(
        settle(&device.control, &id),
        InvocationUpdate::Cancelled { .. }
    ));
}

#[test]
fn what_the_device_does_not_offer_is_refused_before_anything_runs() {
    let device = device(|_, _, _| panic!("nothing runs"));
    let mut elsewhere = request("status", json!({}));
    elsewhere.target.address.resource = "devices/00/vm".into();
    assert_eq!(
        invoke(&device.control, elsewhere),
        Err(ActionCallError::UnknownAction)
    );
    assert_eq!(
        invoke(&device.control, request("format-disk", json!({}))),
        Err(ActionCallError::UnknownAction)
    );
    let Err(ActionCallError::InvalidRequest { reason }) = invoke(
        &device.control,
        request("account-rm", json!({"name": "dev", "secret": "hunter2"})),
    ) else {
        panic!("an unknown argument is refused")
    };
    assert!(!reason.contains("hunter2"), "{reason}");
}

/// Unlocking the disk from other devices is admitted by what its catalogue declares (a secret passphrase, the
/// person's confirmation), and its passphrase reaches only the step: not the unit's name, not
/// what the invocation answers, not a refusal, and not the device once the step settled.
#[test]
fn disk_unlock_from_other_devices_is_admitted_by_its_declaration_and_keeps_its_secret_to_the_step()
{
    const SECRET: &str = "private-fixture-answer";
    let seen = Arc::new(Mutex::new(None));
    let device = device({
        let seen = Arc::clone(&seen);
        move |_, dir, _| {
            let args = std::fs::read_to_string(dir.join("args.json")).expect("the step's args");
            *seen.lock().expect("seen") = Some(args);
            std::fs::write(dir.join("result.json"), r#"{"ok":{}}"#).expect("result");
            Ok(())
        }
    });
    let query = CatalogueQuery {
        needle: "device-unlock-enrol".into(),
        ..CatalogueQuery::default()
    };
    let page = device
        .control
        .query(&caller(), query)
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("query answers")
        .expect("the catalogue is published");
    assert_eq!(page.rows.len(), 1);
    let unlock = page.rows[0].action.as_ref().expect("an action row");
    assert_eq!(
        unlock.contract.confirmation,
        tilo_actions::ConfirmationPolicy::Confirm
    );
    let passphrase = unlock
        .params
        .iter()
        .find(|param| &*param.name == "passphrase")
        .expect("it asks for the passphrase");
    assert_eq!(passphrase.ty, tilo_actions::ParamType::Secret);

    let unconfirmed = request("device-unlock-enrol", json!({"passphrase": SECRET}));
    let refused = invoke(&device.control, unconfirmed).unwrap_err();
    assert!(matches!(
        refused,
        ActionCallError::ConfirmationRequired { .. }
    ));
    assert!(!format!("{refused:?}").contains(SECRET), "{refused:?}");
    let mut malformed = request(
        "device-unlock-enrol",
        json!({"passphrase": SECRET, "unknown": SECRET}),
    );
    malformed.confirmation = Some(approved(&malformed));
    let refused = invoke(&device.control, malformed).unwrap_err();
    assert!(!format!("{refused:?}").contains(SECRET), "{refused:?}");
    assert!(device.started.lock().expect("started").is_empty());

    let mut confirmed = request("device-unlock-enrol", json!({"passphrase": SECRET}));
    let mut written = request("device-unlock-enrol", json!({"passphrase": SECRET}));
    written.confirmation = Some(Confirmation::Operated);
    assert!(
        matches!(
            invoke(&device.control, written).unwrap_err(),
            ActionCallError::ConfirmationRequired { .. }
        ),
        "a yes the call writes itself answers nothing"
    );
    confirmed.confirmation = Some(approved(&confirmed));
    let id = confirmed.invocation;
    invoke(&device.control, confirmed).expect("admitted once the person approved it");
    let settled = settle(&device.control, &id);
    assert!(matches!(settled, InvocationUpdate::Succeeded { .. }));
    assert!(!format!("{settled:?}").contains(SECRET), "{settled:?}");
    let args = seen.lock().expect("seen").take().expect("the step ran");
    assert!(
        args.contains(SECRET),
        "the step reads the passphrase: {args}"
    );
    let started = device.started.lock().expect("started").clone();
    assert_eq!(started.len(), 1);
    assert!(!started[0].contains(SECRET), "{started:?}");
    assert!(
        !device.steps.join(id.0.simple().to_string()).exists(),
        "the staged passphrase is gone once the step settled"
    );
}

/// The person's approve of `request`, kept as its asked record, as the dispatch hands it back.
fn approved(request: &InvokeRequest) -> Confirmation {
    Confirmation::Asked(tilo_actions::remote::asked::Approval::new(
        tilo_actions::remote::asked::RecordRef {
            workspace: "workspace".to_owned(),
            record: request.invocation.0.to_string(),
            device: None,
        },
        None,
    ))
}
