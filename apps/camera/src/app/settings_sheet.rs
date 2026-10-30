//! The capture settings: a modal sheet over the viewfinder holding one
//! grouped settings form. Every control runs Camera's published `Set…`
//! effect, the one the palette, an agent or a script runs; the sheet's values
//! are refreshed from the settings on every sync.
//!
//! A window too short for the whole form gets a sheet as tall as the window
//! allows, and the dialog scrolls the form inside it, so every setting stays
//! reachable.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{ActionControl, Actions};
use ui::tilo_resource::Binds;
use ui::widget::{Dropdown, Field, Form, FormSection, ModalDialog, Toggle};

use super::Action;
use crate::actions::CameraEffect;
use crate::model::{Facing, PhotoFormat, Resolution, Settings, Timer};
use crate::record::Record;

/// Actions raised by retained controls, applied by the app after the event
/// that raised them.
pub(super) type ActionQueue = Rc<RefCell<Vec<Action>>>;

const TITLE: &str = "Camera settings";

/// The settings sheet and the local values its controls show.
pub(super) struct SettingsSheet {
    pub(super) dialog: ModalDialog<Form>,
    states: States,
    /// What the controls run, once Camera is open on a record.
    actions: Option<Actions<Record>>,
    /// Whether the form offers a default lens: only where there is more
    /// than one camera to start with.
    lenses: bool,
}

struct States {
    resolution: LocalState<String>,
    photo_format: LocalState<String>,
    timer: LocalState<String>,
    default_camera: LocalState<String>,
    hdr: LocalState<bool>,
    grid: LocalState<bool>,
    level: LocalState<bool>,
    mirror: LocalState<bool>,
    geotag: LocalState<bool>,
}

/// One choice: its stored value and its visible label.
fn options<T: Copy>(
    all: &[T],
    value: fn(T) -> &'static str,
    label: fn(T) -> &'static str,
) -> Vec<(String, String)> {
    all.iter()
        .map(|item| (value(*item).to_owned(), label(*item).to_owned()))
        .collect()
}

fn timer_label(timer: Timer) -> &'static str {
    match timer {
        Timer::Off => "Off",
        Timer::S3 => "3 seconds",
        Timer::S10 => "10 seconds",
    }
}

fn timer_value(timer: Timer) -> &'static str {
    match timer {
        Timer::Off => "0",
        Timer::S3 => "3",
        Timer::S10 => "10",
    }
}

/// `control`, running `effect`; inert until Camera is open on a record.
fn bound<I: 'static, C: ActionControl<I>>(
    control: C,
    actions: Option<&Actions<Record>>,
    effect: impl Binds<CameraEffect, I>,
) -> C {
    match actions {
        Some(actions) => actions.bind_effect(control, effect),
        None => control.unavailable(),
    }
}

/// A chooser row. The dropdowns of a section share one column at the
/// trailing edge; once the section stacks, each spans the row under its
/// label.
fn choice(
    label: &str,
    all: Vec<(String, String)>,
    state: &LocalState<String>,
    actions: Option<&Actions<Record>>,
    effect: impl Binds<CameraEffect, String>,
) -> Field {
    let mut dropdown = Dropdown::from_state(Vec::new(), state);
    dropdown.set_value_options(all);
    Field::new(label, bound(dropdown, actions, effect))
}

fn switch(
    label: &str,
    state: &LocalState<bool>,
    actions: Option<&Actions<Record>>,
    effect: impl Binds<CameraEffect, bool>,
) -> Field {
    Field::new(label, bound(Toggle::from_state(state), actions, effect))
}

impl States {
    fn of(settings: &Settings) -> Self {
        Self {
            resolution: LocalState::new(settings.resolution.as_str().to_owned()),
            photo_format: LocalState::new(settings.photo_format.as_str().to_owned()),
            timer: LocalState::new(timer_value(settings.timer).to_owned()),
            default_camera: LocalState::new(settings.default_facing.as_str().to_owned()),
            hdr: LocalState::new(settings.hdr),
            grid: LocalState::new(settings.grid),
            level: LocalState::new(settings.level),
            mirror: LocalState::new(settings.mirror_front),
            geotag: LocalState::new(settings.geotag),
        }
    }

