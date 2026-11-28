//! Slides on the big screen, the presenter's view on yours.
//!
//! An app presenting to a room opens a second toplevel beside its window, named for its own app
//! id with [`AUDIENCE_SUFFIX`]: what the audience sees. That window fills one display, the one an
//! audience looks at ([`FullscreenTarget::Largest`]: a plugged-in display before the built-in
//! panel, then the physically larger) unless the app names one, and the app's own window moves to
//! another as the presenter's view: the built-in panel when there is one, else the smaller screen.
//! That is where a laptop plugged into a television is expected to put things, whichever screen
//! the window happened to be on.
//!
//! The move is the compositor's, not the desktop's: the audience's window takes no place in the
//! policy's layout, and the presenter's window keeps its own, its tab where it was, while it
//! covers its display. Where the policy places it meanwhile is where it goes back to, at the size
//! the policy gave it, when the audience's window goes. On one display the audience's window
//! fills it and the presenter's stays where it is: nowhere else to go.
//!
//! Only a window of the same client counts as a window's audience: an app id is the client's own
//! word, so a window of another program could otherwise name itself another app's audience and
//! pull that app's window across the screens.

use std::collections::HashMap;

use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource as _;
use smithay::utils::{Logical, Rectangle};
use tilo_ui_base::fullscreen::{
    FullscreenTarget, MonitorInfo, connector_is_builtin, select_audience_output,
    select_presenter_output,
};

use crate::policy::{WindowPlacement, WindowRequest};
use crate::state::{State, describe};

/// What an app's audience window adds to the app's own id: `so.tilo.slides.audience` is what
/// `so.tilo.slides` shows the room.
pub const AUDIENCE_SUFFIX: &str = ".audience";

/// A presentation going on: the audience's window and the display it fills, and the presenter's
/// window and the display it was moved to, when there is a second one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presenting {
    /// The audience's window ([`crate::WindowInfo::id`]).
    pub audience: u64,
    /// The output it fills, by connector name.
    pub audience_on: String,
    /// The window it belongs to and the output it covers as the presenter's view.
    pub presenter: Option<(u64, String)>,
}

/// The presenter's window, held on a display of its own while the audience has another.
struct Pinned {
    presenter: u64,
    output: String,
    /// Where the policy puts it and the size it gives it: where it goes back to.
    home: Rectangle<i32, Logical>,
    /// It covered its display of its own asking before: it goes back to that.
    was_fullscreen: bool,
}

/// The presentations' state: the display each audience window asked for, and the presenter's
/// window held on its own.
#[derive(Default)]
pub(crate) struct Presentation {
    /// The output each audience window named (`xdg_toplevel.set_fullscreen`), by window id.
    asked: HashMap<u64, String>,
    pinned: Option<Pinned>,
}

/// The outputs a presentation can use, each with its logical rectangle.
type Screens = Vec<(Output, Rectangle<i32, Logical>)>;

/// One window's part in the presentation the windows call for.
struct Part {
    window: Window,
    id: u64,
    output: String,
    rect: Rectangle<i32, Logical>,
}

/// The presentation the windows call for.
struct Wanted {
    audience: Part,
    presenter: Option<Part>,
}

/// The app id and client of `window`, when it is a Wayland toplevel that named an app.
fn app_of(
    window: &Window,
) -> Option<(
    String,
    smithay::reexports::wayland_server::backend::ClientId,
)> {
    let toplevel = window.toplevel()?;
    let client = toplevel.wl_surface().client()?.id();
    let app_id = smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.app_id.clone())
    })?;
    Some((app_id, client))
}

impl State {
    /// The window `window` shows an audience for: the topmost window of the same client whose app
    /// id is `window`'s without [`AUDIENCE_SUFFIX`]. `None` for any other window.
    fn audience_owner(&self, window: &Window) -> Option<Window> {
        let (app_id, client) = app_of(window)?;
        let owner_app = app_id.strip_suffix(AUDIENCE_SUFFIX)?;
        if owner_app.is_empty() {
            return None;
        }
        self.space
            .elements()
            .rev()
            .find(|candidate| {
                *candidate != window
                    && app_of(candidate).is_some_and(|(app, of)| app == owner_app && of == client)
            })
            .cloned()
    }

    /// Whether `window` is an app's audience window, which takes no place in the policy's layout.
    pub(crate) fn is_audience(&self, window: &Window) -> bool {
        self.audience_owner(window).is_some()
    }

