//! The capture settings: a modal sheet over the viewfinder holding one
//! grouped settings form. Every control reports a change as a camera
//! [`Action`], so the sheet and the viewfinder's quick controls share one
//! mutation path; the sheet's values are refreshed from the document on every
//! sync.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::widget::{Dropdown, Field, Form, FormSection, ModalDialog, Toggle};

use super::Action;
use crate::model::{Facing, PhotoFormat, Resolution, Settings, Timer};

/// Actions raised by retained controls, applied by the app after the event
/// that raised them.
pub(super) type ActionQueue = Rc<RefCell<Vec<Action>>>;

/// The settings sheet and the local values its controls show.
pub(super) struct SettingsSheet {
    pub(super) dialog: ModalDialog<Form>,
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

fn choice(
    all: Vec<(String, String)>,
    state: &LocalState<String>,
    queue: &ActionQueue,
    to_action: fn(&str) -> Action,
) -> Dropdown {
    let queue = Rc::clone(queue);
    let mut dropdown = Dropdown::from_state(Vec::new(), state).on_change(move |value| {
        queue.borrow_mut().push(to_action(value));
    });
    dropdown.set_value_options(all);
    dropdown
}

fn switch(state: &LocalState<bool>, queue: &ActionQueue, action: Action) -> Toggle {
    let queue = Rc::clone(queue);
    Toggle::from_state(state).on_change(move |_| {
        queue.borrow_mut().push(action.clone());
    })
}

impl SettingsSheet {
    pub(super) fn new(queue: &ActionQueue, settings: &Settings) -> Self {
        let resolution = LocalState::new(settings.resolution.as_str().to_owned());
        let photo_format = LocalState::new(settings.photo_format.as_str().to_owned());
        let timer = LocalState::new(timer_value(settings.timer).to_owned());
        let default_camera = LocalState::new(settings.default_facing.as_str().to_owned());
        let hdr = LocalState::new(settings.hdr);
        let grid = LocalState::new(settings.grid);
        let level = LocalState::new(settings.level);
        let mirror = LocalState::new(settings.mirror_front);
        let geotag = LocalState::new(settings.geotag);

        // Each section holds one kind of control, so every control in it
        // lines up at the trailing edge.
        let capture = FormSection::new("Capture")
            .grouped()
            .field(Field::new(
                "Resolution",
                choice(
                    options(&Resolution::ALL, Resolution::as_str, Resolution::label),
                    &resolution,
                    queue,
                    |value| Action::SetResolution(Resolution::parse_str(value)),
                ),
            ))
            .field(Field::new(
                "Photo format",
                choice(
                    options(&PhotoFormat::ALL, PhotoFormat::as_str, PhotoFormat::label),
                    &photo_format,
                    queue,
                    |value| Action::SetPhotoFormat(PhotoFormat::parse_str(value)),
                ),
            ))
            .field(Field::new(
                "Self-timer",
                choice(
                    options(&Timer::ALL, timer_value, timer_label),
                    &timer,
                    queue,
                    |value| Action::SetTimer(Timer::from_seconds(value.parse().unwrap_or(0))),
                ),
            ))
            .field(Field::new(
                "Default camera",
                choice(
                    options(&Facing::ALL, Facing::as_str, Facing::label),
                    &default_camera,
                    queue,
                    |value| Action::SetDefaultFacing(Facing::parse_str(value)),
                ),
            ));
        let options = FormSection::new("Viewfinder and photos")
            .grouped()
            .field(Field::new("HDR", switch(&hdr, queue, Action::ToggleHdr)))
            .field(Field::new(
                "Rule-of-thirds grid",
                switch(&grid, queue, Action::ToggleGrid),
            ))
            .field(Field::new(
                "Level guide",
                switch(&level, queue, Action::ToggleLevel),
            ))
            .field(Field::new(
                "Mirror front camera",
                switch(&mirror, queue, Action::ToggleMirror),
            ))
            .field(Field::new(
                "Save location with photos",
                switch(&geotag, queue, Action::ToggleGeotag),
            ));
        let form = Form::new().section(capture).section(options);
        let dismiss = Rc::clone(queue);
        let dialog = ModalDialog::new("Camera settings", form)
            .with_test_id("camera.settings")
            .on_dismiss(move || dismiss.borrow_mut().push(Action::Back));
        Self {
            dialog,
            resolution,
            photo_format,
            timer,
            default_camera,
            hdr,
            grid,
            level,
            mirror,
            geotag,
        }
    }

    /// Show `settings` in the sheet's controls.
    pub(super) fn present(&self, settings: &Settings) {
        set_if_changed(&self.resolution, settings.resolution.as_str().to_owned());
        set_if_changed(
            &self.photo_format,
            settings.photo_format.as_str().to_owned(),
        );
        set_if_changed(&self.timer, timer_value(settings.timer).to_owned());
        set_if_changed(
            &self.default_camera,
            settings.default_facing.as_str().to_owned(),
        );
        set_if_changed(&self.hdr, settings.hdr);
        set_if_changed(&self.grid, settings.grid);
        set_if_changed(&self.level, settings.level);
        set_if_changed(&self.mirror, settings.mirror_front);
        set_if_changed(&self.geotag, settings.geotag);
    }
}

fn set_if_changed<T: Clone + PartialEq + 'static>(state: &LocalState<T>, value: T) {
    if state.get() != value {
        state.set(value);
    }
}
