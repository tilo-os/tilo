//! The guided setup: the steps from destination to an installed system, and
//! the effects the pages ask for.
//!
//! A route this host, build or catalogue cannot fulfil is shown unavailable with
//! its reason. `--demo` data never enters the live setup or its workers.

#[cfg(test)]
mod back_navigation_tests;
mod capabilities;
mod compatibility;
mod demo;
mod scenarios;
pub use scenarios::SCENARIOS;
mod desktop;
mod devices;
mod direct;
mod draft;
mod machine;
#[cfg(test)]
mod machine_tests;
mod migrate;
mod model;
#[cfg(test)]
mod password_validation_tests;
mod plan_view;
mod storage;
mod target;
#[cfg(test)]
mod target_tests;
mod view;
#[cfg(test)]
mod visual_review_tests;
#[cfg(test)]
mod visual_tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::hosting::{EffectAnswer, EffectCx, Effected, Hosted, Opened, Port, Store};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_resource::EffectStatus;
use tilo_resource::{self, AbsolutePath, ActionRef, Effect};
use ui::icons::Icon;
use ui::theme::Theme;

use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    AppModel, Button, ButtonRole, Card, Container, DrawChild as _, Field, IconWidget,
    LabelPlacement, ListRow, ModelPath, NumberInput, Row, Spacer, Text, TextInput, TextInputKind,
    Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::strings::{t, t_args};
use crate::page;
use crate::resource::Imager;
use model::{Destination, Journey, Page};

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.installer-preview")]
pub enum InstallerPreviewEffect {
    /// Read the preview's choices, excluding secrets.
    #[effect(returns, kind = "read")]
    ReadPreview,
    /// Choose an option on this page.
    #[effect(kind = "outside", returns)]
    ChoosePreview {
        #[param(
            "Choice: computer, another, desktop; os, server; purpose-computer, purpose-server"
        )]
        choice: String,
    },
    /// Choose an option on this page and continue to the next page.
    #[effect(kind = "outside", returns)]
    ChooseAndContinuePreview {
        #[param(
            "Choice: computer, another, desktop; os, server; purpose-computer, purpose-server"
        )]
        choice: String,
    },
    /// Name one identifying field of the other computer.
    #[effect(kind = "outside", returns)]
    SetTargetIdentityField {
        #[param("Field: manufacturer, model, variant, board or sku")]
        field: String,
        #[param("Value as shown on the other computer")]
        value: String,
    },
    /// Select the architecture the prepared medium must start on.
    #[effect(kind = "outside", returns)]
    SetTargetArchitecture {
        #[param("Architecture: x86_64 or aarch64")]
        architecture: String,
    },
    #[effect(kind = "outside", attended, returns)]
    #[effect(label = "installer-preview-target-file-title")]
    ChooseTargetHardwareReport,
    /// Read a hardware report produced on the other computer from this exact file.
    #[effect(kind = "outside", returns)]
    ImportTargetHardwareReport {
        #[param(secret, "Absolute selected hardware report file")]
        source_file: AbsolutePath,
    },
    /// Stop using an imported report and name a model instead.
    #[effect(kind = "outside", returns)]
    ClearTargetHardwareReport,
    /// Check the other computer's description again; this computer is not probed.
    #[effect(returns, kind = "read")]
    RecheckTargetHardware,
    /// Enter the ticket `tilo device add` printed on one of your devices.
    #[effect(kind = "outside", returns)]
    SetMigrateTicket {
        #[param(secret, "Ticket from tilo device add; never persisted or returned")]
        ticket: String,
    },
    /// Sign this computer in to your workspace with the entered ticket.
    #[effect(kind = "outside", returns)]
    SignInForMigration,
    /// Show the migration app, which brings over what this computer holds.
    #[effect(kind = "outside", returns)]
    OpenMigrations,
    /// Set this device's name.
    #[effect(kind = "outside", returns)]
    SetPreviewDeviceName { value: String },
    /// Enter the destination unlock password, in private memory only.
    #[effect(kind = "outside", returns)]
    SetPreviewPassword {
        #[param(secret, "Unlock password; never persisted or returned")]
        value: String,
    },
    /// Confirm the destination unlock password.
    #[effect(kind = "outside", returns)]
    SetPreviewConfirmation {
        #[param(secret, "Unlock confirmation; never persisted or returned")]
        value: String,
    },
    /// Inspect this computer's disk again. Never writes to disk.
    #[effect(kind = "outside", returns)]
    CheckStorage,
    /// Show or hide the inspected disk allocation breakdown.
    #[effect(kind = "outside", returns)]
    ToggleStorageDetails,
    /// Choose the space reserved for Tilo, within the inspected disk's bounds.
    #[effect(kind = "outside", returns)]
    SetStorageSize { gib: u64 },
    /// Open the real USB media writer. Choosing and confirming a drive happens there.
    #[effect(kind = "outside", returns)]
    PrepareUsb,
    /// Prepare the reviewed installation on this computer, without writing its disk.
    #[effect(kind = "outside", returns)]
    PrepareDirectInstallation,
    /// Prepare the current user’s Tilo Desktop setup.
    #[effect(kind = "outside", returns)]
    PrepareDesktopInstallation,
    /// Change a section of the reviewed setup.
    #[effect(kind = "outside", returns)]
    EditPreview { section: String },
    /// Continue to the next relevant page.
    #[effect(kind = "outside", returns)]
    ContinuePreview,
    /// Return to the preceding page.
    #[effect(kind = "outside", returns)]
    BackInPreview,
    /// Start the preview again.
    #[effect(kind = "outside", returns)]
    RestartPreview,
}

type Commands = Rc<RefCell<Vec<InstallerPreviewEffect>>>;

fn queue_continue(commands: &Commands) {
    let mut pending = commands.borrow_mut();
    if !pending
        .iter()
        .any(|effect| matches!(effect, InstallerPreviewEffect::ContinuePreview))
    {
        pending.push(InstallerPreviewEffect::ContinuePreview);
    }
}

/// `input`, continuing the page when the person presses Enter in it.
pub(super) fn submits(commands: &Commands, input: TextInput) -> TextInput {
    let commands = commands.clone();
    input.on_submit(move |_| queue_continue(&commands))
}

/// `input`, running the effect `effect` makes of each value the person types.
pub(super) fn changes(
    commands: &Commands,
    input: TextInput,
    effect: impl Fn(String) -> InstallerPreviewEffect + 'static,
) -> TextInput {
    let commands = commands.clone();
    input.on_change(move |value| commands.borrow_mut().push(effect(value.to_owned())))
}

/// The guided setup's window on `store`: the preview, and the USB media writer it hands the
/// reviewed names to. Each keeps what it shows while the other one does.
pub(crate) fn window(store: &Store, wake: &ui::WakeHandle, opened: Opened<'_>) -> Box<dyn Widget> {
    let media = ui::LocalState::new(false);
    let (store, wake) = (store.clone(), wake.clone());
    let preview_media = media.clone();
    let opened_live = opened.is_live();
    let mut preview = Some(InstallerPreview {
        media: Some(preview_media),
        port: Port::new(store.clone(), wake.clone()),
        ..InstallerPreview::default()
    });
    if let Some(preview) = &mut preview {
        preview.attach(opened);
    }
    Box::new(ui::widget::Switch::from_state(
        &media,
        |showing: &bool| *showing,
        move |showing: &bool| -> ui::widget::Element {
            if *showing {
                crate::app::media_writer(Port::new(store.clone(), wake.clone()), opened_live)
            } else {
                match preview.take() {
                    Some(preview) => Box::new(Hosted(preview)),
                    None => unreachable!("the switch keeps the preview it built"),
                }
            }
        },
    ))
}