    /// The outputs a presentation can use, with their rectangles, and each described as the
    /// shared ranking reads a monitor: a zero-sized output is mid-modeset and no candidate.
    fn presentation_outputs(&self) -> (Screens, Vec<MonitorInfo>) {
        let viable: Screens = self
            .space
            .outputs()
            .filter_map(|output| {
                let rect = self.space.output_geometry(output)?;
                (rect.size.w > 0 && rect.size.h > 0).then(|| (output.clone(), rect))
            })
            .collect();
        let infos = viable
            .iter()
            .enumerate()
            .map(|(index, (output, rect))| {
                let mode = output
                    .current_mode()
                    .map_or((rect.size.w, rect.size.h), |mode| {
                        (mode.size.w, mode.size.h)
                    });
                let physical = output.physical_properties().size;
                MonitorInfo {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    width: mode.0.unsigned_abs(),
                    height: mode.1.unsigned_abs(),
                    primary: self.primary_output.as_deref() == Some(output.name().as_str()),
                    scale: output.current_scale().fractional_scale(),
                    builtin: connector_is_builtin(&output.name()),
                    physical_mm: (physical.w > 0 && physical.h > 0)
                        .then(|| (physical.w.unsigned_abs(), physical.h.unsigned_abs())),
                }
            })
            .collect();
        (viable, infos)
    }

    /// The output `audience` fills, and the one its presenter's window `presenter` moves to when
    /// there is a second: indices into [`Self::presentation_outputs`].
    fn presentation_screens(
        &self,
        audience: u64,
        presenter_at: Option<Rectangle<i32, Logical>>,
        outputs: &[(Output, Rectangle<i32, Logical>)],
        infos: &[MonitorInfo],
    ) -> Option<(usize, Option<usize>)> {
        if outputs.is_empty() {
            return None;
        }
        let presenter_on = presenter_at.and_then(|rect| {
            let middle = smithay::utils::Point::from((
                rect.loc.x.saturating_add(rect.size.w / 2),
                rect.loc.y.saturating_add(rect.size.h / 2),
            ));
            outputs
                .iter()
                .position(|(_, output)| output.contains(middle))
                .and_then(|index| u32::try_from(index).ok())
        });
        let named = self.presentation.asked.get(&audience).and_then(|name| {
            outputs
                .iter()
                .position(|(output, _)| output.name() == *name)
        });
        let audience_index = named
            .or_else(|| {
                select_audience_output(infos, FullscreenTarget::Largest, presenter_on)
                    .map(|index| index as usize)
            })
            .unwrap_or(0);
        let presenter_index = u32::try_from(audience_index)
            .ok()
            .and_then(|index| select_presenter_output(infos, index))
            .map(|index| index as usize);
        Some((audience_index, presenter_index))
    }

    /// Where the audience window `window`, not mapped yet, goes: the rectangle of the display it
    /// will fill. `None` when it is no audience window.
    pub(crate) fn audience_rect(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        let owner = self.audience_owner(window)?;
        let id = describe(self, window)?.id;
        let (outputs, infos) = self.presentation_outputs();
        let presenter_at = self.space.element_geometry(&owner);
        let (index, _) = self.presentation_screens(id, presenter_at, &outputs, &infos)?;
        outputs.get(index).map(|(_, rect)| *rect)
    }

    /// The presentation the windows call for now: the topmost audience window and the window it
    /// belongs to.
    fn wanted_presentation(&self) -> Option<Wanted> {
        let (audience, owner) = self
            .space
            .elements()
            .rev()
            .find_map(|window| Some((window.clone(), self.audience_owner(window)?)))?;
        let audience_id = describe(self, &audience)?.id;
        let owner_id = describe(self, &owner)?.id;
        let (outputs, infos) = self.presentation_outputs();
        // Judged from where the presenter's window stands in the layout, not where it is held.
        let presenter_at = match &self.presentation.pinned {
            Some(pinned) if pinned.presenter == owner_id => Some(pinned.home),
            _ => self.space.element_geometry(&owner),
        };
        let (audience_index, presenter_index) =
            self.presentation_screens(audience_id, presenter_at, &outputs, &infos)?;
        let part = |window: Window, id: u64, index: usize| {
            outputs.get(index).map(|(output, rect)| Part {
                window,
                id,
                output: output.name(),
                rect: *rect,
            })
        };
        Some(Wanted {
            audience: part(audience, audience_id, audience_index)?,
            presenter: presenter_index.and_then(|index| part(owner, owner_id, index)),
        })
    }

    /// The presentation going on, if one is.
    pub(crate) fn presenting(&self) -> Option<Presenting> {
        let wanted = self.wanted_presentation()?;
        Some(Presenting {
            audience: wanted.audience.id,
            audience_on: wanted.audience.output,
            presenter: wanted
                .presenter
                .map(|presenter| (presenter.id, presenter.output)),
        })
    }

    /// The policy placed the window `placement` names: a held presenter's window keeps where it
    /// is held and goes there once the audience's window goes. Whether the placement was taken
    /// that way, so the caller applies none of it.
    pub(crate) fn hold_placement(&mut self, placement: &WindowPlacement) -> bool {
        let Some(pinned) = self
            .presentation
            .pinned
            .as_mut()
            .filter(|pinned| pinned.presenter == placement.id)
        else {
            return false;
        };
        pinned.home.loc = placement.location;
        if let Some((w, h)) = placement.size {
            pinned.home.size = (w, h).into();
        }
        true
    }

