//! The installer's published effects — what each one is offered as right
//! now, the items some are bound to, the tree its console enumerates — and
//! [`InstallerApp::apply`], the one function every door ends in.
//!
//! Availability mirrors the wizard: an effect is available exactly when its
//! control is live on screen, and otherwise carries the reason a person
//! would be told. Nothing is hidden to make the surface look smaller; a
//! caller that cannot see the screen learns the whole wizard from one
//! enumeration and why each part is or is not reachable now.

use serde_json::{Map, Value};
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_actions::{ActionOutput, ActionSpec, ResourcePresentation, ResourceSpec};
use ui::tilo_resource::Effect as _;
use zeroize::Zeroizing;

use super::{
    ALONGSIDE_BOOT_GUIDANCE, ALONGSIDE_FILES_GUIDANCE, FILESYSTEMS, InstallMethod, InstallState,
    InstallerApp, STEP_LABELS, Step, VerifiedBootState, alongside_install_impact,
    clear_secret_buffer, prev_step, secret_copy, step_index, valid_full_name, valid_hostname,
    valid_locale, valid_timezone_name, valid_username, validate_passwords,
    validate_startup_secret_for,
};
use crate::setup::destination::disk_row::capacity;
use crate::setup::destination::effects::{
    DestinationEffect, PROVIDER, REVIEW, STORAGE, disk_row, reference, region_row, reviewed_digest,
};
use imager_backend::destination::install::PowerAction;
use imager_backend::destination::layouts;

/// How long a copied recovery key stays on the clipboard: time to paste it
/// into a password manager. Rebooting or powering off clears it sooner.
const RECOVERY_KEY_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(300);