/// The plan an "on another computer" preparation made, the computers this
/// person already planned, and why a plan could not be made.
#[derive(Default)]
struct Machine {
    created: Option<machine::Created>,
    /// A plan made here and not yet written into the person's Servers
    /// workspace: the request that writes it, followed until it answers.
    making: Option<(machine::Created, ui::InvocationCall)>,
    /// Whether the review was waiting for the plan before it could continue.
    plan_continue: bool,
    /// The waiting computers, as the session answered the list.
    listing: Option<ui::InvocationCall>,
    waiting: Vec<machine::Waiting>,
    error: Option<&'static str>,
}

pub struct InstallerPreview {
    journey: Journey,
    machine: Machine,
    linux: bool,
    /// A host whose only installation here is the native Full Tilo setup,
    /// without the guided pages: Windows.
    full_os_host: bool,
    capabilities: capabilities::GuidedCapabilities,
    commands: Commands,
    composed: Option<Box<dyn Widget>>,
    composition_replaced: bool,
    dirty: bool,
    rect: Rect,
    cursor: ui::kurbo::Point,
    error: Option<&'static str>,
    validation_focus: Option<String>,
    /// The field the page moved the keyboard to in its last composition, until the window takes
    /// it ([`Widget::take_focus_request`]): the window restores the focus it had across a rebuild,
    /// and only a request moves it on.
    focus_request: Option<String>,
    password_next_focus: Rc<Cell<bool>>,
    password_touched: [bool; 2],
    password_focused: Option<String>,
    capture: bool,
    last_frame: Option<Scene>,
    outgoing: Option<Scene>,
    slide: Option<(f64, ui::motion::Presence)>,
    pending_slide: Option<f64>,
    layout_metrics: Option<(f32, f32, f32)>,
    profile_breakpoint: f64,
    field_values: [ui::LocalState<String>; 4],
    storage: storage::Storage,
    install_preferences: imager_backend::imager_model::InstallProfile,
    direct: direct::Direct,
    compatibility: compatibility::Compatibility,
    target: target::Target,
    sign_in: migrate::SignIn,
    /// The choices already kept on this computer, so an unchanged page writes nothing.
    remembered: Option<draft::Choices>,
    desktop_review_pending: bool,
    native_full_os_review_pending: bool,
    live: bool,
    demo: bool,
    media: Option<ui::LocalState<bool>>,
    port: Port<InstallerPreview>,
}

impl Default for InstallerPreview {
    fn default() -> Self {
        let mut journey = Journey::new();
        journey.device_name = t("installer-preview-device-default");
        Self {
            capabilities: capabilities::GuidedCapabilities::native(),
            capture: false,
            last_frame: None,
            outgoing: None,
            slide: None,
            pending_slide: None,
            layout_metrics: None,
            profile_breakpoint: 0.0,
            field_values: std::array::from_fn(|_| ui::LocalState::new(String::new())),
            storage: storage::Storage::default(),
            install_preferences: imager_backend::imager_model::InstallProfile::default(),
            direct: direct::Direct::default(),
            compatibility: compatibility::Compatibility::default(),
            target: target::Target::default(),
            sign_in: migrate::SignIn::default(),
            remembered: None,
            desktop_review_pending: false,
            native_full_os_review_pending: false,
            live: false,
            demo: false,
            media: None,
            port: Port::detached(),
            journey,
            machine: Machine::default(),
            linux: cfg!(target_os = "linux"),
            full_os_host: cfg!(windows),
            commands: Rc::default(),
            composed: None,
            composition_replaced: false,
            dirty: true,
            rect: Rect::ZERO,
            cursor: ui::kurbo::Point::ZERO,
            error: None,
            validation_focus: None,
            focus_request: None,
            password_next_focus: Rc::default(),
            password_touched: [false; 2],
            password_focused: None,
        }
    }
}

impl InstallerPreview {
    fn sensitive_handoff_deferred(&self) -> bool {
        self.journey.password.is_empty()
    }

    fn busy(&self) -> bool {
        self.sign_in.busy()
    }

    fn guided_host_available(&self) -> bool {
        !self.live || self.capabilities.prepared_installation
    }

    fn direct_route_enabled(&self) -> bool {
        if !self.live {
            return true;
        }
        super::internal_install_flow::live_install_allowed()
    }

