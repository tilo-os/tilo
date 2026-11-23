//! The desktop's own sections: how the shell is navigated, which app opens
//! each kind of file, the clock, the displays' placement and the display
//! change being tried. The session keeps them like every other section, so
//! an agent reads and changes them whether or not Settings is open.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
};
use settings_store::person_displays::{PlacedDisplay, Placements};
use ui::tilo_resource;

/// The shell's desktop navigation preferences, as the person's settings
/// hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControls {
    /// Add Super+Ctrl+h/j/k/l aliases for moving between windows and strips,
    /// beside the arrow keys.
    #[serde(default)]
    pub vim_mode: bool,
}

impl settings_model::SettingsSchema for DesktopControls {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::desktop_controls::DESKTOP_CONTROLS_SECTION);
}

#[ui::tilo_resource::resource(
    kind = "settings.desktop-controls",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl DesktopControls {}

/// The shell's desktop preferences this device holds: a small laptop panel
/// and a large shell display want different answers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControlsDevice {
    /// Hide the top bar until the pointer reaches for it; shown in the
    /// Appearance pane.
    #[serde(default)]
    pub hide_top_bar: bool,
}

impl settings_model::SettingsSchema for DesktopControlsDevice {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared(
        settings_store::desktop_controls::DESKTOP_CONTROLS_DEVICE_SECTION,
    );
}

#[ui::tilo_resource::resource(kind = "settings.desktop-controls-device")]
impl DesktopControlsDevice {}

/// The person's default apps, as their settings hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub struct DefaultApps {
    /// The app each kind of file or link opens with, by the kind's id
    /// (`ext:pdf`, `mime:text/html`, `scheme:https`).
    #[serde(default)]
    #[model(readonly)]
    pub handlers: BTreeMap<String, String>,
}

impl settings_model::SettingsSchema for DefaultApps {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(desktop_policy::PERSONAL_DEFAULTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.default-apps")]
impl DefaultApps {
    /// Open one kind of file or link with an app. Only the chosen kind
    /// changes: the handlers kept for every other kind, including kinds a
    /// newer version knows, stay.
    #[action(id = "set-default-app-handler")]
    pub fn open_with(
        &mut self,
        #[param("The kind of file or link: `ext:pdf`, `mime:text/html`, `scheme:https`")]
        kind: String,
        #[param("The app's id")] app: String,
    ) -> Result<(), tilo_resource::Refusal> {
        let kind = desktop_policy::ResourceKind::parse(&kind).ok_or_else(|| {
            tilo_resource::Refusal::new(format!("`{kind}` is not a kind of file or link"))
        })?;
        let app = app.trim();
        if app.is_empty() {
            return Err(tilo_resource::Refusal::new("an app id is needed"));
        }
        self.handlers
            .insert(kind.as_str().to_owned(), app.to_owned());
        Ok(())
    }
}

/// This device's clock settings: whether its time zone follows its
/// location. The session's zone follower reads the same leaf
/// ([`settings_store::clock::AUTOMATIC_ZONE`]).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct ClockPrefs {
    /// The zone follows the device's location.
    #[serde(default)]
    pub automatic_zone: bool,
}

impl settings_model::SettingsSchema for ClockPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::clock::SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.clock")]
impl ClockPrefs {}

/// How the person's displays are arranged, wherever they connect them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct SharedDisplayPrefs {
    #[model(readonly)]
    pub placement: Placements,
}

impl settings_model::SettingsSchema for SharedDisplayPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::person_displays::PLACEMENTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.display-placement")]
impl SharedDisplayPrefs {
    /// Arrange the person's displays: each display entry given takes the
    /// place of the stored one where it differs, at a revision above every
    /// stored one, so edits made elsewhere to other displays are kept.
    #[action(id = "arrange-displays")]
    pub fn arrange_displays(
        &mut self,
        #[param("The placement, as its display entries")] placement: Placements,
    ) -> Result<(), tilo_resource::Refusal> {
        self.placement = self.placement.with_changes(placement.displays);
        Ok(())
    }

