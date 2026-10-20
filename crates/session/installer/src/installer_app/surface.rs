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
    ALONGSIDE_BOOT_GUIDANCE, ALONGSIDE_FILES_GUIDANCE, CODECS_DESCRIPTION, FILESYSTEMS,
    InstallMethod, InstallState, InstallerApp, STEP_LABELS, Step, VerifiedBootState,
    alongside_install_impact, clear_secret_buffer, local_text, prev_step, secret_copy, step_index,
    valid_full_name, valid_hostname, valid_locale, valid_timezone_name, valid_username,
    validate_passwords, validate_startup_secret_for,
};
use crate::install::PowerAction;
use crate::layouts;
use crate::operation::{
    ACCOUNT, KEYBOARD, MEDIA, NETWORK, OPERATION_TYPE, OPTIONS, Operation, POWER, PROVIDER,
    RECOVERY, REVIEW, SESSION, STORAGE, disk_resource, region_resource,
};

/// How long a copied recovery key stays on the clipboard: time to paste it
/// into a password manager. Rebooting or powering off clears it sooner.
const RECOVERY_KEY_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(300);

/// A reason an action cannot run now: a stable code and the sentence the
/// wizard would show.
type Blocked = Option<(&'static str, String)>;

// Only fixed controls cross the action door. Identity fields and requests stay
// in AccountsForm's process-local queue, never in action parameters or history.
const ONLINE_ACCOUNTS: &str = "installer.accounts";
const ONLINE_OPERATION_TYPE: &str = crate::operation::ONLINE_ACCOUNT_OPERATION_TYPE;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum OnlineControl {
    Add,
    Retry,
    Cancel,
    SkipOffline,
}

/// Keep guidance within publication limits without silently dropping it.
fn with_guidance(mut resource: Resource, id: &str, guidance: &str) -> Resource {
    for (index, sentence) in guidance.split(". ").enumerate() {
        resource = resource.with_child(
            Resource::new(
                format!("{id}.guidance:{index}"),
                "installer.guidance",
                "Installation guidance",
            )
            .with_presentation(ResourcePresentation {
                preview: Some(sentence.to_owned()),
                ..ResourcePresentation::default()
            }),
        );
    }
    resource
}

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

/// An operation that switches one option on or off through its `enabled` flag.
fn toggle(
    id: &str,
    label: &str,
    description: &'static str,
    build: impl Fn(bool) -> Operation + Send + Sync + 'static,
) -> ActionSpec {
    typed(
        id,
        label,
        vec![ActionParam::required(
            "enabled",
            ParamType::Boolean,
            description,
        )],
        move |args| build(args.boolean("enabled").unwrap_or(false)),
    )
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

    pub(crate) fn tpm_pin_policy(&self) -> bool {
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
        if let Some(spec) = self.return_to_host_spec() {
            root = root.with_action(spec);
        }
        root = root
            .with_child(self.keyboard_resource())
            .with_child(self.network_resource())
            .with_child(self.online_accounts_resource())
            .with_child(self.storage_resource())
            .with_child(self.account_resource())
            .with_child(self.options_resource())
            .with_child(self.media_resource())
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

    fn online_accounts_resource(&self) -> Resource {
        use crate::online_accounts::view::AccountsStatus;
        let status = self.accounts_controller.as_ref().map_or(
            if self.accounts_initialized {
                AccountsStatus::Failed
            } else {
                AccountsStatus::Ready
            },
            crate::online_accounts::AccountsController::status,
        );
        let badge = match status {
            AccountsStatus::Ready => "ready",
            AccountsStatus::SigningIn => "signing-in",
            AccountsStatus::ReviewCalendars => "review-calendars",
            AccountsStatus::Failed => "failed",
            AccountsStatus::Added => "added",
        };
        let mut resource = Resource::new(
            ONLINE_ACCOUNTS,
            "installer.online-accounts",
            "Online accounts",
        )
        .with_presentation(ResourcePresentation {
            badge: Some(badge.into()),
            ..ResourcePresentation::default()
        });
        for (id, label, control) in [
            (
                "add",
                if status == AccountsStatus::ReviewCalendars {
                    "Confirm selected calendars"
                } else {
                    "Sign in using private form"
                },
                OnlineControl::Add,
            ),
            ("retry", "Retry using private form", OnlineControl::Retry),
            ("cancel", "Cancel sign-in", OnlineControl::Cancel),
            (
                "skip-offline",
                "Continue offline",
                OnlineControl::SkipOffline,
            ),
        ] {
            let blocked = self.off_step(Step::Accounts).or_else(|| match control {
                OnlineControl::Add | OnlineControl::Retry
                    if status == AccountsStatus::SigningIn =>
                {
                    Some((
                        "signing_in",
                        "Wait for sign-in to finish or cancel it".into(),
                    ))
                }
                OnlineControl::Retry if status != AccountsStatus::Failed => Some((
                    "not_failed",
                    "Retry is available after a failed sign-in".into(),
                )),
                _ => None,
            });
            resource = resource.with_action(gate(
                ActionSpec::typed_operation(
                    id,
                    label,
                    "",
                    OperationKind::Effect,
                    ONLINE_OPERATION_TYPE,
                    Vec::new(),
                    move |_| {
                        EncodedAction::encode_operation(ONLINE_OPERATION_TYPE, &control)
                            .unwrap_or_else(|_| EncodedAction {
                                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                                payload: Vec::new(),
                            })
                    },
                ),
                blocked,
            ));
        }
        resource
    }

    fn apply_online_control(&mut self, payload: &[u8]) -> Applied {
        self.require_step(Step::Accounts)?;
        let control: OnlineControl = serde_json::from_slice(payload)
            .map_err(|_| "Invalid online account control".to_string())?;
        match control {
            OnlineControl::Add | OnlineControl::Retry => self
                .accounts_form
                .submit(matches!(control, OnlineControl::Retry))
                .map_err(|_| {
                    "Check the private account form or wait for sign-in to finish".to_string()
                })?,
            OnlineControl::Cancel => self.accounts_form.cancel(),
            OnlineControl::SkipOffline => self.accounts_form.skip_offline(),
        }
        self.sync_accounts();
        Ok(ActionOutput::default())
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

    /// Staged installs only: the way back to the retained operating system,
    /// offered on every screen except while installing or once done.
    fn return_to_host_spec(&self) -> Option<ActionSpec> {
        let host = self.internal_host()?;
        let blocked = match self.install_state {
            InstallState::Running => Some((
                "install_running",
                "The install is running; wait for it to finish or fail".to_owned(),
            )),
            InstallState::Done => Some((
                "install_done",
                "Tilo is installed; use Reboot now (reboot@installer.power) instead".to_owned(),
            )),
            InstallState::Idle | InstallState::Failed => None,
        };
        Some(
            gate(
                nullary(
                    "return-to-host",
                    &format!("Return to {}", host.display_name()),
                    &Operation::ReturnToHost,
                ),
                blocked,
            )
            .with_description(
                "Leave the installer without changing this disk and restart into the existing system.",
            ),
        )
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
                detail: Some(self.keymap().trim().to_owned()),
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

    pub(super) fn alongside_capacity_binding(&self) -> Option<tilo_actions::ActionBinding<f64>> {
        let resource = self.storage_resource();
        let spec = resource.actions.iter().find(|spec| spec.id == "set-size")?;
        tilo_actions::ActionBinding::fallible(&resource, spec, |value: f64| {
            let gib = value
                .to_string()
                .parse::<i64>()
                .map_err(|_| "Choose a whole number of GiB".to_owned())?;
            Ok(serde_json::Map::from_iter([(
                "gib".into(),
                Value::from(gib),
            )]))
        })
        .ok()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the method, the disks and the free regions are one Disk-step resource"
    )]
    fn storage_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Disk);
        let mut storage = Resource::new(STORAGE, "installer.storage", "Installation target")
            .with_presentation(ResourcePresentation {
                detail: Some(if self.install_method == InstallMethod::Alongside {
                    bounded(&self.alongside_storage_guidance(), 160)
                } else {
                    "Erase the selected disk".to_owned()
                }),
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
        if self.install_method == InstallMethod::Alongside {
            storage = with_guidance(storage, STORAGE, &self.alongside_storage_guidance());
        }
        for (index, disk) in self.disks.iter().enumerate() {
            let blocked = on_step.clone().or_else(|| {
                if disk.blocks_install() {
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
            } else if disk.blocks_install() {
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
                storage = storage.with_child(
                    Resource::new(
                        region_resource(index),
                        "installer.region",
                        format!(
                            "{} free at {}",
                            ui::format::memory(plan.free_size_bytes),
                            ui::format::memory(plan.free_start_bytes)
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
        if let Some(extent) = self.selected_full_guided_plan()
            && let Some(disk) = self
                .selected_disk_ref()
                .filter(|disk| !disk.hosts_staged_installer())
                .cloned()
        {
            storage = storage.with_action(gate(
                typed(
                    "set-size",
                    "Use N GiB for Tilo",
                    vec![
                        ActionParam::required(
                            "gib",
                            ParamType::Integer,
                            "Total Tilo capacity in whole GiB (at least 35)",
                        )
                        .with_schema(serde_json::json!({
                            "type": "integer", "minimum": 35,
                            "maximum": extent.free_size_bytes / (1 << 30)
                        })),
                    ],
                    move |args| Operation::SetAlongsideSize {
                        gib: args.integer("gib").unwrap_or(0),
                        disk: Box::new(disk.clone()),
                        extent: extent.clone(),
                    },
                ),
                on_step,
            ));
        }
        storage
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one resource publishes every account control beside its gate"
    )]
    fn account_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Account);
        let secret = if self.startup_secret_is_pin() {
            "TPM-protected startup PIN: at least 6 digits, not a trivial sequence"
        } else {
            "Startup and unlock passphrase: at least 15 characters, ideally several unrelated words"
        };
        Resource::new(ACCOUNT, "installer.account", "Account")
            .with_presentation(ResourcePresentation {
                detail: self.username.with(|name| {
                    (!name.trim().is_empty()).then(|| bounded(name.trim(), 160))
                }),
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
                        "Set account and default startup secret",
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
                    "Sets the account password and, unless a separate disk secret is supplied, the startup secret. Held only in installer memory; never written to state, logs or replies.",
                ),
            )
            .with_action(
                gate(
                    typed(
                        "set-disk-passphrase",
                        "Set a separate disk startup secret",
                        vec![
                            ActionParam::required_owned("passphrase", ParamType::Secret, secret),
                            ActionParam::required(
                                "confirmation",
                                ParamType::Secret,
                                "The same disk secret again",
                            ),
                            ActionParam::optional(
                                "mode",
                                ParamType::Text,
                                "`tpm2` requires the measured-boot boundary; `passphrase` forces an independent strong passphrase. Omit for the reviewed target's normal policy.",
                            ),
                        ],
                        |args| Operation::SetDiskPassphrase {
                            passphrase: args.text("passphrase").unwrap_or_default().to_owned(),
                            confirmation: args.text("confirmation").unwrap_or_default().to_owned(),
                            mode: args.text("mode").unwrap_or_default().to_owned(),
                        },
                    ),
                    self.off_step(Step::Account),
                )
                .with_description(
                    "Replaces the disk startup secret without changing the account password. Held only in installer memory; never written to state, logs or replies.",
                ),
            )
            .with_action(gate(
                typed(
                    "set-password-hash",
                    "Set a pre-hashed account password",
                    vec![ActionParam::required(
                        "hash",
                        ParamType::Secret,
                        "A canonical SHA-512 crypt hash, recorded by the installed system's normal account creation",
                    )],
                    |args| Operation::SetPasswordHash {
                        hash: args.text("hash").unwrap_or_default().to_owned(),
                    },
                ),
                if self.unattended_mode {
                    self.off_step(Step::Account)
                } else {
                    Some((
                        "interactive_only",
                        "A password hash is accepted only by an explicit unattended installation"
                            .to_owned(),
                    ))
                },
            ))
    }

    fn options_resource(&self) -> Resource {
        let on_step = self.off_step(Step::Options);
        Resource::new(OPTIONS, "installer.options", "Installation options")
            .with_presentation(ResourcePresentation {
                detail: Some(bounded(
                    &format!(
                        "{} · {} · {} · {}",
                        self.filesystem(),
                        local_text(&self.hostname).trim(),
                        self.locale().trim(),
                        self.timezone().trim()
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
                toggle(
                    "set-security-key",
                    "Enroll a security key",
                    "Enroll a FIDO2 security key as another unlock factor",
                    |enabled| Operation::SetSecurityKey { enabled },
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
                            .with_choices(install_profile_model::locales()),
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

    fn media_resource(&self) -> Resource {
        Resource::new(MEDIA, "installer.media", "More video and audio formats")
            .with_presentation(ResourcePresentation {
                detail: Some(
                    if self.codecs {
                        "chosen: installed once the new system is online"
                    } else {
                        "not chosen"
                    }
                    .to_owned(),
                ),
                ..ResourcePresentation::default()
            })
            .with_action(gate(
                toggle(
                    "set-media-codecs",
                    "Install media codecs",
                    CODECS_DESCRIPTION,
                    |enabled| Operation::SetMediaCodecs { enabled },
                )
                .with_description(CODECS_DESCRIPTION),
                self.off_step(Step::Media),
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
                validate_startup_secret_for(&self.startup_secret(), self.startup_secret_is_pin())
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
                "{} {} {} Destructive: type the confirmation phrase.",
                alongside_install_impact(self.uses_guided_alongside(), device),
                crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE),
                crate::i18n::t(ALONGSIDE_FILES_GUIDANCE)
            ),
            (None, _) => "Select a destination first".to_owned(),
        };
        let phrase = target
            .clone()
            .unwrap_or_else(|| "select a disk first".to_owned());
        let begin_target = target.unwrap_or_default();
        let allocation = self.guided_alongside_plan();
        let begin = gate(
            typed("begin", "Begin installation", Vec::new(), move |_| {
                Operation::BeginInstall {
                    target: begin_target.clone(),
                    allocation: allocation.clone(),
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
        let review = Resource::new(REVIEW, "installer.review", "Review and install");
        let review = if erase {
            review
        } else {
            with_guidance(
                review,
                REVIEW,
                &format!(
                    "{} {} {} {}",
                    alongside_install_impact(
                        self.uses_guided_alongside(),
                        &self.selected_dev().unwrap_or_default()
                    ),
                    self.guided_alongside_plan()
                        .as_ref()
                        .map(|plan| self.alongside_allocation_summary(plan))
                        .unwrap_or_default(),
                    crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE),
                    crate::i18n::t(ALONGSIDE_FILES_GUIDANCE)
                ),
            )
        };
        review
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
                let _ = self.apply_encoded(&admitted.encoded);
            }
            Err(refusal) => self.set_status(&refusal.message),
        }
    }

    /// Invoke one published action exactly as the console door does:
    /// resolve it in the current tree, refuse it when unavailable, enforce its
    /// confirmation contract, bind its arguments strictly, then apply it.
    /// The unattended driver and host-side tests use this instead of a second
    /// operation path.
    ///
    /// # Errors
    ///
    /// Returns the door's stable code and sentence when the action is not
    /// published, unavailable, unconfirmed or given arguments it cannot bind.
    pub fn invoke_published(
        &mut self,
        id: &str,
        args: &Map<String, Value>,
    ) -> Result<ActionOutput, String> {
        let resources = self.publish();
        let admitted = console_control::admit(PROVIDER, &resources, PROVIDER, id, args)
            .map_err(|refusal| format!("{}: {}", refusal.code, refusal.message))?;
        self.apply_encoded(&admitted.encoded)
    }

    /// Decode and apply one encoded installer operation.
    pub(super) fn apply_encoded(&mut self, encoded: &EncodedAction) -> Applied {
        if encoded.type_id == ONLINE_OPERATION_TYPE {
            return self.apply_online_control(&encoded.payload);
        }
        if encoded.type_id != OPERATION_TYPE {
            return Err(format!("not an installer operation: {}", encoded.type_id));
        }
        let operation = Operation::decode(&encoded.payload)?;
        self.apply(operation)
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
    pub fn apply(&mut self, operation: Operation) -> Applied {
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
                self.keymap.set(layout.trim().to_owned());
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
                if method == InstallMethod::Erase && self.selected_disk_hosts_installer() {
                    // The backend refuses it too: this disk holds the running
                    // installer and the operating system it was staged from.
                    return Err(
                        "This disk holds the running installer and your existing system; Tilo can only be installed alongside it"
                            .into(),
                    );
                }
                if self.install_method != method {
                    self.clear_erase_confirmation();
                    self.selected_guided_extent = None;
                    self.alongside_capacity = None;
                    self.storage_preflight_request = None;
                    self.storage_preflight_passed = false;
                }
                self.install_method = method;
                if let Some(identity) = self
                    .selected_disk_ref()
                    .and_then(super::DiskMethodIdentity::of)
                {
                    self.disk_method_choices.insert(identity, method);
                }
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            Operation::RefreshDisks => {
                self.require_step(Step::Disk)?;
                let disks = self.backend.read_disks();
                self.refresh_disks_with(disks);
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
                    .is_some_and(|disk| !disk.blocks_install() && !disk.read_only);
                if !usable {
                    return Err(format!("{device} cannot be an installation target"));
                }
                if self.selected_disk != Some(index) {
                    self.clear_erase_confirmation();
                    self.selected_guided_extent = None;
                    self.alongside_capacity = None;
                    self.storage_preflight_request = None;
                    self.storage_preflight_passed = false;
                }
                self.selected_disk = Some(index);
                self.reset_disk_method();
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
                    self.clear_erase_confirmation();
                    self.selected_guided_extent = Some(index);
                    self.alongside_capacity = None;
                    self.storage_preflight_request = None;
                    self.storage_preflight_passed = false;
                    self.status.clear();
                    self.dirty_structure = true;
                }
                done()
            }
            Operation::SetAlongsideSize {
                gib,
                disk: reviewed_disk,
                extent,
            } => {
                self.require_step(Step::Disk)?;
                let disk = self
                    .selected_disk_ref()
                    .filter(|disk| !disk.hosts_staged_installer())
                    .ok_or_else(|| {
                        "Select an existing disk with unallocated space first".to_owned()
                    })?;
                let full = self.selected_full_guided_plan().ok_or_else(|| {
                    "Select an unallocated region first; refresh disks if it changed".to_owned()
                })?;
                if disk != reviewed_disk.as_ref() || full != extent {
                    return Err("The selected disk or free extent changed; review the new target before choosing its capacity".into());
                }
                let bytes = u64::try_from(gib)
                    .ok()
                    .and_then(|n| n.checked_mul(1 << 30))
                    .ok_or_else(|| "Tilo capacity is outside the supported range".to_owned())?;
                full.with_capacity_bytes(bytes).ok_or_else(|| {
                    "Choose at least 35 GiB within the selected unallocated region".to_owned()
                })?;
                self.alongside_capacity = Some((disk.clone(), full, bytes));
                self.clear_erase_confirmation();
                self.storage_preflight_request = None;
                self.storage_preflight_passed = false;
                self.status.clear();
                self.dirty_structure = true;
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
                    self.username.set(username);
                }
                if let Some(full_name) = full_name {
                    self.full_name.set(full_name);
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
                self.password_is_hash = false;
                let passphrase = Zeroizing::new(passphrase);
                let confirmation = Zeroizing::new(confirmation);
                if !secret_copy(&self.disk_password).is_empty()
                    && passphrase.as_str() == secret_copy(&self.disk_password).as_str()
                {
                    return Err("Choose a disk secret different from the account password".into());
                }
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
                if secret_copy(&self.disk_password).is_empty() {
                    validate_startup_secret_for(&passphrase, self.startup_secret_is_pin())
                        .map_err(str::to_owned)?;
                }
                done()
            }
            Operation::SetDiskPassphrase {
                passphrase,
                confirmation,
                mode,
            } => {
                self.require_step(Step::Account)?;
                let passphrase = Zeroizing::new(passphrase);
                let confirmation = Zeroizing::new(confirmation);
                if passphrase.as_str() != confirmation.as_str() {
                    return Err("The disk passphrases do not match".into());
                }
                let (pin, force_passphrase) = match mode.as_str() {
                    "" => (self.tpm_pin_policy(), false),
                    "tpm2" => {
                        if !self.tpm_pin_policy() {
                            return Err(
                                "This destination does not prove the measured-boot TPM2 boundary; choose a passphrase instead"
                                    .into(),
                            );
                        }
                        (true, false)
                    }
                    "passphrase" => (false, true),
                    other => return Err(format!("Unknown startup-secret mode {other:?}")),
                };
                validate_startup_secret_for(&passphrase, pin).map_err(str::to_owned)?;
                if passphrase.as_str() == secret_copy(&self.password).as_str() {
                    return Err("Choose a disk secret different from the account password".into());
                }
                clear_secret_buffer(&self.disk_password);
                if let Ok(mut held) = self.disk_password.lock() {
                    held.push_str(&passphrase);
                }
                self.force_startup_passphrase = force_passphrase;
                self.dirty_structure = true;
                done()
            }
            Operation::SetPasswordHash { hash } => {
                self.require_step(Step::Account)?;
                if !self.unattended_mode {
                    return Err(
                        "A password hash is accepted only by an explicit unattended installation"
                            .into(),
                    );
                }
                let hash = Zeroizing::new(hash);
                if !crate::unattended::valid_sha512_crypt(&hash) {
                    return Err(
                        "The account credential is not a canonical SHA-512 crypt hash".into(),
                    );
                }
                if hash.as_str() == secret_copy(&self.disk_password).as_str() {
                    return Err("Choose a disk secret different from the account password".into());
                }
                clear_secret_buffer(&self.password);
                if let Ok(mut held) = self.password.lock() {
                    held.push_str(&hash);
                }
                clear_secret_buffer(&self.password_confirmation);
                if let Ok(mut held) = self.password_confirmation.lock() {
                    held.push_str(&hash);
                }
                self.password_is_hash = true;
                self.dirty_structure = true;
                done()
            }
            Operation::SetFilesystem { filesystem } => {
                self.require_step(Step::Options)?;
                let index = FILESYSTEMS
                    .iter()
                    .position(|candidate| *candidate == filesystem)
                    .ok_or_else(|| format!("Unknown filesystem {filesystem:?}"))?;
                if self.supported_filesystems.get(index) != Some(&true) {
                    return Err(format!(
                        "{} cannot boot from this image's initramfs",
                        FILESYSTEMS
                            .get(index)
                            .copied()
                            .unwrap_or("unknown filesystem")
                    ));
                }
                self.selected_fs = index;
                self.dirty_structure = true;
                done()
            }
            Operation::SetSecurityKey { enabled } => {
                self.require_step(Step::Options)?;
                self.fido2 = enabled;
                self.dirty_structure = true;
                done()
            }
            Operation::SetMediaCodecs { enabled } => {
                self.require_step(Step::Media)?;
                self.codecs = enabled;
                self.dirty_structure = true;
                done()
            }
            Operation::SetHostname { hostname } => {
                self.require_step(Step::Options)?;
                if !valid_hostname(&hostname) {
                    return Err("Enter a valid hostname (letters, digits, hyphens)".into());
                }
                self.hostname.set(hostname.trim().to_owned());
                self.dirty_structure = true;
                done()
            }
            Operation::SetLocale { locale } => {
                self.require_step(Step::Options)?;
                if !valid_locale(locale.trim()) {
                    return Err("Choose a supported language and locale from the list".into());
                }
                self.locale.set(locale.trim().to_owned());
                self.dirty_structure = true;
                done()
            }
            Operation::SetTimezone { timezone } => {
                self.require_step(Step::Options)?;
                let timezone = timezone.trim().to_owned();
                if !valid_timezone_name(&timezone) || !self.timezones.contains(&timezone) {
                    return Err("Choose a valid time zone from the list".into());
                }
                self.timezone.set(timezone);
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
            Operation::BeginInstall { target, allocation } => {
                if allocation != self.guided_alongside_plan() {
                    return Err(
                        "The reviewed allocation changed; review the new target before installing"
                            .into(),
                    );
                }
                self.require_step(Step::Review)?;
                if self.destructive_confirmation_target().as_deref() != Some(target.as_str()) {
                    return Err(
                        "The reviewed installation target changed; review it again before installing"
                            .into(),
                    );
                }
                // The door verified the typed phrase. Reflect it into the
                // Review field so the screen and the launch agree.
                self.set_erase_confirmation(target);
                self.begin_install();
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
                let copy = ui::try_clipboard_copy_secret(key, RECOVERY_KEY_ON_CLIPBOARD_FOR);
                if !self.recovery_key_copy.watch(copy) {
                    return Err("Could not copy the recovery key to the clipboard".into());
                }
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
            Operation::ReturnToHost => {
                if self.internal_host().is_none() {
                    return Err("This installer was not staged next to an existing system".into());
                }
                if matches!(
                    self.install_state,
                    InstallState::Running | InstallState::Done
                ) {
                    return Err(
                        "Returning is offered before the install starts or after it fails".into(),
                    );
                }
                if self.run_power(PowerAction::Reboot) {
                    done()
                } else {
                    Err(self.status_or("Could not restart"))
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
