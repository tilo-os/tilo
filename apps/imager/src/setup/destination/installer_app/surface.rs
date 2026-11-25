//! The installer's published effects: what each one is offered on, and why one
//! cannot run now.

use serde_json::{Map, Value};
use tilo_resource::Effect as _;
use tilo_resource::LiveRow;
use tilo_resource::RowBindings;
use ui::i18n::ArgValue;
use ui::tilo_actions::{ActionOutput, ResourcePresentation};
use zeroize::Zeroizing;

use super::{
    ALONGSIDE_BOOT_GUIDANCE, ALONGSIDE_FILES_GUIDANCE, FILESYSTEMS, InstallMethod, InstallState,
    InstallerApp, STEP_KEYS, Step, VerifiedBootState, alongside_install_impact,
    clear_secret_buffer, prev_step, secret_copy, step_index, valid_full_name, valid_hostname,
    valid_locale, valid_timezone_name, valid_username, validate_passwords,
    validate_startup_secret_for,
};
use crate::setup::destination::disk_row::capacity;
use crate::setup::destination::effects::{
    DestinationEffect, Disk, Network, PROVIDER, REVIEW, Region, Review, STORAGE, Session, Storage,
    disk_row, reference, region_row, reviewed_digest,
};
use crate::setup::strings::{t, t_args};
use imager_backend::destination::install::PowerAction;
use imager_backend::destination::layouts;

/// How long a copied recovery key stays on the clipboard: time to paste it
/// into a password manager. Rebooting or powering off clears it sooner.
const RECOVERY_KEY_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(300);