    /// Move one of the person's screens to a new top-left in the layout: `screen` is the
    /// `device/output` key the layout names it by.
    #[action(id = "move-display")]
    pub fn move_display(
        &mut self,
        #[param("The screen, as `device/output`")] screen: String,
        #[param("Its new left, in logical pixels")] x: i32,
        #[param("Its new top, in logical pixels")] y: i32,
    ) -> Result<(), tilo_resource::Refusal> {
        let (device, output) = screen
            .split_once('/')
            .filter(|(device, output)| !device.is_empty() && !output.is_empty())
            .ok_or_else(|| tilo_resource::Refusal::new("a screen is named `device/output`"))?;
        let Some(entry) = self.placement.entry(device, output) else {
            return Err(tilo_resource::Refusal::new("the layout has no such screen"));
        };
        let moved = PlacedDisplay {
            x,
            y,
            placed: true,
            ..entry.clone()
        };
        self.placement = self.placement.with_changes([moved]);
        Ok(())
    }
}

/// Whether a display nearby offers itself as this device's input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct AutomaticDisplayPrefs {
    /// A device administrator's choice: changed only through its verb.
    #[model(readonly)]
    pub automatic_display_input: bool,
}

impl Default for AutomaticDisplayPrefs {
    fn default() -> Self {
        Self {
            automatic_display_input: settings_store::continuity_privacy::ON_UNLESS_SET,
        }
    }
}

impl settings_model::SettingsSchema for AutomaticDisplayPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(kind = "settings.automatic-display")]
impl AutomaticDisplayPrefs {
    /// Opt this device into automatic grants to its enrolled online input
    /// devices, or out. The separate Continuity master and Display/input
    /// switches must also be on.
    #[action(id = "set-automatic-display-input")]
    pub fn set_automatic_display_input(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.automatic_display_input = on;
        Ok(())
    }
}

/// The display change being tried on this device, as the session writes it:
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct PendingDisplayChange {
    /// The change's id; none is pending while it is 0.
    #[model(readonly)]
    pub id: u64,
    /// How it stands: asked for, awaiting an answer, kept, reverted or
    /// refused.
    #[model(readonly)]
    pub state: DisplayTransactionState,
    /// The monitors the change was arranged for.
    #[model(readonly)]
    pub set: DisplaySet,
    /// The configuration before it, which reverting restores.
    #[model(readonly)]
    pub previous: DisplayConfig,
    /// The configuration being tried.
    #[model(readonly)]
    pub candidate: DisplayConfig,
    /// When it reverts unless kept, in Unix milliseconds.
    #[model(readonly)]
    pub deadline_unix_ms: Option<u64>,
    /// The session's diagnostic, in its own terms.
    #[model(readonly)]
    pub message: Option<String>,
}

impl settings_model::SettingsSchema for PendingDisplayChange {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::hardware::DISPLAY_TRANSACTION_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.display-change")]
impl PendingDisplayChange {
    /// Try a layout now: the displays take it while the person decides, and it stays only when
    /// kept before its deadline. The monitors attached now are the ones it is arranged for, so a
    /// change staged for a dock does not apply without it.
    #[action(id = "apply-display-change")]
    pub fn apply_display_change(
        &mut self,
        #[param("The output configuration to try")] candidate: DisplayConfig,
    ) -> Result<(), tilo_resource::Refusal> {
        let observed = settings_store::hardware::DisplayHardwareSnapshot::load().ok_or_else(|| {
            tilo_resource::Refusal::new("no display is publishing what it can show")
        })?;
        let descriptors: Vec<settings_store::hardware::OutputDescriptor> = observed
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        if descriptors.is_empty() {
            return Err(tilo_resource::Refusal::new("no display is connected"));
        }
        if candidate.outputs.len() != descriptors.len()
            || descriptors
                .iter()
                .any(|output| !candidate.outputs.contains_key(&output.connector))
        {
            return Err(tilo_resource::Refusal::new(
                "the configuration must name exactly the connected displays",
            ));
        }
        candidate
            .validate_for(&descriptors)
            .map_err(|error| tilo_resource::Refusal::new(format!("{error:?}")))?;
        let set = DisplaySet::of_descriptors(&descriptors);
        let transaction = DisplayTransaction::prepare_request(set, candidate)
            .map_err(|error| tilo_resource::Refusal::new(error.to_string()))?;
        self.absorb(transaction);
        Ok(())
    }

