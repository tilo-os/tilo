//! Drive the installer wizard through its published actions.
//!
//! The driver reads the installer's state, performs the actions of whatever
//! step it is on, and repeats. It never assumes the step order: a medium
//! customised by Tilo Imager skips screens whose choices it already carries,
//! and the loop simply finds itself further along. It never sees a secret
//! it did not supply itself: the recovery key stays on the guest's screen
//! and clipboard, and the passphrase is sent once and never read back.

use std::time::Duration;

use super::console::{Console, ConsoleError};
use super::{Layout, Secret, Unlock};
use serde_json::{Map, Value, json};

/// The console provider name the installer answers under.
pub const INSTALLER: &str = "installer";

/// What the scenario installs, in the installer's own terms.
pub struct Choices {
    pub layout: Layout,
    pub filesystem: Option<String>,
    pub unlock: Unlock,
    pub username: String,
    pub full_name: String,
    pub hostname: String,
    pub account_secret: Secret,
    pub disk_secret: Option<Secret>,
}

/// What the install reported, secret-free.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Installed {
    /// Steps in the order the driver found them.
    pub steps: Vec<String>,
    /// The object the destructive confirmation named.
    pub target: String,
    /// The composefs digest the live installer booted and installed.
    pub composefs: Option<String>,
    /// Human milestones the installer showed while installing.
    pub stages: Vec<String>,
    /// Status polls until the install finished.
    pub polls: u32,
}

/// Why driving stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveError {
    /// An action was refused or failed; `message` is the installer's own.
    Refused {
        action: String,
        code: String,
        message: String,
    },
    /// The console stopped answering.
    Console(String),
    /// The wizard stayed on one step, or reached a state the scenario cannot
    /// continue from.
    Stuck { step: String, detail: String },
    /// The installer reported a failed install; `log` is its redacted tail.
    InstallFailed { log: Vec<String> },
    /// The install ran past its time limit.
    Timeout { waited: Duration },
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused {
                action,
                code,
                message,
            } => write!(f, "{action} was refused ({code}): {message}"),
            Self::Console(why) => write!(f, "{why}"),
            Self::Stuck { step, detail } => write!(f, "stuck on the {step} step: {detail}"),
            Self::InstallFailed { log } => write!(
                f,
                "the installer reported a failed install; last output: {}",
                log.last().map_or("(none)", String::as_str)
            ),
            Self::Timeout { waited } => {
                write!(f, "the install did not finish within {}s", waited.as_secs())
            }
        }
    }
}

impl From<ConsoleError> for DriveError {
    fn from(error: ConsoleError) -> Self {
        Self::Console(error.to_string())
    }
}

/// Pacing for the polling loops, injectable so tests run instantly.
pub trait Pace {
    fn sleep(&mut self, duration: Duration);
}

/// Real time.
pub struct RealPace;