    /// Bring the windows in line with the presentation they call for: the audience's window over
    /// its display, the presenter's over another, and a presenter's window no longer held back
    /// where the policy puts it. Runs after every placement.
    pub(crate) fn present(&mut self) {
        let wanted = self.wanted_presentation();
        // A presenter's window moved to another display keeps where it goes back to.
        let keep = |pinned: &Pinned| {
            wanted
                .as_ref()
                .and_then(|wanted| wanted.presenter.as_ref())
                .is_some_and(|presenter| presenter.id == pinned.presenter)
        };
        if self
            .presentation
            .pinned
            .as_ref()
            .is_some_and(|pinned| !keep(pinned))
        {
            self.release_presenter();
        }
        let gone: Vec<u64> = self
            .presentation
            .asked
            .keys()
            .copied()
            .filter(|id| {
                self.mapped_window(*id).is_none()
                    && !self
                        .unmapped
                        .iter()
                        .any(|window| describe(self, window).is_some_and(|info| info.id == *id))
            })
            .collect();
        self.presentation.asked.retain(|id, _| !gone.contains(id));
        let Some(wanted) = wanted else {
            return;
        };
        self.cover_for_presentation(&wanted.audience);
        if let Some(presenter) = wanted.presenter {
            if let Some(pinned) = self.presentation.pinned.as_mut() {
                pinned.output.clone_from(&presenter.output);
            } else {
                let home = self
                    .space
                    .element_geometry(&presenter.window)
                    .unwrap_or(presenter.rect);
                self.presentation.pinned = Some(Pinned {
                    presenter: presenter.id,
                    output: presenter.output.clone(),
                    home,
                    was_fullscreen: crate::requests::display_granted(&presenter.window),
                });
                tracing::info!(
                    presenter = presenter.id,
                    output = %presenter.output,
                    "the presenter's view moved to its own display"
                );
            }
            self.cover_for_presentation(&presenter);
        }
    }

    /// Have `part`'s window cover its display: configured to it and placed over it.
    fn cover_for_presentation(&mut self, part: &Part) {
        crate::requests::grant_display(&part.window, true);
        if let Some(toplevel) = part.window.toplevel() {
            toplevel.with_pending_state(|pending| {
                pending.states.set(xdg_toplevel::State::Fullscreen);
                pending.size = Some(part.rect.size);
            });
            toplevel.send_pending_configure();
        }
        let moved = self.space.element_location(&part.window) != Some(part.rect.loc)
            || self.clips.remove(&part.id).is_some();
        if moved {
            self.space
                .map_element(part.window.clone(), part.rect.loc, false);
            self.space.refresh();
            self.mark_redraw_all(tilo_frame_metrics::paint_reason::SESSION);
        }
    }

    /// Put the held presenter's window back where the policy places it, at the size it gave it.
    fn release_presenter(&mut self) {
        let Some(pinned) = self.presentation.pinned.take() else {
            return;
        };
        tracing::info!(
            presenter = pinned.presenter,
            "the presenter's view is back in its place"
        );
        let Some(window) = self.mapped_window(pinned.presenter).cloned() else {
            return;
        };
        crate::requests::grant_display(&window, pinned.was_fullscreen);
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|pending| {
                if !pinned.was_fullscreen {
                    pending.states.unset(xdg_toplevel::State::Fullscreen);
                }
                pending.size = Some(pinned.home.size);
            });
            toplevel.send_pending_configure();
        }
        self.space.map_element(window, pinned.home.loc, false);
        self.space.refresh();
        self.mark_redraw_all(tilo_frame_metrics::paint_reason::SESSION);
    }

    /// `window` asked for `request` while it is part of a presentation: an audience window naming
    /// its display moves there, and nothing else either asks for is done while it presents (the
    /// presenter's window dropping a fullscreen of its own stays on its display). Whether it was
    /// such a window, so the request goes no further.
    pub(crate) fn presentation_request(
        &mut self,
        window: &Window,
        request: &WindowRequest,
    ) -> bool {
        if self.is_audience(window) {
            if let (
                WindowRequest::Fullscreen {
                    on: true,
                    output: Some(output),
                },
                Some(info),
            ) = (request, describe(self, window))
            {
                self.presentation.asked.insert(info.id, output.clone());
                if self.mapped_window(info.id).is_some() {
                    self.rearrange();
                    return true;
                }
            }
            if let Some(toplevel) = window.toplevel()
                && toplevel.is_initial_configure_sent()
            {
                toplevel.send_pending_configure();
            }
            return true;
        }
        let Some(id) = describe(self, window).map(|info| info.id) else {
            return false;
        };
        let Some(pinned) = self
            .presentation
            .pinned
            .as_mut()
            .filter(|pinned| pinned.presenter == id)
        else {
            return false;
        };
        if let WindowRequest::Fullscreen { on, .. } = request {
            pinned.was_fullscreen = *on;
        }
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_pending_configure();
        }
        true
    }

    /// Stack the windows of a presentation in front of every other: each covers its display.
    pub(crate) fn raise_presentation(&mut self) {
        let Some(wanted) = self.wanted_presentation() else {
            return;
        };
        if let Some(presenter) = wanted.presenter {
            self.space.raise_element(&presenter.window, false);
        }
        self.space.raise_element(&wanted.audience.window, false);
    }
}
