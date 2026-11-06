//! Quick settings, as the session keeps it: the app that draws the panel runs
//! for the session's life, standing where the stack places it, shown while the
//! stack has its slot up and hidden between openings, and shows what the
//! session's services publish.
//!
//! The panel is a process of its own, so nothing here crosses an `Rc` between
//! the two: what it shows of the device is the services' readings, and what a
//! press does is the services' actions, both reached over the broker as any
//! app of the session reaches them. What is left to the session is where the
//! popover stands and whether it shows, which only the overlay stack knows.
//!
//! Opening Settings from the panel, and from the dictation card that sends the
//! person there, is the session's too: it is the desk's own search, and the
//! panel's app has no workspace of its own to search in.

use std::path::PathBuf;

use tilo_desktop::overlays::Slot;
use tilo_desktop_surface::navigation::ChromeEffect;
use ui::kurbo::Rect;

use crate::Shared;
use crate::surfaces::{Anchor, Keyboard, Layer, StartError, Surface, program_path};

/// What the session calls quick settings' app: its name in the log, the key
/// it stops by, and the program it is installed as.
pub const SURFACE: &str = "quick-settings";

/// Its program, as it is installed beside the session's own.
const PROGRAM: &str = "tilo-quick-settings";

/// The name its layer surface asks the compositor for, which the session
/// shows and hides as its slot opens and closes.
pub const SURFACE_NAMESPACE: &str = "tilo-quick-settings";

/// How many times the panel's app is started for the same place before it is
/// given up on: one that will not stay up leaves the device's controls to
/// their keys rather than starting for ever.
const TRIES: u32 = 3;

/// The settings app, which quick settings opens.
const SETTINGS_APP: &str = "so.tilo.settings";

/// Where Settings installs the local speech models dictation needs: the page
/// of the models the person's devices run themselves.
const SPEECH_MODELS_ANCHOR: &str = "settings.section:agent_models";

/// Quick settings' surface app, when one is installed: a window of its own
/// that the session places where the stack placed the slot, and that takes the
/// keyboard when the person works it.
///
/// # Errors
/// No quick settings app is installed, or its program is not there.
pub fn surface() -> Result<Surface, StartError> {
    Ok(surface_of(program_path(PROGRAM)?))
}

/// Quick settings' surface app, run from `program`.
#[must_use]
pub fn surface_of(program: impl Into<PathBuf>) -> Surface {
    Surface::new(SURFACE, program)
        .layer(Layer::Overlay)
        // It stands where the stack placed the slot: wherever that is, it is
        // above the windows it covers.
        .anchor(Anchor::TOP | Anchor::LEFT)
        // Its field takes the keyboard when the person presses it, and the
        // window they were typing in keeps it until then.
        .keyboard(Keyboard::OnDemand)
}

/// Whether a quick settings app is among the session's surface apps: what the
/// bar's status keys and the desk's own status answer from.
#[must_use]
pub fn installed(surfaces: &[Surface]) -> bool {
    surfaces.iter().any(|surface| surface.name == SURFACE)
}

/// Where the panel's app stands while the stack has the slot up: the display
/// it is placed on, its size, and its margin from that display's edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// It does not run: the panel is down, or its app is not installed.
    Down,
    /// It runs where the stack placed the slot.
    Standing {
        /// The display it stands on.
        output: String,
        /// Its size in logical pixels.
        size: (u32, u32),
        /// Its margin from the display's edges: top, right, bottom, left.
        margin: (i32, i32, i32, i32),
    },
}

