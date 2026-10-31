//! The display sheet in the session: where the desktop appears, and which
//! wireless display it is streamed to.
//!
//! The sheet itself is the app's ([`tilo_sheets::displays`]): this module
//! puts it up, keeps what it shows current (the layouts the attached displays
//! allow, the receivers the projector service found, the stream's options,
//! and how a receiver's session stands), and carries out what the person
//! chose. A layout choice is a published action like every other
//! ([`crate::controls::DISPLAYS_RESOURCE`]), so the person at the sheet passes
//! the same authority check as an agent or a script; a wireless display goes
//! to the projector service through [`crate::projection`], and a PIN typed in
//! the sheet comes to it through the seat, never through an action.
//!
//! Nothing counts anything down on a timer of its own: the projector's worker
//! wakes the loop when it has news, and every pass reads what came back.

use tilo_compositor::OutputInfo;
use tilo_projector::{
    DisplayResolutionLimit, ProjectionDevice, ProjectionMode, ProjectionProtocol,
};
use tilo_sheets::Seat as _;
use tilo_sheets::displays::{
    Band, Discovery, DisplaysEffect, DisplaysSeat, Layout, LayoutRow, Link, Receiver, Sheet, Stream,
};

use crate::controls::DISPLAYS_RESOURCE;
use crate::displays::{self, DisplayMode};
use crate::keys::Invocations;
use crate::projection::{ProjectionController, ProjectionStatus, SystemProjectionBackend};

/// The display sheet as the session drives it.
pub struct DisplaySheet {
    /// The sheet's seat; `None` when the sheet did not mount.
    seat: Option<DisplaysSeat>,
    controller: ProjectionController,
    /// The receiver the person chose on this opening, by id. Its stream
    /// options and its PIN show only once one is chosen or a stream runs.
    chosen: Option<String>,
    /// The opening the search was made for, so every opening looks again.
    searched: Option<u64>,
}

impl DisplaySheet {
    /// The sheet, streaming over the projector service on `socket` (`None`:
    /// the service's own), waking the session's loop with `wake` when the
    /// service has news.
    pub(crate) fn new(
        seat: Option<DisplaysSeat>,
        socket: Option<std::path::PathBuf>,
        wake: &tilo_service::Wake,
    ) -> Self {
        Self {
            seat,
            controller: ProjectionController::new(
                Box::new(SystemProjectionBackend::new(socket)),
                wake,
            ),
            chosen: None,
            searched: None,
        }
    }

    /// The sheet's seat: whether it is up, what it shows and what it asked;
    /// `None` when the sheet did not mount.
    #[must_use]
    pub fn seat(&self) -> Option<&DisplaysSeat> {
        self.seat.as_ref()
    }

    /// Its presentation: where it sits and whether it is up, which the sheets
    /// over the desktop are placed by.
    #[must_use]
    pub(crate) fn presentation(&self) -> Option<&tilo_sheets::Presentation> {
        self.seat.as_ref().map(tilo_sheets::Seat::presentation)
    }

    /// Put the sheet up, as a key or an action asked: a sheet already up
    /// stays as it is.
    pub(crate) fn open(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        if !seat.presentation().is_open() {
            seat.presentation().open(true);
        }
    }

    /// Take the sheet down.
    pub(crate) fn close(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        seat.presentation().close();
        seat.presentation().gone();
    }

    /// One pass of the sheet: for a new opening, look for wireless displays
    /// again; hear what the projector answered; carry out what the person
    /// chose; and show what stands now.
    pub(crate) fn follow(
        &mut self,
        outputs: &[OutputInfo],
        answering_a_display_change: bool,
        invocations: &Invocations,
    ) {
        let Some(seat) = &self.seat else {
            return;
        };
        let presentation = seat.presentation();
        if presentation.is_open() {
            let opening = presentation.openings();
            if self.searched != Some(opening) {
                self.searched = Some(opening);
                self.chosen = None;
                self.controller.refresh();
            }
        } else if !presentation.is_drawn() {
            self.searched = None;
        }
        self.controller.poll();
        let asked: Vec<DisplaysEffect> = seat.take_asked();
        let presentation = presentation.clone();
        for effect in asked {
            self.carry_out(effect, invocations);
        }
        if presentation.is_open()
            && let Some(seat) = &self.seat
        {
            seat.show(self.sheet(outputs, answering_a_display_change));
        }
    }