    fn storage_content(&self) -> ui::widget::Column {
        let mut content = page::stack();
        if !self.direct_route_enabled() {
            return content;
        }
        if self.storage.checking() {
            return content.push(Box::new(
                Text::new(&t("installer-preview-storage-checking"))
                    .with_id("installer.storage.checking"),
            ));
        }
        if let Some(Ok(plan)) = &self.storage.plan {
            if let (Some(volume), Some(shrink)) = (&plan.basis.linux_btrfs_volume, &plan.shrink) {
                content = content.push(Box::new(
                    Text::new(&t_args(
                        "installer-storage-btrfs-resize",
                        &[
                            ("before", ui::format::memory(shrink.from_bytes).into()),
                            ("after", ui::format::memory(shrink.to_bytes).into()),
                            ("reserve", ui::format::memory(volume.reserve_bytes).into()),
                        ],
                    ))
                    .with_id("installer.storage.retained-resize"),
                ));
            }

            if let Some((min, max)) = self.storage.bounds {
                content = content
                    .push(Box::new(
                        Field::new(
                            t("installer-preview-storage-size-label"),
                            NumberInput::from_state(&self.storage.size)
                                .with_id("installer.storage.size")
                                .with_range(min as f64, max as f64)
                                .with_precision(0)
                                .with_step(1.0)
                                .with_suffix("GiB")
                                .with_accessible_name(t("installer-preview-storage-size-label")),
                        )
                        .with_label_placement(Some(LabelPlacement::Above)),
                    ))
                    .push(Box::new(
                        Text::new(&t_args(
                            "installer-preview-storage-size-range",
                            &[("min", min.into()), ("max", max.into())],
                        ))
                        .with_role(TextRole::Caption),
                    ));
            }
            content = content.push(Box::new(
                self.button(
                    "installer.storage.details",
                    if self.storage.details {
                        "installer-preview-hide-details"
                    } else {
                        "installer-preview-storage-show-details"
                    },
                    InstallerPreviewEffect::ToggleStorageDetails,
                )
                .role(ButtonRole::Plain),
            ));
            if self.storage.details {
                content = content.push(Box::new(
                    Text::new(&t_args(
                        "installer-preview-storage-capacity",
                        &[(
                            "capacity",
                            ui::format::memory(plan.basis.disk.size_bytes).into(),
                        )],
                    ))
                    .with_id("installer.storage.capacity"),
                ));
                let existing_bytes = plan.shrink.as_ref().map_or_else(
                    || {
                        plan.basis
                            .disk
                            .partitions
                            .iter()
                            .map(|p| (p.last_lba - p.first_lba + 1) * plan.basis.disk.sector_bytes)
                            .sum()
                    },
                    |shrink| shrink.to_bytes,
                );
                content = content
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.existing",
                            t("installer-preview-storage-existing"),
                        )
                        .secondary(ui::format::memory(existing_bytes))
                        .icon(Icon::Monitor),
                    ))
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.tilo",
                            t("installer-preview-storage-tilo"),
                        )
                        .secondary(ui::format::memory(self.storage.selected() << 30))
                        .icon(Icon::MonitorSmartphone),
                    ))
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.temporary",
                            t("installer-preview-storage-temporary"),
                        )
                        .secondary(ui::format::memory(
                            (plan.staging_esp.extent().lbas() + plan.staging_root.extent().lbas())
                                * plan.basis.disk.sector_bytes,
                        ))
                        .icon(Icon::Download),
                    ));
                if plan.shrink.is_some() {
                    content =
                        content.push(Box::new(Text::new(&t("installer-preview-storage-shrink"))));
                }
            }
        } else if let Some(Err(super::staging::plan::PlanError::InsufficientSpace {
            needed_bytes,
            available_bytes,
        })) = &self.storage.plan
        {
            content = content.push(Box::new(Text::new(&t_args(
                "installer-preview-storage-shortfall",
                &[
                    ("needed", ui::format::memory(*needed_bytes).into()),
                    ("available", ui::format::memory(*available_bytes).into()),
                ],
            ))));
        } else {
            content = content.push(Box::new(Text::new(&t(
                "installer-preview-storage-unreadable",
            ))));
        }
        if let Some(facts) = &self.storage.facts {
            for blocker in &facts.blockers {
                content = content.push(Box::new(Text::new(
                    &super::internal_install_flow::blocker_text(
                        *blocker,
                        if self.linux {
                            super::model::HostOs::Linux
                        } else {
                            super::model::HostOs::Windows
                        },
                    ),
                )));
            }
        }
        if self.live {
            if !matches!(self.storage.plan, Some(Ok(_)))
                || self
                    .storage
                    .facts
                    .as_ref()
                    .is_none_or(|facts| !facts.blockers.is_empty())
            {
                content = content.push(Box::new(
                    self.button(
                        "installer.storage.recheck",
                        "installer-storage-check-capacity",
                        InstallerPreviewEffect::CheckStorage,
                    )
                    .role(ButtonRole::Plain),
                ));
            }
        }
        content
    }

    fn effect_page_admitted(&self, id: &str) -> bool {
        if self.full_os_host && self.live {
            if self.direct.active {
                return matches!(id, "read-preview" | "back-in-preview");
            }
            return matches!(
                id,
                "read-preview"
                    | "choose-preview"
                    | "choose-and-continue-preview"
                    | "continue-preview"
                    | "back-in-preview"
                    | "prepare-usb"
            );
        }
        let page = self.journey.page;
        match id {
            "set-preview-password" | "set-preview-confirmation" => page == Page::Password,
            "set-migrate-ticket" => page == Page::Migrate,
            "sign-in-for-migration" | "open-migrations" => page == Page::Migrate && self.live,
            "set-preview-device-name" => page == Page::Devices || page == Page::Machine,
            "set-target-identity-field"
            | "set-target-architecture"
            | "choose-target-hardware-report"
            | "import-target-hardware-report"
            | "clear-target-hardware-report"
            | "recheck-target-hardware" => page == Page::Target,
            "check-storage" => page == Page::Storage && self.live,
            "toggle-storage-details" => page == Page::Storage && self.direct_route_enabled(),
            "set-storage-size" => page == Page::Storage,
            "edit-preview"
            | "prepare-usb"
            | "prepare-direct-installation"
            | "prepare-desktop-installation" => page == Page::Review,
            _ => true,
        }
    }

    fn apply(&mut self, effect: InstallerPreviewEffect) -> Result<(), &'static str> {
        let result = self.apply_effect(effect);
        if result.is_ok() {
            self.persist_choices();
        }
        result
    }

    /// The choices this opening has, with the suggestions a first opening shows left out.
    fn choices(&self) -> draft::Choices {
        draft::Choices::current(
            &self.journey,
            &self.target,
            &t("installer-preview-device-default"),
        )
    }

    /// Keep this opening's choices for the next one. A page that changed nothing
    /// writes nothing.
    fn persist_choices(&mut self) {
        if !self.live || !self.port.store().keeps() {
            return;
        }
        let choices = self.choices();
        if self.remembered.as_ref() == Some(&choices) {
            return;
        }
        draft::save(&choices);
        self.remembered = Some(choices);
    }

    /// The choices an earlier opening kept, where this host can carry them.
    fn restore_choices(&mut self) {
        let choices = if self.port.store().keeps() {
            draft::load()
        } else {
            None
        };
        let mut journey = Journey::new();
        journey.device_name = t("installer-preview-device-default");
        if let Some(choices) = &choices {
            choices.restore(&mut journey, &mut self.target);
        }
        if journey.destination == Some(Destination::Desktop) && !self.linux {
            journey.destination = None;
        }
        if journey.destination != Some(Destination::Desktop) && !self.guided_host_available() {
            journey.destination = None;
        }
        self.journey = journey;
        self.field_values[1].set(self.journey.device_name.clone());
        self.remembered = choices;
    }

    fn apply_effect(&mut self, effect: InstallerPreviewEffect) -> Result<(), &'static str> {
        if let InstallerPreviewEffect::ChooseAndContinuePreview { choice } = effect {
            self.apply(InstallerPreviewEffect::ChoosePreview { choice })?;
            return self.apply(InstallerPreviewEffect::ContinuePreview);
        }
        if self.demo && !demo::admits(&effect) {
            return Err("installer-demo-unavailable");
        }
        if self.full_os_host && self.live && !self.direct.active {
            return self.apply_full_os_host(effect);
        }
        let page = self.journey.page;
        if self.journey.destination == Some(Destination::Desktop)
            && page == Page::Review
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::RestartPreview
                    | InstallerPreviewEffect::EditPreview { .. }
                    | InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::PrepareDesktopInstallation
            )
        {
            return Err("installer-preview-wrong-page");
        }
        let choices_edit = self.journey.editing_choices();
        if choices_edit
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::ChoosePreview { .. }
                    | InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::CheckStorage
                    | InstallerPreviewEffect::ToggleStorageDetails
                    | InstallerPreviewEffect::SetStorageSize { .. }
                    | InstallerPreviewEffect::SetPreviewDeviceName { .. }
                    | InstallerPreviewEffect::SetTargetIdentityField { .. }
                    | InstallerPreviewEffect::SetTargetArchitecture { .. }
                    | InstallerPreviewEffect::ChooseTargetHardwareReport
                    | InstallerPreviewEffect::ImportTargetHardwareReport { .. }
                    | InstallerPreviewEffect::ClearTargetHardwareReport
                    | InstallerPreviewEffect::RecheckTargetHardware
            )
        {
            return Err("installer-preview-wrong-page");
        }
        if self.direct.active {
            match effect {
                InstallerPreviewEffect::ReadPreview => return Ok(()),
                InstallerPreviewEffect::BackInPreview if self.direct.navigation.get() => {
                    self.direct.active = false;
                    self.compatibility = compatibility::Compatibility::default();
                    if self.journey.destination == Some(Destination::Desktop) {
                        self.journey.back()?;
                    }
                    self.composed = None;
                    self.dirty = true;
                    return Ok(());
                }
                _ => return Err("installer-preview-identity-busy"),
            }
        }
        if self.sign_in.busy()
            && matches!(
                effect,
                InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::RestartPreview
                    | InstallerPreviewEffect::SetMigrateTicket { .. }
                    | InstallerPreviewEffect::SignInForMigration
            )
        {
            return Err("installer-migrate-busy");
        }
        let direction = if matches!(effect, InstallerPreviewEffect::BackInPreview) {
            -1.0
        } else {
            1.0
        };
        if !self.effect_page_admitted(effect.id()) {
            return Err("installer-preview-wrong-page");
        }
        if matches!(
            effect,
            InstallerPreviewEffect::ChoosePreview { .. }
                | InstallerPreviewEffect::SetPreviewDeviceName { .. }
                | InstallerPreviewEffect::SetPreviewPassword { .. }
                | InstallerPreviewEffect::SetPreviewConfirmation { .. }
        ) {
            // A changed choice makes a new plan; the one already made no
            // longer says what the person decided.
            self.machine.created = None;
            self.machine.error = None;
        }
        match effect {
            InstallerPreviewEffect::ReadPreview => return Ok(()),
            InstallerPreviewEffect::ChoosePreview { choice } => {
                if page == Page::Destination
                    && !self.guided_host_available()
                    && !(choice == "desktop" && self.linux)
                {
                    return Err("installer-guided-host-unavailable");
                }
                self.journey.choose(&choice, self.linux)?;
                if page == Page::Destination
                    && self.journey.destination != Some(Destination::Desktop)
                    && self.journey.destination != Some(Destination::Another)
                    && self.journey.device_name.is_empty()
                {
                    self.journey.device_name = t("installer-preview-device-default");
                }
                if page == Page::Destination
                    && choice == "another"
                    && self.journey.destination == Some(Destination::Another)
                {
                    // The name entered so far belongs to this computer; the
                    // other computer gets a name of its own.
                    self.journey.device_name.clear();
                    self.field_values[1].set(String::new());
                }
            }
            InstallerPreviewEffect::ContinuePreview => {
                if page == Page::Destination
                    && !self.guided_host_available()
                    && self.journey.destination != Some(Destination::Desktop)
                {
                    return Err("installer-guided-host-unavailable");
                }
                if page == Page::Destination
                    && self.live
                    && self.linux
                    && self.direct_route_enabled()
                    && self.storage.facts.is_none()
                    && self.journey.destination == Some(Destination::Computer)
                {
                    self.compatibility.pending = true;
                    return Ok(());
                }
                if page == Page::Destination
                    && self.journey.destination == Some(Destination::Desktop)
                {
                    self.journey.next()?;
                    self.dirty = true;
                    self.desktop_review_pending = self.live;
                    self.error = None;
                    return Ok(());
                }
                if self.live && page == Page::Review && !choices_edit {
                    let preparation = match self.journey.destination {
                        Some(Destination::Desktop) => {
                            InstallerPreviewEffect::PrepareDesktopInstallation
                        }
                        Some(Destination::Computer) if self.direct_route_enabled() => {
                            InstallerPreviewEffect::PrepareDirectInstallation
                        }
                        Some(Destination::Computer) => {
                            return Err(if self.usb_backend_available() {
                                "installer-review-usb-choice-required"
                            } else {
                                "installer-review-installation-unavailable"
                            });
                        }
                        Some(Destination::Another) => InstallerPreviewEffect::PrepareUsb,
                        None => return Err("installer-preview-choose-first"),
                    };
                    return self.apply(preparation);
                }
                if self.live && page == Page::Ready {
                    return Err("installer-preview-wrong-page");
                }
                if page == Page::Target && !self.target.ready() {
                    self.target.mark_submitted();
                    if self.target.blocking() {
                        return Err("installer-preview-target-blocking");
                    }
                    self.target.recheck()?;
                    return Err("installer-preview-target-incomplete");
                }
                if let Some(error) = self.journey.validation() {
                    return Err(error);
                }
                self.advance_journey()?;
            }
            InstallerPreviewEffect::EditPreview { section } => {
                self.journey.edit(&section)?;
            }
            InstallerPreviewEffect::SetMigrateTicket { ticket } => {
                self.field_values[0].set(ticket.clone());
                self.sign_in.set_ticket(ticket);
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::SignInForMigration => {
                self.sign_in.start()?;
                self.error = None;
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::OpenMigrations => {
                self.sign_in.open_migrations()?;
                self.error = None;
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::CheckStorage if page == Page::Storage && self.live => {
                self.storage.review_capacity();
            }
            InstallerPreviewEffect::ToggleStorageDetails
                if page == Page::Storage && self.direct_route_enabled() =>
            {
                self.storage.details = !self.storage.details;
            }
            InstallerPreviewEffect::SetStorageSize { gib } if page == Page::Storage => {
                self.storage.set_size(gib)?;
            }
            InstallerPreviewEffect::PrepareDesktopInstallation
                if page == Page::Review
                    && self.live
                    && self.journey.destination == Some(Destination::Desktop)
                    && self.live =>
            {
                self.desktop_review_pending = true;
            }
            InstallerPreviewEffect::PrepareDirectInstallation
                if page == Page::Review && self.live =>
            {
                self.prepare_direct()?;
            }
            InstallerPreviewEffect::PrepareUsb
                if page == Page::Review
                    && self.live
                    && matches!(
                        self.journey.destination,
                        Some(Destination::Computer | Destination::Another)
                    ) =>
            {
                if self.journey.destination == Some(Destination::Another) && !self.target.ready() {
                    return Err("installer-preview-target-incomplete");
                }
                if self.journey.destination == Some(Destination::Another)
                    && self.target.architecture() != std::env::consts::ARCH
                {
                    match self.target.payload_available() {
                        Ok(true) => {}
                        Ok(false) => return Err("installer-preview-target-payload-missing"),
                        Err(error) => return Err(error),
                    }
                }
                if self.journey.destination == Some(Destination::Another)
                    && self.journey.plan.delivery != machine::Delivery::Usb
                {
                    if !self.make_plan()? {
                        return Ok(());
                    }
                    self.journey.next()?;
                    self.error = None;
                    self.dirty = true;
                    return Ok(());
                }
                #[cfg(target_os = "linux")]
                {
                    if !self.usb_backend_available() {
                        return Err("installer-preview-storage-unavailable");
                    }
                    if self.journey.destination == Some(Destination::Another)
                        && !self.make_plan()?
                    {
                        return Ok(());
                    }
                    self.open_media_writer()?;
                    return Ok(());
                }
                #[cfg(not(target_os = "linux"))]
                return Err("installer-preview-storage-unavailable");
            }
            InstallerPreviewEffect::BackInPreview => {
                self.journey.back()?;
            }
            InstallerPreviewEffect::RestartPreview => {
                let capture = self.capture;
                let linux = self.linux;
                let live = self.live;
                let demo = self.demo;
                let media = self.media.clone();
                let port = self.port.clone();
                let last_frame = self.last_frame.take();
                *self = Self::default();
                self.last_frame = last_frame;
                self.pending_slide = (page != Page::Destination).then_some(-1.0);
                self.capture = capture;
                self.linux = linux;
                self.live = live;
                self.demo = demo;
                self.media = media;
                self.port = port;
                return Ok(());
            }
            InstallerPreviewEffect::SetTargetIdentityField { field, value }
                if page == Page::Target =>
            {
                self.target.set_field(&field, value)?;
                return Ok(());
            }
            InstallerPreviewEffect::SetTargetArchitecture { architecture }
                if page == Page::Target =>
            {
                self.target.set_architecture(architecture)?;
                return Ok(());
            }
            InstallerPreviewEffect::ChooseTargetHardwareReport if page == Page::Target => {
                self.target.choose_report()?;
                return Ok(());
            }
            InstallerPreviewEffect::ImportTargetHardwareReport { source_file }
                if page == Page::Target =>
            {
                self.target.import_file(&source_file)?;
                return Ok(());
            }
            InstallerPreviewEffect::ClearTargetHardwareReport if page == Page::Target => {
                self.target.clear_report();
                return Ok(());
            }
            InstallerPreviewEffect::RecheckTargetHardware if page == Page::Target => {
                self.target.recheck()?;
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewDeviceName { value }
                if page == Page::Devices || page == Page::Machine =>
            {
                if value.len() > 64 || value.chars().any(char::is_control) {
                    return Err("installer-preview-device-label-invalid");
                }
                self.field_values[1].set(value.clone());
                self.journey.device_name = value;
                self.plan_names_the_computer();
                self.dirty |= page != Page::Devices;
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewPassword { value } if page == Page::Password => {
                let enabled = self.password_continue_enabled();
                self.field_values[2].set(value.clone());
                self.journey.password = value;
                self.clear_password_validation();
                self.dirty |= enabled != self.password_continue_enabled();
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewConfirmation { value } if page == Page::Password => {
                let enabled = self.password_continue_enabled();
                self.field_values[3].set(value.clone());
                self.journey.confirmation = value;
                self.password_touched[1] = true;
                self.clear_password_validation();
                self.dirty |= enabled != self.password_continue_enabled();
                return Ok(());
            }
            _ => return Err("installer-preview-wrong-page"),
        }
        if self.journey.page == Page::Ready {
            self.field_values[2].set(String::new());
            self.field_values[3].set(String::new());
        }
        if self.journey.page != page {
            self.pending_slide = Some(direction);
            if self.journey.page == Page::Storage
                && self.live
                && self.journey.destination == Some(Destination::Computer)
                && self.storage.facts.is_none()
            {
                self.storage.check();
            }
        }
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    /// Hand the reviewed names to the USB media writer and show it. Choosing
    /// and confirming a drive happens there.
    fn open_media_writer(&mut self) -> Result<(), &'static str> {
        let Some(media) = self.media.as_ref().filter(|_| self.live) else {
            return Err("installer-preview-storage-unavailable");
        };
        self.port
            .store()
            .edit(
                &Imager::paths()
                    .profile
                    .device_label
                    .replace(self.journey.device_name.clone()),
            )
            .map_err(|_| "installer-preview-storage-unavailable")?;
        media.set(true);
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    /// The plan carries the computer's name only when one is named: an empty
    /// name leaves it for the computer to answer when it installs.
    fn plan_names_the_computer(&mut self) {
        if self.journey.destination == Some(Destination::Another) {
            self.journey.plan.name_open = self.journey.device_name.trim().is_empty();
        }
    }

    fn drain(&mut self) -> bool {
        let password_focus_changed = self.track_password_focus();
        let compatibility_changed = self.drain_compatibility();
        let target_changed = self.target.poll();
        let desktop_review_changed = std::mem::take(&mut self.desktop_review_pending);
        let native_full_os_review_changed = std::mem::take(&mut self.native_full_os_review_pending);
        if native_full_os_review_changed && let Err(error) = self.prepare_native_full_os() {
            self.error = Some(error);
            self.dirty = true;
        }
        if desktop_review_changed && let Err(error) = self.prepare_desktop() {
            self.error = Some(error);
            self.dirty = true;
        }
        let plan_changed = if self.live { self.poll_plan() } else { false };
        self.dirty |= plan_changed;
        let storage_changed = self.storage.poll();
        let sign_in_changed = self.sign_in.poll();
        self.dirty |= target_changed || storage_changed || sign_in_changed;
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = compatibility_changed
            || target_changed
            || native_full_os_review_changed
            || desktop_review_changed
            || plan_changed
            || storage_changed
            || sign_in_changed
            || !commands.is_empty();
        if changed {
            self.persist_choices();
        }
        for command in commands {
            let submitted = matches!(
                command,
                InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::ChooseAndContinuePreview { .. }
            );
            if let Err(error) = self.apply(command) {
                self.show_refusal(error, submitted);
            }
        }
        let focus_confirmation = self.password_next_focus.replace(false)
            && self.journey.page == Page::Password
            && !self.busy();
        if focus_confirmation {
            self.password_touched[0] = true;
            self.validation_focus = Some("installer.confirmation".into());
            self.dirty = true;
        }
        changed || focus_confirmation || password_focus_changed
    }

    fn button(&self, id: &str, label: &str, effect: InstallerPreviewEffect) -> Button {
        let commands = self.commands.clone();
        let mut enabled = !self.busy();
        if self.journey.page == Page::Password
            && matches!(effect, InstallerPreviewEffect::ContinuePreview)
        {
            enabled &= self.password_continue_enabled();
        }
        Button::new(id, &t(label))
            .with_enabled(enabled)
            .on_click(move || {
                if matches!(effect, InstallerPreviewEffect::ContinuePreview) {
                    queue_continue(&commands);
                } else {
                    commands.borrow_mut().push(effect.clone());
                }
            })
    }

    fn review_row(&self, section: &str, title: String, secondary: String) -> ListRow {
        self.review_selection_row(section, section, title, secondary)
    }

    fn review_device_row(&self) -> ListRow {
        self.review_row(
            "devices",
            self.journey.device_name.clone(),
            t("installer-review-device-body"),
        )
        .icon(Icon::Monitor)
    }

    fn review_selection_row(
        &self,
        id: &str,
        section: &str,
        title: String,
        secondary: String,
    ) -> ListRow {
        let commands = self.commands.clone();
        let section = section.to_string();
        ListRow::new(format!("installer.review.{id}"), title)
            .secondary(secondary)
            .two_line_secondary()
            .disclosure()
            .on_activate(move || {
                commands
                    .borrow_mut()
                    .push(InstallerPreviewEffect::EditPreview {
                        section: section.clone(),
                    })
            })
    }

    fn usb_backend_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.live && self.media.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn has_storage_decision(&self) -> bool {
        match self.journey.destination {
            Some(Destination::Another) => true,
            Some(Destination::Computer) => self.direct_route_enabled(),
            _ => false,
        }
    }

    fn advance_journey(&mut self) -> Result<(), &'static str> {
        self.journey.next()?;
        if !self.live && self.journey.page == Page::Target {
            self.journey.next()?;
            if self.journey.history.last() == Some(&Page::Target) {
                self.journey.history.pop();
            }
        }
        if self.journey.page == Page::Devices
            && self.journey.destination == Some(Destination::Another)
            && self.journey.device_name.trim().is_empty()
        {
            self.journey.device_name = self.target.suggested_name();
            self.field_values[1].set(self.journey.device_name.clone());
        }
        self.plan_names_the_computer();
        if self.live && !self.has_storage_decision() {
            if self.journey.page == Page::Storage {
                self.journey.next()?;
            }
            self.journey.history.retain(|page| *page != Page::Storage);
        }
        Ok(())
    }

    fn choice(&self, key: &str, selected: bool) -> Card {
        self.option_card(key, selected)
    }

    /// `key` as a choice of this page that the person confirms and continues
    /// from.
    fn option_card(&self, key: &str, selected: bool) -> Card {
        let title = t(&format!("installer-preview-{key}-title"));
        let description = if key == "computer" && self.live && self.full_os_host {
            t(if self.direct_route_enabled() {
                "installer-native-full-os-body"
            } else {
                "imager-setup-internal-not-enabled"
            })
        } else if key == "computer" && !self.direct_route_enabled() {
            t("installer-preview-computer-usb-body")
        } else {
            t(&format!("installer-preview-{key}-body"))
        };
        self.labelled_option(key, title, description, selected)
    }

    /// One option with words of its own: a value the person typed, or a name
    /// the catalogue cannot spell ahead of time.
    fn labelled_option(
        &self,
        key: &str,
        title: String,
        description: String,
        selected: bool,
    ) -> Card {
        let choice = key.strip_prefix("purpose-").unwrap_or(key).to_string();
        let icon = match key {
            "computer" | "purpose-computer" | "purpose-server" | "os" | "server" => Icon::Monitor,
            "another" => Icon::Usb,
            "desktop" => Icon::MonitorSmartphone,
            _ => Icon::Shield,
        };
        let copy = page::related()
            .push(Box::new(Text::new(&title).with_role(TextRole::Title)))
            .push(Box::new(Text::new(&description)));
        let contents = Row::new()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(
                IconWidget::new(icon)
                    .sized(ui::widget::IconSize::Large)
                    .accent(),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(copy)),
            ))
            .push(Box::new(IconWidget::new(Icon::ChevronRight)));
        let card = Card::new(Box::new(contents))
            .with_accessible_name(format!("{title}. {description}"))
            .selected(selected);
        let commands = self.commands.clone();
        card.on_activate(format!("installer.choice.{key}"), move || {
            commands
                .borrow_mut()
                .push(InstallerPreviewEffect::ChooseAndContinuePreview {
                    choice: choice.clone(),
                });
        })
    }

    fn track_password_focus(&mut self) -> bool {
        if self.journey.page != Page::Password {
            self.password_focused = None;
            self.password_touched = [false; 2];
            return false;
        }
        let focused = self
            .composed
            .as_ref()
            .and_then(|root| page::focused_id(root.as_ref()));
        if focused == self.password_focused {
            return false;
        }
        if let Some(previous) = self.password_focused.take() {
            match previous.as_str() {
                "installer.password" => self.password_touched[0] = true,
                "installer.confirmation" => self.password_touched[1] = true,
                _ => {}
            }
            self.dirty = true;
        }
        self.password_focused = focused;
        self.dirty
    }

    fn password_input_state(root: &dyn Widget) -> Vec<(String, String)> {
        let mut states = Vec::new();
        if matches!(
            root.test_id(),
            Some("installer.password" | "installer.confirmation")
        ) && let Some(input) = root
            .as_any()
            .and_then(|widget| widget.downcast_ref::<TextInput>())
            && let Some(state) = ui::widget::undo_points::UndoPoints::get_state(input)
        {
            states.push((root.test_id().unwrap().to_string(), state));
        }
        for child in root.children() {
            states.extend(Self::password_input_state(child));
        }
        states
    }

    fn restore_password_input_state(root: &mut dyn Widget, states: &[(String, String)]) {
        if let Some((_, state)) = states
            .iter()
            .find(|(id, _)| root.test_id() == Some(id.as_str()))
            && let Some(input) = root
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<TextInput>())
        {
            ui::widget::undo_points::UndoPoints::set_state(input, state);
        }
        for child in root.children_mut() {
            Self::restore_password_input_state(child, states);
        }
    }

    fn clear_password_validation(&mut self) {
        if Self::password_error_field(self.error).is_some() {
            self.error = None;
            self.validation_focus = None;
        }
        self.dirty = true;
    }

    fn show_refusal(&mut self, error: &'static str, submitted: bool) {
        self.error = Some(error);
        self.dirty = true;
        if submitted && let Some((field, _)) = self.password_validation() {
            self.password_touched[usize::from(field == "confirmation")] = true;
            self.validation_focus = Some(format!("installer.{field}"));
        }
        if submitted && self.journey.page == Page::Target {
            self.target.mark_submitted();
            if let Some(focus) = self.target.first_invalid_focus() {
                self.validation_focus = Some(focus);
            }
        }
    }

    fn password_continue_enabled(&self) -> bool {
        self.journey.validation().is_none()
    }

    fn password_validation(&self) -> Option<(&'static str, &'static str)> {
        if self.journey.page != Page::Password {
            return None;
        }
        self.password_field_validation("password")
            .map(|error| ("password", error))
            .or_else(|| {
                self.password_field_validation("confirmation")
                    .map(|error| ("confirmation", error))
            })
    }

    fn password_field_validation(&self, key: &str) -> Option<&'static str> {
        if self.journey.page != Page::Password {
            return None;
        }
        if let Some((field, error)) = Self::password_error_field(self.error)
            && field == key
        {
            return Some(error);
        }
        match key {
            "password" if self.password_touched[0] => self
                .journey
                .validation()
                .filter(|error| *error != "installer-preview-password-mismatch"),
            "confirmation"
                if self.password_touched[1]
                    && self.journey.password != self.journey.confirmation =>
            {
                Some("installer-preview-password-mismatch")
            }
            _ => None,
        }
    }

    fn password_error_field(error: Option<&'static str>) -> Option<(&'static str, &'static str)> {
        match error {
            Some(
                error @ ("installer-preview-password-short"
                | "installer-preview-password-spaces"
                | "installer-preview-password-common"
                | "installer-preview-password-numeric"
                | "installer-preview-password-repetitive"
                | "installer-preview-password-invalid"),
            ) => Some(("password", error)),
            Some("installer-preview-password-mismatch") => {
                Some(("confirmation", "installer-preview-password-mismatch"))
            }
            _ => None,
        }
    }

    fn field(&self, key: &str, value: &str, kind: TextInputKind) -> Field {
        let next_focus = self.password_next_focus.clone();
        let focus_confirmation = key == "password";
        let submit_enabled = !self.busy();
        let index = match key {
            "ticket" => 0,
            "device-name" => 1,
            "password" => 2,
            _ => 3,
        };
        self.field_values[index].set(value.to_string());
        let set: fn(String) -> InstallerPreviewEffect = match key {
            "ticket" => |ticket| InstallerPreviewEffect::SetMigrateTicket { ticket },
            "device-name" => |value| InstallerPreviewEffect::SetPreviewDeviceName { value },
            "password" => |value| InstallerPreviewEffect::SetPreviewPassword { value },
            _ => |value| InstallerPreviewEffect::SetPreviewConfirmation { value },
        };
        let input = changes(
            &self.commands,
            TextInput::from_state(&self.field_values[index])
                .with_id(&format!("installer.{key}"))
                .with_kind(kind)
                .with_secret(kind == TextInputKind::Password),
            set,
        );
        let input = if key == "ticket" {
            let commands = self.commands.clone();
            input.on_submit(move |_| {
                if submit_enabled {
                    commands
                        .borrow_mut()
                        .push(InstallerPreviewEffect::SignInForMigration);
                }
            })
        } else if focus_confirmation {
            input.on_submit(move |_| {
                if submit_enabled {
                    next_focus.set(true);
                }
            })
        } else if submit_enabled && self.password_continue_enabled() {
            submits(&self.commands, input)
        } else if submit_enabled {
            let commands = self.commands.clone();
            input.on_submit(move |_| {
                commands
                    .borrow_mut()
                    .push(InstallerPreviewEffect::ContinuePreview);
            })
        } else {
            input.with_enter_submits(true)
        };
        let mut widget = Field::new(t(&format!("installer-preview-{key}-label")), input)
            .with_label_placement(Some(LabelPlacement::Above));
        if let Some(error) = self.password_field_validation(key) {
            widget = widget.with_error_live(t(error), ui::a11y::Politeness::Assertive);
        } else if key == "password" {
            widget = widget.with_help(t("installer-preview-password-hint"));
        }
        widget
    }
}