impl crate::Session {
    /// Follow quick settings: its app runs for the session's life, standing
    /// where the stack places it, shown while the stack has the slot up and
    /// hidden between openings, and is asked for the page the keys asked
    /// for.
    ///
    /// The popover's place is the stack's: the app is started again when the
    /// place moved under it — the caret that opened it, another display, or
    /// one that changed shape — as the on-screen keyboard is. Opening it
    /// where it already stands costs no start.
    pub(crate) fn follow_quick_settings(&mut self) {
        if !self.shared.quick_settings {
            // No app of this session draws the panel: what the stack holds is
            // the stack's, and nothing here runs to fill it.
            return;
        }
        self.follow_panel_back();
        let open = self.shared.overlays.is_open(Slot::QuickSettings);
        if !open {
            self.hide_quick_settings();
            // Asked again when the panel opens again, a panel that would not
            // stay up is tried afresh.
            self.quick_settings_starts = 0;
            self.shared.panel_asks.borrow_mut().clear();
            self.shared.panel_back.set(None);
            return;
        }
        let Some(place) = self.shared.overlays.place(Slot::QuickSettings) else {
            // The stack has it up where it cannot say: nothing to place the
            // app at.
            self.stop_quick_settings();
            return;
        };
        let Some(standing) = self.quick_settings_run_where(place) else {
            tracing::warn!("quick settings asked for a display this session does not show");
            self.stop_quick_settings();
            return;
        };
        // The app may have exited since the last pass: it stands on nothing
        // then, and is started again where the stack places it.
        if self.quick_settings_run != Run::Down && !self.surfaces.running(SURFACE) {
            self.quick_settings_run = Run::Down;
            self.quick_settings_starts = self.quick_settings_starts.saturating_add(1);
            if self.quick_settings_starts >= TRIES {
                tracing::error!(
                    starts = self.quick_settings_starts,
                    "quick settings would not stay up: it is not started again until the panel is opened anew"
                );
                self.shared.overlays.close(Slot::QuickSettings);
                return;
            }
        }
        if self.quick_settings_run != standing {
            if self.quick_settings_run != Run::Down {
                // Another display, or one that changed shape: a fresh start,
                // and its tries with it.
                self.quick_settings_starts = 0;
                self.stop_quick_settings();
            }
            let Some(surface) = self
                .surface_apps
                .iter()
                .find(|surface| surface.name == SURFACE)
            else {
                // The app was installed when the session started and is gone
                // from its list now: nothing draws the panel.
                tracing::warn!("no quick settings app is installed");
                return;
            };
            let Run::Standing {
                output,
                size,
                margin,
            } = &standing
            else {
                return;
            };
            let placed = surface
                .clone()
                .on(output.clone())
                .size(*size)
                .margin(*margin);
            if let Err(error) = self.start_surface(&placed) {
                tracing::warn!(%error, "quick settings did not start");
                self.shared.overlays.close(Slot::QuickSettings);
                return;
            }
            tracing::debug!(%output, ?size, "quick settings stands");
            self.quick_settings_run = standing;
        }
        // Up, where it stands: the surface is shown rather than started, so
        // an opening the panel already stands for costs nothing.
        if self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true) {
            self.compositor.show_layer(SURFACE_NAMESPACE, true);
            // An opening nobody asked a page of shows the controls, not the
            // page the panel was left on.
            if self.shared.panel_asks.borrow().is_empty() {
                self.shared.panel_asks.borrow_mut().push("show-controls");
            }
        }
        // What the keys asked the panel to show, which it can only be told
        // while it runs.
        let asked = std::mem::take(&mut *self.shared.panel_asks.borrow_mut());
        for action in asked {
            let id = self.ask_surface(
                SURFACE,
                crate::keys::QUICK_SETTINGS,
                action,
                tilo_actions::remote::InvokeArgument::None,
            );
            match (action, id) {
                ("back", Some(id)) => self.shared.panel_back.set(Some(id)),
                // A panel that does not run cannot go back: Escape closes
                // what is open.
                ("back", None) => self.shared.overlays.close(Slot::QuickSettings),
                (_, None) => {
                    tracing::debug!(action, "quick settings was not asked: it does not run");
                }
                (_, Some(_)) => {}
            }
        }
    }

    /// Take what the panel answered when Escape asked it to go back: the
    /// panel closes from its main page, and says so by having nothing left to
    /// go back to (`{"handled": false}`).
    fn follow_panel_back(&self) {
        let Some(id) = self.shared.panel_back.get() else {
            return;
        };
        let Some(state) = self.surfaces.outcome(id) else {
            return;
        };
        match &state.update {
            tilo_actions::InvocationUpdate::Accepted { .. }
            | tilo_actions::InvocationUpdate::Progress { .. }
            | tilo_actions::InvocationUpdate::Item { .. } => return,
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                let handled = output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("handled"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if !handled {
                    self.shared.overlays.close(Slot::QuickSettings);
                }
            }
            // A panel that could not go back still hears the person's Escape:
            // it closes.
            tilo_actions::InvocationUpdate::Failed { .. }
            | tilo_actions::InvocationUpdate::Cancelled { .. } => {
                self.shared.overlays.close(Slot::QuickSettings);
            }
        }
        self.shared.panel_back.set(None);
    }

    /// Hide the panel: its app keeps running, and opening it again shows the
    /// surface it already stands on.
    fn hide_quick_settings(&mut self) {
        self.compositor.show_layer(SURFACE_NAMESPACE, false);
    }

    /// Stop quick settings' app, if it runs: it stands somewhere else now.
    pub(crate) fn stop_quick_settings(&mut self) {
        if self.quick_settings_run != Run::Down {
            self.quick_settings_run = Run::Down;
            self.surfaces.stop(SURFACE);
        }
    }

    /// Where the panel's app stands, as the stack placed it: the display the
    /// place is on, its size, and the margin from that display's edge.
    fn quick_settings_run_where(&self, place: Rect) -> Option<Run> {
        let displays: Vec<(String, Rect)> = self
            .compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .filter_map(|output| {
                let (x, y, width, height) = output.rect?;
                Some((
                    output.name,
                    Rect::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(x.saturating_add(width)),
                        f64::from(y.saturating_add(height)),
                    ),
                ))
            })
            .collect();
        let center = place.center();
        let (output, display) = displays
            .iter()
            .find(|(_, display)| display.contains(center))
            .or_else(|| displays.first())?;
        Some(Run::Standing {
            output: output.clone(),
            size: (whole(place.width()), whole(place.height())),
            margin: (
                offset(place.y0 - display.y0),
                0,
                0,
                offset(place.x0 - display.x0),
            ),
        })
    }
}