type Blocked = Option<(&'static str, String)>;

struct Check {
    id: &'static str,
    blocked: Blocked,
    label: Option<String>,
}

impl Check {
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
}

struct Item {
    id: String,
    kind: ItemKind,
    label: String,
    presentation: ResourcePresentation,
    choice: Option<Choice>,
}

/// What an item's row offers: one declared effect with the arguments its
/// state knows, and whether its state refuses it now.
struct Choice {
    effect: DestinationEffect,
    bound: Vec<(&'static str, serde_json::Value)>,
    prefill: Vec<(&'static str, serde_json::Value)>,
    blocked: Blocked,
    description: Option<String>,
}

impl Choice {
    fn bindings(&self) -> RowBindings {
        RowBindings::new().bind_id(self.effect.id(), self.bound.clone())
    }
}

/// What an item is, as the type its row is an item of.
#[derive(Clone, Copy)]
enum ItemKind {
    Network,
    Disk,
    Region,
    Storage,
    Review,
}

impl ItemKind {
    fn row(self, id: String, label: String, choice: Option<&Choice>) -> LiveRow {
        let bindings = choice.map_or_else(RowBindings::new, Choice::bindings);
        let row = match self {
            Self::Network => LiveRow::typed_with::<Network>(id, label, &bindings),
            Self::Disk => LiveRow::typed_with::<Disk>(id, label, &bindings),
            Self::Region => LiveRow::typed_with::<Region>(id, label, &bindings),
            Self::Storage => LiveRow::typed_with::<Storage>(id, label, &bindings),
            Self::Review => LiveRow::typed_with::<Review>(id, label, &bindings),
        };
        match (self, choice) {
            (Self::Storage, Some(choice)) if !choice.prefill.is_empty() => {
                row.prefilled(DestinationEffect::SET_SIZE, choice.prefill.clone())
            }
            _ => row,
        }
    }
}

struct Guidance;
impl tilo_resource::words::RowType for Guidance {
    const KIND: &'static str = "installer.guidance";
    const LABEL: &'static str = "Guidance";
}

fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

pub(super) fn step_name(step: Step) -> String {
    let index = step_index(step).min(STEP_KEYS.len().saturating_sub(1));
    t(STEP_KEYS[index])
}

/// Apply each row's refusals to the row published under that id.
fn apply_refusals(
    row: LiveRow,
    refusals: &[(String, Vec<(&'static str, &'static str, String)>)],
) -> LiveRow {
    let Some((_, for_row)) = refusals.iter().find(|(id, _)| id.as_str() == row.id()) else {
        return row;
    };
    for_row.iter().fold(row, |row, (effect, code, reason)| {
        row.unavailable(effect, *code, reason.clone())
    })
}

/// Say what each item's declared effect is for on this row (the review's own
/// description of what it is about to do).
fn apply_descriptions(row: LiveRow, items: &[Item]) -> LiveRow {
    let Some(item) = items.iter().find(|item| item.id == row.id()) else {
        return row;
    };
    let Some(choice) = &item.choice else {
        return row;
    };
    let Some(description) = &choice.description else {
        return row;
    };
    row.describe(choice.effect.id(), description.clone())
}

fn with_guidance(row: LiveRow, guidance: &str) -> LiveRow {
    let id = row.id().to_owned();
    guidance
        .split(". ")
        .enumerate()
        .fold(row, |row, (index, sentence)| {
            row.row(
                LiveRow::typed::<Guidance>(
                    format!("{id}.guidance:{index}"),
                    crate::resource::CATALOG.t("imager-installer-guidance"),
                )
                .presentation(ResourcePresentation {
                    preview: Some(sentence.to_owned()),
                    ..ResourcePresentation::default()
                }),
            )
        })
}

pub(super) type Applied = Result<ActionOutput, String>;

impl InstallerApp {
    fn off_step(&self, step: Step) -> Blocked {
        (self.step != step).then(|| {
            (
                "other_step",
                t_args(
                    "installer-blocked-other-step",
                    &[
                        ("wanted", ArgValue::Text(step_name(step))),
                        ("current", ArgValue::Text(step_name(self.step))),
                    ],
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

    pub(super) fn install_state_key(&self) -> &'static str {
        match self.install_state {
            InstallState::Idle => "installer-state-idle",
            InstallState::Running => "installer-state-running",
            InstallState::Done => "installer-state-done",
            InstallState::Failed => "installer-state-failed",
        }
    }

    fn checks(&self) -> Vec<Check> {
        let account = self.off_step(Step::Account);
        let options = self.off_step(Step::Options);
        let mut checks = vec![
            Check::new("status", None),
            self.next_check(),
            self.back_check(),
            self.return_to_host_check(),
            Check::new("set-layout", self.off_step(Step::Keyboard)),
            Check::new("set-method", self.off_step(Step::Disk)),
            Check::new("refresh-disks", self.off_step(Step::Disk)),
            Check::new("set-identity", account.clone()),
            Check::new("set-passphrase", account.clone()),
            Check::new("set-disk-passphrase", account.clone()),
            Check::new(
                "set-password-hash",
                if self.unattended_mode {
                    account
                } else {
                    Some(("interactive_only", t("installer-blocked-password-hash")))
                },
            ),
            Check::new("set-filesystem", options.clone()),
            Check::new("set-security-key", options.clone()),
            Check::new("set-hostname", options.clone()),
            Check::new("set-locale", options.clone()),
            Check::new("set-timezone", options),
            Check::new("set-media-codecs", self.off_step(Step::Media)),
            Check::new("set-recovery-promise", self.off_step(Step::Review)),
        ];
        checks.extend(self.network_checks());
        checks.extend(self.recovery_checks());
        checks
    }

    pub(super) fn blocked(&self, effect: &DestinationEffect) -> Blocked {
        let id = effect.id();
        if let Some(check) = self.checks().into_iter().find(|check| check.id == id) {
            return check.blocked;
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
                            t_args(
                                "installer-blocked-no-such-disk",
                                &[("device", ArgValue::Text(device.clone()))],
                            ),
                        ))
                    },
                    |disk| self.disk_blocked(disk),
                ),
            DestinationEffect::Begin { .. } => self.begin_blocked(),
            _ => self.off_step(Step::Disk),
        }
    }

    pub(super) fn published_rows(&self) -> Vec<LiveRow> {
        let alongside = self.install_method == InstallMethod::Alongside;
        self.items()
            .into_iter()
            .map(|item| {
                let guidance = match item.id.as_str() {
                    REVIEW if alongside => Some(self.review_guidance()),
                    STORAGE if alongside => Some(self.alongside_storage_guidance()),
                    _ => None,
                };
                let row = item
                    .kind
                    .row(item.id, item.label, item.choice.as_ref())
                    .presentation(item.presentation);
                match guidance {
                    Some(guidance) => with_guidance(row, &guidance),
                    None => row,
                }
            })
            .collect()
    }

    pub fn published(&self) -> Vec<LiveRow> {
        let checks = self.checks();
        let mut bindings = RowBindings::new();
        for declaration in DestinationEffect::declarations() {
            bindings = bindings.offer_id(declaration.id);
        }
        let mut root = LiveRow::typed_with::<Session>(
            PROVIDER,
            crate::resource::CATALOG.t("imager-installer-title"),
            &bindings,
        )
        .presentation(ResourcePresentation {
            detail: Some(t_args(
                "installer-detail-step",
                &[
                    (
                        "number",
                        ArgValue::Number(f64::from(
                            u32::try_from(step_index(self.step).saturating_add(1)).unwrap_or(1),
                        )),
                    ),
                    ("count", ArgValue::Number(f64::from(STEP_KEYS.len() as u32))),
                    ("step", ArgValue::Text(step_name(self.step))),
                ],
            )),
            badge: Some(t(self.install_state_key())),
            preview: None,
        });
        for check in &checks {
            if let Some(label) = &check.label {
                root = root.labelled(reference(check.id), label.clone());
            }
        }
        let refusals = self.refusals();
        let items = self.items();
        let root = apply_refusals(root, &refusals);
        let root = apply_descriptions(root, &items);
        let root = self.published_rows().into_iter().fold(root, |root, row| {
            root.row(apply_descriptions(apply_refusals(row, &refusals), &items))
        });
        vec![root]
    }

    /// What each published row refuses now: its effect's id, the code a
    /// program reads and the reason a person reads.
    pub(crate) fn refusals(&self) -> Vec<(String, Vec<(&'static str, &'static str, String)>)> {
        let mut by_row = Vec::new();
        let root: Vec<(&'static str, &'static str, String)> = self
            .checks()
            .into_iter()
            .filter_map(|check| check.blocked.map(|(code, reason)| (check.id, code, reason)))
            .collect();
        if !root.is_empty() {
            by_row.push((PROVIDER.to_owned(), root));
        }
        for item in self.items() {
            if let Some(choice) = &item.choice
                && let Some((code, reason)) = &choice.blocked
            {
                by_row.push((
                    item.id.clone(),
                    vec![(choice.effect.id(), *code, reason.clone())],
                ));
            }
        }
        by_row
    }

    fn next_check(&self) -> Check {
        let label = match self.step {
            Step::Network
                if self.network.snapshot.connectivity
                    == imager_backend::destination::network::ConnectivityState::Full =>
            {
                crate::resource::CATALOG.t("imager-installer-continue")
            }
            Step::Network => crate::resource::CATALOG.t("imager-installer-continue-offline"),
            _ => crate::resource::CATALOG.t("imager-installer-next"),
        };
        let blocked = match self.step {
            Step::Review => Some(("begin_required", t("installer-blocked-next-review"))),
            Step::Install => Some(("install_started", t("installer-blocked-next-install"))),
            _ => None,
        };
        Check::new("next", blocked).labelled(label)
    }

    fn back_check(&self) -> Check {
        let failed = self.step == Step::Install && self.install_state == InstallState::Failed;
        let blocked = if failed || prev_step(self.step).is_some() {
            None
        } else if self.step == Step::Install {
            Some(("install_started", t("installer-blocked-back-install")))
        } else {
            Some(("first_step", t("installer-blocked-back-first")))
        };
        let check = Check::new("back", blocked);
        if failed {
            check.labelled(t("installer-action-review-storage"))
        } else {
            check
        }
    }

    fn return_to_host_check(&self) -> Check {
        let Some(host) = self.internal_host() else {
            return Check::new(
                "return-to-host",
                Some(("not_staged", t("installer-blocked-not-staged"))),
            );
        };
        let blocked = match self.install_state {
            InstallState::Running => {
                Some(("install_running", t("installer-blocked-return-running")))
            }
            InstallState::Done => Some(("install_done", t("installer-blocked-return-done"))),
            InstallState::Idle | InstallState::Failed => None,
        };
        Check::new("return-to-host", blocked).labelled(t_args(
            "installer-action-return-to-host",
            &[("host", ArgValue::Text(host.display_name().to_owned()))],
        ))
    }

    fn network_checks(&self) -> Vec<Check> {
        let on_step = self.off_step(Step::Network);
        let busy = self
            .network
            .is_busy()
            .then(|| ("network_busy", t("installer-blocked-network-busy")));
        let select = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            self.network.snapshot.networks.is_empty().then(|| {
                (
                    "no_networks",
                    self.network
                        .snapshot
                        .wifi_error
                        .clone()
                        .unwrap_or_else(|| t("installer-blocked-no-networks")),
                )
            })
        });
        let connect = on_step.clone().or_else(|| busy.clone()).or_else(|| {
            match self.network.selected_network() {
                None => Some(("no_selection", t("installer-blocked-no-network-choice"))),
                Some(network) if network.connected => Some((
                    "already_connected",
                    t("installer-blocked-network-connected"),
                )),
                Some(_) => None,
            }
        });
        vec![
            Check::new("refresh-networks", on_step.or(busy)),
            Check::new("select-network", select),
            Check::new("connect-network", connect),
        ]
    }

    fn recovery_checks(&self) -> Vec<Check> {
        let no_key = self
            .recovery_key
            .is_none()
            .then(|| ("no_recovery_key", t("installer-blocked-no-recovery-key")));
        let finished = (self.step != Step::Install || self.install_state != InstallState::Done)
            .then(|| ("not_finished", t("installer-blocked-power-not-finished")));
        let unsaved = |key: &str| {
            finished
                .clone()
                .or_else(|| (!self.recovery_key_saved).then(|| ("recovery_key_unsaved", t(key))))
        };
        vec![
            Check::new(
                "copy-recovery-key",
                no_key.clone().or_else(|| {
                    (!ui::clipboard_available())
                        .then(|| ("no_clipboard", t("installer-blocked-no-clipboard")))
                }),
            ),
            Check::new("confirm-recovery-saved", no_key),
            Check::new(
                "reboot",
                unsaved("installer-blocked-recovery-unsaved-reboot"),
            ),
            Check::new(
                "power-off",
                unsaved("installer-blocked-recovery-unsaved-power-off"),
            ),
        ]
    }

    fn disk_blocked(&self, disk: &imager_backend::destination::Disk) -> Blocked {
        self.off_step(Step::Disk).or_else(|| {
            if disk.blocks_install() {
                Some(("installer_disk", t("installer-blocked-installer-disk")))
            } else if disk.read_only {
                Some(("read_only", t("installer-blocked-read-only")))
            } else if disk.too_small() {
                Some((
                    "too_small",
                    crate::setup::destination::disk_row::too_small(),
                ))
            } else {
                None
            }
        })
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for network in self.network.snapshot.networks.iter().take(5) {
            let ssid = network.ssid.clone();
            items.push(Item {
                id: format!("installer.network:{ssid}"),
                kind: ItemKind::Network,
                label: ssid.clone(),
                presentation: ResourcePresentation {
                    badge: (self.network.selected_ssid.as_deref() == Some(ssid.as_str()))
                        .then(|| "selected".to_owned()),
                    ..ResourcePresentation::default()
                },
                choice: Some(Choice {
                    effect: DestinationEffect::SelectNetwork { ssid: ssid.clone() },
                    bound: vec![("ssid", serde_json::Value::from(ssid.clone()))],
                    prefill: Vec::new(),
                    blocked: self.blocked(&DestinationEffect::SelectNetwork {
                        ssid: network.ssid.clone(),
                    }),
                    description: None,
                }),
            });
        }
        for (index, disk) in self.disks.iter().enumerate() {
            let badge = if self.selected_disk == Some(index) {
                "selected"
            } else if disk.blocks_install() {
                "installer medium"
            } else if disk.read_only {
                "read-only"
            } else if disk.too_small() {
                "too small"
            } else {
                "available"
            };
            items.push(Item {
                id: disk_row(&disk.name),
                kind: ItemKind::Disk,
                label: disk.label(),
                presentation: ResourcePresentation {
                    detail: Some(bounded(
                        &crate::contents::drive_line(
                            &capacity(disk),
                            disk.partitions.len(),
                            &disk.partition_filesystems(),
                        ),
                        160,
                    )),
                    badge: Some(badge.to_owned()),
                    preview: None,
                },
                choice: Some(Choice {
                    effect: DestinationEffect::SelectDisk {
                        device: disk.name.clone(),
                    },
                    bound: vec![("device", serde_json::Value::from(disk.name.clone()))],
                    prefill: Vec::new(),
                    blocked: self.disk_blocked(disk),
                    description: None,
                }),
            });
        }
        let plans = self.guided_alongside_plans();
        if plans.len() > 1 {
            for (index, plan) in plans.iter().enumerate() {
                items.push(Item {
                    id: region_row(index),
                    kind: ItemKind::Region,
                    label: t_args(
                        "installer-item-region",
                        &[
                            (
                                "size",
                                ArgValue::Text(ui::format::memory(plan.free_size_bytes)),
                            ),
                            (
                                "start",
                                ArgValue::Text(ui::format::memory(plan.free_start_bytes)),
                            ),
                        ],
                    ),
                    presentation: ResourcePresentation {
                        badge: (self.selected_guided_extent == Some(index))
                            .then(|| "selected".to_owned()),
                        ..ResourcePresentation::default()
                    },
                    choice: Some(Choice {
                        effect: DestinationEffect::SelectRegion {
                            index: i64::try_from(index).unwrap_or(i64::MAX),
                        },
                        bound: vec![(
                            "index",
                            serde_json::Value::from(i64::try_from(index).unwrap_or(i64::MAX)),
                        )],
                        prefill: Vec::new(),
                        blocked: self.off_step(Step::Disk),
                        description: None,
                    }),
                });
            }
        }
        items.push(Item {
            id: STORAGE.to_owned(),
            kind: ItemKind::Storage,
            label: t("installer-item-storage"),
            presentation: ResourcePresentation {
                detail: Some(if self.install_method == InstallMethod::Alongside {
                    bounded(&self.alongside_storage_guidance(), 160)
                } else {
                    t("installer-item-storage-erase")
                }),
                ..ResourcePresentation::default()
            },
            choice: self.capacity_effect().map(|effect| {
                let bound = match &effect {
                    DestinationEffect::SetSize { extent, .. } => {
                        vec![("extent", serde_json::Value::from(extent.clone()))]
                    }
                    _ => Vec::new(),
                };
                Choice {
                    effect,
                    bound,
                    prefill: vec![("gib", serde_json::Value::from(0))],
                    blocked: self.off_step(Step::Disk),
                    description: None,
                }
            }),
        });
        items.push(self.review_item());
        items
    }

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

    pub(super) fn begin_effect(&self) -> DestinationEffect {
        DestinationEffect::Begin {
            target: self.destructive_confirmation_target().unwrap_or_default(),
            allocation: reviewed_digest(&self.guided_alongside_plan()),
        }
    }

    fn begin_blocked(&self) -> Blocked {
        self.off_step(Step::Review)
            .or_else(|| {
                self.destructive_confirmation_target()
                    .is_none()
                    .then(|| ("no_target", t("installer-blocked-begin-no-target")))
            })
            .or_else(|| {
                (!self.recovery_ack).then(|| {
                    (
                        "recovery_key_promise",
                        t("installer-blocked-begin-recovery"),
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
            (Some(device), true) => t_args(
                "installer-review-erase",
                &[("device", ArgValue::Text(device.to_owned()))],
            ),
            (Some(device), false) => t_args(
                "installer-review-alongside",
                &[
                    (
                        "impact",
                        ArgValue::Text(alongside_install_impact(
                            self.uses_guided_alongside(),
                            device,
                        )),
                    ),
                    ("boot", ArgValue::Text(t(ALONGSIDE_BOOT_GUIDANCE))),
                    ("files", ArgValue::Text(t(ALONGSIDE_FILES_GUIDANCE))),
                ],
            ),
            (None, _) => t("installer-item-review-no-target"),
        };
        let effect = self.begin_effect();
        let bound = match &effect {
            DestinationEffect::Begin { target, allocation } => vec![
                ("target", serde_json::Value::from(target.clone())),
                ("allocation", serde_json::Value::from(allocation.clone())),
            ],
            _ => Vec::new(),
        };
        Item {
            id: REVIEW.to_owned(),
            kind: ItemKind::Review,
            label: t("installer-item-review"),
            presentation: ResourcePresentation::default(),
            choice: Some(Choice {
                effect,
                bound,
                prefill: Vec::new(),
                blocked: self.begin_blocked(),
                description: Some(description),
            }),
        }
    }

    pub(super) fn invoke_control(&mut self, effect: DestinationEffect, phrase: Option<&str>) {
        if let Some((_, reason)) = self.blocked(&effect) {
            self.set_status(&reason);
            return;
        }
        if let Some(expected) = effect.confirmation_phrase()
            && phrase.map(str::trim) != Some(expected.as_str())
        {
            self.set_status(&t_args(
                "installer-status-type-confirmation",
                &[("phrase", ArgValue::Text(expected))],
            ));
            return;
        }
        let _ = self.apply(effect);
    }

    /// Run the published action `address` with `args`, admitted as a caller's invocation is.
    ///
    /// # Errors
    ///
    /// Why it was refused, or why it failed.
    pub fn invoke_published(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        args: &Map<String, Value>,
        confirmation: Option<&ui::tilo_actions::remote::Confirmation>,
    ) -> Result<ActionOutput, String> {
        let resources: Vec<_> = self
            .published()
            .iter()
            .map(|row| ui::tilo_actions::Exports::exported(row).clone())
            .collect();
        let encoded = ui::tilo_actions::api::admit(&resources, address, args, confirmation)
            .map_err(|refusal| format!("{}: {refusal}", refusal.code()))?;
        self.apply_encoded(&encoded)
    }

    /// Run one admitted action, as its encoded form carries it.
    ///
    /// # Errors
    ///
    /// Why it failed.
    pub fn apply_encoded(&mut self, encoded: &ui::tilo_actions::EncodedAction) -> Applied {
        let effect =
            DestinationEffect::from_payload(&encoded.payload).map_err(|error| error.to_string())?;
        self.apply(effect)
    }

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
                    .ok_or_else(|| {
                        t_args(
                            "installer-blocked-no-such-disk",
                            &[("device", ArgValue::Text(device.clone()))],
                        )
                    })?;
                let usable = self.disks.get(index).is_some_and(|disk| {
                    !disk.blocks_install() && !disk.read_only && !disk.too_small()
                });
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
                    format!(
                        "Choose at least {} within the selected unallocated region",
                        ui::format::memory(imager_backend::MIN_TARGET_BYTES)
                    )
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
                    .ok_or_else(|| {
                        t_args(
                            "installer-error-unknown-filesystem",
                            &[("filesystem", ArgValue::Text(filesystem.clone()))],
                        )
                    })?;
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
                if !ui::clipboard_available() {
                    return Err(t("installer-blocked-no-clipboard"));
                }
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
