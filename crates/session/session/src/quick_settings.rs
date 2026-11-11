//! Quick settings, as the session keeps it: the app that draws the panel runs for the session's
//! life, started with the session so the first opening only shows it, standing where the stack
//! places it, shown while the stack has its slot up and hidden between openings, and shows what
//! the session's services publish. It is as tall as the page it shows, inside the room the stack
//! gives a popover.
//! The panel is a process of its own, so nothing here crosses an `Rc` between the two: what it
//! shows of the device is the services' readings, and what a press does is the services' actions,
//! both reached over the broker as any app of the session reaches them. What is left to the session
//! is where the popover stands and whether it shows, which only the overlay stack knows.
//! Opening Settings from the panel, and from the dictation card that sends the person there, is the
//! session's too: it is the desk's own search, and the panel's app has no workspace of its own to
//! search in.

use std::path::PathBuf;

use tilo_desktop::overlays::{POPOVER_HEIGHT, POPOVER_WIDTH, Slot};
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

/// How many times the panel's app is started before it is given up on until the panel is opened
/// anew: one that will not stay up leaves the device's controls to their keys rather than starting
/// for ever.
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
        // It stands where the stack placed the slot, which the compositor is told: wherever that
        // is, it is above the windows it covers. Its app starts at the most room a popover has and
        // asks for the height of the page it shows.
        .anchor(Anchor::TOP | Anchor::LEFT)
        .size((whole(POPOVER_WIDTH), whole(POPOVER_HEIGHT)))
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

