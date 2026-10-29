//! What the panel needs from the desktop that shows it.
//!
//! The device's state and the things only the desktop can do (its radios
//! through the OS, its clipboard history, its notifications, its device
//! fabric) stay the desktop's. It lends the panel a [`PanelHost`]: plain
//! readings in ([`PanelHost::attach`]), effects out ([`PanelHost::run`]).
//! A screenshot or a test lends nothing and reads [`FixtureHost`] instead,
//! so the panel renders headless with stand-in readings.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;

use crate::QuickSettingsState;
use crate::effect::QsEffect;
pub use crate::faces::visible_controls;

/// What the panel shows now, so the host reads what it lists: Wi-Fi's
/// networks while that section is open, an accessory's devices while its
/// page is, and so on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Showing {
    pub page: crate::Page,
    /// The accessory the page is about, by address, on its page.
    pub accessory: String,
    /// The notification read in full, on its page.
    pub notification: u64,
    /// The section open in place on the root page.
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
        !self.pasting && (self.page != crate::Page::Root || self.section.is_some() || self.editing)
    }
}

/// The desktop's side of the panel.
pub trait PanelHost {
    /// Keep `feed` current: set it now, and again whenever a reading
    /// changes. A panel mounted again attaches a new feed.
    fn attach(&self, feed: Feed);
    /// The panel shows `showing` now.
    fn showing(&self, showing: &Showing);
    /// Carry out `effect`. An error is the effect's answer to whoever
    /// invoked it; the host also says so where the person sees it.
    ///
    /// # Errors
    /// Why the device did not do it.
    fn run(&self, effect: QsEffect) -> Result<(), String>;
    /// How tall the panel's card is laid out, in logical pixels: the host
    /// draws the popover's surface that tall and takes a press below it as
    /// one outside the panel.
    fn laid_out(&self, _height: f64) {}
}

/// The host a desktop lends the panel when it mounts it
/// ([`ui::application::HostContext::lend`]).
#[derive(Clone)]
pub struct Lent(pub Rc<dyn PanelHost>);

/// The panel's readings, as the host hands them over.
#[derive(Clone)]
pub struct Feed {
    readings: Readings,
}

impl Feed {
    /// The device as it reads now.
    pub fn set(&self, state: &QuickSettingsState) {
        self.readings.set(state);
    }

    /// Change the readings in place.
    pub fn update(&self, change: impl FnOnce(&mut QuickSettingsState)) {
        let mut state = self.readings.current();
        change(&mut state);
        self.readings.set(&state);
    }
}

type Projection = Box<dyn Fn(&QuickSettingsState)>;

/// The readings the view shows, and what each part of it reads of them:
/// a part is built again only when what it reads moved, so a level that
/// changes rebuilds its slider and nothing else.
#[derive(Clone)]
pub(crate) struct Readings {
    inner: Rc<RefCell<ReadingsInner>>,
}

struct ReadingsInner {
    current: QuickSettingsState,
    projections: Vec<Projection>,
}

impl Readings {
    pub(crate) fn new(initial: QuickSettingsState) -> Self {
        Self {
            inner: Rc::new(RefCell::new(ReadingsInner {
                current: initial,
                projections: Vec::new(),
            })),
        }
    }

    pub(crate) fn feed(&self) -> Feed {
        Feed {
            readings: self.clone(),
        }
    }

    pub(crate) fn current(&self) -> QuickSettingsState {
        self.inner.borrow().current.clone()
    }

    pub(crate) fn read<V>(&self, read: impl FnOnce(&QuickSettingsState) -> V) -> V {
        read(&self.inner.borrow().current)
    }

    fn set(&self, state: &QuickSettingsState) {
        let projections = {
            let mut inner = self.inner.borrow_mut();
            if inner.current == *state {
                return;
            }
            inner.current.clone_from(state);
            std::mem::take(&mut inner.projections)
        };
        for project in &projections {
            project(state);
        }
        let mut inner = self.inner.borrow_mut();
        let added = std::mem::replace(&mut inner.projections, projections);
        inner.projections.extend(added);
    }

    /// What `read` reads of the readings, kept current: a part keyed on it
    /// is built again when it moves.
    pub(crate) fn project<K>(
        &self,
        read: impl Fn(&QuickSettingsState) -> K + 'static,
    ) -> LocalState<K>
    where
        K: Clone + PartialEq + Send + Sync + 'static,
    {
        let state = LocalState::new(self.read(&read));
        let writer = state.writer();
        let kept = state.clone();
        self.inner
            .borrow_mut()
            .projections
            .push(Box::new(move |readings| {
                let value = read(readings);
                if kept.with(|shown| *shown != value) {
                    writer.set(value);
                }
            }));
        state
    }
}

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