    /// Move one of this device's screens to a new top-left: the layout takes it at once, and
    /// stays only when kept before its deadline.
    #[action(id = "move-display-screen")]
    pub fn move_display_screen(
        &mut self,
        #[param("The output's connector, such as `eDP-1`")] connector: String,
        #[param("Its new left, in logical pixels")] x: i32,
        #[param("Its new top, in logical pixels")] y: i32,
    ) -> Result<(), tilo_resource::Refusal> {
        let observed = settings_store::hardware::DisplayHardwareSnapshot::load().ok_or_else(|| {
            tilo_resource::Refusal::new("no display is publishing what it can show")
        })?;
        let descriptors: Vec<settings_store::hardware::OutputDescriptor> = observed
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        let set = DisplaySet::of_descriptors(&descriptors);
        if !descriptors
            .iter()
            .any(|output| output.connector == connector)
        {
            return Err(tilo_resource::Refusal::new("no such display is connected"));
        }
        let mut candidate = DisplayConfig::load_effective(&set);
        let Some(entry) = candidate.outputs.get_mut(&connector) else {
            return Err(tilo_resource::Refusal::new(
                "the display has no configuration to move",
            ));
        };
        entry.x = x;
        entry.y = y;
        candidate
            .validate_for(&descriptors)
            .map_err(|error| tilo_resource::Refusal::new(format!("{error:?}")))?;
        let transaction = DisplayTransaction::prepare_request(set, candidate)
            .map_err(|error| tilo_resource::Refusal::new(error.to_string()))?;
        self.absorb(transaction);
        Ok(())
    }

    /// Keep the layout being tried: it becomes what the person chose, instead of reverting when
    /// its deadline passes.
    #[action(id = "keep-display-change")]
    pub fn keep_display_change(&mut self) -> Result<(), tilo_resource::Refusal> {
        if self.state != DisplayTransactionState::AwaitingConfirmation
            || self.deadline_unix_ms.is_some_and(|deadline| {
                tilo_clock::unix_now_ms() >= deadline
            })
        {
            return Err(tilo_resource::Refusal::new(
                "no display change is waiting to be kept",
            ));
        }
        self.state = DisplayTransactionState::ConfirmRequested;
        Ok(())
    }

    /// Put the layout back as it was before the change being tried.
    #[action(id = "revert-display-change")]
    pub fn revert_display_change(&mut self) -> Result<(), tilo_resource::Refusal> {
        if !matches!(
            self.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        ) {
            return Err(tilo_resource::Refusal::new("no display change is pending"));
        }
        self.state = DisplayTransactionState::RevertRequested;
        Ok(())
    }
}

impl PendingDisplayChange {
    /// Take on `transaction`: it becomes the change this record holds.
    fn absorb(&mut self, transaction: DisplayTransaction) {
        self.id = transaction.id;
        self.state = transaction.state;
        self.set = transaction.set;
        self.previous = transaction.previous;
        self.candidate = transaction.candidate;
        self.deadline_unix_ms = transaction.deadline_unix_ms;
        self.message = transaction.message;
    }

    /// The change the section holds, if it holds one.
    #[must_use]
    pub fn transaction(&self) -> Option<DisplayTransaction> {
        (self.id != 0).then(|| DisplayTransaction {
            id: self.id,
            state: self.state,
            set: self.set.clone(),
            previous: self.previous.clone(),
            candidate: self.candidate.clone(),
            deadline_unix_ms: self.deadline_unix_ms,
            message: self.message.clone(),
        })
    }
}