/// Whether the panel's app runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// It is to be started: it was not yet, or it exited.
    Down,
    /// It was started, and runs while its process does.
    Started,
    /// It would not stay up, or cannot start: it is not started again until the panel is opened
    /// anew.
    GivenUp,
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
    /// The popover's place is the stack's, and the compositor stands the panel there: the caret that
    /// opened it, another display, or one that changed shape moves it, and costs its app no start.
    pub(crate) fn follow_quick_settings(&mut self) {
        if !self.shared.quick_settings {
            // No app of this session draws the panel: what the stack holds is the stack's, and
            // nothing here runs to fill it.
            return;
        }
        self.follow_panel_back();
        let open = self.shared.overlays.is_open(Slot::QuickSettings);
        let place = self.shared.overlays.place(Slot::QuickSettings);
        if let Some(place) = place {
            // Where the stack places it, and the most room it has there. Down, it keeps the last
            // place, so it leaves from where it stood.
            self.compositor.place_layer_in(
                SURFACE_NAMESPACE,
                Some((
                    offset(place.x0),
                    offset(place.y0),
                    offset(place.width()).max(1),
                    offset(place.height()).max(1),
                )),
            );
        }
        // Where it stands is as tall as its page: what a press outside it is judged by.
        let stands = self
            .compositor
            .layer_rect(SURFACE_NAMESPACE)
            .map(|(x, y, width, height)| {
                Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x.saturating_add(width)),
                    f64::from(y.saturating_add(height)),
                )
            });
        self.shared
            .overlays
            .stands(Slot::QuickSettings, stands.filter(|_| open));
        let drawn = self.compositor.layer_drawn(SURFACE_NAMESPACE);
        self.quick_settings_panel
            .follow(open, drawn, stands.or(place));
        if !open || place.is_none() {
            // Down, or up where the stack cannot say: nothing shows, and its app runs on hidden, so
            // the next opening only shows it.
            self.hide_quick_settings();
            self.quick_settings_keyboard = PanelKeys::Settled;
            self.shared.panel_asks.borrow_mut().clear();
            self.shared.panel_back.set(None);
            self.run_quick_settings(false);
            return;
        }
        if !self.run_quick_settings(true) {
            return;
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

    /// Keep the panel's app running, `open` or not: started when it does not run, and given up on
    /// when it will not stay up, until the panel is opened anew. Whether it runs now.
    fn run_quick_settings(&mut self, open: bool) -> bool {
        if self.quick_settings_run == Run::GivenUp {
            if !open {
                return false;
            }
            // Opened anew: a panel that would not stay up is tried afresh.
            self.quick_settings_starts = 0;
            self.quick_settings_run = Run::Down;
        }
        if self.quick_settings_run == Run::Started {
            if self.surfaces.running(SURFACE) {
                return true;
            }
            self.quick_settings_starts = self.quick_settings_starts.saturating_add(1);
            self.quick_settings_run = Run::Down;
            if self.quick_settings_starts >= TRIES {
                tracing::error!(
                    starts = self.quick_settings_starts,
                    "quick settings would not stay up: it is not started again until the panel is opened anew"
                );
                self.give_up_quick_settings();
                return false;
            }
        }
        let Some(surface) = self
            .surface_apps
            .iter()
            .find(|surface| surface.name == SURFACE)
            .cloned()
        else {
            // The app was installed when the session started and is gone from its list now:
            // nothing draws the panel.
            tracing::warn!("no quick settings app is installed");
            self.give_up_quick_settings();
            return false;
        };
        if !open {
            // Started between openings, it stands hidden from its first frame.
            self.hide_quick_settings();
        }
        if let Err(error) = self.start_surface(&surface) {
            tracing::warn!(%error, "quick settings did not start");
            self.give_up_quick_settings();
            return false;
        }
        tracing::debug!(open, "quick settings started");
        self.quick_settings_run = Run::Started;
        true
    }

    /// Stop trying to run the panel's app until the panel is opened anew; an open panel closes.
    fn give_up_quick_settings(&mut self) {
        self.quick_settings_run = Run::GivenUp;
        self.shared.overlays.close(Slot::QuickSettings);
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

/// Which controls the panel shows, and in what order: the person's own setting of this device,
/// published as an action so that a press in the panel, an agent and a script change it the same
/// way, whether the panel is drawn or not.
pub const ARRANGEMENT: &str = "session:quick-settings";
/// What the arrangement resource is, to a reader with no words of its own.
const ARRANGEMENT_KIND: &str = "session.quick-settings";

/// What the arrangement resource offers: the controls the panel shows.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.quick-settings")]
pub(crate) enum ArrangementAction {
    /// Show a control on the panel
    #[effect(own_device, idempotent)]
    ShowControl {
        /// The control's name, as the panel's arrangement lists it
        #[param(
            "The control: `wifi`, `bluetooth`, `vpn`, `night_light`, `airplane_mode`, `mobile_data`, `do_not_disturb`, `rotation_lock` or `power_mode`"
        )]
        control: String,
    },
    /// Take a control off the panel. Wi-Fi stays: it is how a desktop without a network gets online
    #[effect(own_device, idempotent)]
    HideControl {
        /// The control's name, as the panel's arrangement lists it
        #[param("The control, as Show control names it")]
        control: String,
    },
    /// Move a control to another place on the panel
    #[effect(own_device, idempotent)]
    MoveControl {
        /// The control's name, as the panel's arrangement lists it
        #[param("The control, as Show control names it")]
        control: String,
        /// The control it stands in front of; none for the last place
        #[param("The control it stands in front of; none for the last place")]
        before: Option<String>,
    },
    /// Put the panel's controls back as they were at first
    #[effect(own_device)]
    ResetControls,
}

/// The arrangement resource: which controls the panel shows, read by the panel and by anyone else
/// who asks.
fn arrangement_resource() -> tilo_actions::ResourceSpec {
    tilo_service::offer::<ArrangementAction>(
        tilo_actions::ResourceSpec::new(
            ARRANGEMENT,
            ARRANGEMENT_KIND,
            crate::i18n::t("session-quick-settings"),
        )
        .with_granted_reading(),
    )
}

/// Which controls the panel shows, as the panel reads them: the person's arrangement of this
/// device, kept in its settings document.
#[derive(Default)]
pub(crate) struct QuickSettingsService {
    /// The arrangement as last read, published as the reading.
    shown: std::cell::RefCell<settings_store::hardware::QuickSettingsConfig>,
    /// Whether the settings document was read at all, and the version that was read: one that did
    /// not move is not read again.
    seen: bool,
    stamp: Option<settings_store::SettingsStamp>,
}

impl QuickSettingsService {
    /// The service, holding the person's arrangement as their settings document has it.
    pub(crate) fn new() -> Self {
        let mut service = Self::default();
        service.follow();
        service
    }

