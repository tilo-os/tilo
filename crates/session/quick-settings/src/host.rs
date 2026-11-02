//! What the panel needs from what shows it.
//!
//! The panel is a process of its own: what it shows arrives as the
//! session's published readings ([`crate::live`]), and what a press does goes
//! back to them. It reaches both through a [`PanelHost`]: readings in
//! ([`PanelHost::attach`]), effects out ([`PanelHost::run`]). A screenshot or
//! a test lends a [`FixtureHost`] instead, so the panel renders headless with
//! stand-in readings.

use std::cell::RefCell;
use std::rc::Rc;

use crate::QuickSettingsState;
use crate::effect::QsEffect;
pub use crate::faces::visible_controls;
pub use crate::readings::Feed;

/// What the panel shows now, so what shows it reads what it lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Showing {
    pub page: crate::Page,
    /// The accessory the page is about, by address, on its page.
    pub accessory: String,
    /// The notification read in full, on its page.
    pub notification: u64,
    /// The section whose page stands in the controls' place.
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

    /// Whether the page stands deeper than the controls: a section, the
    /// notifications, one of them, an accessory, the clipboard opened to
    /// paste or the arrangement. Such a page arrives over the controls and
    /// Back returns to them.
    #[must_use]
    pub fn is_deeper(&self) -> bool {
        self.pasting || self.editing || self.page != crate::Page::Root || self.section.is_some()
    }
}

/// The panel's side of what shows it.
pub trait PanelHost {
    /// Keep `feed` current: set it now, and again whenever a reading
    /// changes.
    fn attach(&self, feed: Feed);
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
    readings: QuickSettingsState,
    feed: RefCell<Option<Feed>>,
    ran: RefCell<Vec<QsEffect>>,
    shown: RefCell<Vec<Showing>>,
}

impl FixtureHost {
    #[must_use]
    pub fn new(readings: QuickSettingsState) -> Self {
        Self {
            readings,
            feed: RefCell::new(None),
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
        if let Some(feed) = self.feed.borrow().as_ref() {
            feed.update(change);
        }
    }
}

impl PanelHost for FixtureHost {
    fn attach(&self, feed: Feed) {
        feed.set(&self.readings);
        *self.feed.borrow_mut() = Some(feed);
    }

    fn showing(&self, showing: &Showing) {
        self.shown.borrow_mut().push(showing.clone());
    }

    fn run(&self, effect: QsEffect) -> Result<(), String> {
        // The arrangement is the panel's own to show: a fixture keeps it,
        // so arranging can be seen without a desktop.
        if let Some(feed) = self.feed.borrow().as_ref() {
            feed.update(|state| {
                let visible = crate::faces::visible_controls(&state.arrangement, state);
                arrange(&mut state.arrangement, &visible, &effect);
            });
        }
        self.ran.borrow_mut().push(effect);
        Ok(())
    }
}

/// Apply an arrangement effect (show, hide, move or reset controls) to
/// `arrangement`, whose `visible` controls are the ones the panel shows on
/// this machine. Whether it was one, and changed something.
pub fn arrange(
    arrangement: &mut settings_store::hardware::QuickSettingsConfig,
    visible: &[settings_store::hardware::QuickSettingsControl],
    effect: &QsEffect,
) -> bool {
    let before = arrangement.clone();
    match effect {
        QsEffect::ShowControl { control } => {
            if let Some(control) = crate::faces::control_named(control)
                && !arrangement.controls.contains(&control)
            {
                arrangement.controls.push(control);
            }
        }
        QsEffect::HideControl { control } => {
            if let Some(control) = crate::faces::control_named(control)
                && !control.required()
            {
                arrangement.controls.retain(|shown| *shown != control);
            }
        }
        QsEffect::MoveControl { control, to } => {
            // `to` is a place among the controls shown: the control takes
            // the place of the one shown there, so a control this machine
            // does not show never decides where another lands.
            let target = usize::try_from(*to)
                .ok()
                .and_then(|to| visible.get(to))
                .copied();
            if let (Some(control), Some(target)) = (crate::faces::control_named(control), target)
                && let Some(from) = arrangement.controls.iter().position(|c| *c == control)
            {
                let moved = arrangement.controls.remove(from);
                let at = arrangement
                    .controls
                    .iter()
                    .position(|c| *c == target)
                    .map_or(arrangement.controls.len(), |at| {
                        if at >= from { at.saturating_add(1) } else { at }
                    });
                arrangement
                    .controls
                    .insert(at.min(arrangement.controls.len()), moved);
            }
        }
        QsEffect::ResetControls => {
            *arrangement = settings_store::hardware::QuickSettingsConfig::default();
        }
        _ => return false,
    }
    *arrangement = arrangement.clone().normalised();
    *arrangement != before
}
