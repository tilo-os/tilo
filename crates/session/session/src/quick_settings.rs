//! Quick settings, as the session keeps it: the app that draws the panel runs for the session's
//! life, standing where the stack places it, shown while the stack has its slot up and hidden
//! between openings, and shows what the session's services publish.
//! The panel is a process of its own, so nothing here crosses an `Rc` between the two: what it
//! shows of the device is the services' readings, and what a press does is the services' actions,
//! both reached over the broker as any app of the session reaches them. What is left to the session
//! is where the popover stands and whether it shows, which only the overlay stack knows.
//! Opening Settings from the panel, and from the dictation card that sends the person there, is the
//! session's too: it is the desk's own search, and the panel's app has no workspace of its own to
//! search in.

use std::path::PathBuf;

use tilo_desktop::overlays::Slot;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_session_core::surface::LayerPose;
use ui::kurbo::Rect;
use ui::motion::{Appear, Appearing};
use ui::tokens::EffectiveMotion;

use crate::Shared;
use crate::surfaces::{Anchor, Keyboard, Layer, StartError, Surface, program_path};

/// What the session calls quick settings' app: its name in the log, the key it stops by, and the
/// program it is installed as.
pub const SURFACE: &str = "quick-settings";

/// Its program, as it is installed beside the session's own.
const PROGRAM: &str = "tilo-quick-settings";

/// The name its layer surface asks the compositor for, which the session shows and hides as its
/// slot opens and closes.
pub const SURFACE_NAMESPACE: &str = "tilo-quick-settings";

/// How many times the panel's app is started for the same place before it is given up on: one that
/// will not stay up leaves the device's controls to their keys rather than starting for ever.
const TRIES: u32 = 3;

/// The settings app, which quick settings opens.
const SETTINGS_APP: &str = "so.tilo.settings";

/// Where Settings installs the local speech models dictation needs: the page of the models the
/// person's devices run themselves.
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

/// Quick settings' surface app, run from `program` .
#[must_use]
pub fn surface_of(program: impl Into<PathBuf>) -> Surface {
    Surface::new(SURFACE, program)
        .layer(Layer::Overlay)
        // It stands where the stack placed the slot: wherever that is, it is above the windows it
        // covers.
        .anchor(Anchor::TOP | Anchor::LEFT)
        // Its field takes the keyboard when the person presses it, and the window they were typing
        // in keeps it until then.
        .keyboard(Keyboard::OnDemand)
}

/// Whether a quick settings app is among the session's surface apps: what the bar's status keys and
/// the desk's own status answer from.
#[must_use]
pub fn installed(surfaces: &[Surface]) -> bool {
    surfaces.iter().any(|surface| surface.name == SURFACE)
}

/// Where the panel's app stands while the stack has the slot up: the display it is placed on, its
/// size, and its margin from that display's edge.
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

/// Whether the panel, opened for its controls, is still to take the keyboard once its surface
/// stands, or that is settled: it took it, or the field the person types in keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelKeys {
    Wanted,
    Settled,
}

/// The panel's own coming and going, which the compositor draws for it: its app is another
/// process's layer surface, hidden here between openings, so it cannot tell an opening from a
/// hiding and draws as it stands. The session poses it on the toolkit's [`Appear`] motion instead,
/// every frame it moves, from the corner its slot anchors it by.
pub(crate) struct PanelMotion {
    appear: Appearing,
    motion: EffectiveMotion,
    /// The panel's own rectangle, at its origin: what the pose scales and offsets in ([`Appearing`]
    /// works in the surface's own coordinates).
    surface: Rect,
}

/// The corner the panel's slot anchors it by (`Anchor::TopTrailing`): it grows out of and shrinks
/// back into the display's top-right corner, where the status area is.
const ANCHOR: (f64, f64) = (1.0, 0.0);

impl PanelMotion {
    pub(crate) fn new(motion: EffectiveMotion) -> Self {
        Self {
            appear: Appearing::closed(),
            motion,
            surface: Rect::ZERO,
        }
    }

    /// Follow the slot: `up` whether the stack has the panel open, `drawn` whether its app has
    /// drawn it yet, and `place` where the stack put it. A panel that is up but not drawn has no
    /// first frame to grow from, so its arrival starts with that frame rather than running unseen
    /// while its app starts.
    pub(crate) fn follow(&mut self, up: bool, drawn: bool, place: Option<Rect>) {
        if let Some(place) = place {
            self.surface = Rect::new(0.0, 0.0, place.width(), place.height());
        }
        if up && !drawn && !self.appear.is_open() {
            return;
        }
        self.appear.follow(up, self.motion);
    }

    /// Whether anything of the panel is drawn now: up, opening, or still going.
    pub(crate) fn is_drawn(&self) -> bool {
        self.appear.is_drawn()
    }

    /// When the panel's next pose is due, while it moves: the pass that draws it again. Its app
    /// draws its own pixels once and the pose is this session's, so nothing else would ask for that
    /// pass, and a panel that stopped after its first frame would never finish arriving or leaving.
    pub(crate) fn next_pass(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.appear.is_moving().then_some(now)
    }