    /// Carry out one choice from the sheet.
    fn carry_out(&mut self, effect: DisplaysEffect, invocations: &Invocations) {
        match effect {
            // A layout is the session's own published action, as the display
            // key and Settings use it.
            DisplaysEffect::Extend => self.choose_layout(Layout::Extend, invocations),
            DisplaysEffect::Mirror => self.choose_layout(Layout::Mirror, invocations),
            DisplaysEffect::BuiltinOnly => self.choose_layout(Layout::BuiltinOnly, invocations),
            DisplaysEffect::ExternalOnly => self.choose_layout(Layout::ExternalOnly, invocations),
            // Keep and Revert answer the question about a display change,
            // which has a sheet of its own that stands on every display
            // (see [`crate::display_confirm`]).
            DisplaysEffect::Keep | DisplaysEffect::Revert => {}
            // The person's other devices' screens take part in a shared
            // display layout, which is no part of this session's displays.
            DisplaysEffect::Include {
                device,
                output,
                include,
                ..
            } => {
                tracing::info!(
                    %device,
                    %output,
                    include,
                    "a shared display layout is not kept here"
                );
            }
            DisplaysEffect::Choose { receiver } => self.choose_receiver(&receiver),
            DisplaysEffect::Connect { receiver } => {
                let pin = self
                    .seat
                    .as_ref()
                    .map(DisplaysSeat::pin)
                    .unwrap_or_default();
                self.connect(&receiver, &pin);
            }
            DisplaysEffect::Stop => self.controller.disconnect(),
            DisplaysEffect::Search => self.controller.refresh(),
            DisplaysEffect::ShareAudio { on } => {
                self.controller.options.audio = on;
                self.restart_live_stream();
            }
            DisplaysEffect::AdaptiveQuality { on } => {
                self.controller.options.stream_policy.adaptive = on;
                self.restart_live_stream();
            }
            DisplaysEffect::NextResolution => self.next_resolution(),
            DisplaysEffect::FrameRate { fps } => {
                self.controller.options.stream_policy.max_fps = if fps <= 30 { 30 } else { 60 };
                self.restart_live_stream();
            }
            DisplaysEffect::Close => self.close(),
        }
    }

    /// Ask for `layout` of the desktop, and stream wireless displays that
    /// way too: a receiver is an external display, so only this screen has
    /// no stream.
    fn choose_layout(&mut self, layout: Layout, invocations: &Invocations) {
        if let Some(mode) = wireless_mode_of(layout)
            && (self.controller.devices.is_empty()
                || self.controller.selected_device_supports(mode))
            && self.controller.options.mode != mode
        {
            self.controller.options.mode = mode;
            self.restart_live_stream();
        }
        invocations.borrow_mut().push(
            crate::keys::Invocation::new(DISPLAYS_RESOURCE, "set-mode")
                .with("mode", layout_name(layout)),
        );
    }

    /// The receiver whose stream the sheet configures: the one streaming or
    /// connecting, else the one the person chose on this opening.
    fn target(&self) -> Option<&ProjectionDevice> {
        let live = match &self.controller.status {
            ProjectionStatus::Connecting { device_id }
            | ProjectionStatus::Connected { device_id } => Some(device_id.as_str()),
            _ => None,
        };
        let id = live.or(self.chosen.as_deref())?;
        self.controller
            .devices
            .iter()
            .find(|device| device.id == id)
    }

