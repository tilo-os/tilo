//! The capture settings: a modal sheet over the viewfinder holding one
//! grouped settings form. Every control reports a change as a camera
//! [`Action`], so the sheet and the viewfinder's quick controls share one
//! mutation path; the sheet's values are refreshed from the document on every
//! sync.
//!
//! A window too short for the whole form gets a sheet as tall as the window
//! allows, and the dialog scrolls the form inside it, so every setting stays
//! reachable.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::widget::{Dropdown, Field, Form, FormSection, ModalDialog, Toggle};

use super::Action;
use crate::model::{Facing, PhotoFormat, Resolution, Settings, Timer};

/// Actions raised by retained controls, applied by the app after the event
/// that raised them.
pub(super) type ActionQueue = Rc<RefCell<Vec<Action>>>;

const TITLE: &str = "Camera settings";

/// The settings sheet and the local values its controls show.
pub(super) struct SettingsSheet {
    pub(super) dialog: ModalDialog<Form>,
    states: States,
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

/// A chooser row. The dropdowns of a section share one column at the
/// trailing edge; once the section stacks, each spans the row under its
/// label.
fn choice(
    label: &str,
    all: Vec<(String, String)>,
    state: &LocalState<String>,
    queue: &ActionQueue,
    to_action: fn(&str) -> Action,
) -> Field {
    let queue = Rc::clone(queue);
    let mut dropdown = Dropdown::from_state(Vec::new(), state).on_change(move |value| {
        queue.borrow_mut().push(to_action(value));
    });
    dropdown.set_value_options(all);
    Field::new(label, dropdown)
}

fn switch(label: &str, state: &LocalState<bool>, queue: &ActionQueue, action: Action) -> Field {
    let queue = Rc::clone(queue);
    Field::new(
        label,
        Toggle::from_state(state).on_change(move |_| {
            queue.borrow_mut().push(action.clone());
        }),
    )
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
    fn form(&self, queue: &ActionQueue) -> Form {
        let capture = FormSection::new("Capture")
            .grouped()
            .field(choice(
                "Resolution",
                options(&Resolution::ALL, Resolution::as_str, Resolution::label),
                &self.resolution,
                queue,
                |value| Action::SetResolution(Resolution::parse_str(value)),
            ))
            .field(choice(
                "Photo format",
                options(&PhotoFormat::ALL, PhotoFormat::as_str, PhotoFormat::label),
                &self.photo_format,
                queue,
                |value| Action::SetPhotoFormat(PhotoFormat::parse_str(value)),
            ))
            .field(choice(
                "Self-timer",
                options(&Timer::ALL, timer_value, timer_label),
                &self.timer,
                queue,
                |value| Action::SetTimer(Timer::from_seconds(value.parse().unwrap_or(0))),
            ))
            .field(choice(
                "Default camera",
                options(&Facing::ALL, Facing::as_str, Facing::label),
                &self.default_camera,
                queue,
                |value| Action::SetDefaultFacing(Facing::parse_str(value)),
            ));
        let options = FormSection::new("Viewfinder and photos")
            .grouped()
            .field(switch("HDR", &self.hdr, queue, Action::ToggleHdr))
            .field(switch(
                "Rule-of-thirds grid",
                &self.grid,
                queue,
                Action::ToggleGrid,
            ))
            .field(switch(
                "Level guide",
                &self.level,
                queue,
                Action::ToggleLevel,
            ))
            .field(switch(
                "Mirror front camera",
                &self.mirror,
                queue,
                Action::ToggleMirror,
            ))
            .field(switch(
                "Save location with photos",
                &self.geotag,
                queue,
                Action::ToggleGeotag,
            ));
        Form::new().section(capture).section(options)
    }
}

impl SettingsSheet {
    pub(super) fn new(queue: &ActionQueue, settings: &Settings) -> Self {
        let states = States::of(settings);
        let dismiss = Rc::clone(queue);
        let dialog = ModalDialog::new(TITLE, states.form(queue))
            .with_test_id("camera.settings")
            .on_dismiss(move || dismiss.borrow_mut().push(Action::Back));
        Self { dialog, states }
    }

    /// Show `settings` in the sheet's controls.
    pub(super) fn present(&self, settings: &Settings) {
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