    /// How the compositor draws it this frame; `None` while it stands in place. Reduced motion only
    /// fades it, as [`Appear`] says.
    pub(crate) fn pose(&self) -> Option<LayerPose> {
        let progress = self.appear.progress();
        if progress >= 1.0 {
            return None;
        }
        let coming = Appear::coming(
            progress,
            ui::motion::Direction::of(self.appear.is_open()),
            self.surface,
            ui::kurbo::Point::new(self.surface.x1, self.surface.y0),
            self.motion,
        );
        Some(LayerPose {
            scale: coming.scale,
            opacity: coming.opacity,
            anchor: ANCHOR,
            offset: (coming.offset.x, coming.offset.y),
        })
    }
}

impl crate::Session {
    /// Follow quick settings: its app runs for the session's life, standing where the stack places
    /// it, shown while the stack has the slot up and hidden between openings, and is asked for the
    /// page the keys asked for.
    /// The popover's place is the stack's: the app is started again when the place moved under it —
    /// the caret that opened it, another display, or one that changed shape — as the on-screen
    /// keyboard is. Opening it where it already stands costs no start.
    pub(crate) fn follow_quick_settings(&mut self) {
        if !self.shared.quick_settings {
            // No app of this session draws the panel: what the stack holds is the stack's, and
            // nothing here runs to fill it.
            return;
        }
        self.follow_panel_back();
        let open = self.shared.overlays.is_open(Slot::QuickSettings);
        let drawn = self.compositor.layer_drawn(SURFACE_NAMESPACE);
        let place = self.shared.overlays.place(Slot::QuickSettings);
        self.quick_settings_panel.follow(open, drawn, place);
        if !open {
            self.hide_quick_settings();
            // Asked again when the panel opens again, a panel that would not stay up is tried
            // afresh.
            self.quick_settings_starts = 0;
            self.quick_settings_keyboard = PanelKeys::Settled;
            self.shared.panel_asks.borrow_mut().clear();
            self.shared.panel_back.set(None);
            return;
        }
        let Some(place) = place else {
            // The stack has it up where it cannot say: nothing to place the app at.
            self.stop_quick_settings();
            return;
        };
        let Some(standing) = self.quick_settings_run_where(place) else {
            tracing::warn!("quick settings asked for a display this session does not show");
            self.stop_quick_settings();
            return;
        };
        // The app may have exited since the last pass: it stands on nothing then, and is started
        // again where the stack places it.
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
                // The app was installed when the session started and is gone from its list now:
                // nothing draws the panel.
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
        // Up, where it stands: the surface is shown rather than started, so an opening the panel
        // already stands for costs nothing.
        let opening = self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true);
        if opening && self.shared.panel_asks.borrow().is_empty() {
            // An opening nobody asked a page of shows the controls, not the page the panel was left
            // on.
            self.shared.panel_asks.borrow_mut().push("show-controls");
        }
        self.show_quick_settings();
        // What the keys asked the panel to show, which it can only be told while it runs.
        let asked = std::mem::take(&mut *self.shared.panel_asks.borrow_mut());
        // Opened for its controls, the panel takes the keyboard: Tab walks them, Enter opens a
        // section. Opened to paste, the field the person types in keeps it, so the copy they
        // choose lands there.
        if opening {
            self.quick_settings_keyboard = if asked.contains(&"paste") {
                PanelKeys::Settled
            } else {
                PanelKeys::Wanted
            };
        }
        if self.quick_settings_keyboard == PanelKeys::Wanted
            && self.compositor.lend_layer_keyboard(SURFACE_NAMESPACE)
        {
            self.quick_settings_keyboard = PanelKeys::Settled;
        }
        self.ask_panel(asked);
    }

    /// Tell the running panel what the keys asked of it, in order.
    fn ask_panel(&mut self, asked: Vec<&'static str>) {
        for action in asked {
            let id = self.ask_surface(
                SURFACE,
                crate::keys::QUICK_SETTINGS,
                action,
                tilo_actions::remote::InvokeArgument::None,
            );
            match (action, id) {
                ("back", Some(id)) => self.shared.panel_back.set(Some(id)),
                // A panel that does not run cannot go back: Escape closes what is open.
                ("back", None) => self.shared.overlays.close(Slot::QuickSettings),
                (_, None) => {
                    tracing::debug!(action, "quick settings was not asked: it does not run");
                }
                (_, Some(_)) => {}
            }
        }
    }

    /// Take what the panel answered when Escape asked it to go back: the panel closes from its main
    /// page, and says so by having nothing left to go back to (`{"handled": false}`).
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
            // A panel that could not go back still hears the person's Escape: it closes.
            tilo_actions::InvocationUpdate::Failed { .. }
            | tilo_actions::InvocationUpdate::Cancelled { .. } => {
                self.shared.overlays.close(Slot::QuickSettings);
            }
        }
        self.shared.panel_back.set(None);
    }

    /// Show the panel where it stands, posed as far as it has come.
    fn show_quick_settings(&mut self) {
        self.compositor
            .set_layer_pose(SURFACE_NAMESPACE, self.quick_settings_panel.pose());
        if self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true) {
            self.compositor.show_layer(SURFACE_NAMESPACE, true);
        }
    }

    /// Hide the panel: its app keeps running, and opening it again shows the surface it already
    /// stands on. While it leaves it stays shown, shrinking back into its corner, and is hidden
    /// once nothing of it is left.
    fn hide_quick_settings(&mut self) {
        if self.quick_settings_panel.is_drawn() {
            self.compositor
                .set_layer_pose(SURFACE_NAMESPACE, self.quick_settings_panel.pose());
            if self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true) {
                self.compositor.show_layer(SURFACE_NAMESPACE, true);
            }
            return;
        }
        self.compositor.set_layer_pose(SURFACE_NAMESPACE, None);
        if self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(false) {
            self.compositor.show_layer(SURFACE_NAMESPACE, false);
        }
    }

    /// Stop quick settings' app, if it runs: it stands somewhere else now.
    pub(crate) fn stop_quick_settings(&mut self) {
        if self.quick_settings_run != Run::Down {
            self.quick_settings_run = Run::Down;
            self.surfaces.stop(SURFACE);
        }
    }

    /// Where the panel's app stands, as the stack placed it: the display the place is on, its size,
    /// and the margin from that display's edge.
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

