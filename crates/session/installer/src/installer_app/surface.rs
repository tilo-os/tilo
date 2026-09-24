//! The installer's published surface — the resources it has open, the
//! actions each one exports — and [`InstallerApp::apply`], the one function
//! every door ends in.
//!
//! Availability mirrors the wizard: an action is available exactly when its
//! control is live on screen, and otherwise carries the reason a person
//! would be told. Nothing is hidden to make the surface look smaller; a
//! caller that cannot see the screen learns the whole wizard from one
//! enumeration and why each part is or is not reachable now.

use serde_json::{Map, Value, json};
use tilo_actions::{
    ActionArgs, ActionContract, ActionOutput, ActionParam, ActionSpec, Availability,
    ConfirmationPolicy, EncodedAction, OperationKind, ParamType, Resource, ResourcePresentation,
    ResultSpec, RiskLevel,
};
use zeroize::Zeroizing;

use super::{
    ERASE_CONFIRM_PATH, FILESYSTEMS, FULL_NAME_PATH, HOSTNAME_PATH, InstallMethod, InstallState,
    InstallerApp, KEYMAP_PATH, LOCALE_PATH, STEP_LABELS, Step, TIMEZONE_PATH, USERNAME_PATH,
    VerifiedBootState, clear_secret_buffer, prev_step, secret_copy, step_index, valid_full_name,
    valid_hostname, valid_locale, valid_timezone_name, valid_username, validate_passwords,
    validate_startup_secret_for,
};
use crate::install::PowerAction;
use crate::layouts;
use crate::operation::{
    ACCOUNT, KEYBOARD, NETWORK, OPERATION_TYPE, OPTIONS, Operation, POWER, RECOVERY, REVIEW,
    SESSION, STORAGE, disk_resource, region_resource,
};
use automerge::{AutoCommit, ObjId};

/// A reason an action cannot run now: a stable code and the sentence the
/// wizard would show.
type Blocked = Option<(&'static str, String)>;

fn encoded(operation: &Operation) -> EncodedAction {
    EncodedAction::encode_operation(OPERATION_TYPE, operation).unwrap_or_else(|_| EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: b"installer operation could not be encoded".to_vec(),
    })
}

/// An effect operation with typed parameters, built by `build` from the
/// arguments the door already bound strictly against `params`.
fn typed(
    id: &str,
    label: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> Operation + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "",
        OperationKind::Effect,
        OPERATION_TYPE,
        params,
        move |args| encoded(&build(args)),
    )
}

fn nullary(id: &str, label: &str, operation: &Operation) -> ActionSpec {
    let operation = operation.clone();
    typed(id, label, Vec::new(), move |_| operation.clone())
}

fn gate(spec: ActionSpec, blocked: Blocked) -> ActionSpec {
    match blocked {
        Some((code, reason)) => spec.unavailable(code, reason),
        None => spec,
    }
}

/// Longest prefix of `text` within `limit` bytes, on a character boundary.
fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn step_name(step: Step) -> &'static str {
    STEP_LABELS
        .get(step_index(step))
        .copied()
        .unwrap_or("wizard")
}

/// Everything an action handler can report back to its caller.
pub(super) type Applied = Result<ActionOutput, String>;

impl InstallerApp {
    /// `Some` when the wizard is not on `step`: the control for this action
    /// is not on screen, so the action is not available either.
    fn off_step(&self, step: Step) -> Blocked {
        (self.step != step).then(|| {
            (
                "other_step",
                format!(
                    "This belongs to the {} step; the installer is on {}. Use next or back to get there.",
                    step_name(step),
                    step_name(self.step)
                ),
            )
        })
    }

    fn require_step(&self, step: Step) -> Result<(), String> {
        self.off_step(step)
            .map_or(Ok(()), |(_, reason)| Err(reason))
    }

    pub(super) fn tpm_pin_policy(&self) -> bool {
        matches!(
            self.verified_boot_state(),
            VerifiedBootState::TargetVerified
        )
    }