impl InstallerPreview {
    fn answer_preview(&mut self, effect: InstallerPreviewEffect) -> EffectAnswer {
        self.drain();
        let submitted = matches!(
            effect,
            InstallerPreviewEffect::ContinuePreview
                | InstallerPreviewEffect::ChooseAndContinuePreview { .. }
        );
        if let Err(error) = self.apply(effect) {
            self.show_refusal(error, submitted);
            return Err(t(error));
        }
        let mut status = self.journey.status();
        status["guided_capabilities"] = serde_json::json!({
            "prepared_installation": self.guided_host_available(),
        });
        status["machine"] = self.machine_status();
        status["target"] = self.target.projection();
        status["migrate"] = self.sign_in.projection();
        status["direct_install"] = serde_json::json!({
            "active": self.direct.active,
            "can_return": self.direct.active && self.direct.navigation.get(),
        });
        let direct_available = self.direct_available();
        let plan_digest = self
            .storage
            .plan
            .as_ref()
            .and_then(|plan| plan.as_ref().ok())
            .map(|plan| plan.digest())
            .transpose()
            .map_err(|error| error.to_string())?;
        status["storage"] = serde_json::json!({
            "checking": self.storage.checking(),
            "size_gib": self.storage.selected(),
            "bounds_gib": self.storage.bounds,
            "plan_digest": plan_digest,
            "direct_install_available": direct_available,
        });
        Ok(Effected::value(status))
    }
}