    /// Choose `receiver` for its stream options, without connecting.
    fn choose_receiver(&mut self, receiver: &str) {
        self.select(receiver);
    }

    /// Stream to `receiver`, with the PIN typed for it when there is one.
    fn connect(&mut self, receiver: &str, pin: &str) {
        if self.select(receiver) {
            self.controller.options.credential = (!pin.is_empty()).then(|| pin.to_owned());
            self.controller.connect_selected();
        }
    }

    /// Select `receiver`, by id.
    fn select(&mut self, receiver: &str) -> bool {
        let Some(index) = self
            .controller
            .devices
            .iter()
            .position(|device| device.id == receiver)
        else {
            return false;
        };
        if self.controller.selected != index {
            self.controller.options.credential = None;
        }
        self.controller.selected = index;
        self.chosen = Some(receiver.to_owned());
        true
    }

    fn next_resolution(&mut self) {
        let limits = DisplayResolutionLimit::ALL;
        let current = limits
            .iter()
            .position(|limit| *limit == self.controller.options.stream_policy.resolution_limit)
            .unwrap_or(0);
        let next = current
            .wrapping_add(1)
            .checked_rem(limits.len())
            .unwrap_or(0);
        if let Some(&limit) = limits.get(next) {
            self.controller.options.stream_policy.resolution_limit = limit;
        }
        self.restart_live_stream();
    }

    /// Restart the running stream so a changed option takes effect: the
    /// receiver is sent the options once, when it connects.
    fn restart_live_stream(&mut self) {
        let live = matches!(
            &self.controller.status,
            ProjectionStatus::Connected { device_id }
                if self.controller.devices.get(self.controller.selected)
                    .is_some_and(|device| &device.id == device_id)
        );
        if live {
            self.controller.connect_selected();
        }
    }

    /// What the sheet shows now: the layouts the attached displays allow, and
    /// what the projector reported of the wireless ones.
    fn sheet(&self, outputs: &[OutputInfo], answering_a_display_change: bool) -> Sheet {
        let active = self.active_layout(outputs);
        let layouts = Layout::ALL
            .into_iter()
            .filter(|layout| self.layout_possible(*layout, outputs))
            .map(|layout| LayoutRow {
                layout,
                enabled: !answering_a_display_change,
                current: active == Some(layout),
                // A layout the session applies is applied at once: nothing
                // waits here for Keep or Revert.
                stages: false,
            })
            .collect();
        let chosen = self.target().map(|device| device.id.clone());
        let receivers = self
            .controller
            .devices
            .iter()
            .enumerate()
            .map(|(index, device)| Receiver {
                id: device.id.clone(),
                name: device.name.clone(),
                protocol: device.protocol.label().to_owned(),
                detail: device.detail.clone(),
                link: match &self.controller.status {
                    ProjectionStatus::Connected { device_id } if *device_id == device.id => {
                        Link::Connected
                    }
                    ProjectionStatus::Connecting { device_id } if *device_id == device.id => {
                        Link::Connecting
                    }
                    ProjectionStatus::Failed(failure) if index == self.controller.selected => {
                        Link::Failed(failure.title())
                    }
                    _ => Link::Idle,
                },
                asks_pin: device.protocol == ProjectionProtocol::AirPlay,
                chosen: chosen.as_ref() == Some(&device.id),
            })
            .collect();
        let discovery = match &self.controller.status {
            ProjectionStatus::Scanning => Discovery::Searching,
            ProjectionStatus::Failed(failure) if self.controller.devices.is_empty() => {
                Discovery::Failed {
                    title: failure.title(),
                    next_step: failure.next_step(),
                }
            }
            _ => Discovery::NoneFound,
        };
        let policy = &self.controller.options.stream_policy;
        Sheet {
            layouts,
            band: Band::Idle,
            // The person's other devices' screens take part in a shared
            // display layout, which this session has none of.
            screens: Vec::new(),
            screens_editable: false,
            receivers,
            discovery,
            stream: self.target().map(|_| Stream {
                audio: self.controller.options.audio,
                adaptive: policy.adaptive,
                resolution: policy.resolution_limit.label().to_owned(),
                fps: policy.max_fps,
            }),
            asks_pin: self
                .target()
                .is_some_and(|device| device.protocol == ProjectionProtocol::AirPlay),
        }
    }