/// `length` logical pixels as a whole number of them, never below one.
fn whole(length: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a placed surface is at most a few thousand logical pixels"
    )]
    let length = length.round().max(1.0) as u32;
    length
}

/// `length` logical pixels as a signed whole number of them.
fn offset(length: f64) -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a placed surface is at most a few thousand logical pixels"
    )]
    let offset = length.round() as i32;
    offset
}

/// Close the panel: what a press that takes the person elsewhere does first.
pub(crate) fn close(shared: &Shared) {
    shared.overlays.close(Slot::QuickSettings);
}

/// Take the person where the speech models are set up when the dictation
/// card's "Set up" was pressed: Settings at its models page, through the
/// action path search rows use; a plain launch opens Settings too when no
/// action host runs an app's actions.
pub(crate) fn follow_dictation(shared: &Shared) {
    if !shared.dictation.take_model_setup() {
        return;
    }
    open_settings(shared, SPEECH_MODELS_ANCHOR);
}

/// Open Settings at the page `link` names — a `settings://` page or a page's
/// anchor — through the action path search rows run; a plain launch opens
/// Settings itself too when no action host runs an app's actions.
pub(crate) fn open_settings(shared: &Shared, link: &str) {
    close(shared);
    let workspace = shared.active_workspace_id();
    let anchor = match link.strip_prefix("settings://") {
        Some(slug) => Some(format!("settings.section:{slug}")),
        None => (!link.is_empty()).then(|| link.to_owned()),
    };
    if let Some(anchor) = anchor {
        let target = tilo_desktop_search::Target::Invoke(Box::new(
            tilo_desktop_search::InvokeTarget::open(SETTINGS_APP, workspace.clone(), anchor),
        ));
        let opened = shared
            .search
            .perform_checked(&workspace, target, &mut |own| {
                shared
                    .doors
                    .invoke(
                        &own.address,
                        &own.arguments,
                        &mut tilo_surface_host::Look::none(),
                    )
                    .map(drop)
            });
        if opened.is_ok() {
            return;
        }
    }
    let launch = ChromeEffect::OpenApp {
        app: SETTINGS_APP.into(),
        beside: None,
        background: None,
    };
    if let Err(error) = shared.handle.dispatch(launch) {
        // The person asked for Settings and it did not open: they hear why.
        shared.answer_failed(&crate::i18n::t("action-failed-that-settings"), &error);
    }
}