    /// Read the person's arrangement, and say the resource changed when it is not what was
    /// published.
    fn follow(&mut self) -> bool {
        let stamp = settings_store::stamp();
        if !settings_store::document_changed(self.seen, self.stamp, stamp) {
            return false;
        }
        self.seen = true;
        self.stamp = stamp;
        let arrangement = settings_store::hardware::QuickSettingsConfig::load().normalised();
        let changed = *self.shown.borrow() != arrangement;
        *self.shown.borrow_mut() = arrangement;
        changed
    }

    /// Carry `action` out on the person's arrangement, and keep it: whether it changed.
    fn carry(&mut self, action: &ArrangementAction) -> Result<bool, tilo_actions::DoorError> {
        let mut arrangement = self.shown.borrow().clone();
        let changed = match action {
            ArrangementAction::ShowControl { control } => {
                tilo_quick_settings::host::show_control(&mut arrangement, control)
            }
            ArrangementAction::HideControl { control } => {
                tilo_quick_settings::host::hide_control(&mut arrangement, control)
            }
            ArrangementAction::MoveControl { control, before } => {
                tilo_quick_settings::host::move_control(
                    &mut arrangement,
                    control,
                    before.as_deref(),
                )
            }
            ArrangementAction::ResetControls => {
                tilo_quick_settings::host::reset_controls(&mut arrangement)
            }
        };
        if !changed {
            return Ok(false);
        }
        arrangement.save().map_err(|error| {
            tilo_actions::DoorError::Failed(format!(
                "the Quick Settings arrangement was not kept: {error}"
            ))
        })?;
        *self.shown.borrow_mut() = arrangement;
        self.stamp = settings_store::stamp();
        Ok(true)
    }
}

impl tilo_service::Service for QuickSettingsService {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![arrangement_resource()]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == ARRANGEMENT).then(|| {
            serde_json::json!({
                "controls": self
                    .shown
                    .borrow()
                    .controls
                    .iter()
                    .map(|control| tilo_quick_settings::control_id(*control))
                    .collect::<Vec<_>>(),
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        let action = tilo_service::decode::<ArrangementAction>(
            &self.resources(),
            resource,
            action,
            arguments,
        )?;
        self.carry(&action)?;
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn poll(&mut self) -> bool {
        self.follow()
    }
}

#[cfg(test)]
mod tests {
    use tilo_service::Service as _;

    use super::*;

    /// Ask the arrangement resource `action` with `arguments`, as the door decodes it.
    fn asked(service: &mut QuickSettingsService, action: &str, arguments: &serde_json::Value) {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        service
            .invoke(ARRANGEMENT, action, &arguments)
            .expect("the arrangement takes it");
    }

    /// The controls the arrangement publishes now.
    fn shown(service: &QuickSettingsService) -> String {
        service
            .read(ARRANGEMENT)
            .expect("the arrangement is published")["controls"]
            .to_string()
    }

    /// The arrangement is the person's own setting, so it is read, changed and kept whether the
    /// panel is drawn or not, and a control moves in front of the one it was dropped on.
    #[test]
    fn the_arrangement_is_published_by_the_session_and_kept() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let mut service = QuickSettingsService::new();
        assert!(shown(&service).contains("bluetooth"), "{}", shown(&service));

        asked(
            &mut service,
            "hide-control",
            &serde_json::json!({ "control": "bluetooth" }),
        );
        assert!(
            !shown(&service).contains("bluetooth"),
            "{}",
            shown(&service)
        );
        asked(
            &mut service,
            "hide-control",
            &serde_json::json!({ "control": "wifi" }),
        );
        assert!(
            shown(&service).contains("wifi"),
            "Wi-Fi stays: {}",
            shown(&service)
        );
        asked(
            &mut service,
            "move-control",
            &serde_json::json!({ "control": "vpn", "before": "wifi" }),
        );
        assert!(
            shown(&service).starts_with(r#"["vpn","wifi""#),
            "the control stands in front of the one it was dropped on: {}",
            shown(&service)
        );
        asked(&mut service, "reset-controls", &serde_json::json!({}));
        assert!(shown(&service).contains("bluetooth"), "{}", shown(&service));

        // Kept: the session reads it back from the person's settings document, as a panel that
        // starts later does.
        let kept = QuickSettingsService::new();
        assert_eq!(shown(&kept), shown(&service));
    }

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