    /// The layout the sheet marks as current: the desktop's when two or more
    /// displays are attached, else the wireless stream's.
    fn active_layout(&self, outputs: &[OutputInfo]) -> Option<Layout> {
        if outputs.len() > 1 {
            Some(layout_of(displays::current(outputs)))
        } else {
            // One display shows nothing to arrange; a receiver is the only
            // other screen there is.
            self.target()
                .map(|_| layout_of(wireless_layout_of(self.controller.options.mode)))
        }
    }

    /// Whether `layout` is something the displays or a receiver can do now:
    /// the attached displays can be arranged into it, or the chosen receiver
    /// can stream it. Nothing else is shown.
    fn layout_possible(&self, layout: Layout, outputs: &[OutputInfo]) -> bool {
        let arranged = outputs.len() > 1
            && match layout {
                Layout::Extend | Layout::Mirror => true,
                Layout::BuiltinOnly => !displays::arrange(DisplayMode::BuiltIn, outputs).is_empty(),
                Layout::ExternalOnly => {
                    !displays::arrange(DisplayMode::External, outputs).is_empty()
                }
            };
        arranged
            || wireless_mode_of(layout).is_some_and(|mode| {
                self.target().is_some() && self.controller.selected_device_supports(mode)
            })
    }
}

/// The `set-mode` action's name for `layout`.
fn layout_name(layout: Layout) -> &'static str {
    match layout {
        Layout::Extend => "extend",
        Layout::Mirror => "mirror",
        Layout::BuiltinOnly => "built-in",
        Layout::ExternalOnly => "external",
    }
}

/// The layout the sheet names for `mode`.
fn layout_of(mode: DisplayMode) -> Layout {
    match mode {
        DisplayMode::Next | DisplayMode::Extend => Layout::Extend,
        DisplayMode::Mirror => Layout::Mirror,
        DisplayMode::BuiltIn => Layout::BuiltinOnly,
        DisplayMode::External => Layout::ExternalOnly,
    }
}

/// The wireless stream layout that corresponds to a desktop layout. A
/// receiver is an external display, so "only this screen" has no stream.
fn wireless_mode_of(layout: Layout) -> Option<ProjectionMode> {
    match layout {
        Layout::Extend => Some(ProjectionMode::Extend),
        Layout::Mirror => Some(ProjectionMode::Mirror),
        Layout::ExternalOnly => Some(ProjectionMode::ExternalOnly),
        Layout::BuiltinOnly => None,
    }
}

/// The desktop layout a wireless stream layout is.
fn wireless_layout_of(mode: ProjectionMode) -> DisplayMode {
    match mode {
        ProjectionMode::Mirror => DisplayMode::Mirror,
        ProjectionMode::Extend => DisplayMode::Extend,
        ProjectionMode::ExternalOnly => DisplayMode::External,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name a layout is chosen by is the one the displays' own action
    /// takes, so a choice at the sheet arranges them.
    #[test]
    fn every_layout_is_chosen_by_the_name_its_arrangement_answers_to() {
        for (layout, mode) in [
            (Layout::Extend, DisplayMode::Extend),
            (Layout::Mirror, DisplayMode::Mirror),
            (Layout::BuiltinOnly, DisplayMode::BuiltIn),
            (Layout::ExternalOnly, DisplayMode::External),
        ] {
            assert_eq!(
                DisplayMode::named(layout_name(layout)),
                Some(mode),
                "{layout:?} is chosen by its own name"
            );
        }
    }
}