impl crate::hosting::Window for InstallerPreview {
    type Effect = InstallerPreviewEffect;

    fn port(&self) -> &Port<Self> {
        &self.port
    }

    fn effect(
        &mut self,
        effect: InstallerPreviewEffect,
        _cx: &mut EffectCx<'_, Self>,
    ) -> EffectAnswer {
        self.answer_preview(effect)
    }

    fn state(&self) -> serde_json::Value {
        self.journey.status()
    }

    fn present(&mut self, state: &Imager) {
        self.install_preferences = state.profile.clone();
        self.install_preferences.user.password.clear();
        self.install_preferences.encryption_passphrase.clear();
        if self.install_preferences.user.username
            == imager_backend::imager_model::UserAccount::default().username
        {
            if let Ok(login) = std::env::var("USER") {
                let mut candidate = self.install_preferences.clone();
                candidate.user.username = login;
                if candidate.validate_media_profile().is_ok() {
                    self.install_preferences.user.username = candidate.user.username;
                }
            }
        }
    }
    fn statuses(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }
}

impl InstallerPreview {
    /// Take up how the window is opened: the demo stages its walk; a live preview reaches this
    /// computer and the person's planned computers; a shot stages its scenario.
    fn attach(&mut self, opened: Opened<'_>) {
        match opened {
            Opened::Live { demo: true, .. } => {
                self.stage(Some("installer-destination"));
                self.demo = true;
            }
            Opened::Live { demo: false, .. } => {
                self.live = true;
                self.machine.listing = Some(machine::waiting());
                self.restore_choices();
                self.dirty = true;
            }
            Opened::Shot(scenario) => self.stage(scenario),
        }
    }
    fn stage(&mut self, scenario: Option<&str>) {
        let Some(scenario) = scenario else {
            return;
        };
        if !SCENARIOS.contains(&scenario) {
            return;
        }
        self.linux = true;
        self.full_os_host = false;
        self.capture = true;
        self.live = false;
        self.machine.making = None;
        self.machine.listing = None;
        self.storage.stage();
        self.journey = Journey::new();
        self.journey.device_name = t("installer-preview-device-default");
        self.journey.destination = Some(Destination::Computer);
        self.journey.page = scenarios::page(&mut self.journey, scenario);
        if scenario == "installer-plan-ready" {
            self.journey.device_name = "cupboard".into();
            self.machine.created = Some(machine::example());
        }
        if scenario == "installer-plan-waiting" {
            self.machine.waiting = machine::examples();
        }
        self.dirty = true;
    }
    fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        for declaration in InstallerPreviewEffect::declarations() {
            if self.demo && !demo::admits_id(declaration.id) {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(
                        declaration.id,
                        &[],
                        InstallerPreviewEffect::kind(),
                    ),
                    "demo",
                    t("installer-demo-unavailable"),
                ));
                continue;
            }
            if !self.effect_page_admitted(declaration.id) {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(
                        declaration.id,
                        &[],
                        InstallerPreviewEffect::kind(),
                    ),
                    "wrong_page",
                    t("installer-preview-wrong-page"),
                ));
            }
        }
        if self.direct.active && !self.direct.navigation.get() {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::BACK_IN_PREVIEW,
                "installation_busy",
                t("installer-preview-identity-busy"),
            ));
        }
        if self.full_os_host && self.live {
            return statuses;
        }
        if !self.guided_host_available()
            && (self.journey.page == Page::Target
                || (self.journey.page == Page::Destination
                    && self.journey.destination != Some(Destination::Desktop)))
        {
            let actions: &[&str] = if self.journey.page == Page::Target {
                &[
                    "set-target-identity-field",
                    "set-target-architecture",
                    "choose-target-hardware-report",
                    "import-target-hardware-report",
                    "clear-target-hardware-report",
                    "recheck-target-hardware",
                    "continue-preview",
                ]
            } else {
                &["continue-preview"]
            };
            for action in actions {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(
                        action,
                        &[],
                        InstallerPreviewEffect::kind(),
                    ),
                    "guided_host_unavailable",
                    t("installer-guided-host-unavailable"),
                ));
            }
        }
        if !self.direct_available() {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::PREPARE_DIRECT_INSTALLATION,
                "installation_unavailable",
                t("installer-preview-storage-unavailable"),
            ));
        }
        if self.sign_in.busy() {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::SIGN_IN_FOR_MIGRATION,
                "signing_in",
                t("installer-migrate-busy"),
            ));
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::CONTINUE_PREVIEW,
                "signing_in",
                t("installer-migrate-busy"),
            ));
        }
        if self.journey.history.is_empty() && !self.journey.cancels_edit() {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::BACK_IN_PREVIEW,
                "at_start",
                t("installer-preview-at-start"),
            ));
        }
        if self.journey.page == Page::Ready {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::CONTINUE_PREVIEW,
                "finished",
                t("installer-preview-finished"),
            ));
        }
        statuses
    }
}