    /// The form, its controls bound to these values.
    fn form(&self, actions: Option<&Actions<Record>>, lenses: bool) -> Form {
        let mut capture = FormSection::new("Capture")
            .grouped()
            .field(choice(
                "Resolution",
                options(&Resolution::ALL, Resolution::as_str, Resolution::label),
                &self.resolution,
                actions,
                CameraEffect::SET_RESOLUTION,
            ))
            .field(choice(
                "Photo format",
                options(&PhotoFormat::ALL, PhotoFormat::as_str, PhotoFormat::label),
                &self.photo_format,
                actions,
                CameraEffect::SET_PHOTO_FORMAT,
            ))
            .field(choice(
                "Self-timer",
                options(&Timer::ALL, timer_value, timer_label),
                &self.timer,
                actions,
                CameraEffect::SET_TIMER,
            ));
        if lenses {
            capture = capture.field(choice(
                "Default camera",
                options(&Facing::ALL, Facing::as_str, Facing::label),
                &self.default_camera,
                actions,
                CameraEffect::SET_DEFAULT_LENS,
            ));
        }
        let options = FormSection::new("Viewfinder and photos")
            .grouped()
            .field(switch("HDR", &self.hdr, actions, CameraEffect::SET_HDR))
            .field(switch(
                "Rule-of-thirds grid",
                &self.grid,
                actions,
                CameraEffect::SET_GRID,
            ))
            .field(switch(
                "Level guide",
                &self.level,
                actions,
                CameraEffect::SET_LEVEL,
            ))
            .field(switch(
                "Mirror front camera",
                &self.mirror,
                actions,
                CameraEffect::SET_MIRROR_FRONT,
            ))
            .field(switch(
                "Save location with photos",
                &self.geotag,
                actions,
                CameraEffect::SET_GEOTAG,
            ));
        Form::new().section(capture).section(options)
    }
}

impl SettingsSheet {
    pub(super) fn new(queue: &ActionQueue, settings: &Settings) -> Self {
        let states = States::of(settings);
        let dismiss = Rc::clone(queue);
        let dialog = ModalDialog::new(TITLE, states.form(None, false))
            .with_test_id("camera.settings")
            .on_dismiss(move || dismiss.borrow_mut().push(Action::Back));
        Self {
            dialog,
            states,
            actions: None,
            lenses: false,
        }
    }

    /// Bind the controls to Camera's published effects.
    pub(super) fn attach(&mut self, actions: &Actions<Record>) {
        self.actions = Some(actions.clone());
        *self.dialog.content_mut() = self.states.form(self.actions.as_ref(), self.lenses);
    }

    /// Whether the sheet offers a default lens.
    #[cfg(test)]
    pub(super) fn offers_lenses(&self) -> bool {
        self.lenses
    }

    /// Show `settings` in the sheet's controls, offering a default lens
    /// only when `lenses` says there is more than one.
    pub(super) fn present(&mut self, settings: &Settings, lenses: bool) {
        if lenses != self.lenses {
            self.lenses = lenses;
            *self.dialog.content_mut() = self.states.form(self.actions.as_ref(), lenses);
        }
        let s = &self.states;
        set_if_changed(&s.resolution, settings.resolution.as_str().to_owned());
        set_if_changed(&s.photo_format, settings.photo_format.as_str().to_owned());
        set_if_changed(&s.timer, timer_value(settings.timer).to_owned());
        set_if_changed(
            &s.default_camera,
            settings.default_facing.as_str().to_owned(),
        );
        set_if_changed(&s.hdr, settings.hdr);
        set_if_changed(&s.grid, settings.grid);
        set_if_changed(&s.level, settings.level);
        set_if_changed(&s.mirror, settings.mirror_front);
        set_if_changed(&s.geotag, settings.geotag);
    }
}

fn set_if_changed<T: Clone + PartialEq + 'static>(state: &LocalState<T>, value: T) {
    if state.get() != value {
        state.set(value);
    }
}