    /// The resource tree the installer has open right now. Recomputed on
    /// every call; an address that stopped naming anything is refused by
    /// the door instead of reaching a stale handler.
    pub fn publish(&self) -> Vec<Resource> {
        let mut root = Resource::new(SESSION, "installer.session", "Tilo installer")
            .with_presentation(ResourcePresentation {
                detail: Some(format!(
                    "Step {} of {}: {}",
                    step_index(self.step).saturating_add(1),
                    STEP_LABELS.len(),
                    step_name(self.step)
                )),
                badge: Some(self.install_state_name().to_owned()),
                preview: None,
            });
        root = root
            .with_action(
                ActionSpec::typed_operation(
                    "status",
                    "Installer status",
                    "",
                    OperationKind::View,
                    OPERATION_TYPE,
                    Vec::new(),
                    |_| encoded(&Operation::Status),
                )
                .with_description(
                    "The current step, every choice made so far, progress and what blocks the next step. Never includes a passphrase or the recovery key.",
                )
                .returns(ResultSpec::Json {
                    schema: json!({ "type": "object" }),
                }),
            )
            .with_action(self.next_spec())
            .with_action(self.back_spec());
        root = root
            .with_child(self.keyboard_resource())
            .with_child(self.network_resource())
            .with_child(self.storage_resource())
            .with_child(self.account_resource())
            .with_child(self.options_resource())
            .with_child(self.review_resource());
        if let Some(recovery) = self.recovery_resource() {
            root = root.with_child(recovery);
        }
        if let Some(power) = self.power_resource() {
            root = root.with_child(power);
        }
        vec![root]
    }