impl Widget for InstallerPreview {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.drain();
        if let Some(child) = &mut self.composed {
            child.sync(state, path);
        }
    }
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.drain();
        self.composed
            .as_mut()
            .is_some_and(|child| child.update(state, path))
            || changed
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = (
            theme.tokens.metrics.readable_width,
            theme.tokens.metrics.content_min_width,
            theme.text(TextRole::LargeTitle).size,
        );
        if self.layout_metrics != Some(metrics) {
            self.dirty = true;
            self.last_frame = None;
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        }
        self.layout_metrics = Some(metrics);
        self.profile_breakpoint = f64::from(theme.tokens.metrics.content_min_width);
        let showing_direct = self.direct.active;
        if !showing_direct && (self.dirty || self.composed.is_none()) {
            let moved = self.validation_focus.take();
            let focus = moved.clone().or_else(|| {
                self.composed
                    .as_ref()
                    .and_then(|root| page::focused_id(root.as_ref()))
            });
            let password_state = if self.journey.page == Page::Password {
                self.composed
                    .as_ref()
                    .map(|root| Self::password_input_state(root.as_ref()))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            self.composed = Some(self.composition(theme));
            if let Some(root) = &mut self.composed {
                Self::restore_password_input_state(root.as_mut(), &password_state);
            }
            if let (Some(focus), Some(root)) = (focus, &mut self.composed) {
                page::refocus(root.as_mut(), &focus);
            }
            if moved.is_some() {
                self.focus_request = moved;
            }
            self.dirty = false;
        }
        let child = if let Some(composed) = &mut self.composed {
            composed.build_layout(taffy, theme, nodes)
        } else {
            use ui::widget::TaffyTreeExt as _;
            taffy.leaf_node(Style::default())
        };
        if let Ok(style) = taffy.style(child) {
            let mut style = style.clone();
            style.size.height = Dimension::percent(1.0);
            style.max_size.height = Dimension::percent(1.0);
            style.min_size.height = Dimension::length(0.0);
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(child, style);
        }
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    justify_content: Some(taffy::JustifyContent::Center),
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let previous_rect = self.rect;
        let profile_compact = self.rect.width() < self.profile_breakpoint;
        if let Ok(layout) = taffy.layout(node) {
            self.rect = Rect::from_origin_size(
                (
                    origin.x + f64::from(layout.location.x),
                    origin.y + f64::from(layout.location.y),
                ),
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
        }
        if profile_compact != (self.rect.width() < self.profile_breakpoint) {
            self.dirty = true;
        }
        if self.rect != previous_rect {
            self.last_frame = None;
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        }
        if let Some(child) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&node) = children.first()
        {
            child.apply_layout(taffy, node, self.rect.origin());
        }
    }
    fn draw(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let now = ui::motion::frame_now();
        if theme.reduce_motion || self.capture {
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        } else if let Some(direction) = self.pending_slide.take() {
            self.outgoing = self.last_frame.take();
            self.slide = self.outgoing.as_ref().map(|_| {
                let mut arriving = ui::motion::Presence::closed();
                arriving.show(
                    true,
                    ui::motion::Drill::SPEC,
                    theme.motion(),
                    ui::tokens::MotionSource::Pointer,
                    now,
                );
                (direction, arriving)
            });
        }
        if self
            .slide
            .as_ref()
            .is_some_and(|(_, slide)| !slide.is_moving(now) && !slide.is_held())
        {
            self.slide = None;
            self.outgoing = None;
        }
        let mut current = Scene::new();
        if let Some(child) = &mut self.composed {
            child.draw_child(&mut current, Affine::IDENTITY, text, theme, &env);
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        if let Some((direction, slide)) = &self.slide {
            let arrived = slide.progress(now);
            let width = self.rect.width();
            let motion = theme.motion();
            let incoming =
                ui::motion::Drill::screen_pose(direction * (1.0 - arrived), width, motion);
            let leaving = ui::motion::Drill::screen_pose(-direction * arrived, width, motion);
            if let Some(outgoing) = &self.outgoing {
                scene.append(outgoing, Some(leaving.applied_to(transform)));
            }
            scene.append(&current, Some(incoming.applied_to(transform)));
        } else {
            scene.append(&current, Some(transform));
        }
        scene.pop_layer();
        self.last_frame = Some(current);
    }
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = cx.logical(position);
        }
        if (self.pending_slide.is_some() || self.slide.is_some())
            && matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::Touch(_)
            )
        {
            return;
        }
        if let Some(child) = &mut self.composed
            && let Ok(children) = cx.taffy.children(cx.node_id)
            && let Some(&node) = children.first()
        {
            child.handle_event(event, &mut cx.reborrow(cx.path, node));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .iter()
            .map(|child| child.as_ref() as &dyn Widget)
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .iter_mut()
            .map(|child| child.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn take_focus_request(&mut self) -> Option<String> {
        let own = self.focus_request.take();
        let asked = self
            .composed
            .as_mut()
            .and_then(|child| child.take_focus_request());
        own.or(asked)
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        ui::widget::Widget::mark_children_drawn(self);
    }
    fn name(&self) -> &'static str {
        "InstallerPreview"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_animation_frame(&self) -> bool {
        self.dirty
            || self.compatibility.pending
            || self.native_full_os_review_pending
            || self.pending_slide.is_some()
            || self.slide.is_some()
            || self.storage.checking()
            || self.sign_in.busy()
    }
    fn needs_idle_sync(&self) -> bool {
        self.sign_in.busy()
    }
    fn take_structure_dirty(&mut self) -> bool {
        let child = self
            .composed
            .as_mut()
            .is_some_and(|child| child.take_structure_dirty());
        let replaced = std::mem::take(&mut self.composition_replaced);
        self.dirty || replaced || child
    }
}

#[cfg(all(test, unix))]
mod desktop_model_tests;
#[cfg(all(test, unix))]
mod purpose_tests;
#[cfg(test)]
mod structure_tests;