/// A reason an effect cannot run now: a stable code and the sentence the
/// wizard would show.
type Blocked = Option<(&'static str, String)>;

/// What one of the installer's own effects is offered as now: why it cannot
/// run, and the label it carries when that is not its declared one.
struct Gate {
    id: &'static str,
    blocked: Blocked,
    label: Option<String>,
}

impl Gate {
    fn new(id: &'static str, blocked: Blocked) -> Self {
        Self {
            id,
            blocked,
            label: None,
        }
    }

    fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// `spec` as this gate says it is offered now.
    fn overlay(&self, mut spec: ActionSpec) -> ActionSpec {
        if let Some(label) = &self.label {
            spec.type_mut().label.clone_from(label);
        }
        match &self.blocked {
            Some((code, reason)) => spec.unavailable(*code, reason.clone()),
            None => spec,
        }
    }
}

/// One item an effect is bound to: a disk, a free region, the capacity
/// choice, the review, a visible network.
struct Item {
    id: String,
    kind: &'static str,
    label: String,
    presentation: ResourcePresentation,
    operation: Option<ActionSpec>,
}

fn gated(spec: ActionSpec, blocked: Blocked) -> ActionSpec {
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

/// Keep guidance within publication limits without silently dropping it.
fn with_guidance(mut resource: ResourceSpec, id: &str, guidance: &str) -> ResourceSpec {
    for (index, sentence) in guidance.split(". ").enumerate() {
        resource = resource.with_child(
            ResourceSpec::new(
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

/// Everything an effect handler can report back to its caller.
pub(super) type Applied = Result<ActionOutput, String>;

impl InstallerApp {
    /// `Some` when the wizard is not on `step`: the control for this effect
    /// is not on screen, so the effect is not available either.
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

    pub(super) fn install_state_name(&self) -> &'static str {
        match self.install_state {
            InstallState::Idle => "idle",
            InstallState::Running => "running",
            InstallState::Done => "done",
            InstallState::Failed => "failed",
        }
    }

    /// What each of the installer's own (unbound) effects is offered as now.
    fn gates(&self) -> Vec<Gate> {
        let account = self.off_step(Step::Account);
        let options = self.off_step(Step::Options);
        let mut gates = vec![
            Gate::new("status", None),
            self.next_gate(),
            self.back_gate(),
            self.return_to_host_gate(),
            Gate::new("set-layout", self.off_step(Step::Keyboard)),
            Gate::new("set-method", self.off_step(Step::Disk)),
            Gate::new("refresh-disks", self.off_step(Step::Disk)),
            Gate::new("set-identity", account.clone()),
            Gate::new("set-passphrase", account.clone()),
            Gate::new("set-disk-passphrase", account.clone()),
            Gate::new(
                "set-password-hash",
                if self.unattended_mode {
                    account
                } else {
                    Some((
                        "interactive_only",
                        "A password hash is accepted only by an explicit unattended installation"
                            .to_owned(),
                    ))
                },
            ),
            Gate::new("set-filesystem", options.clone()),
            Gate::new("set-security-key", options.clone()),
            Gate::new("set-hostname", options.clone()),
            Gate::new("set-locale", options.clone()),
            Gate::new("set-timezone", options),
            Gate::new("set-media-codecs", self.off_step(Step::Media)),
            Gate::new("set-recovery-promise", self.off_step(Step::Review)),
        ];
        gates.extend(self.network_gates());
        gates.extend(self.online_account_gates());
        gates.extend(self.recovery_gates());
        gates
    }

    /// Why `effect` cannot run now, as its gate or the item it is bound to
    /// says.
    fn blocked(&self, effect: &DestinationEffect) -> Blocked {
        let id = effect.id();
        if let Some(gate) = self.gates().into_iter().find(|gate| gate.id == id) {
            return gate.blocked;
        }
        match effect {
            DestinationEffect::SelectDisk { device } => self
                .disks
                .iter()
                .find(|disk| &disk.name == device)
                .map_or_else(
                    || {
                        Some((
                            "no_disk",
                            format!("No disk {device} is listed; refresh disks"),
                        ))
                    },
                    |disk| self.disk_blocked(disk),
                ),
            DestinationEffect::Begin { .. } => self.begin_blocked(),
            _ => self.off_step(Step::Disk),
        }
    }

    /// What the framework's door offers each effect as now.
    pub(super) fn effect_statuses(&self) -> Vec<EffectStatus> {
        self.gates()
            .into_iter()
            .filter_map(|gate| {
                let label = gate.label;
                let status = match gate.blocked {
                    Some((code, reason)) => {
                        EffectStatus::unavailable(reference(gate.id), code, reason)
                    }
                    None => EffectStatus::labelled(reference(gate.id), label.clone()?),
                };
                Some(match label {
                    Some(label) => status.with_label(label),
                    None => status,
                })
            })
            .collect()
    }

    /// The items effects are bound to, as live rows beneath the installer.
    pub(super) fn published_rows(&self) -> Vec<LiveRow> {
        self.items()
            .into_iter()
            .map(|item| {
                let row =
                    LiveRow::new(item.id, item.kind, item.label).presentation(item.presentation);
                match item.operation {
                    Some(operation) => row.operation(operation),
                    None => row,
                }
            })
            .collect()
    }

    /// The tree the console enumerates and admits against: the installer
    /// with its effects as offered now, and the items some are bound to.
    /// Recomputed on every call; an address that stopped naming anything is
    /// refused instead of reaching a stale handler.
    pub fn published(&self) -> Vec<ResourceSpec> {
        let gates = self.gates();
        let mut root = ResourceSpec::new(PROVIDER, "installer.session", "Tilo installer")
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
        for spec in DestinationEffect::specs() {
            let spec = match gates.iter().find(|gate| gate.id == spec.id) {
                Some(gate) => gate.overlay(spec),
                None => spec,
            };
            root = root.with_action(spec);
        }
        for item in self.items() {
            let mut child = ResourceSpec::new(item.id.clone(), item.kind, item.label)
                .with_presentation(item.presentation);
            if let Some(operation) = item.operation {
                child = child.with_action(operation);
            }
            if item.id == REVIEW && self.install_method == InstallMethod::Alongside {
                child = with_guidance(child, REVIEW, &self.review_guidance());
            }
            if item.id == STORAGE && self.install_method == InstallMethod::Alongside {
                child = with_guidance(child, STORAGE, &self.alongside_storage_guidance());
            }
            root = root.with_child(child);
        }
        vec![root]
    }

    fn next_gate(&self) -> Gate {
        let label = match self.step {
            Step::Network
                if self.network.snapshot.connectivity
                    == imager_backend::destination::network::ConnectivityState::Full =>
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
        Gate::new("next", blocked).labelled(label)
    }

    fn back_gate(&self) -> Gate {
        let failed = self.step == Step::Install && self.install_state == InstallState::Failed;
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
        let gate = Gate::new("back", blocked);
        if failed {
            gate.labelled("Review storage")
        } else {
            gate
        }
    }

    /// Staged installs only: the way back to the retained operating system,
    /// offered on every screen except while installing or once done.
    fn return_to_host_gate(&self) -> Gate {
        let Some(host) = self.internal_host() else {
            return Gate::new(
                "return-to-host",
                Some((
                    "not_staged",
                    "This installer was not staged next to an existing system".to_owned(),
                )),
            );
        };
        let blocked = match self.install_state {
            InstallState::Running => Some((
                "install_running",
                "The install is running; wait for it to finish or fail".to_owned(),
            )),
            InstallState::Done => Some((
                "install_done",
                "Tilo is installed; use Reboot now (reboot@installer) instead".to_owned(),
            )),
            InstallState::Idle | InstallState::Failed => None,
        };
        Gate::new("return-to-host", blocked).labelled(format!("Return to {}", host.display_name()))
    }

    fn network_gates(&self) -> Vec<Gate> {
        let on_step = self.off_step(Step::Network);
        let busy = self.network.is_busy().then(|| {
            (
                "network_busy",
                "A network operation is already in progress".to_owned(),
            )
        });
        let select = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            self.network.snapshot.networks.is_empty().then(|| {
                (
                    "no_networks",
                    self.network.snapshot.wifi_error.clone().unwrap_or_else(|| {
                        "No Wi-Fi networks are visible; refresh the list".into()
                    }),
                )
            })
        });
        let connect = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            match self.network.selected_network() {
                None => Some(("no_selection", "Choose a Wi-Fi network first".to_owned())),
                Some(network) if network.connected => Some((
                    "already_connected",
                    "That Wi-Fi network is already connected".to_owned(),
                )),
                Some(_) => None,
            }
        });
        vec![
            Gate::new("refresh-networks", on_step.or(busy)),
            Gate::new("select-network", select),
            Gate::new("connect-network", connect),
        ]
    }

    fn online_account_gates(&self) -> Vec<Gate> {
        use crate::setup::destination::accounts_view::AccountsStatus;
        let status = self.accounts_controller.as_ref().map_or(
            if self.accounts_initialized {
                AccountsStatus::Failed
            } else {
                AccountsStatus::Ready
            },
            imager_backend::destination::online_accounts::AccountsController::status,
        );
        let on_step = self.off_step(Step::Accounts);
        let signing_in = (status == AccountsStatus::SigningIn).then(|| {
            (
                "signing_in",
                "Wait for sign-in to finish or cancel it".to_owned(),
            )
        });
        let add = Gate::new(
            "add-account",
            on_step.clone().or_else(|| signing_in.clone()),
        );
        let add = if status == AccountsStatus::ReviewCalendars {
            add.labelled("Confirm selected calendars")
        } else {
            add
        };
        vec![
            add,
            Gate::new(
                "retry-account",
                on_step.clone().or(signing_in).or_else(|| {
                    (status != AccountsStatus::Failed).then(|| {
                        (
                            "not_failed",
                            "Retry is available after a failed sign-in".to_owned(),
                        )
                    })
                }),
            ),
            Gate::new("cancel-sign-in", on_step.clone()),
            Gate::new("continue-offline", on_step),
        ]
    }

    fn recovery_gates(&self) -> Vec<Gate> {
        let no_key = self.recovery_key.is_none().then(|| {
            (
                "no_recovery_key",
                "No recovery key has been generated".to_owned(),
            )
        });
        let finished = (self.step != Step::Install || self.install_state != InstallState::Done)
            .then(|| {
                (
                    "not_finished",
                    "Power actions are offered once the install has finished".to_owned(),
                )
            });
        let unsaved = |verb: &str| {
            finished.clone().or_else(|| {
                (!self.recovery_key_saved).then(|| {
                    (
                        "recovery_key_unsaved",
                        format!(
                            "Save the recovery key off this computer and confirm it before {verb}"
                        ),
                    )
                })
            })
        };
        vec![
            Gate::new("copy-recovery-key", no_key.clone()),
            Gate::new("confirm-recovery-saved", no_key),
            Gate::new("reboot", unsaved("rebooting")),
            Gate::new("power-off", unsaved("powering off")),
        ]
    }

    fn disk_blocked(&self, disk: &imager_backend::destination::Disk) -> Blocked {
        self.off_step(Step::Disk).or_else(|| {
            if disk.blocks_install() {
                Some((
                    "installer_disk",
                    "The running installer disk cannot be used as its own destination".to_owned(),
                ))
            } else if disk.read_only {
                Some((
                    "read_only",
                    "This disk is hardware or policy write-protected.".to_owned(),
                ))
            } else {
                None
            }
        })
    }

    /// The disks, the free regions, the capacity choice, the visible
    /// networks and the review, each with the effect bound to it.
    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for network in self.network.snapshot.networks.iter().take(5) {
            let ssid = network.ssid.clone();
            items.push(Item {
                id: format!("installer.network:{ssid}"),
                kind: "installer.network",
                label: ssid.clone(),
                presentation: ResourcePresentation {
                    badge: (self.network.selected_ssid.as_deref() == Some(ssid.as_str()))
                        .then(|| "selected".to_owned()),
                    ..ResourcePresentation::default()
                },
                operation: Some(gated(
                    DestinationEffect::SelectNetwork { ssid }.bound(),
                    self.blocked(&DestinationEffect::SelectNetwork {
                        ssid: network.ssid.clone(),
                    }),
                )),
            });
        }
        for (index, disk) in self.disks.iter().enumerate() {
            let badge = if self.selected_disk == Some(index) {
                "selected"
            } else if disk.blocks_install() {
                "installer medium"
            } else if disk.read_only {
                "read-only"
            } else {
                "available"
            };
            items.push(Item {
                id: disk_row(&disk.name),
                kind: "installer.disk",
                label: disk.label(),
                presentation: ResourcePresentation {
                    detail: Some(bounded(
                        &format!("{}, {}", capacity(disk), disk.contents_summary()),
                        160,
                    )),
                    badge: Some(badge.to_owned()),
                    preview: None,
                },
                operation: Some(gated(
                    DestinationEffect::SelectDisk {
                        device: disk.name.clone(),
                    }
                    .bound(),
                    self.disk_blocked(disk),
                )),
            });
        }
        let plans = self.guided_alongside_plans();
        if plans.len() > 1 {
            for (index, plan) in plans.iter().enumerate() {
                items.push(Item {
                    id: region_row(index),
                    kind: "installer.region",
                    label: format!(
                        "{} free at {}",
                        ui::format::memory(plan.free_size_bytes),
                        ui::format::memory(plan.free_start_bytes)
                    ),
                    presentation: ResourcePresentation {
                        badge: (self.selected_guided_extent == Some(index))
                            .then(|| "selected".to_owned()),
                        ..ResourcePresentation::default()
                    },
                    operation: Some(gated(
                        DestinationEffect::SelectRegion {
                            index: i64::try_from(index).unwrap_or(i64::MAX),
                        }
                        .bound(),
                        self.off_step(Step::Disk),
                    )),
                });
            }
        }
        items.push(Item {
            id: STORAGE.to_owned(),
            kind: "installer.storage",
            label: "Installation target".to_owned(),
            presentation: ResourcePresentation {
                detail: Some(if self.install_method == InstallMethod::Alongside {
                    bounded(&self.alongside_storage_guidance(), 160)
                } else {
                    "Erase the selected disk".to_owned()
                }),
                ..ResourcePresentation::default()
            },
            operation: self
                .capacity_effect()
                .map(|effect| gated(effect.bound_asking(&["gib"]), self.off_step(Step::Disk))),
        });
        items.push(self.review_item());
        items
    }

    /// The capacity choice of the selected free extent, bound to the disk
    /// and extent it divides: `None` where there is none to make.
    pub(super) fn capacity_effect(&self) -> Option<DestinationEffect> {
        let extent = self.selected_full_guided_plan()?;
        let disk = self
            .selected_disk_ref()
            .filter(|disk| !disk.hosts_staged_installer())?;
        Some(DestinationEffect::SetSize {
            gib: 0,
            extent: reviewed_digest(&(disk, &extent)),
        })
    }

    /// The destructive `begin`, bound to the target and plan reviewed now.
    pub(super) fn begin_effect(&self) -> DestinationEffect {
        DestinationEffect::Begin {
            target: self.destructive_confirmation_target().unwrap_or_default(),
            allocation: reviewed_digest(&self.guided_alongside_plan()),
        }
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

    fn review_guidance(&self) -> String {
        format!(
            "{} {} {} {}",
            alongside_install_impact(
                self.uses_guided_alongside(),
                &self.selected_dev().unwrap_or_default()
            ),
            self.guided_alongside_plan()
                .as_ref()
                .map(|plan| self.alongside_allocation_summary(plan))
                .unwrap_or_default(),
            crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE),
            crate::setup::strings::t(ALONGSIDE_FILES_GUIDANCE)
        )
    }

    fn review_item(&self) -> Item {
        let target = self.destructive_confirmation_target();
        let erase = self.install_method == InstallMethod::Erase;
        let description = match (&target, erase) {
            (Some(device), true) => format!(
                "ERASES {device} and all data on it, then installs Tilo. Destructive: type the device name as the confirmation phrase."
            ),
            (Some(device), false) => format!(
                "{} {} {} Destructive: type the confirmation phrase.",
                alongside_install_impact(self.uses_guided_alongside(), device),
                crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE),
                crate::setup::strings::t(ALONGSIDE_FILES_GUIDANCE)
            ),
            (None, _) => "Select a destination first".to_owned(),
        };
        let mut begin = gated(self.begin_effect().bound(), self.begin_blocked());
        begin.type_mut().description = Some(description);
        Item {
            id: REVIEW.to_owned(),
            kind: "installer.review",
            label: "Review and install".to_owned(),
            presentation: ResourcePresentation::default(),
            operation: Some(begin),
        }
    }

    fn apply_online_control(&mut self, effect: &DestinationEffect) -> Applied {
        self.require_step(Step::Accounts)?;
        match effect {
            DestinationEffect::AddAccount | DestinationEffect::RetryAccount => self
                .accounts_form
                .submit(matches!(effect, DestinationEffect::RetryAccount))
                .map_err(|_| {
                    "Check the private account form or wait for sign-in to finish".to_string()
                })?,
            DestinationEffect::CancelSignIn => self.accounts_form.cancel(),
            _ => self.accounts_form.skip_offline(),
        }
        self.sync_accounts();
        Ok(ActionOutput::default())
    }

    /// Run a wizard control's effect through the same rules the console's
    /// door applies: refused with the published reason while unavailable,
    /// and the destructive one only when `phrase` names what it formats. A
    /// refusal shows the sentence an external caller receives.
    pub(super) fn invoke_control(&mut self, effect: DestinationEffect, phrase: Option<&str>) {
        if let Some((_, reason)) = self.blocked(&effect) {
            self.set_status(&reason);
            return;
        }
        if let Some(expected) = effect.confirmation_phrase()
            && phrase.map(str::trim) != Some(expected.as_str())
        {
            let message = format!("Type {expected} exactly to confirm formatting it");
            self.set_status(&message);
            return;
        }
        // Handlers set the status or notice the wizard shows; the returned
        // error only matters to a caller that cannot see it.
        let _ = self.apply(effect);
    }

    /// Invoke one published effect exactly as the console door does:
    /// resolve it in the current tree, refuse it when unavailable, enforce
    /// its confirmation contract, bind its arguments strictly, then apply it.
    /// The unattended driver and host-side tests use this instead of a second
    /// path.
    ///
    /// # Errors
    ///
    /// Returns the door's stable code and sentence when the effect is not
    /// published, unavailable, unconfirmed or given arguments it cannot bind.
    pub fn invoke_published(
        &mut self,
        id: &str,
        args: &Map<String, Value>,
    ) -> Result<ActionOutput, String> {
        use imager_backend::destination::console;
        let resources = self.published();
        let admitted = console::admit(PROVIDER, &resources, PROVIDER, id, args)
            .map_err(|refusal| format!("{}: {}", refusal.code, refusal.message))?;
        let effect = DestinationEffect::from_payload(&admitted.encoded.payload)?;
        self.apply(effect)
    }

    /// Apply one effect. Every door ends here, after availability,
    /// confirmation and binding; the handlers still re-check the wizard's own
    /// preconditions, so a caller that reached this function some other way
    /// cannot skip a step.
    ///
    /// # Errors
    ///
    /// Returns the sentence the wizard would show when the effect cannot do
    /// its job now.
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per effect keeps each handler next to its siblings"
    )]
    pub fn apply(&mut self, effect: DestinationEffect) -> Applied {
        let done = || Ok(ActionOutput::default());
        match effect {
            DestinationEffect::Status => Ok(ActionOutput {
                value: Some(self.console_state()),
                resources: Vec::new(),
            }),
            DestinationEffect::Next => {
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
            DestinationEffect::Back => {
                let before = (self.step, self.install_state);
                self.go_back();
                if (self.step, self.install_state) == before {
                    Err("There is no step to go back to".into())
                } else {
                    done()
                }
            }
            DestinationEffect::SetLayout { layout } => {
                self.require_step(Step::Keyboard)?;
                if layouts::by_display_name(layout.trim()).is_none() {
                    return Err(format!("Unknown keyboard layout {layout:?}"));
                }
                self.keymap.set(layout.trim().to_owned());
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::RefreshNetworks => {
                self.require_step(Step::Network)?;
                let Some(wake) = self.network_wake() else {
                    self.network.notice = "Network controls are still starting; try again.".into();
                    self.dirty_structure = true;
                    return Err(self.network.notice.clone());
                };
                self.network.start_scan(wake);
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SelectNetwork { ssid } => {
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
            DestinationEffect::ConnectNetwork { password } => {
                self.require_step(Step::Network)?;
                let password = Zeroizing::new(password.unwrap_or_default());
                let Some(wake) = self.network_wake() else {
                    self.network.notice = "Network controls are still starting; try again.".into();
                    self.dirty_structure = true;
                    return Err(self.network.notice.clone());
                };
                let result = self.network.start_connect(password, wake);
                self.dirty_structure = true;
                result.map_err(|message| {
                    self.network.notice = message.to_string();
                    message.to_string()
                })?;
                done()
            }
            DestinationEffect::SetMethod { method } => {
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
            DestinationEffect::RefreshDisks => {
                self.require_step(Step::Disk)?;
                let disks = self.backend.read_disks();
                self.refresh_disks_with(disks);
                done()
            }
            DestinationEffect::SelectDisk { device } => {
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
            DestinationEffect::SelectRegion { index } => {
                self.require_step(Step::Disk)?;
                let count = self.guided_alongside_plans().len();
                let index = usize::try_from(index).unwrap_or(usize::MAX);
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
            DestinationEffect::SetSize { gib, extent } => {
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
                if reviewed_digest(&(disk, &full)) != extent {
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
            DestinationEffect::SetIdentity {
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
            DestinationEffect::SetPassphrase {
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
            DestinationEffect::SetDiskPassphrase {
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
                let (pin, force_passphrase) = match mode.as_deref().unwrap_or_default() {
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
            DestinationEffect::SetPasswordHash { hash } => {
                self.require_step(Step::Account)?;
                if !self.unattended_mode {
                    return Err(
                        "A password hash is accepted only by an explicit unattended installation"
                            .into(),
                    );
                }
                let hash = Zeroizing::new(hash);
                if !crate::setup::destination::unattended::valid_sha512_crypt(&hash) {
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
            DestinationEffect::SetFilesystem { filesystem } => {
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
            DestinationEffect::SetSecurityKey { enabled } => {
                self.require_step(Step::Options)?;
                self.fido2 = enabled;
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SetMediaCodecs { enabled } => {
                self.require_step(Step::Media)?;
                self.codecs = enabled;
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SetHostname { hostname } => {
                self.require_step(Step::Options)?;
                if !valid_hostname(&hostname) {
                    return Err("Enter a valid hostname (letters, digits, hyphens)".into());
                }
                self.hostname.set(hostname.trim().to_owned());
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SetLocale { locale } => {
                self.require_step(Step::Options)?;
                if !valid_locale(locale.trim()) {
                    return Err("Choose a supported language and locale from the list".into());
                }
                self.locale.set(locale.trim().to_owned());
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SetTimezone { timezone } => {
                self.require_step(Step::Options)?;
                let timezone = timezone.trim().to_owned();
                if !valid_timezone_name(&timezone) || !self.timezones.contains(&timezone) {
                    return Err("Choose a valid time zone from the list".into());
                }
                self.timezone.set(timezone);
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::SetRecoveryPromise { acknowledged } => {
                self.require_step(Step::Review)?;
                self.recovery_ack = acknowledged;
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::Begin { target, allocation } => {
                if allocation != reviewed_digest(&self.guided_alongside_plan()) {
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
            DestinationEffect::CopyRecoveryKey => {
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
            DestinationEffect::ConfirmRecoverySaved { saved } => {
                if self.recovery_key.is_none() {
                    return Err("No recovery key has been generated".into());
                }
                self.recovery_key_saved = saved;
                self.status.clear();
                self.dirty_structure = true;
                done()
            }
            DestinationEffect::Reboot | DestinationEffect::PowerOff => {
                let (action, verb) = if matches!(effect, DestinationEffect::Reboot) {
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
            DestinationEffect::AddAccount
            | DestinationEffect::RetryAccount
            | DestinationEffect::CancelSignIn
            | DestinationEffect::ContinueOffline => self.apply_online_control(&effect),
            DestinationEffect::ReturnToHost => {
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
