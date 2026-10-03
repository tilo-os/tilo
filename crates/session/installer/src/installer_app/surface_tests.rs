//! The installer's action model: validation, the destructive confirmation,
//! state transitions, and that the wizard's own controls, the console and
//! the host door all run the same published actions.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::operation::{self as op, Operation};

/// A recording install backend: disks from a fixture, an install that
/// streams a recovery-key marker and progress, and power actions noted
/// instead of taken.
struct FakeBackend {
    disks: Vec<Disk>,
    installs: Mutex<Vec<(String, String, String, InstallOptions)>>,
    power: Mutex<Vec<PowerAction>>,
    succeed: bool,
}

const KEY: &str = "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
const SECRET: &str = "correct horse battery staple";

impl FakeBackend {
    fn new() -> Arc<Self> {
        Self::finishing(true)
    }

    fn finishing(succeed: bool) -> Arc<Self> {
        Arc::new(Self {
            disks: sample_disks(),
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed,
        })
    }
}

impl InstallBackend for FakeBackend {
    fn read_disks(&self) -> Vec<Disk> {
        self.disks.clone()
    }

    fn run_install(
        &self,
        disk: &str,
        user: &str,
        password: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        self.installs.lock().unwrap().push((
            disk.to_owned(),
            user.to_owned(),
            password.to_owned(),
            opts.clone(),
        ));
        on_line("==> Installing Tilo onto the target".to_owned());
        on_line(format!("{RECOVERY_KEY_PREFIX}{KEY}"));
        // A helper that printed the key raw must not leak it either.
        on_line(format!("cryptenroll: new key {KEY}"));
        on_line("==> Setting hostname 'lab-1' and time zone 'UTC'".to_owned());
        Ok(self.succeed)
    }

    fn run_alongside_preflight(
        &self,
        _disk: &str,
        _opts: &InstallOptions,
        _on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        Ok(true)
    }

    fn run_power(&self, action: PowerAction) -> std::io::Result<bool> {
        self.power.lock().unwrap().push(action);
        Ok(true)
    }
}