    pub(super) fn install_state_name(&self) -> &'static str {
        match self.install_state {
            InstallState::Idle => "idle",
            InstallState::Running => "running",
            InstallState::Done => "done",
            InstallState::Failed => "failed",
        }
    }

    fn next_spec(&self) -> ActionSpec {
        let label = match self.step {
            Step::Network
                if self.network.snapshot.connectivity == os_actions::ConnectivityState::Full =>
            {
                "Continue"
            }
            Step::Network => "Continue offline",
            _ => "Next",
        };
        let blocked = match self.step {
            Step::Review => Some((
                "begin_required",
                "Review ends with Begin installation (begin@installer.review), not Next".to_owned(),
            )),
            Step::Install => Some((
                "install_started",
                "The install has started; there is no next step".to_owned(),
            )),
            _ => None,
        };
        gate(nullary("next", label, &Operation::Next), blocked)
            .with_description("Validate this step and move to the next one, exactly as the wizard's Next button does.")
    }

    fn back_spec(&self) -> ActionSpec {
        let failed = self.step == Step::Install && self.install_state == InstallState::Failed;
        let label = if failed { "Review storage" } else { "Back" };
        let blocked = if failed || prev_step(self.step).is_some() {
            None
        } else if self.step == Step::Install {
            Some((
                "install_started",
                "The install has started; the wizard cannot go back".to_owned(),
            ))
        } else {
            Some(("first_step", "Keyboard is the first step".to_owned()))
        };
        gate(nullary("back", label, &Operation::Back), blocked)
    }

    fn keyboard_resource(&self) -> Resource {
        Resource::new(KEYBOARD, "installer.keyboard", "Keyboard layout")
            .with_presentation(ResourcePresentation {
                detail: Some(self.keymap.trim().to_owned()),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                typed(
                    "set-layout",
                    "Choose keyboard layout",
                    vec![
                        ActionParam::required("layout", ParamType::Text, "Keyboard layout")
                            .with_choices(layouts::display_names()),
                    ],
                    |args| Operation::SetKeyboardLayout {
                        layout: args.text("layout").unwrap_or_default().to_owned(),
                    },
                ),
                self.off_step(Step::Keyboard),
            ))
    }

    fn network_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Network);
        let busy = self.network.is_busy().then(|| {
            (
                "network_busy",
                "A network operation is already in progress".to_owned(),
            )
        });
        let visible: Vec<String> = self
            .network
            .snapshot
            .networks
            .iter()
            .take(5)
            .map(|network| network.ssid.clone())
            .collect();
        let select_blocked = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            visible.is_empty().then(|| {
                (
                    "no_networks",
                    self.network.snapshot.wifi_error.clone().unwrap_or_else(|| {
                        "No Wi-Fi networks are visible; refresh the list".into()
                    }),
                )
            })
        });
        let connect_blocked = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            match self.network.selected_network() {
                None => Some(("no_selection", "Choose a Wi-Fi network first".to_owned())),
                Some(network) if network.connected => Some((
                    "already_connected",
                    "That Wi-Fi network is already connected".to_owned(),
                )),
                Some(_) => None,
            }
        });
        let mut select_param = ActionParam::required("ssid", ParamType::Text, "Network name");
        if !visible.is_empty() {
            select_param = select_param.with_choices(visible);
        }
        Resource::new(NETWORK, "installer.network", "Network")
            .with_presentation(ResourcePresentation {
                detail: Some(bounded(&self.network.connection_summary(), 160)),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                nullary("refresh", "Refresh networks", &Operation::RefreshNetworks),
                on_step.or(busy),
            ))
            .with_action(gate(
                typed(
                    "select",
                    "Choose Wi-Fi network",
                    vec![select_param],
                    |args| Operation::SelectNetwork {
                        ssid: args.text("ssid").unwrap_or_default().to_owned(),
                    },
                ),
                select_blocked,
            ))
            .with_action(gate(
                typed(
                    "connect",
                    "Connect",
                    vec![ActionParam::optional(
                        "password",
                        ParamType::Secret,
                        "Wi-Fi password (secured networks only)",
                    )],
                    |args| Operation::ConnectNetwork {
                        password: args.text("password").map(str::to_owned),
                    },
                ),
                connect_blocked,
            ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the method, the disks and the free regions are one Disk-step resource"
    )]
    fn storage_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Disk);
        let mut storage = Resource::new(STORAGE, "installer.storage", "Installation target")
            .with_presentation(ResourcePresentation {
                detail: Some(
                    if self.install_method == InstallMethod::Alongside {
                        "Install alongside existing systems"
                    } else {
                        "Erase the selected disk"
                    }
                    .to_owned(),
                ),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                typed(
                    "set-method",
                    "Choose install method",
                    vec![
                        ActionParam::required(
                            "method",
                            ParamType::Text,
                            "erase: use the whole disk; alongside: keep existing partitions",
                        )
                        .with_choices(["erase", "alongside"]),
                    ],
                    |args| Operation::SetInstallMethod {
                        method: args.text("method").unwrap_or_default().to_owned(),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(gate(
                nullary("refresh", "Refresh disks", &Operation::RefreshDisks),
                on_step.clone(),
            ));
        for (index, disk) in self.disks.iter().enumerate() {
            let blocked = on_step.clone().or_else(|| {
                if disk.is_system {
                    Some((
                        "installer_disk",
                        "The running installer disk cannot be used as its own destination"
                            .to_owned(),
                    ))
                } else if disk.read_only {
                    Some((
                        "read_only",
                        "This disk is hardware or policy write-protected.".to_owned(),
                    ))
                } else {
                    None
                }
            });
            let badge = if self.selected_disk == Some(index) {
                "selected"
            } else if disk.is_system {
                "installer medium"
            } else if disk.read_only {
                "read-only"
            } else {
                "available"
            };
            let device = disk.name.clone();
            storage = storage.with_child(
                Resource::new(disk_resource(&disk.name), "installer.disk", disk.label())
                    .with_presentation(ResourcePresentation {
                        detail: Some(bounded(
                            &format!("{} · {}", disk.size, disk.contents_summary()),
                            160,
                        )),
                        badge: Some(badge.to_owned()),
                        preview: None,
                    })
                    .with_action(gate(
                        typed("select", "Use this disk", Vec::new(), move |_| {
                            Operation::SelectDisk {
                                device: device.clone(),
                            }
                        }),
                        blocked,
                    )),
            );
        }
        let plans = self.guided_alongside_plans();
        if plans.len() > 1 {
            for (index, plan) in plans.iter().enumerate() {
                let gib = 1024_u64 * 1024 * 1024;
                storage = storage.with_child(
                    Resource::new(
                        region_resource(index),
                        "installer.region",
                        format!(
                            "{} GiB free at {} GiB",
                            plan.free_size_bytes / gib,
                            plan.free_start_bytes / gib
                        ),
                    )
                    .with_presentation(ResourcePresentation {
                        badge: (self.selected_guided_extent == Some(index))
                            .then(|| "selected".to_owned()),
                        ..ResourcePresentation::default()
                    })
                    .with_action(gate(
                        typed("select", "Use this region", Vec::new(), move |_| {
                            Operation::SelectFreeRegion { index }
                        }),
                        on_step.clone(),
                    )),
                );
            }
        }
        storage
    }

    fn account_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Account);
        let secret = if self.tpm_pin_policy() {
            "TPM-protected startup PIN: at least 6 digits, not a trivial sequence"
        } else {
            "Startup and unlock passphrase: at least 15 characters, ideally several unrelated words"
        };
        Resource::new(ACCOUNT, "installer.account", "Account")
            .with_presentation(ResourcePresentation {
                detail: (!self.username.trim().is_empty()).then(|| bounded(self.username.trim(), 160)),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                typed(
                    "set-identity",
                    "Set username and name",
                    vec![
                        ActionParam::optional(
                            "username",
                            ParamType::Text,
                            "Up to 32 lowercase letters, digits, _ or -; starts with a letter or _",
                        ),
                        ActionParam::optional("full_name", ParamType::Text, "Full name (optional)"),
                    ],
                    |args| Operation::SetIdentity {
                        username: args.text("username").map(str::to_owned),
                        full_name: args.text("full_name").map(str::to_owned),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(
                gate(
                    typed(
                        "set-passphrase",
                        "Set startup secret",
                        vec![
                            ActionParam::required_owned("passphrase", ParamType::Secret, secret),
                            ActionParam::required(
                                "confirmation",
                                ParamType::Secret,
                                "The same secret again",
                            ),
                        ],
                        |args| Operation::SetPassphrase {
                            passphrase: args.text("passphrase").unwrap_or_default().to_owned(),
                            confirmation: args.text("confirmation").unwrap_or_default().to_owned(),
                        },
                    ),
                    on_step,
                )
                .with_description(
                    "Decrypts the disk at startup and unlocks the account. Held only in installer memory; never written to state, logs or replies.",
                ),
            )
    }

    fn options_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Options);
        Resource::new(OPTIONS, "installer.options", "Installation options")
            .with_presentation(ResourcePresentation {
                detail: Some(bounded(
                    &format!(
                        "{} · {} · {} · {}",
                        self.filesystem(),
                        self.hostname.trim(),
                        self.locale.trim(),
                        self.timezone.trim()
                    ),
                    160,
                )),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                typed(
                    "set-filesystem",
                    "Choose filesystem",
                    vec![
                        ActionParam::required("filesystem", ParamType::Text, "Root filesystem")
                            .with_choices(FILESYSTEMS),
                    ],
                    |args| Operation::SetFilesystem {
                        filesystem: args.text("filesystem").unwrap_or_default().to_owned(),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(gate(
                typed(
                    "set-security-key",
                    "Enroll a security key",
                    vec![ActionParam::required(
                        "enabled",
                        ParamType::Boolean,
                        "Enroll a FIDO2 security key as another unlock factor",
                    )],
                    |args| Operation::SetSecurityKey {
                        enabled: args.boolean("enabled").unwrap_or(false),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(gate(
                typed(
                    "set-hostname",
                    "Set hostname",
                    vec![ActionParam::required(
                        "hostname",
                        ParamType::Text,
                        "Letters, digits and hyphens; no leading or trailing hyphen; at most 63",
                    )],
                    |args| Operation::SetHostname {
                        hostname: args.text("hostname").unwrap_or_default().to_owned(),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(gate(
                typed(
                    "set-locale",
                    "Choose language and locale",
                    vec![
                        ActionParam::required("locale", ParamType::Text, "Language and locale")
                            .with_choices(crate::locales::list()),
                    ],
                    |args| Operation::SetLocale {
                        locale: args.text("locale").unwrap_or_default().to_owned(),
                    },
                ),
                on_step.clone(),
            ))
            .with_action(gate(
                typed(
                    "set-timezone",
                    "Choose time zone",
                    vec![
                        ActionParam::required("timezone", ParamType::Text, "IANA time zone")
                            .with_choices(self.timezones.clone()),
                    ],
                    |args| Operation::SetTimezone {
                        timezone: args.text("timezone").unwrap_or_default().to_owned(),
                    },
                ),
                on_step,
            ))
    }

    /// What stands between the Review step and a launch, in the order the
    /// wizard would say it.
    fn begin_blocked(&self) -> Blocked {
        self.off_step(Step::Review)
            .or_else(|| {
                self.destructive_confirmation_target().is_none().then(|| {
                    (
                        "no_target",
                        "Select a disk (and, alongside, a region) before installing".to_owned(),
                    )
                })
            })
            .or_else(|| {
                (!self.recovery_ack).then(|| {
                    (
                        "recovery_key_promise",
                        "Confirm you'll save your recovery key first".to_owned(),
                    )
                })
            })
            .or_else(|| {
                validate_startup_secret_for(&secret_copy(&self.password), self.tpm_pin_policy())
                    .err()
                    .map(|reason| ("startup_secret", reason.to_owned()))
            })
    }

    fn review_resource(&self) -> Resource {
        let target = self.destructive_confirmation_target();
        let erase = self.install_method == InstallMethod::Erase;
        let description = match (&target, erase) {
            (Some(device), true) => format!(
                "ERASES {device} and all data on it, then installs Tilo. Destructive: type the device name as the confirmation phrase."
            ),
            (Some(device), false) => format!(
                "Installs Tilo alongside existing systems, formatting only {device}'s reviewed Tilo space. Destructive: type the confirmation phrase."
            ),
            (None, _) => "Select a destination first".to_owned(),
        };
        let phrase = target
            .clone()
            .unwrap_or_else(|| "select a disk first".to_owned());
        let begin_target = target.unwrap_or_default();
        let begin = gate(
            typed("begin", "Begin installation", Vec::new(), move |_| {
                Operation::BeginInstall {
                    target: begin_target.clone(),
                }
            }),
            self.begin_blocked(),
        )
        .with_description(description)
        .with_contract(ActionContract {
            availability: self
                .begin_blocked()
                .map_or(Availability::Available, |(code, reason)| {
                    Availability::Unavailable {
                        code: code.to_owned(),
                        reason,
                    }
                }),
            risk: RiskLevel::Destructive,
            confirmation: ConfirmationPolicy::Phrase { phrase },
            ..ActionContract::default()
        });
        Resource::new(REVIEW, "installer.review", "Review and install")
            .with_action(gate(
                typed(
                    "set-recovery-promise",
                    "I will save my recovery key",
                    vec![ActionParam::required(
                        "acknowledged",
                        ParamType::Boolean,
                        "A recovery key is shown once during install; it is the only backup if the TPM or passphrase is lost",
                    )],
                    |args| Operation::SetRecoveryPromise {
                        acknowledged: args.boolean("acknowledged").unwrap_or(false),
                    },
                ),
                self.off_step(Step::Review),
            ))
            .with_action(begin)
    }

    fn recovery_resource(&self) -> Option<Resource> {
        self.recovery_key.as_ref()?;
        Some(
            Resource::new(RECOVERY, "installer.recovery", "Recovery key")
                .with_presentation(ResourcePresentation {
                    detail: Some(
                        if self.recovery_key_saved {
                            "saved off this computer"
                        } else {
                            "shown on screen; not yet confirmed saved"
                        }
                        .to_owned(),
                    ),
                    ..ResourcePresentation::default()
                })
                .with_action(
                    nullary("copy", "Copy recovery key", &Operation::CopyRecoveryKey)
                        .with_description(
                            "Copy the key to this live session's clipboard. The key itself is never returned to the caller.",
                        ),
                )
                .with_action(typed(
                    "confirm-saved",
                    "I saved this key off this computer",
                    vec![ActionParam::required(
                        "saved",
                        ParamType::Boolean,
                        "The recovery key is stored somewhere other than this computer",
                    )],
                    |args| Operation::ConfirmRecoverySaved {
                        saved: args.boolean("saved").unwrap_or(false),
                    },
                )),
        )
    }

    fn power_resource(&self) -> Option<Resource> {
        if self.step != Step::Install || self.install_state != InstallState::Done {
            return None;
        }
        let unsaved = |verb: &str| {
            (!self.recovery_key_saved).then(|| {
                (
                    "recovery_key_unsaved",
                    format!("Save the recovery key off this computer and confirm it before {verb}"),
                )
            })
        };
        Some(
            Resource::new(POWER, "installer.power", "Finish")
                .with_action(gate(
                    nullary("reboot", "Reboot now", &Operation::Reboot),
                    unsaved("rebooting"),
                ))
                .with_action(gate(
                    nullary("power-off", "Power off", &Operation::PowerOff),
                    unsaved("powering off"),
                )),
        )
    }

    /// Resolve a wizard control's action against the tree published now and
    /// run it through the same door the console uses. A control whose action
    /// is unavailable shows the published reason, which is exactly the
    /// sentence an external caller receives.
    pub(super) fn invoke_control(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        resource: &str,
        action: &str,
        mut args: Map<String, Value>,
        phrase: Option<&str>,
    ) {
        let resources = self.publish();
        let address = tilo_actions::ActionAddress::new(resource, action);
        let Some(spec) = tilo_actions::door::resolve(&resources, &address) else {
            return;
        };
        if let Availability::Unavailable { reason, .. } = &spec.contract.availability {
            let reason = reason.clone();
            self.set_status(&reason);
            return;
        }
        if let ConfirmationPolicy::Phrase { phrase: expected } = &spec.contract.confirmation {
            if phrase.map(str::trim) != Some(expected.as_str()) {
                let message = format!("Type {expected} exactly to confirm formatting it");
                self.set_status(&message);
                return;
            }
            args.insert(
                console_control::catalog::PHRASE_ARG.into(),
                Value::String(expected.clone()),
            );
        }
        let id = console_control::catalog::id_for(&address);
        match console_control::admit(
            crate::operation::PROVIDER,
            &resources,
            crate::operation::PROVIDER,
            &id,
            &args,
        ) {
            // Handlers set the status or notice the wizard shows; the
            // returned error only matters to a caller that cannot see it.
            Ok(admitted) => {
                let _ = self.apply_encoded(&admitted.encoded, state, root);
            }
            Err(refusal) => self.set_status(&refusal.message),
        }
    }

    /// Decode and apply one encoded installer operation.
    pub(super) fn apply_encoded(
        &mut self,
        encoded: &EncodedAction,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Applied {
        if encoded.type_id != OPERATION_TYPE {
            return Err(format!("not an installer operation: {}", encoded.type_id));
        }
        let operation = Operation::decode(&encoded.payload)?;
        self.apply(operation, state, root)
    }

    /// Apply one operation. Every door ends here, after resolution,
    /// availability, confirmation and binding; the handlers still re-check
    /// the wizard's own preconditions, so a caller that reached this
    /// function some other way cannot skip a step.
    ///
    /// # Errors
    ///
    /// Returns the sentence the wizard would show when the operation cannot
    /// do its job now.
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per operation keeps each handler next to its siblings"
    )]
    pub fn apply(&mut self, operation: Operation, state: &mut AutoCommit, root: &ObjId) -> Applied {
        let done = || Ok(ActionOutput::default());
        match operation {
            Operation::Status => Ok(ActionOutput {
                value: Some(self.console_state()),
                resources: Vec::new(),
            }),
            Operation::Next => {
                if matches!(self.step, Step::Review | Step::Install) {
                    return Err("Next is not available on this step".into());
                }
                let before = self.step;
                self.go_next();
                if self.step != before || self.storage_preflight_request.is_some() {
                    done()
                } else {
                    Err(self.status_or("This step is not complete yet"))
                }
            }
            Operation::Back => {
                let before = (self.step, self.install_state);
                self.go_back();
                if (self.step, self.install_state) == before {
                    Err("There is no step to go back to".into())
                } else {
                    done()
                }
            }
            Operation::SetKeyboardLayout { layout } => {
                self.require_step(Step::Keyboard)?;
                if layouts::by_display_name(layout.trim()).is_none() {
                    return Err(format!("Unknown keyboard layout {layout:?}"));
                }
                ui::put_text_diffed(state, root, KEYMAP_PATH, layout.trim());
                layout.trim().clone_into(&mut self.keymap);
                self.dirty_structure = true;
                done()
            }
            Operation::RefreshNetworks => {
                self.require_step(Step::Network)?;
                let Some(proxy) = self.event_proxy() else {
                    self.network.notice = "Network controls are still starting; try again.".into();
                    self.dirty_structure = true;
                    return Err(self.network.notice.clone());
                };
                self.network.start_scan(proxy);
                self.dirty_structure = true;
                done()
            }
            Operation::SelectNetwork { ssid } => {
                self.require_step(Step::Network)?;
                if !self
                    .network
                    .snapshot
                    .networks
                    .iter()
                    .any(|n| n.ssid == ssid)
                {
                    return Err("That Wi-Fi network is no longer visible; refresh the list".into());
                }
                if self.network.selected_ssid.as_deref() != Some(ssid.as_str()) {
                    self.network.selected_ssid = Some(ssid);
                    clear_secret_buffer(&self.wifi_password);
                }
                self.network.notice = self.network.snapshot.guidance().to_string();
                self.dirty_structure = true;
                done()
            }
            Operation::ConnectNetwork { password } => {
                self.require_step(Step::Network)?;
                let password = Zeroizing::new(password.unwrap_or_default());
                let Some(proxy) = self.event_proxy() else {
                    self.network.notice = "Network controls are still starting; try again.".into();
                    self.dirty_structure = true;
                    return Err(self.network.notice.clone());
                };
                let result = self.network.start_connect(password, proxy);
                self.dirty_structure = true;
                result.map_err(|message| {
                    self.network.notice = message.to_string();
                    message.to_string()
                })?;
                done()
            }
            Operation::SetInstallMethod { method } => {
                self.require_step(Step::Disk)?;
                let method = match method.as_str() {
                    "erase" => InstallMethod::Erase,
                    "alongside" => InstallMethod::Alongside,
                    other => return Err(format!("Unknown install method {other:?}")),
                };
                if self.install_method != method {
                    Self::clear_text(state, root, ERASE_CONFIRM_PATH);
                    self.erase_confirmation.clear();
                    self.selected_guided_extent = None;
                    self.storage_preflight_request = None;
                    self.storage_preflight_passed = false;
                }
                self.install_method = method;
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::RefreshDisks => {
                self.require_step(Step::Disk)?;
                Self::clear_text(state, root, ERASE_CONFIRM_PATH);
                let disks = self.backend.read_disks();
                self.refresh_disks_with(disks);
                self.ensure_disk_paths(state, root);
                done()
            }
            Operation::SelectDisk { device } => {
                self.require_step(Step::Disk)?;
                let index = self
                    .disks
                    .iter()
                    .position(|disk| disk.name == device)
                    .ok_or_else(|| format!("No disk {device} is listed; refresh disks"))?;
                let usable = self
                    .disks
                    .get(index)
                    .is_some_and(|disk| !disk.is_system && !disk.read_only);
                if !usable {
                    return Err(format!("{device} cannot be an installation target"));
                }
                if self.selected_disk != Some(index) {
                    Self::clear_text(state, root, ERASE_CONFIRM_PATH);
                    self.erase_confirmation.clear();
                    self.selected_guided_extent = None;
                    self.storage_preflight_request = None;
                    self.storage_preflight_passed = false;
                }
                self.selected_disk = Some(index);
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::SelectFreeRegion { index } => {
                self.require_step(Step::Disk)?;
                let count = self.guided_alongside_plans().len();
                if count < 2 || index >= count {
                    return Err("That unallocated region is not offered; refresh disks".into());
                }
                if self.selected_guided_extent != Some(index) {
                    Self::clear_text(state, root, ERASE_CONFIRM_PATH);
                    self.erase_confirmation.clear();
                    self.selected_guided_extent = Some(index);
                    self.status.clear();
                    self.dirty_structure = true;
                }
                done()
            }
            Operation::SetIdentity {
                username,
                full_name,
            } => {
                self.require_step(Step::Account)?;
                if let Some(username) = &username
                    && !valid_username(username)
                {
                    return Err(
                        "Use up to 32 lowercase letters, digits, _ or -; start with a letter or _"
                            .into(),
                    );
                }
                if let Some(full_name) = &full_name
                    && !valid_full_name(full_name.trim())
                {
                    return Err(
                        "Use at most 100 characters for the name, without colons or control characters"
                            .into(),
                    );
                }
                if let Some(username) = username {
                    ui::put_text_diffed(state, root, USERNAME_PATH, &username);
                    self.username = username;
                }
                if let Some(full_name) = full_name {
                    ui::put_text_diffed(state, root, FULL_NAME_PATH, &full_name);
                    self.full_name = full_name;
                }
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::SetPassphrase {
                passphrase,
                confirmation,
            } => {
                self.require_step(Step::Account)?;
                let passphrase = Zeroizing::new(passphrase);
                let confirmation = Zeroizing::new(confirmation);
                for (buffer, value) in [
                    (&self.password, &passphrase),
                    (&self.password_confirmation, &confirmation),
                ] {
                    clear_secret_buffer(buffer);
                    if let Ok(mut held) = buffer.lock() {
                        held.push_str(value);
                    }
                }
                self.dirty_structure = true;
                validate_passwords(&passphrase, &confirmation)?;
                validate_startup_secret_for(&passphrase, self.tpm_pin_policy())
                    .map_err(str::to_owned)?;
                done()
            }
            Operation::SetFilesystem { filesystem } => {
                self.require_step(Step::Options)?;
                self.selected_fs = FILESYSTEMS
                    .iter()
                    .position(|candidate| *candidate == filesystem)
                    .ok_or_else(|| format!("Unknown filesystem {filesystem:?}"))?;
                self.dirty_structure = true;
                done()
            }
            Operation::SetSecurityKey { enabled } => {
                self.require_step(Step::Options)?;
                self.fido2 = enabled;
                self.dirty_structure = true;
                done()
            }
            Operation::SetHostname { hostname } => {
                self.require_step(Step::Options)?;
                if !valid_hostname(&hostname) {
                    return Err("Enter a valid hostname (letters, digits, hyphens)".into());
                }
                ui::put_text_diffed(state, root, HOSTNAME_PATH, hostname.trim());
                hostname.trim().clone_into(&mut self.hostname);
                self.dirty_structure = true;
                done()
            }
            Operation::SetLocale { locale } => {
                self.require_step(Step::Options)?;
                if !valid_locale(locale.trim()) {
                    return Err("Choose a supported language and locale from the list".into());
                }
                ui::put_text_diffed(state, root, LOCALE_PATH, locale.trim());
                locale.trim().clone_into(&mut self.locale);
                self.dirty_structure = true;
                done()
            }
            Operation::SetTimezone { timezone } => {
                self.require_step(Step::Options)?;
                let timezone = timezone.trim().to_owned();
                if !valid_timezone_name(&timezone) || !self.timezones.contains(&timezone) {
                    return Err("Choose a valid time zone from the list".into());
                }
                ui::put_text_diffed(state, root, TIMEZONE_PATH, &timezone);
                self.timezone = timezone;
                self.dirty_structure = true;
                done()
            }
            Operation::SetRecoveryPromise { acknowledged } => {
                self.require_step(Step::Review)?;
                self.recovery_ack = acknowledged;
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::BeginInstall { target } => {
                self.require_step(Step::Review)?;
                if self.destructive_confirmation_target().as_deref() != Some(target.as_str()) {
                    return Err(
                        "The reviewed installation target changed; review it again before installing"
                            .into(),
                    );
                }
                // The door verified the typed phrase. Reflect it into the
                // Review field so the screen and the launch agree.
                ui::put_text_diffed(state, root, ERASE_CONFIRM_PATH, &target);
                self.erase_confirmation = target;
                self.begin_install(state, root);
                if self.install_launched {
                    done()
                } else {
                    Err(self.status_or("The install did not start"))
                }
            }
            Operation::CopyRecoveryKey => {
                let Some(key) = &self.recovery_key else {
                    return Err("No recovery key has been generated".into());
                };
                ui::clipboard_copy(key);
                self.recovery_key_copied = true;
                self.dirty_structure = true;
                done()
            }
            Operation::ConfirmRecoverySaved { saved } => {
                if self.recovery_key.is_none() {
                    return Err("No recovery key has been generated".into());
                }
                self.recovery_key_saved = saved;
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::Reboot | Operation::PowerOff => {
                let (action, verb) = if matches!(operation, Operation::Reboot) {
                    (PowerAction::Reboot, "rebooting")
                } else {
                    (PowerAction::PowerOff, "powering off")
                };
                if self.step != Step::Install || self.install_state != InstallState::Done {
                    return Err("Power actions are offered once the install has finished".into());
                }
                if !self.recovery_key_saved {
                    let message = format!(
                        "Save the recovery key off this computer and confirm it before {verb}"
                    );
                    self.set_status(&message);
                    return Err(message);
                }
                if self.run_power(action) {
                    done()
                } else {
                    Err(self.status_or("Could not run the power action"))
                }
            }
        }
    }

    fn status_or(&self, fallback: &str) -> String {
        if self.status.trim().is_empty() {
            fallback.to_owned()
        } else {
            self.status.clone()
        }
    }
}
