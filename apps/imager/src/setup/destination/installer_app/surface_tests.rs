//! The installer's action model: validation, the destructive confirmation and its state
//! transitions.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::setup::destination::effects::{DestinationEffect, STORAGE, reviewed_digest};
use automerge::AutoCommit;
use imager_backend::destination::disks::LINUX_FILESYSTEM_GUID;
use tilo_action_line::frame_line;
use tilo_resource::Effect as _;
use ui::tilo_actions::remote::{
    CatalogueQuery, CatalogueRow, Confirmation, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};

mod capacity_tests;
mod method_intent_tests;

pub(super) struct FakeBackend {
    disks: Vec<Disk>,
    pub(super) installs: Mutex<Vec<(String, String, String, InstallOptions)>>,
    pub(super) power: Mutex<Vec<PowerAction>>,
    succeed: bool,
}

pub(super) const KEY: &str =
    "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
pub(super) const SECRET: &str = "correct horse battery staple";

impl FakeBackend {
    pub(super) fn new() -> Arc<Self> {
        Self::finishing(true)
    }

    pub(super) fn finishing(succeed: bool) -> Arc<Self> {
        Arc::new(Self {
            disks: Disk::samples(),
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed,
        })
    }

    pub(super) fn with_disks(disks: Vec<Disk>) -> Arc<Self> {
        Arc::new(Self {
            disks,
            installs: Mutex::new(Vec::new()),
            power: Mutex::new(Vec::new()),
            succeed: true,
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
    app.tpm2 = false;
    let doc = AutoCommit::new();
    app.sync(&doc, &automerge::ROOT);
    (app, doc)
}

/// The installer's answer to one console line, as an endpoint gives it.
fn exchange(app: &mut InstallerApp, line: &str) -> Value {
    let console = imager_backend::destination::console::ConsoleServer::new(
        crate::setup::destination::effects::PROVIDER,
    );
    serde_json::from_str(&console.exchange(&mut Console(app), line)).unwrap()
}

fn frame(line: &Value) -> RemoteActionMessage {
    RemoteActionFrame::decode(&serde_json::to_vec(line).unwrap())
        .unwrap_or_else(|error| panic!("{error}: {line}"))
        .message
}

/// The actions the installer offers that `needle` names, as its console lists them.
fn listed(app: &mut InstallerApp, _doc: &mut AutoCommit, needle: &str) -> Vec<CatalogueRow> {
    // A listing is one level; a caller browses down into what each resource holds.
    let mut rows = Vec::new();
    let mut parents = vec![None];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(parent_resource) = parents.pop() {
        let asked = frame_line(RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                parent_resource,
                ..CatalogueQuery::default()
            },
        });
        let page = match frame(&exchange(app, &asked)) {
            RemoteActionMessage::CatalogueResponse {
                result: Ok(page), ..
            } => page,
            other => panic!("no catalogue: {other:?}"),
        };
        for row in page.rows {
            if seen.insert(row.resource.id.clone()) {
                parents.push(Some(row.resource.id.clone()));
            }
            rows.push(row);
        }
    }
    let needle = needle.to_lowercase();
    rows.retain(|row| {
        serde_json::to_string(row)
            .unwrap()
            .to_lowercase()
            .contains(&needle)
    });
    rows
}

/// Everything the console publishes, as one text: what a leak check reads.
fn public(app: &mut InstallerApp, doc: &mut AutoCommit) -> String {
    let rows = listed(app, doc, "");
    format!(
        "{}{}",
        state(app, doc),
        serde_json::to_string(&rows).unwrap()
    )
}

/// Invoke `action@resource` with `args`: `{"answer": ..}` with what it answered, or
/// `{"err": {"code", "message"}}` with why it was refused or failed. A test spells the
/// confirmation it gives in `args`: `confirmation_phrase` types the phrase. `confirm` claims a yes
/// the call writes itself, which answers nothing: only the system's confirm view gives one.
fn invoke(app: &mut InstallerApp, _doc: &mut AutoCommit, id: &str, args: &Value) -> Value {
    let (action, resource) = id.split_once('@').expect("action@resource");
    let mut arguments = args.as_object().cloned().unwrap_or_default();
    let phrase = arguments.remove("confirmation_phrase");
    arguments.remove("confirm");
    let published: Vec<_> = app
        .published()
        .iter()
        .map(|row| ui::tilo_actions::Exports::exported(row).clone())
        .collect();
    let address = ui::tilo_actions::api::published(&published, resource, action)
        .unwrap_or_else(|| panic!("{resource} publishes no {action}"));
    let mut request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: crate::setup::destination::effects::PROVIDER.into(),
            address,
        },
        arguments,
    );
    request.confirmation = match phrase {
        Some(Value::String(phrase)) => Some(Confirmation::Phrase(phrase)),
        _ => None,
    };
    let answer = exchange(app, &frame_line(RemoteActionMessage::Invoke(request)));
    let RemoteActionMessage::Receipt(receipt) = frame(&answer) else {
        panic!("no receipt: {answer}");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => json!({ "answer": output.value }),
        ReceiptStatus::Failed { error } => {
            json!({ "err": { "code": error.code, "message": error.message } })
        }
        ReceiptStatus::Rejected { error } => {
            json!({ "err": { "code": error.code(), "message": error.to_string() } })
        }
        other => panic!("a console action answers once it ran: {other:?}"),
    }
}