fn app(backend: &Arc<FakeBackend>) -> (InstallerApp, AutoCommit) {
    let mut app = InstallerApp::with_disks(Vec::new()).with_backend(backend.clone());
    // Pin the hardware so the secret policy is deterministic on any host.
    app.tpm2 = false;
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

fn console(app: &mut InstallerApp, doc: &mut AutoCommit, request: &Value) -> Value {
    serde_json::from_str(&app.answer_console(&request.to_string(), doc, &automerge::ROOT)).unwrap()
}

fn invoke(app: &mut InstallerApp, doc: &mut AutoCommit, id: &str, args: &Value) -> Value {
    console(
        app,
        doc,
        &json!({"op": "actions_invoke", "provider": "installer", "id": id, "args": args}),
    )
}

fn refused(reply: &Value) -> Option<&str> {
    reply.pointer("/err/code").and_then(Value::as_str)
}

fn state(app: &mut InstallerApp, doc: &mut AutoCommit) -> Value {
    console(app, doc, &json!({"op": "state_dump"}))["state"].clone()
}

#[test]
fn the_published_surface_is_valid_and_offers_a_way_back_only_when_staged() {
    // Phase `the_published_surface_is_valid_on_every_step`.
    {
        let backend = FakeBackend::new();
        let (mut app, _doc) = app(&backend);
        let handlers: HashSet<String> = op::HANDLERS.iter().map(|h| (*h).to_owned()).collect();
        for step in [
            Step::Keyboard,
            Step::Network,
            Step::Disk,
            Step::Account,
            Step::Options,
            Step::Review,
            Step::Install,
        ] {
            app.step = step;
            let resources = app.publish();
            let violations = tilo_actions::validate_surface(
                &resources,
                &tilo_actions::ActionRegistry::new(),
                &handlers,
            );
            assert!(violations.is_empty(), "{step:?}: {violations:?}");
            let described = tilo_actions::door::survey(&resources);
            assert!(
                described.iter().any(|d| d.address.action == "status"),
                "{step:?} has no status"
            );
            // Exactly the current step's controls are available; every other
            // action says why not.
            for descriptor in &described {
                if let tilo_actions::Availability::Unavailable { code, reason } =
                    &descriptor.contract.availability
                {
                    assert!(!code.is_empty() && !reason.is_empty(), "{descriptor:?}");
                }
            }
        }
    }
    // Phase `a_staged_installer_offers_only_alongside_and_the_way_back`.
    // A staged (USB-free) installer publishes its way back and its one allowed
    // plan through the same door as every other control.
    {
        let backend = Arc::new(FakeBackend {
            disks: crate::disks::parse_lsblk(
                &crate::internal_handoff::tests::lsblk_json().to_string(),
            ),
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed: true,
        });
        let (mut app, mut doc) = app(&backend);
        app.refresh_disks_with(backend.read_disks());
        app.apply_internal_handoff(crate::internal_handoff::tests::handoff());
        let doc = &mut doc;
        app.step = Step::Disk;
        let device = app.disks[0].name.clone();

        let selected = invoke(
            &mut app,
            doc,
            &format!("select@{}", op::disk_resource(&device)),
            &json!({}),
        );
        assert!(selected.get("answer").is_some(), "{selected}");
        let erase = invoke(
            &mut app,
            doc,
            "set-method@installer.storage",
            &json!({"method": "erase"}),
        );
        assert_eq!(refused(&erase), Some("action_failed"), "{erase}");
        assert_eq!(app.install_method, InstallMethod::Alongside);

        app.install_state = InstallState::Running;
        let running = invoke(&mut app, doc, "return-to-host@installer", &json!({}));
        assert_eq!(refused(&running), Some("action_unavailable"), "{running}");
        assert!(backend.power.lock().unwrap().is_empty());

        app.install_state = InstallState::Failed;
        let back = invoke(&mut app, doc, "return-to-host@installer", &json!({}));
        assert!(back.get("answer").is_some(), "{back}");
        assert_eq!(*backend.power.lock().unwrap(), vec![PowerAction::Reboot]);
    }
    // Phase `an_ordinary_installer_publishes_no_way_back`.
    {
        let backend = FakeBackend::new();
        let (mut app, mut doc) = app(&backend);
        let reply = invoke(&mut app, &mut doc, "return-to-host@installer", &json!({}));
        assert!(refused(&reply).is_some(), "{reply}");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
)]
fn every_door_reaches_the_same_handler_and_refuses_stale_or_private_input() {
    // Phase `wizard_controls_run_the_published_actions`.
    {
        // Selecting a disk by its row, and by its action, is one operation.
        let backend = FakeBackend::new();
        let (mut by_row, mut row_doc) = app(&backend);
        let (mut by_action, mut action_doc) = app(&backend);
        for app in [&mut by_row, &mut by_action] {
            app.step = Step::Disk;
            app.erase_confirmation = "stale".into();
        }
        let _ = row_doc.put(&automerge::ROOT, disk_path(1).as_str(), true);
        by_row.sync(&mut row_doc, &automerge::ROOT);
        let _ = invoke(
            &mut by_action,
            &mut action_doc,
            "select@installer.disk:/dev/nvme0n1",
            &json!({}),
        );
        assert_eq!(by_row.selected_disk, Some(1));
        assert_eq!(by_row.selected_disk, by_action.selected_disk);
        assert_eq!(by_row.erase_confirmation, by_action.erase_confirmation);

        // A control whose action is unavailable shows the published reason: the
        // same sentence an external caller receives.
        by_row.step = Step::Install;
        by_row.install_state = InstallState::Done;
        by_row.recovery_key = Some(KEY.into());
        let _ = row_doc.put(&automerge::ROOT, REBOOT_PATH, true);
        by_row.sync(&mut row_doc, &automerge::ROOT);
        let reason = by_row
            .publish()
            .iter()
            .flat_map(|root| root.children.iter())
            .find(|resource| resource.id.as_str() == op::POWER)
            .and_then(|power| power.actions.iter().find(|a| a.id == "reboot"))
            .map(|reboot| match &reboot.contract.availability {
                tilo_actions::Availability::Unavailable { reason, .. } => reason.clone(),
                tilo_actions::Availability::Available => String::new(),
            })
            .unwrap();
        assert_eq!(by_row.status, reason);
        assert!(backend.power.lock().unwrap().is_empty());
    }
    // Phase `the_host_door_reaches_the_same_handler_and_refuses_a_stale_target`.
    {
        let backend = FakeBackend::new();
        let (mut app, mut doc) = app(&backend);
        let resources = app.publish();
        let next = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(op::SESSION, "next"),
        )
        .unwrap();
        let encoded = next.build_json(&serde_json::Map::new()).unwrap();
        let started = app
            .start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
        assert!(matches!(
            started,
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.step, Step::Network);

        // A confirmed begin for a target the review no longer names is refused,
        // not redirected to whatever is selected now.
        app.step = Step::Review;
        app.selected_disk = Some(1);
        app.recovery_ack = true;
        *app.password.lock().unwrap() = SECRET.to_owned();
        let stale = serde_json::to_vec(&Operation::BeginInstall {
            target: "/dev/sda".into(),
        })
        .unwrap();
        let refused = app.start_operation(
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                op::OPERATION_TYPE,
                &stale,
            ),
            &mut doc,
            &automerge::ROOT,
        );
        assert!(refused.is_err());
        assert!(backend.installs.lock().unwrap().is_empty());
    }
    // Phase `a_failed_install_goes_back_to_storage_through_the_same_action`.
    {
        let backend = FakeBackend::finishing(false);
        let (mut app, mut doc) = app(&backend);
        app.step = Step::Review;
        app.selected_disk = Some(1);
        app.recovery_ack = true;
        *app.password.lock().unwrap() = SECRET.to_owned();
        *app.password_confirmation.lock().unwrap() = SECRET.to_owned();
        let started = invoke(
            &mut app,
            &mut doc,
            "begin@installer.review",
            &json!({"confirmation_phrase": "/dev/nvme0n1"}),
        );
        assert!(started.get("answer").is_some(), "{started}");
        assert_eq!(app.install_state, InstallState::Failed);
        let listed = console(
            &mut app,
            &mut doc,
            &json!({"op": "actions_enumerate", "query": "Review storage"}),
        );
        assert_eq!(listed["answer"]["count"], 1, "{listed}");
        let back = invoke(&mut app, &mut doc, "back@installer", &json!({}));
        assert_eq!(back["answer"]["step"], "disk", "{back}");
        assert_eq!(
            app.selected_disk, None,
            "a failed target is never pre-selected again"
        );
    }
    // Phase `separate_disk_secret_action_keeps_backend_credentials_distinct_and_private`.
    {
        let backend = FakeBackend::new();
        let (mut app, mut doc) = app(&backend);
        app.step = Step::Account;
        let account = "account-random-value-2026";
        let disk = "disk-random-value-2026";
        let set_disk = invoke(
            &mut app,
            &mut doc,
            "set-disk-passphrase@installer.account",
            &json!({"passphrase": disk, "confirmation": disk}),
        );
        assert!(set_disk.get("answer").is_some(), "{set_disk}");
        let set_account = invoke(
            &mut app,
            &mut doc,
            "set-passphrase@installer.account",
            &json!({"passphrase": account, "confirmation": account}),
        );
        assert!(set_account.get("answer").is_some(), "{set_account}");
        assert_ne!(app.install_options().pin.as_str(), account);
        assert_eq!(app.install_options().pin.as_str(), disk);
        for output in [
            set_disk.to_string(),
            set_account.to_string(),
            state(&mut app, &mut doc).to_string(),
        ] {
            assert!(!output.contains(account) && !output.contains(disk));
        }
        let same = invoke(
            &mut app,
            &mut doc,
            "set-passphrase@installer.account",
            &json!({"passphrase": disk, "confirmation": disk}),
        );
        assert_eq!(refused(&same), Some("action_failed"));
        assert!(!same.to_string().contains(disk));
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one test walks every wizard step in order, asserting each refusal where it happens"
)]
fn the_whole_wizard_runs_through_the_console_with_its_confirmations() {
    let backend = FakeBackend::new();
    let (mut app, mut doc) = app(&backend);
    let doc = &mut doc;

    // Off-step actions are refused with the reason the wizard would give.
    let early = invoke(
        &mut app,
        doc,
        "set-hostname@installer.options",
        &json!({"hostname": "x"}),
    );
    assert_eq!(refused(&early), Some("action_unavailable"));
    assert!(early.to_string().contains("Options"), "{early}");

    assert!(
        invoke(
            &mut app,
            doc,
            "set-layout@installer.keyboard",
            &json!({"layout": "German (QWERTZ)"})
        )
        .get("answer")
        .is_some()
    );
    let bad = invoke(
        &mut app,
        doc,
        "set-layout@installer.keyboard",
        &json!({"layout": "Klingon"}),
    );
    assert_eq!(refused(&bad), Some("invalid_arguments"), "{bad}");
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "network"
    );
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "disk"
    );

    // Disk: nothing selected yet — Next fails with the wizard's own words.
    let blocked = invoke(&mut app, doc, "next@installer", &json!({}));
    assert_eq!(refused(&blocked), Some("action_failed"));
    assert!(
        blocked.to_string().contains("Select a disk to continue"),
        "{blocked}"
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "set-method@installer.storage",
            &json!({"method": "erase"})
        )
        .get("answer")
        .is_some()
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "select@installer.disk:/dev/nvme0n1",
            &json!({})
        )
        .get("answer")
        .is_some()
    );
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "account"
    );

    // Account: typed validation, and secrets never echo.
    let bad_user = invoke(
        &mut app,
        doc,
        "set-identity@installer.account",
        &json!({"username": "Root"}),
    );
    assert_eq!(refused(&bad_user), Some("action_failed"));
    assert!(
        invoke(
            &mut app,
            doc,
            "set-identity@installer.account",
            &json!({"username": "ada", "full_name": "Ada L"})
        )
        .get("answer")
        .is_some()
    );
    let mismatch = invoke(
        &mut app,
        doc,
        "set-passphrase@installer.account",
        &json!({"passphrase": SECRET, "confirmation": "different words entirely"}),
    );
    assert_eq!(refused(&mismatch), Some("action_failed"));
    assert!(!mismatch.to_string().contains(SECRET));
    let set = invoke(
        &mut app,
        doc,
        "set-passphrase@installer.account",
        &json!({"passphrase": SECRET, "confirmation": SECRET}),
    );
    assert!(set.get("answer").is_some(), "{set}");
    assert!(!set.to_string().contains(SECRET));
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "options"
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "set-hostname@installer.options",
            &json!({"hostname": "lab-1"})
        )
        .get("answer")
        .is_some()
    );
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "review"
    );

    // Review: the destructive action is published with its exact phrase and
    // refused until the recovery promise, then until the phrase is right.
    let listed = console(
        &mut app,
        doc,
        &json!({"op": "actions_enumerate", "query": "begin"}),
    );
    let begin = &listed["answer"]["actions"][0];
    assert_eq!(begin["id"], "begin@installer.review");
    assert_eq!(begin["contract"]["risk"], "destructive");
    assert_eq!(begin["contract"]["confirmation"]["phrase"], "/dev/nvme0n1");
    assert_eq!(begin["available"], false);
    assert_eq!(begin["unavailable"]["code"], "recovery_key_promise");
    assert_eq!(
        refused(&invoke(
            &mut app,
            doc,
            "begin@installer.review",
            &json!({"confirmation_phrase": "/dev/nvme0n1"})
        )),
        Some("action_unavailable")
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "set-recovery-promise@installer.review",
            &json!({"acknowledged": true})
        )
        .get("answer")
        .is_some()
    );
    for wrong in [
        json!({}),
        json!({"confirmation_phrase": "/dev/sda"}),
        json!({"confirm": true}),
    ] {
        assert_eq!(
            refused(&invoke(&mut app, doc, "begin@installer.review", &wrong)),
            Some("action_confirmation_required"),
            "{wrong}"
        );
    }
    assert!(
        backend.installs.lock().unwrap().is_empty(),
        "nothing ran before confirmation"
    );
    let started = invoke(
        &mut app,
        doc,
        "begin@installer.review",
        &json!({"confirmation_phrase": "/dev/nvme0n1"}),
    );
    assert!(started.get("answer").is_some(), "{started}");

    // The backend got exactly the reviewed choices, and the secret only there.
    let installs = backend.installs.lock().unwrap().clone();
    assert_eq!(installs.len(), 1);
    let (disk, user, password, opts) = &installs[0];
    assert_eq!(
        (disk.as_str(), user.as_str(), password.as_str()),
        ("/dev/nvme0n1", "ada", SECRET)
    );
    assert_eq!(opts.hostname, "lab-1");
    assert_eq!(opts.keymap, "de");
    assert_eq!(opts.pin.as_str(), SECRET);

    // Finish: the key exists but never leaves; power waits for the saved
    // acknowledgement and then goes through the backend.
    let finished = state(&mut app, doc);
    assert_eq!(finished["install"]["state"], "done");
    assert_eq!(finished["recovery_key"]["available"], true);
    let everything = format!(
        "{finished}{}",
        console(&mut app, doc, &json!({"op": "actions_enumerate"}))
    );
    assert!(
        !everything.contains(KEY),
        "recovery key leaked through the console"
    );
    assert!(
        !everything.contains(SECRET),
        "passphrase leaked through the console"
    );
    assert!(
        finished["install"]["log_tail"]
            .to_string()
            .contains("[redacted]")
    );
    assert_eq!(
        refused(&invoke(
            &mut app,
            doc,
            "power-off@installer.power",
            &json!({})
        )),
        Some("action_unavailable")
    );
    assert!(backend.power.lock().unwrap().is_empty());
    assert!(
        invoke(
            &mut app,
            doc,
            "confirm-saved@installer.recovery",
            &json!({"saved": true})
        )
        .get("answer")
        .is_some()
    );
    assert!(
        invoke(&mut app, doc, "power-off@installer.power", &json!({}))
            .get("answer")
            .is_some()
    );
    assert_eq!(*backend.power.lock().unwrap(), [PowerAction::PowerOff]);
}
