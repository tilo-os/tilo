//! The capture settings: a modal sheet over the viewfinder holding one
//! grouped settings form. Every control reports a change as a camera
//! [`Action`], so the sheet and the viewfinder's quick controls share one
//! mutation path; the sheet's values are refreshed from the document on every
//! sync.
//!
//! A window too short for the whole form gets a sheet as tall as the window
//! allows, and the form scrolls inside it, so every setting stays reachable.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_dialog::{self, MODAL_MAX_WIDTH};
use ui::widget::{
    Dropdown, EventCtx, Field, Form, FormSection, LabelPlacement, ModalDialog, ModalSpec,
    Scrollable, Toggle, Widget,
};
use vello::kurbo::Rect;
use winit::event::WindowEvent;

use super::Action;
use crate::model::{Facing, PhotoFormat, Resolution, Settings, Timer};

/// Actions raised by retained controls, applied by the app after the event
/// that raised them.
pub(super) type ActionQueue = Rc<RefCell<Vec<Action>>>;

const TITLE: &str = "Camera settings";

/// The settings sheet and the local values its controls show.
pub(super) struct SettingsSheet {
    pub(super) dialog: ModalDialog<Scrollable>,
    queue: ActionQueue,
    states: States,
    /// What the form the sheet holds now was built for.
    fitted: Option<Fit>,
}

/// How the form is built for one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    /// The tallest body the window allows, in whole pixels.
    room: i64,
    /// The form is taller than that: it scrolls and shows its bar.
    overflows: bool,
    /// A compact window: the labels stay beside their choosers, so the
    /// settings remain one list of rows instead of stacking.
    compact: bool,
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
/// trailing edge; on a compact window each fills the room beside its label.
fn choice(
    label: &str,
    all: Vec<(String, String)>,
    state: &LocalState<String>,
    queue: &ActionQueue,
    to_action: fn(&str) -> Action,
    compact: bool,
) -> Field {
    let queue = Rc::clone(queue);
    let mut dropdown = Dropdown::from_state(Vec::new(), state).on_change(move |value| {
        queue.borrow_mut().push(to_action(value));
    });
    dropdown.set_value_options(all);
    let field = Field::new(label, dropdown);
    if compact {
        field
    } else {
        field.with_trailing_control()
    }
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
    fn form(&self, queue: &ActionQueue, compact: bool) -> Form {
        // Each section holds one kind of control, so every control in it
        // lines up at the trailing edge.
        let capture = FormSection::new("Capture")
            .grouped()
            .with_label_placement(compact.then_some(LabelPlacement::Beside))
            .field(choice(
                "Resolution",
                options(&Resolution::ALL, Resolution::as_str, Resolution::label),
                &self.resolution,
                queue,
                |value| Action::SetResolution(Resolution::parse_str(value)),
                compact,
            ))
            .field(choice(
                "Photo format",
                options(&PhotoFormat::ALL, PhotoFormat::as_str, PhotoFormat::label),
                &self.photo_format,
                queue,
                |value| Action::SetPhotoFormat(PhotoFormat::parse_str(value)),
                compact,
            ))
            .field(choice(
                "Self-timer",
                options(&Timer::ALL, timer_value, timer_label),
                &self.timer,
                queue,
                |value| Action::SetTimer(Timer::from_seconds(value.parse().unwrap_or(0))),
                compact,
            ))
            .field(choice(
                "Default camera",
                options(&Facing::ALL, Facing::as_str, Facing::label),
                &self.default_camera,
                queue,
                |value| Action::SetDefaultFacing(Facing::parse_str(value)),
                compact,
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

    /// The form in a vertical scroll view at most `max_height` tall; the
    /// scroll bar and its gutter only when the form overflows.
    fn body(
        &self,
        queue: &ActionQueue,
        max_height: f32,
        overflows: bool,
        compact: bool,
    ) -> Scrollable {
        Scrollable::new_vertical()
            .with_semantic_key("camera.settings.body")
            .with_max_height(max_height)
            .with_scrollbar(overflows)
            .with_focus_reveal()
            .with_child(Box::new(self.form(queue, compact)))
    }
}

impl SettingsSheet {
    pub(super) fn new(queue: &ActionQueue, settings: &Settings) -> Self {
        let states = States::of(settings);
        let dismiss = Rc::clone(queue);
        let dialog = ModalDialog::new(TITLE, states.body(queue, f32::MAX, false, false))
            .with_test_id("camera.settings")
            .on_dismiss(move || dismiss.borrow_mut().push(Action::Back));
        Self {
            dialog,
            queue: Rc::clone(queue),
            states,
            fitted: None,
        }
    }

    /// Fit the sheet to `window`: the body may be as tall as the window
    /// leaves it, and scrolls (with its bar) when the form is taller. Call
    /// before the sheet is drawn; a change shows from that frame on.
    pub(super) fn fit(&mut self, window: Rect, tr: &mut TextRenderer, theme: &Theme) {
        let spec = ModalSpec::new(TITLE)
            .closable(true)
            .body_height(window.height().max(1.0) * 2.0);
        let room = modal_dialog::layout(window, &spec, tr, theme).body.height();
        let room = room.round() as i64;
        let compact = window.width() < f64::from(theme.tokens.metrics.compact_breakpoint);
        // The overflow is measured on the form laid out for this room.
        let overflows = self
            .fitted
            .filter(|fit| fit.room == room && fit.compact == compact)
            .is_some_and(|_| self.dialog.content().vertical_overflows());
        let fit = Fit {
            room,
            overflows,
            compact,
        };
        if self.fitted == Some(fit) {
            return;
        }
        let offset = self.dialog.content().offset_y();
        let mut body = self
            .states
            .body(&self.queue, room as f32, overflows, compact);
        body.set_offset_y(offset);
        *self.dialog.content_mut() = body;
        // Lay the sheet out again around its new body.
        self.dialog.set_max_width(MODAL_MAX_WIDTH);
        self.fitted = Some(fit);
    }

    /// Whether the sheet is still settling on its fit and needs one more
    /// frame.
    pub(super) fn fitting(&self) -> bool {
        self.dialog.is_open()
            && self
                .fitted
                .is_some_and(|fit| fit.overflows != self.dialog.content().vertical_overflows())
    }

    /// A wheel turned over the sheet scrolls its form. The dialog does not
    /// hand wheel events to its content, so the app routes them here.
    pub(super) fn scroll(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if !matches!(event, WindowEvent::MouseWheel { .. }) || !self.dialog.is_open() {
            return;
        }
        let mut taffy = taffy::TaffyTree::<()>::new();
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        self.dialog.content_mut().handle_event(
            event,
            &mut EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: &mut *ctx.state,
                path: ctx.path,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        if handled {
            // The new offset places the form on the next layout.
            self.dialog.set_max_width(MODAL_MAX_WIDTH);
        }
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