fn refused(reply: &Value) -> Option<&str> {
    reply.pointer("/err/code").and_then(Value::as_str)
}

fn state(app: &mut InstallerApp, _doc: &mut AutoCommit) -> Value {
    exchange(app, r#"{"op":"state_dump"}"#)["state"].clone()
}

tilo_test_env::scenario! {
    fn the_published_surface_refuses_what_was_not_reviewed;

    phase linux_without_unallocated_space_never_falls_back_to_erase;
    {
        for filesystem in ["ext4", "btrfs", "crypto_LUKS"] {
            let backend = FakeBackend::new();
            let (mut app, mut doc) = app(&backend);
            let mut disk = Disk::samples().remove(0);
            disk.partitions.truncate(2);
            let root = &mut disk.partitions[1];
            root.filesystem = filesystem.into();
            root.label = "Linux and Nextcloud files".into();
            root.part_type = LINUX_FILESYSTEM_GUID.into();
            root.size_bytes = disk.size_bytes - root.start_bytes - 1_048_576;
            app.refresh_disks_with(vec![disk]);
            app.step = Step::Disk;
            assert!(
                refused(&invoke(
                    &mut app,
                    &mut doc,
                    "set-method@installer",
                    &json!({"method":"alongside"})
                ))
                .is_none()
            );
            assert!(
                refused(&invoke(
                    &mut app,
                    &mut doc,
                    "select-disk@installer.disk:/dev/sda",
                    &json!({})
                ))
                .is_none()
            );
            let storage = app.published()[0].clone().into_resource();
            let storage = storage
                .children
                .iter()
                .find(|r| r.id.0 == STORAGE)
                .unwrap();
            let guidance = storage
                .children
                .iter()
                .filter(|r| r.kind == "installer.guidance")
                .map(|r| {
                    r.presentation
                        .as_ref()
                        .unwrap()
                        .preview
                        .as_ref()
                        .unwrap()
                        .as_str()
                })
                .collect::<Vec<_>>()
                .join(". ");
            assert!(guidance.contains("Free space inside ext4 or Btrfs is not unallocated"));
            assert!(guidance.contains("offline partitioning tools"));
            assert!(guidance.contains("Back up your files"));
            assert!(guidance.contains(
                "This installer will not automatically shrink existing filesystems, LVM, or LUKS"
            ));
            assert!(!guidance.contains(&crate::setup::strings::t(LINUX_ALONGSIDE_READY)));
            assert!(!guidance.contains("32 GiB root +"));
            assert!(!guidance.contains(&crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE)));
            assert!(!guidance.contains("Disk Management"));
            assert!(refused(&invoke(&mut app, &mut doc, "next@installer", &json!({}))).is_some());
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert!(app.destructive_confirmation_target().is_none());
            app.step = Step::Review;
            assert!(
                refused(&invoke(
                    &mut app,
                    &mut doc,
                    "begin@installer.review",
                    &json!({})
                ))
                .is_some()
            );
            assert!(backend.installs.lock().unwrap().is_empty());
        }
    }

    phase the_published_surface_is_valid_and_offers_a_way_back_only_when_staged;
    {
        {
            let backend = FakeBackend::new();
            let (mut app, _doc) = app(&backend);
            let handlers: HashSet<String> = [DestinationEffect::OPERATION.to_owned()].into();
            for step in [
                Step::Keyboard,
                Step::Network,
                Step::Disk,
                Step::Account,
                Step::Options,
                Step::Media,
                Step::Review,
                Step::Install,
            ] {
                app.step = step;
                let resources: Vec<tilo_actions::ResourceSpec> = app
                    .published()
                    .into_iter()
                    .map(tilo_resource::LiveRow::into_resource)
                    .collect();
                let violations = tilo_actions::validate_surface(
                    &resources,
                    &tilo_actions::ActionRegistry::new(),
                    &handlers,
                );
                assert!(violations.is_empty(), "{step:?}: {violations:?}");
                let described = tilo_actions::api::survey(&resources);
                assert!(
                    described.iter().any(|d| d.address.action == "status"),
                    "{step:?} has no status"
                );
                for descriptor in &described {
                    if let tilo_actions::Availability::Unavailable { code, reason } =
                        &descriptor.contract.availability
                    {
                        assert!(!code.is_empty() && !reason.is_empty(), "{descriptor:?}");
                    }
                }
            }
        }
        {
            let backend = Arc::new(FakeBackend {
                disks: imager_backend::destination::disks::parse_lsblk(
                    &imager_backend::destination::internal_handoff::tests::lsblk_json().to_string(),
                ),
                installs: Mutex::new(Vec::new()),
                power: Mutex::new(Vec::new()),
                succeed: true,
            });
            let (mut app, mut doc) = app(&backend);
            app.refresh_disks_with(backend.read_disks());
            app.apply_internal_handoff(imager_backend::destination::internal_handoff::tests::handoff());
            let doc = &mut doc;
            app.step = Step::Disk;
            let device = app.disks[0].name.clone();

            let selected = invoke(
                &mut app,
                doc,
                &DestinationEffect::SelectDisk { device }.console_id(),
                &json!({}),
            );
            assert!(selected.get("answer").is_some(), "{selected}");
            let erase = invoke(
                &mut app,
                doc,
                "set-method@installer",
                &json!({"method": "erase"}),
            );
            assert_eq!(refused(&erase), Some("action_failed"), "{erase}");
            assert_eq!(app.install_method, InstallMethod::Alongside);

            app.install_state = InstallState::Running;
            let running = invoke(&mut app, doc, "return-to-host@installer", &json!({}));
            assert_eq!(refused(&running), Some("refused"), "{running}");
            assert!(backend.power.lock().unwrap().is_empty());

            app.install_state = InstallState::Failed;
            let back = invoke(&mut app, doc, "return-to-host@installer", &json!({}));
            assert!(back.get("answer").is_some(), "{back}");
            assert_eq!(*backend.power.lock().unwrap(), vec![PowerAction::Reboot]);
        }
        {
            let backend = FakeBackend::new();
            let (mut app, mut doc) = app(&backend);
            let reply = invoke(&mut app, &mut doc, "return-to-host@installer", &json!({}));
            assert!(refused(&reply).is_some(), "{reply}");
        }
    }

    phase every_api_reaches_the_same_handler_and_refuses_stale_or_private_input;
    {
        {
            let backend = FakeBackend::new();
            let (mut by_row, mut row_doc) = app(&backend);
            let (mut by_action, mut action_doc) = app(&backend);
            for app in [&mut by_row, &mut by_action] {
                app.step = Step::Disk;
                app.erase_confirmation.set("stale".into());
            }
            tests::activate(&mut by_row, &mut row_doc, "/dev/nvme0n1");
            let _ = invoke(
                &mut by_action,
                &mut action_doc,
                "select-disk@installer.disk:/dev/nvme0n1",
                &json!({}),
            );
            assert_eq!(by_row.selected_disk, Some(1));
            assert_eq!(by_row.selected_disk, by_action.selected_disk);
            assert_eq!(local_text(&by_row.erase_confirmation), "");
            assert_eq!(
                local_text(&by_row.erase_confirmation),
                local_text(&by_action.erase_confirmation)
            );

            by_row.step = Step::Install;
            by_row.install_state = InstallState::Done;
            by_row.recovery_key = Some(KEY.into());
            by_row.control(DestinationEffect::Reboot).fire();
            by_row.sync(&row_doc, &automerge::ROOT);
            let reason = by_row
                .published()
                .into_iter()
                .flat_map(|root| root.into_resource().actions)
                .find(|action| action.id == "reboot")
                .map(|reboot| match &reboot.contract.availability {
                    tilo_actions::Availability::Unavailable { reason, .. } => reason.clone(),
                    tilo_actions::Availability::Available => String::new(),
                })
                .unwrap();
            assert_eq!(by_row.status, reason);
            assert!(backend.power.lock().unwrap().is_empty());
        }
        {
            let backend = FakeBackend::new();
            let (mut app, _doc) = app(&backend);
            app.apply(DestinationEffect::Next).unwrap();
            assert_eq!(app.step, Step::Network);

            app.step = Step::Review;
            app.selected_disk = Some(1);
            app.recovery_ack = true;
            *app.password.lock().unwrap() = SECRET.to_owned();
            let refused = app.apply(DestinationEffect::Begin {
                target: "/dev/sda".into(),
                allocation: reviewed_digest(&app.guided_alongside_plan()),
            });
            assert!(refused.is_err());
            assert!(backend.installs.lock().unwrap().is_empty());
        }
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
            let review = listed(&mut app, &mut doc, "Review storage");
            assert_eq!(review.len(), 1, "{review:?}");
            let back = invoke(&mut app, &mut doc, "back@installer", &json!({}));
            assert_eq!(back["answer"]["step"], "disk", "{back}");
            assert_eq!(
                app.selected_disk, None,
                "a failed target is never pre-selected again"
            );
        }
        {
            let backend = FakeBackend::new();
            let (mut app, mut doc) = app(&backend);
            app.step = Step::Account;
            let account = "account-random-value-2026";
            let disk = "disk-random-value-2026";
            let set_disk = invoke(
                &mut app,
                &mut doc,
                "set-disk-passphrase@installer",
                &json!({"passphrase": disk, "confirmation": disk}),
            );
            assert!(set_disk.get("answer").is_some(), "{set_disk}");
            let set_account = invoke(
                &mut app,
                &mut doc,
                "set-passphrase@installer",
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
                "set-passphrase@installer",
                &json!({"passphrase": disk, "confirmation": disk}),
            );
            assert_eq!(refused(&same), Some("action_failed"));
            assert!(!same.to_string().contains(disk));
        }
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

    let early = invoke(
        &mut app,
        doc,
        "set-hostname@installer",
        &json!({"hostname": "x"}),
    );
    assert_eq!(refused(&early), Some("refused"));
    assert!(early.to_string().contains("Options"), "{early}");

    assert!(
        invoke(
            &mut app,
            doc,
            "set-layout@installer",
            &json!({"layout": "German (QWERTZ)"})
        )
        .get("answer")
        .is_some()
    );
    let bad = invoke(
        &mut app,
        doc,
        "set-layout@installer",
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
            "set-method@installer",
            &json!({"method": "erase"})
        )
        .get("answer")
        .is_some()
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "select-disk@installer.disk:/dev/nvme0n1",
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
        "set-identity@installer",
        &json!({"username": "Root"}),
    );
    assert_eq!(refused(&bad_user), Some("action_failed"));
    assert!(
        invoke(
            &mut app,
            doc,
            "set-identity@installer",
            &json!({"username": "ada", "full_name": "Ada L"})
        )
        .get("answer")
        .is_some()
    );
    let mismatch = invoke(
        &mut app,
        doc,
        "set-passphrase@installer",
        &json!({"passphrase": SECRET, "confirmation": "different words entirely"}),
    );
    assert_eq!(refused(&mismatch), Some("action_failed"));
    assert!(!mismatch.to_string().contains(SECRET));
    let set = invoke(
        &mut app,
        doc,
        "set-passphrase@installer",
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
            "set-hostname@installer",
            &json!({"hostname": "lab-1"})
        )
        .get("answer")
        .is_some()
    );
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "media"
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "set-media-codecs@installer",
            &json!({"enabled": true})
        )
        .get("answer")
        .is_some()
    );
    assert!(app.codecs);
    assert_eq!(
        invoke(&mut app, doc, "next@installer", &json!({}))["answer"]["step"],
        "review"
    );

    let offered = listed(&mut app, doc, "begin");
    let begin = offered
        .iter()
        .find_map(|row| {
            row.action
                .as_ref()
                .filter(|action| action.address.action == "begin")
        })
        .expect("begin is offered");
    assert_eq!(begin.address.resource, "installer.review");
    assert_eq!(
        begin.contract.risk,
        ui::tilo_actions::RiskLevel::Destructive
    );
    assert_eq!(
        begin.contract.confirmation,
        ui::tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "/dev/nvme0n1".into()
        }
    );
    assert!(matches!(
        &begin.contract.availability,
        ui::tilo_actions::Availability::Unavailable { code, .. } if code == "recovery_key_promise"
    ));
    assert_eq!(
        refused(&invoke(
            &mut app,
            doc,
            "begin@installer.review",
            &json!({"confirmation_phrase": "/dev/nvme0n1"})
        )),
        Some("refused")
    );
    assert!(
        invoke(
            &mut app,
            doc,
            "set-recovery-promise@installer",
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

    let finished = state(&mut app, doc);
    assert_eq!(finished["install"]["state"], "done");
    assert_eq!(finished["recovery_key"]["available"], true);
    let everything = format!("{finished}{}", public(&mut app, doc));
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
        refused(&invoke(&mut app, doc, "power-off@installer", &json!({}))),
        Some("refused")
    );
    assert!(backend.power.lock().unwrap().is_empty());
    assert!(
        invoke(
            &mut app,
            doc,
            "confirm-recovery-saved@installer",
            &json!({"saved": true})
        )
        .get("answer")
        .is_some()
    );
    assert!(
        invoke(&mut app, doc, "power-off@installer", &json!({}))
            .get("answer")
            .is_some()
    );
    assert_eq!(*backend.power.lock().unwrap(), [PowerAction::PowerOff]);
}