impl Pace for RealPace {
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How often the install's progress is polled.
pub const POLL: Duration = Duration::from_secs(5);

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn step_of(state: &Value) -> String {
    state
        .get("step")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

fn invoke(
    console: &mut dyn Console,
    resource: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, DriveError> {
    console
        .invoke(INSTALLER, resource, action, args(arguments))
        .map_err(|error| match error {
            ConsoleError::Refused { code, message } => DriveError::Refused {
                action: format!("{action}@{resource}"),
                code,
                message,
            },
            other => DriveError::Console(other.to_string()),
        })
}

/// The one disk the scenario may install onto: not the installer medium,
/// not read-only. Anything other than exactly one is a setup error, never
/// a guess.
///
/// # Errors
///
/// [`DriveError::Stuck`] when no disk, or more than one, qualifies.
pub fn choose_target(state: &Value) -> Result<String, DriveError> {
    let disks = state
        .pointer("/storage/disks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let usable: Vec<String> = disks
        .iter()
        .filter(|disk| {
            disk.get("installer_medium") != Some(&Value::Bool(true))
                && disk.get("read_only") != Some(&Value::Bool(true))
        })
        .filter_map(|disk| {
            disk.get("device")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    match usable.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(DriveError::Stuck {
            step: "disk".into(),
            detail:
                "no usable target disk is listed (only the installer medium, or read-only disks)"
                    .into(),
        }),
        several => Err(DriveError::Stuck {
            step: "disk".into(),
            detail: format!(
                "more than one usable disk ({}); a scenario VM has exactly one blank target",
                several.join(", ")
            ),
        }),
    }
}

/// Walk the wizard from wherever it is to a finished install, then confirm
/// the recovery key was dealt with and power the VM off through the
/// installer's own Finish action.
///
/// # Errors
///
/// [`DriveError`] with the installer's own reason when a step cannot be
/// completed, the install fails, or it runs past `install_timeout`.
#[expect(
    clippy::too_many_lines,
    reason = "one match arm per wizard step keeps the steps side by side"
)]
pub fn install(
    console: &mut dyn Console,
    choices: &Choices,
    install_timeout: Duration,
    pace: &mut dyn Pace,
    progress: &mut dyn FnMut(&str),
) -> Result<Installed, DriveError> {
    let mut installed = Installed::default();
    let mut last_step = String::new();
    let mut repeats = 0_u32;
    loop {
        let state = console.state()?;
        let step = step_of(&state);
        if installed.composefs.is_none() {
            installed.composefs = state
                .pointer("/image/composefs")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if step == last_step {
            repeats = repeats.saturating_add(1);
            if repeats > 3 {
                return Err(DriveError::Stuck {
                    step,
                    detail: state
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|status| !status.is_empty())
                        .unwrap_or("the wizard did not advance")
                        .to_owned(),
                });
            }
        } else {
            repeats = 0;
            installed.steps.push(step.clone());
            progress(&format!("installer: {step} step"));
        }
        last_step.clone_from(&step);
        match step.as_str() {
            "keyboard" | "network" => {
                invoke(console, INSTALLER, "next", json!({}))?;
            }
            "disk" => disk_step(console, &state, choices, pace)?,
            "account" => {
                invoke(
                    console,
                    "installer.account",
                    "set-identity",
                    json!({ "username": choices.username, "full_name": choices.full_name }),
                )?;
                if choices.unlock == Unlock::TpmPin
                    && state
                        .pointer("/account/secret_policy")
                        .and_then(Value::as_str)
                        != Some("tpm-pin")
                {
                    return Err(DriveError::Stuck {
                        step,
                        detail: "--encrypt tpm-pin was asked for, but the installer did not pass verified-boot preflight and offers only a passphrase (attach a software TPM with an enrolled Secure Boot store: TILO_SECBOOT_KEYS)".into(),
                    });
                }
                if let Some(disk_secret) = &choices.disk_secret {
                    invoke(
                        console,
                        "installer.account",
                        "set-disk-passphrase",
                        json!({ "passphrase": disk_secret.expose(), "confirmation": disk_secret.expose() }),
                    )?;
                }
                invoke(
                    console,
                    "installer.account",
                    "set-passphrase",
                    json!({ "passphrase": choices.account_secret.expose(), "confirmation": choices.account_secret.expose() }),
                )?;
                invoke(console, INSTALLER, "next", json!({}))?;
            }
            "options" => {
                if let Some(filesystem) = &choices.filesystem {
                    invoke(
                        console,
                        "installer.options",
                        "set-filesystem",
                        json!({ "filesystem": filesystem }),
                    )?;
                }
                invoke(
                    console,
                    "installer.options",
                    "set-hostname",
                    json!({ "hostname": choices.hostname }),
                )?;
                invoke(console, INSTALLER, "next", json!({}))?;
            }
            "review" => {
                invoke(
                    console,
                    "installer.review",
                    "set-recovery-promise",
                    json!({ "acknowledged": true }),
                )?;
                let target = console
                    .state()?
                    .pointer("/storage/confirmation_target")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| DriveError::Stuck {
                        step: step.clone(),
                        detail: "the installer names no confirmation target".into(),
                    })?;
                progress(&format!("installer: begin, confirming {target}"));
                invoke(
                    console,
                    "installer.review",
                    "begin",
                    json!({ "confirmation_phrase": target }),
                )?;
                installed.target = target;
            }
            "install" => {
                wait_for_install(console, &mut installed, install_timeout, pace, progress)?;
                finish(console, &mut installed, progress)?;
                return Ok(installed);
            }
            other => {
                return Err(DriveError::Stuck {
                    step: other.to_owned(),
                    detail: "the scenario does not know this step".into(),
                });
            }
        }
    }
}

fn disk_step(
    console: &mut dyn Console,
    state: &Value,
    choices: &Choices,
    pace: &mut dyn Pace,
) -> Result<(), DriveError> {
    invoke(
        console,
        "installer.storage",
        "set-method",
        json!({ "method": choices.layout.method() }),
    )?;
    let target = choose_target(state)?;
    invoke(
        console,
        &format!("installer.disk:{target}"),
        "select",
        json!({}),
    )?;
    let regions = console
        .state()?
        .pointer("/storage/disks")
        .and_then(Value::as_array)
        .and_then(|disks| {
            disks
                .iter()
                .find(|disk| disk.get("device").and_then(Value::as_str) == Some(target.as_str()))
                .and_then(|disk| disk.get("free_regions").and_then(Value::as_u64))
        })
        .unwrap_or(0);
    if choices.layout == Layout::GuidedAlongside && regions > 1 {
        invoke(console, "installer.region:0", "select", json!({}))?;
    }
    invoke(console, INSTALLER, "next", json!({}))?;
    // A prepared alongside target runs a read-only preflight before the
    // wizard leaves the Disk step.
    for _ in 0..60 {
        let now = console.state()?;
        if step_of(&now) != "disk"
            || now.pointer("/storage/preflight").and_then(Value::as_str) != Some("running")
        {
            return Ok(());
        }
        pace.sleep(Duration::from_secs(2));
    }
    Err(DriveError::Stuck {
        step: "disk".into(),
        detail: "the prepared-storage preflight did not finish within two minutes".into(),
    })
}

fn wait_for_install(
    console: &mut dyn Console,
    installed: &mut Installed,
    install_timeout: Duration,
    pace: &mut dyn Pace,
    progress: &mut dyn FnMut(&str),
) -> Result<(), DriveError> {
    let limit = u32::try_from(
        install_timeout
            .as_secs()
            .checked_div(POLL.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(u32::MAX);
    let mut seen = 0_usize;
    for poll in 0..=limit {
        installed.polls = poll;
        let state = match console.state() {
            Ok(state) => state,
            // The guest is busy copying gigabytes; one missed poll is not a
            // failure, a silent console for the whole budget is.
            Err(ConsoleError::Unreachable(_)) => {
                pace.sleep(POLL);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let stages: Vec<String> = state
            .pointer("/install/stages")
            .and_then(Value::as_array)
            .map(|stages| {
                stages
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        for stage in stages.iter().skip(seen) {
            progress(&format!("installer: {stage}"));
        }
        seen = seen.max(stages.len());
        installed.stages = stages;
        match state.pointer("/install/state").and_then(Value::as_str) {
            Some("done") => return Ok(()),
            Some("failed") => {
                return Err(DriveError::InstallFailed {
                    log: state
                        .pointer("/install/log_tail")
                        .and_then(Value::as_array)
                        .map(|lines| {
                            lines
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            _ => pace.sleep(POLL),
        }
    }
    Err(DriveError::Timeout {
        waited: install_timeout,
    })
}

fn finish(
    console: &mut dyn Console,
    installed: &mut Installed,
    progress: &mut dyn FnMut(&str),
) -> Result<(), DriveError> {
    let state = console.state()?;
    if state.pointer("/recovery_key/available") == Some(&Value::Bool(true)) {
        // A disposable scenario target: the key is left on the guest's
        // screen, which is the only place it ever appears.
        invoke(
            console,
            "installer.recovery",
            "confirm-saved",
            json!({ "saved": true }),
        )?;
    } else {
        return Err(DriveError::Stuck {
            step: "install".into(),
            detail: "the install finished without generating a recovery key".into(),
        });
    }
    progress("installer: power off");
    match invoke(console, "installer.power", "power-off", json!({})) {
        // The guest going down can cut the reply off; the caller confirms
        // the VM stopped.
        Ok(_) | Err(DriveError::Console(_)) => Ok(()),
        Err(error) => Err(error),
    }?;
    installed.steps.push("power-off".into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted installer: answers `state_dump` from its model and applies
    /// invocations the way the real one does, refusing out-of-step actions.
    struct FakeInstaller {
        step: &'static str,
        method: String,
        selected: Option<String>,
        promise: bool,
        passphrase: Option<String>,
        disk_passphrase: Option<String>,
        install_polls: VecDeque<&'static str>,
        calls: Vec<String>,
        policy: &'static str,
    }

    impl FakeInstaller {
        fn new() -> Self {
            Self {
                step: "keyboard",
                method: "erase".into(),
                selected: None,
                promise: false,
                passphrase: None,
                disk_passphrase: None,
                install_polls: VecDeque::from(["running", "running", "done"]),
                calls: Vec::new(),
                policy: "passphrase",
            }
        }

        fn state(&mut self) -> Value {
            let install = if self.step == "install" {
                self.install_polls.pop_front().unwrap_or("done")
            } else {
                "idle"
            };
            json!({
                "app": "installer",
                "step": self.step,
                "status": "",
                "storage": {
                    "method": self.method,
                    "disks": [
                        {"device": "/dev/vda", "installer_medium": true, "read_only": false, "free_regions": 0},
                        {"device": "/dev/vdb", "installer_medium": false, "read_only": false, "free_regions": 0},
                    ],
                    "confirmation_target": self.selected,
                    "preflight": "not-run",
                },
                "account": {"secret_policy": self.policy},
                "install": {"state": install, "stages": ["Copying verified Tilo system files…"], "log_tail": []},
                "recovery_key": {"available": self.step == "install"},
                "image": {"composefs": "cafe"},
            })
        }

        fn refuse(code: &str) -> Value {
            json!({"err": {"code": code, "message": "refused by the fake"}})
        }

        fn invoke(&mut self, id: &str, args: &Value) -> Value {
            self.calls.push(id.to_owned());
            let ok = json!({"answer": {"id": id}});
            match (self.step, id) {
                ("keyboard", "next@installer") => self.step = "network",
                ("network", "next@installer") => self.step = "disk",
                ("disk", "set-method@installer.storage") => {
                    self.method = args["method"].as_str().unwrap().into();
                }
                ("options", "set-filesystem@installer.options") => {
                    assert_eq!(args["filesystem"], "ext4");
                }
                ("disk", "select@installer.disk:/dev/vdb") => {
                    self.selected = Some("/dev/vdb".into());
                }
                ("disk", "next@installer") if self.selected.is_some() => self.step = "account",
                ("account", "set-passphrase@installer.account") => {
                    assert_eq!(args["passphrase"], args["confirmation"]);
                    self.passphrase = args["passphrase"].as_str().map(str::to_owned);
                }
                ("account", "set-disk-passphrase@installer.account") => {
                    assert_eq!(args["passphrase"], args["confirmation"]);
                    self.disk_passphrase = args["passphrase"].as_str().map(str::to_owned);
                }
                ("account", "next@installer") if self.passphrase.is_some() => self.step = "options",
                ("options", "next@installer") => self.step = "review",
                ("review", "set-recovery-promise@installer.review") => self.promise = true,
                ("review", "begin@installer.review") => {
                    if !self.promise {
                        return Self::refuse("action_unavailable");
                    }
                    if args.get("confirmation_phrase").and_then(Value::as_str)
                        != self.selected.as_deref()
                    {
                        return Self::refuse("action_confirmation_required");
                    }
                    self.step = "install";
                }
                ("account", "set-identity@installer.account")
                | ("options", "set-hostname@installer.options")
                | ("install", "confirm-saved@installer.recovery" | "power-off@installer.power") => {
                }
                _ => return Self::refuse("action_unavailable"),
            }
            ok
        }
    }

    impl Console for FakeInstaller {
        fn request(&mut self, request: Value) -> Result<Value, ConsoleError> {
            match request["op"].as_str() {
                Some("state_dump") => Ok(json!({ "state": self.state() })),
                Some("actions_invoke") => {
                    let id = request["id"].as_str().unwrap().to_owned();
                    Ok(self.invoke(&id, &request["args"]))
                }
                _ => Err(ConsoleError::Protocol("unexpected op".into())),
            }
        }
    }

    struct NoWait(u32);

    impl Pace for NoWait {
        fn sleep(&mut self, _: Duration) {
            self.0 = self.0.saturating_add(1);
        }
    }

    fn choices(unlock: Unlock) -> Choices {
        Choices {
            layout: Layout::Erase,
            filesystem: None,
            unlock,
            username: "scenario".into(),
            full_name: "Scenario User".into(),
            hostname: "scenario-tilo".into(),
            account_secret: Secret::new("scenario-passphrase-2026"),
            disk_secret: None,
        }
    }

    /// The whole wizard is driven through actions in whatever order it presents; distinct credentials
    /// reach distinct published installer actions; a skipped screen is simply not visited.
    #[test]
    fn the_installer_wizard_is_driven_through_its_actions() {
        // Phase: the_whole_wizard_is_driven_through_actions_in_whatever_order_it_presents.
        {
            let mut fake = FakeInstaller::new();
            let mut lines = Vec::new();
            let installed = install(
                &mut fake,
                &choices(Unlock::Passphrase),
                Duration::from_secs(60),
                &mut NoWait(0),
                &mut |line| lines.push(line.to_owned()),
            )
            .unwrap();
            assert_eq!(
                installed.steps,
                [
                    "keyboard",
                    "network",
                    "disk",
                    "account",
                    "options",
                    "review",
                    "install",
                    "power-off"
                ]
            );
            assert_eq!(installed.target, "/dev/vdb");
            assert_eq!(installed.composefs.as_deref(), Some("cafe"));
            assert!(fake.calls.contains(&"begin@installer.review".to_string()));
            assert_eq!(
                fake.calls.last().map(String::as_str),
                Some("power-off@installer.power")
            );
            assert!(
                !lines
                    .iter()
                    .any(|line| line.contains("scenario-passphrase")),
                "progress must never print the passphrase: {lines:?}"
            );
        }

        // Phase: distinct_credentials_reach_distinct_published_installer_actions.
        {
            let mut fake = FakeInstaller::new();
            let mut chosen = choices(Unlock::Passphrase);
            chosen.username = "bob".into();
            chosen.filesystem = Some("ext4".into());
            chosen.account_secret = Secret::new("account-password-for-testing");
            chosen.disk_secret = Some(Secret::new("disk-passphrase-for-testing"));
            let mut lines = Vec::new();
            install(
                &mut fake,
                &chosen,
                Duration::from_secs(60),
                &mut NoWait(0),
                &mut |line| lines.push(line.to_owned()),
            )
            .unwrap();
            assert!(
                fake.calls
                    .contains(&"set-filesystem@installer.options".to_string())
            );
            assert_eq!(
                fake.passphrase.as_deref(),
                Some("account-password-for-testing")
            );
            assert_eq!(
                fake.disk_passphrase.as_deref(),
                Some("disk-passphrase-for-testing")
            );
            assert!(
                fake.calls
                    .contains(&"set-disk-passphrase@installer.account".into())
            );
            assert!(!lines.iter().any(|line| {
                line.contains("account-password-for-testing")
                    || line.contains("disk-passphrase-for-testing")
            }));
        }

        // Phase: a_skipped_screen_is_simply_not_visited.
        {
            let mut fake = FakeInstaller::new();
            fake.step = "disk";
            let installed = install(
                &mut fake,
                &choices(Unlock::Passphrase),
                Duration::from_secs(60),
                &mut NoWait(0),
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(installed.steps.first().map(String::as_str), Some("disk"));
        }
    }

    /// A failed install reports the installer log and a timeout is bounded; a TPM PIN request without
    /// verified boot fails instead of downgrading; the target choice refuses to guess.
    #[test]
    fn the_installer_drive_fails_closed_instead_of_guessing() {
        // Phase: a_failed_install_reports_the_installer_log_and_a_timeout_is_bounded.
        {
            let mut fake = FakeInstaller::new();
            fake.install_polls = VecDeque::from(["running", "failed"]);
            let error = install(
                &mut fake,
                &choices(Unlock::Passphrase),
                Duration::from_secs(60),
                &mut NoWait(0),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(matches!(error, DriveError::InstallFailed { .. }), "{error}");

            let mut slow = FakeInstaller::new();
            slow.install_polls = std::iter::repeat_n("running", 100).collect();
            let mut pace = NoWait(0);
            let error = install(
                &mut slow,
                &choices(Unlock::Passphrase),
                Duration::from_secs(20),
                &mut pace,
                &mut |_| {},
            )
            .unwrap_err();
            assert!(matches!(error, DriveError::Timeout { .. }), "{error}");
            assert!(pace.0 <= 5, "the loop is bounded by the time budget");
        }

        // Phase: a_tpm_pin_request_without_verified_boot_fails_instead_of_downgrading.
        {
            let mut fake = FakeInstaller::new();
            let error = install(
                &mut fake,
                &choices(Unlock::TpmPin),
                Duration::from_secs(60),
                &mut NoWait(0),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(
                matches!(&error, DriveError::Stuck { step, .. } if step == "account"),
                "{error}"
            );
            assert!(
                !fake
                    .calls
                    .contains(&"set-passphrase@installer.account".to_string()),
                "no secret may be sent under a policy the scenario did not ask for"
            );
        }

        // Phase: target_choice_refuses_to_guess.
        {
            let one = json!({"storage": {"disks": [
                {"device": "/dev/vda", "installer_medium": true},
                {"device": "/dev/vdb", "installer_medium": false, "read_only": false},
            ]}});
            assert_eq!(choose_target(&one).unwrap(), "/dev/vdb");
            let two = json!({"storage": {"disks": [
                {"device": "/dev/vdb"}, {"device": "/dev/vdc"},
            ]}});
            assert!(choose_target(&two).is_err());
            let none =
                json!({"storage": {"disks": [{"device": "/dev/vda", "installer_medium": true}]}});
            assert!(choose_target(&none).is_err());
        }
    }
}