/// Take the person where the speech models are set up when the dictation card's "Set up" was
/// pressed: Settings at its models page, through the action path search rows use; a plain launch
/// opens Settings too when no action host runs an app's actions.
pub(crate) fn follow_dictation(shared: &Shared) {
    if !shared.dictation.take_model_setup() {
        return;
    }
    open_settings(shared, SPEECH_MODELS_ANCHOR);
}

/// Open Settings at the page `link` names — a `settings://` page or a page's anchor — through the
/// action path search rows run; a plain launch opens Settings itself too when no action host runs
/// an app's actions.
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
                        &mut tilo_session_core::door::Look::none(),
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
        float: None,
    };
    if let Err(error) = shared.handle.dispatch(launch) {
        // The person asked for Settings and it did not open: they hear why.
        shared.answer_failed(&crate::i18n::t("action-failed-that-settings"), &error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel grows from its corner when its first frame is drawn, stands as it stands, and
    /// shrinks back into the corner while it leaves, until it asks for nothing at all.
    #[test]
    fn the_panel_arrives_from_its_corner_and_leaves_back_into_it() {
        use ui::time::testing::with_monotonic_time;
        use ui::tokens::MotionSource;
        let t0 = ui::time::now();
        let at = |ms: u64| t0 + ui::time::Duration::from_millis(ms);
        let mut panel = PanelMotion::new(EffectiveMotion::default());
        // The place the stack gives it: 380 x 640 at the display's right.
        let place = Some(Rect::new(892.0, 32.0, 1272.0, 672.0));
        // Every read of the motion is pinned at the instant it is read at.
        let look = |panel: &PanelMotion, ms: u64| {
            with_monotonic_time(at(ms), || (panel.is_drawn(), panel.pose()))
        };
        let asks = |panel: &PanelMotion, ms: u64| {
            with_monotonic_time(at(ms), || panel.next_pass(at(ms)).is_some())
        };

        // Up while its app has not drawn it yet: out of sight, not moving.
        with_monotonic_time(t0, || panel.follow(true, false, place));
        let (drawn, waiting) = look(&panel, 0);
        assert!(!drawn, "nothing to animate before its first frame");
        let waiting = waiting.expect("it waits invisible");
        assert!(waiting.opacity < 0.01 && waiting.scale < 1.0, "{waiting:?}");
        assert!(!asks(&panel, 0), "nothing moves, so no pass is due");

        ui::motion::note_source(MotionSource::Pointer);
        with_monotonic_time(t0, || panel.follow(true, true, place));
        let (drawn, coming) = look(&panel, 0);
        assert!(drawn);
        let coming = coming.expect("it arrives");
        assert!(
            coming.opacity < 0.05 && coming.scale < 1.0,
            "from faint and small: {coming:?}"
        );
        assert!(
            coming.offset.0 > 0.0 && coming.offset.1 < 0.0,
            "and from the corner the status area is in, up and to the right: {coming:?}"
        );
        assert!(
            asks(&panel, 0),
            "an arriving panel asks for the pass that moves it"
        );

        assert_eq!(
            look(&panel, 600).1,
            None,
            "at rest it is drawn as it stands"
        );
        assert!(!asks(&panel, 600), "and asks for no pass of its own");

        with_monotonic_time(at(700), || panel.follow(false, true, place));
        assert!(look(&panel, 700).0, "it leaves rather than vanishing");
        // Its leave is a spring settling within `fast` : most of the way out inside the first tenth
        // of a second, so it is read where it is part way rather than near the end of that.
        let leaving = look(&panel, 740).1.expect("its leave is drawn");
        assert!(
            leaving.opacity > 0.05 && leaving.opacity < 0.95 && leaving.scale < 1.0,
            "partway back into its corner: {leaving:?}"
        );
        assert!(
            asks(&panel, 740),
            "a leaving panel asks for the pass that moves it"
        );
        assert!(!look(&panel, 900).0, "and then it has gone");
    }
}
