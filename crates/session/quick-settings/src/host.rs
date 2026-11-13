//! What the panel needs from what shows it.
//!
//! The panel is a process of its own: what it shows arrives as the
//! session's published readings ([`crate::live`]), and what a press does goes
//! back to them. It reaches both through a [`PanelHost`]: readings in
//! ([`PanelHost::feed`]), effects out ([`PanelHost::run`]). A screenshot or
//! a test lends a [`FixtureHost`] instead, so the panel renders headless with
//! stand-in readings.

use std::cell::RefCell;
use std::rc::Rc;

use crate::QuickSettingsState;
use crate::effect::QsEffect;
pub use crate::readings::Feed;

/// What the panel shows now, so what shows it reads what it lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Showing {
    pub page: crate::Page,
    /// The accessory the page is about, by address, on its page.
    pub accessory: String,
    /// The section open on the controls.
    pub section: Option<crate::QsSection>,
    /// Opened to paste a copy into the focused field.
    pub pasting: bool,
    pub editing: bool,
}

impl Showing {
    /// Whether Back has somewhere to go inside the panel: a section to
    /// close, a page to leave, editing to finish. Otherwise Back closes the
    /// panel.
    #[must_use]
    pub fn goes_back(&self) -> bool {
        !self.pasting && self.is_deeper()
    }

    /// Whether the page stands deeper than the controls: a section, an
    /// accessory, the clipboard opened to paste or the arrangement. Such a
    /// page arrives over the controls and Back returns to them.
    #[must_use]
    pub fn is_deeper(&self) -> bool {
        self.pasting || self.editing || self.page != crate::Page::Root || self.section.is_some()
    }
}

/// The panel's side of what shows it.
pub trait PanelHost {
    /// The readings the panel shows, kept current by the host: the
    /// session's, or a fixture's stand-ins.
    fn readings(&self) -> Feed;
    /// The panel shows `showing` now.
    fn showing(&self, showing: &Showing);
    /// Carry out `effect`. An error is the effect's answer to whoever
    /// invoked it; the host also says so where the person sees it.
    ///
    /// # Errors
    /// Why the device did not do it.
    fn run(&self, effect: QsEffect) -> Result<(), String>;
}

/// A host a test hands the panel it runs (`HeadlessApp::for_person_with_lent`):
/// the panel reads what it is fed and its effects are recorded, without a
/// session to publish them.
#[derive(Clone)]
pub struct Lent(pub Rc<dyn PanelHost>);

/// The host of a screenshot or a test: stand-in readings for each scenario,
/// and effects recorded instead of carried out.
pub struct FixtureHost {
    readings: crate::readings::Readings,
    ran: RefCell<Vec<QsEffect>>,
    shown: RefCell<Vec<Showing>>,
}

impl FixtureHost {
    #[must_use]
    pub fn new(readings: QuickSettingsState) -> Self {
        Self {
            readings: crate::readings::Readings::new(readings),
            ran: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
        }
    }

    /// The effects run so far, oldest first.
    #[must_use]
    pub fn ran(&self) -> Vec<QsEffect> {
        self.ran.borrow().clone()
    }

    /// What the panel last said it shows.
    #[must_use]
    pub fn showing(&self) -> Option<Showing> {
        self.shown.borrow().last().cloned()
    }

    /// Change what the device reads, as a host would.
    pub fn update(&self, change: impl FnOnce(&mut QuickSettingsState)) {
        self.readings.update(change);
    }
}

impl PanelHost for FixtureHost {
    fn readings(&self) -> Feed {
        self.readings.feed()
    }

    fn showing(&self, showing: &Showing) {
        self.shown.borrow_mut().push(showing.clone());
    }

    fn run(&self, effect: QsEffect) -> Result<(), String> {
        // The arrangement is a published action like any other: a fixture
        // carries it out on the readings it keeps, so arranging can be seen
        // without a desktop.
        self.readings.update(|state| {
            arrange(&mut state.arrangement, &effect);
            if effect == QsEffect::DismissRefusal {
                state.refusal = None;
            }
        });
        self.ran.borrow_mut().push(effect);
        Ok(())
    }
}

/// Apply an arrangement effect (show, hide, move or reset controls) to
/// `arrangement`. Whether it was one, and changed something.
pub fn arrange(
    arrangement: &mut settings_store::hardware::QuickSettingsConfig,
    effect: &QsEffect,
) -> bool {
    match effect {
        QsEffect::ShowControl { control } => show_control(arrangement, control),
        QsEffect::HideControl { control } => hide_control(arrangement, control),
        QsEffect::MoveControl { control, before } => {
            move_control(arrangement, control, before.as_deref())
        }
        QsEffect::ResetControls => reset_controls(arrangement),
        _ => false,
    }
}

/// Show the control `control` names on the panel. Whether the arrangement changed.
pub fn show_control(
    config: &mut settings_store::hardware::QuickSettingsConfig,
    control: &str,
) -> bool {
    let Some(control) = crate::faces::control_named(control) else {
        return false;
    };
    settled(config, |config| {
        if !config.controls.contains(&control) {
            config.controls.push(control);
        }
    })
}

/// Take the control `control` names off the panel: Wi-Fi stays, as the way a desktop without a
/// network gets online. Whether the arrangement changed.
pub fn hide_control(
    config: &mut settings_store::hardware::QuickSettingsConfig,
    control: &str,
) -> bool {
    let Some(control) = crate::faces::control_named(control) else {
        return false;
    };
    settled(config, |config| {
        if !control.required() {
            config.controls.retain(|shown| *shown != control);
        }
    })
}

/// Move the control `control` names to stand before `before`, the last place when none is named.
/// Whether the arrangement changed.
pub fn move_control(
    config: &mut settings_store::hardware::QuickSettingsConfig,
    control: &str,
    before: Option<&str>,
) -> bool {
    let Some(control) = crate::faces::control_named(control) else {
        return false;
    };
    let before = before.and_then(crate::faces::control_named);
    settled(config, |config| {
        let Some(from) = config.controls.iter().position(|shown| *shown == control) else {
            return;
        };
        let moved = config.controls.remove(from);
        let at = before
            .filter(|before| *before != control)
            .and_then(|before| config.controls.iter().position(|shown| *shown == before))
            .unwrap_or(config.controls.len());
        config.controls.insert(at.min(config.controls.len()), moved);
    })
}

/// Put the panel's controls back as they were at first. Whether the arrangement changed.
pub fn reset_controls(config: &mut settings_store::hardware::QuickSettingsConfig) -> bool {
    let mut fresh = settings_store::hardware::QuickSettingsConfig::default();
    std::mem::swap(config, &mut fresh);
    fresh != *config
}

/// `change` applied to `config`, and the arrangement kept as a panel reads it: duplicates gone,
/// Wi-Fi where every panel starts. Whether it changed.
fn settled(
    config: &mut settings_store::hardware::QuickSettingsConfig,
    change: impl FnOnce(&mut settings_store::hardware::QuickSettingsConfig),
) -> bool {
    let before = config.clone();
    change(config);
    *config = config.clone().normalised();
    *config != before
}
