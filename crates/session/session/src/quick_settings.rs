//! Quick settings, as the session keeps it: the app that draws the panel runs for the session's
//! life, started with the session so the first opening only shows it, where the stack
//! places it, shown while the stack has its part up and hidden between openings, and shows what
//! the session's services publish. It is as tall as the page it shows, inside the room the stack
//! gives a popover.
//! The panel is a process of its own, so nothing here crosses an `Rc` between the two: what it
//! shows of the device is the services' readings, and what a press does is the services' actions,
//! both reached over the broker as any app of the session reaches them. What is left to the session
//! is where the popover stands and whether it shows, which only the overlay stack knows.
//! Opening Settings from the panel, and from the dictation card that sends the person there, is the
//! session's too: it is the shell's own search, and the panel's app has no workspace of its own to
//! search in.

use std::path::PathBuf;

use tilo_desktop::overlays::{POPOVER_HEIGHT, POPOVER_WIDTH, Part};
use tilo_desktop_search::providers::settings::SETTINGS_APP;
use tilo_session_core::desktop::navigation::ChromeEffect;
use tilo_session_core::surface::LayerPose;
use ui::kurbo::{Point, Rect};
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
/// part opens and closes.
pub const SURFACE_NAMESPACE: &str = "tilo-quick-settings";

/// How many times the panel's app is started before it is given up on until the panel is opened
/// anew: one that will not stay up leaves the device's controls to their keys rather than starting
/// for ever.
const TRIES: u32 = 3;

/// Where Settings installs the local speech models dictation needs: the page of the models the
/// person's devices run themselves.
const SPEECH_MODELS_ANCHOR: &str = "settings.section:agent_models";

/// Quick settings' surface app, when one is installed: a window of its own
/// that the session places where the stack placed the part, and that takes the
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
        // It stands where the stack placed the part, which the compositor is told: wherever that
        // is, it is above the windows it covers. Its app starts at a popover's starting size and
        // asks for the height of the page it shows, which the room down to the display's margin
        // holds.
        .anchor(Anchor::TOP | Anchor::LEFT)
        .size((whole(POPOVER_WIDTH), whole(POPOVER_HEIGHT)))
        // Its field takes the keyboard when the person presses it, and the window they were typing
        // in keeps it until then.
        .keyboard(Keyboard::OnDemand)
}

/// Whether a quick settings app is among the session's surface apps: what the bar's status keys and
/// the shell's own status answer from.
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

/// Whether the panel is still to take the keyboard once the person sees it, or it took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelKeys {
    Wanted,
    Settled,
}

/// The panel's own coming and going, which the compositor draws for it: its app is another
/// process's layer surface, hidden here between openings, so it cannot tell an opening from a
/// hiding and draws as it stands. The session poses it on the toolkit's [`Appear`] motion instead,
/// every frame it moves, from where its part was opened: the bar's status area, or the caret.
pub(crate) struct PanelMotion {
    appear: Appearing,
    motion: EffectiveMotion,
    /// The panel's own rectangle, at its origin: what the pose scales and offsets in ([`Appearing`]
    /// works in the surface's own coordinates).
    surface: Rect,
    /// Where it comes from, in the surface's own coordinates.
    from: Point,
}

impl PanelMotion {
    pub(crate) fn new(motion: EffectiveMotion) -> Self {
        Self {
            appear: Appearing::closed(),
            motion,
            surface: Rect::ZERO,
            from: Point::ZERO,
        }
    }

    /// Follow the part: `up` whether the stack has the panel open, `seen` whether the person sees
    /// the frame its app drew for this opening, and `place` where it stands and where it was
    /// opened from. A panel that is up but not seen has no first frame to grow from, so its
    /// arrival starts with that frame rather than running unseen while its app draws the page it
    /// was asked for.
    pub(crate) fn follow(&mut self, up: bool, seen: bool, place: Option<(Rect, Point)>) {
        if let Some((place, from)) = place {
            self.surface = Rect::new(0.0, 0.0, place.width(), place.height());
            self.from = Point::new(from.x - place.x0, from.y - place.y0);
        }
        if up && !seen && !self.appear.is_open() {
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

    /// How the compositor draws it this frame; `None` while it stands in place. It grows out of,
    /// and shrinks back into, the point of it nearest where it was opened from. Reduced motion
    /// only fades it, as [`Appear`] says.
    pub(crate) fn pose(&self) -> Option<LayerPose> {
        let progress = self.appear.progress();
        if progress >= 1.0 {
            return None;
        }
        let coming = Appear::coming(
            progress,
            ui::motion::Direction::of(self.appear.is_open()),
            self.surface,
            self.from,
            self.motion,
        );
        let origin = Appear::origin(self.surface, self.from);
        let share = |at: f64, length: f64| if length > 0.0 { at / length } else { 0.0 };
        Some(LayerPose {
            scale: coming.scale,
            // Opaque from its first frame as it arrives: the panel is floating chrome, and the
            // workspace under it is not what a person opened it to look at. Faded in with the
            // motion's own opacity it let the sharp, bright desktop show through it while it
            // arrived — the cards under it read as drawn over it, and the panel seemed to jump
            // above them once it was opaque. It grows from where it was opened instead, off its
            // material's own tint (`docs/design-language.md`, "Material and transparency"). It
            // still fades as it leaves, which is how a shrunk-back panel goes.
            // Reduced motion moves nothing: the fade is the whole of it, so it keeps it.
            opacity: if self.appear.is_open() && !self.motion.reduced() {
                1.0
            } else {
                coming.opacity
            },
            anchor: (
                share(origin.x, self.surface.width()),
                share(origin.y, self.surface.height()),
            ),
            offset: (coming.offset.x, coming.offset.y),
        })
    }
}

impl crate::Session {
    /// Follow quick settings: its app runs for the session's life, where the stack places
    /// it, shown while the stack has the part up and hidden between openings, and is asked for the
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
        let open = self.shared.overlays.is_open(Part::QuickSettings);
        let place = self.shared.overlays.place(Part::QuickSettings);
        if let Some(placement) = place {
            // Where the stack places it, and the most room it has there, resting against the edge
            // of that room the stack chose: below the caret's line, or above it. Down, it keeps
            // the last place, so it leaves from where it stood.
            self.stand_layer(SURFACE_NAMESPACE, &placement);
        }
        // Where it stands is as tall as its page: what a press outside it is judged by. A surface
        // still drawing the frame it fits takes no press until the person sees it.
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
        let visible = self.compositor.layer_visible(SURFACE_NAMESPACE);
        self.shared
            .overlays
            .stands(Part::QuickSettings, stands.filter(|_| open && visible));
        self.quick_settings_panel.follow(
            open,
            visible,
            place.map(|placement| (stands.unwrap_or(placement.room), placement.from)),
        );
        if !open || place.is_none() {
            // Down, or up where the stack cannot say: nothing shows, and its app runs on hidden, so
            // the next opening only shows it.
            // The keyboard goes back to where the person was typing as soon as it is down, not
            // once it has finished leaving: what they type next is theirs, and a copy chosen in it
            // is pasted there.
            self.compositor.return_layer_keyboard(SURFACE_NAMESPACE);
            self.hide_quick_settings();
            self.quick_settings_keyboard = PanelKeys::Settled;
            self.shared.panel_asks.borrow_mut().clear();
            self.shared.panel_back.set(None);
            // What was asked of this opening is answered for it alone, or not at all.
            self.quick_settings_asked = None;
            self.run_quick_settings(false);
            return;
        }
        if !self.run_quick_settings(true) {
            return;
        }
        // Up, where it stands: the surface is shown rather than started, so an opening the panel
        // already stands for costs nothing.
        let opening = self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true);
        // What the keys asked the panel to show, which it can only be told while it runs. It is
        // told before it shows, and shows only once it has drawn a frame after it answered: the
        // first frame a person sees is that page, fitted, never the one it was left on, which it
        // may draw again at its new place before the ask reaches it. An opening that asks for no
        // page waits for nothing: the panel holds the frame it was left on (it was told to show
        // its controls when it went down), and the compositor shows it at once rather than hold it
        // back for a frame a hidden app never draws.
        let asked = std::mem::take(&mut *self.shared.panel_asks.borrow_mut());
        let page = self.ask_panel(&asked);
        if opening && page.is_some() {
            self.quick_settings_asked = page;
        }
        let after = match self.quick_settings_asked {
            Some(id)
                if self
                    .surfaces
                    .outcome(id)
                    .is_some_and(|state| !state.update.is_terminal()) =>
            {
                // Not answered yet by an app that runs: it stays out of sight, waiting.
                self.compositor
                    .set_layer_pose(SURFACE_NAMESPACE, self.quick_settings_panel.pose());
                return;
            }
            Some(_) => {
                self.quick_settings_asked = None;
                Some(self.compositor.layer_frames(SURFACE_NAMESPACE))
            }
            None => None,
        };
        self.show_quick_settings(after);
        // The panel takes the keyboard once the person sees it: Tab walks its controls and Enter
        // opens a section; opened to paste, the person types to find a copy and Enter pastes it.
        // Until then the keys and what they asked (a copy made just before) stay with the field
        // the person types in, which has the keyboard back when the panel goes.
        if opening {
            self.quick_settings_keyboard = PanelKeys::Wanted;
        }
        if self.quick_settings_keyboard == PanelKeys::Wanted
            && visible
            && self.compositor.lend_layer_keyboard(SURFACE_NAMESPACE)
        {
            self.quick_settings_keyboard = PanelKeys::Settled;
        }
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

    /// Carry out what the clipboard was asked. A copy chosen in the panel is pasted where the
    /// person was typing: the panel goes down first, and that field has the keyboard back before
    /// the paste reaches it.
    pub(crate) fn serve_clipboard_asks(&mut self) {
        if self.shared.clipboard.pasting() {
            self.shared.overlays.close(Part::QuickSettings);
            self.compositor.return_layer_keyboard(SURFACE_NAMESPACE);
        }
        self.shared.clipboard.serve_asks(&mut self.compositor);
    }

    /// Stop trying to run the panel's app until the panel is opened anew; an open panel closes.
    fn give_up_quick_settings(&mut self) {
        self.quick_settings_run = Run::GivenUp;
        self.shared.overlays.close(Part::QuickSettings);
    }

    /// Tell the running panel what the keys asked of it, in order: the last ask for a page, which
    /// its app answers once it shows that page.
    fn ask_panel(&mut self, asked: &[&'static str]) -> Option<u64> {
        let mut page = None;
        for &action in asked {
            let id = self.ask_surface(
                SURFACE,
                crate::keys::QUICK_SETTINGS,
                action,
                tilo_actions::remote::InvokeArgument::None,
            );
            match (action, id) {
                ("back", Some(id)) => self.shared.panel_back.set(Some(id)),
                // A panel that does not run cannot go back: Escape closes what is open.
                ("back", None) => self.shared.overlays.close(Part::QuickSettings),
                (_, None) => {
                    tracing::debug!(action, "quick settings was not asked: it does not run");
                }
                (_, Some(id)) => page = Some(id),
            }
        }
        page
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
                    self.shared.overlays.close(Part::QuickSettings);
                }
            }
            // A panel that could not go back still hears the person's Escape: it closes.
            tilo_actions::InvocationUpdate::Failed { .. }
            | tilo_actions::InvocationUpdate::Cancelled { .. } => {
                self.shared.overlays.close(Part::QuickSettings);
            }
        }
        self.shared.panel_back.set(None);
    }

    /// Show the panel where it stands, posed as far as it has come. Opened with `after`, it shows
    /// once its app has drawn a frame after that commit: the page it was just asked for.
    fn show_quick_settings(&mut self, after: Option<u64>) {
        self.compositor
            .set_layer_pose(SURFACE_NAMESPACE, self.quick_settings_panel.pose());
        if self.compositor.layer_shown(SURFACE_NAMESPACE) != Some(true) {
            match after {
                Some(after) => self.compositor.show_layer_after(SURFACE_NAMESPACE, after),
                None => self.compositor.show_layer(SURFACE_NAMESPACE, true),
            }
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
            // Gone from sight, it goes back to its controls: the next opening shows them, or the
            // page it is asked for built afresh, its field taking the keys.
            let _ = self.ask_panel(&["show-controls"]);
        }
    }

    /// Stand the session's layer surface `namespace` where the overlay stack placed it.
    pub(crate) fn stand_layer(
        &mut self,
        namespace: &str,
        placement: &tilo_desktop::overlays::Placement,
    ) {
        let room = placement.room;
        self.compositor.place_layer_in(
            namespace,
            Some((
                offset(room.x0),
                offset(room.y0),
                offset(room.width()).max(1),
                offset(room.height()).max(1),
            )),
            placement.rests_at_bottom,
        );
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
    shared.overlays.close(Part::QuickSettings);
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
    if let Some(anchor) = ui::shell::settings_anchor(link) {
        let target = tilo_desktop_search::Target::Invoke(Box::new(
            tilo_desktop_search::providers::settings::opening(&workspace, &anchor),
        ));
        let opened = shared.search.perform(&workspace, target, &mut |own| {
            shared
                .published
                .invoke(
                    &own.address,
                    &own.arguments,
                    &mut tilo_session_core::surfaces::Look::none(),
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
pub const QUICK_SETTINGS: &str = "session:quick-settings";
/// The panel's layout as the session publishes it: which controls it shows and in what order.
#[derive(tilo_resource::Row)]
#[row(kind = "session.quick-settings", label = "Quick settings", effects = LayoutAction)]
pub(crate) struct QuickSettings;

/// What the Quick Settings resource offers: the controls the panel shows.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.quick-settings")]
pub(crate) enum LayoutAction {
    /// Show a control on the panel
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    ShowControl {
        /// The control's name, as the panel lists it
        #[param(
            "The control: `wifi`, `bluetooth`, `vpn`, `night_light`, `airplane_mode`, `mobile_data`, `do_not_disturb`, `rotation_lock` or `power_mode`"
        )]
        control: String,
    },
    /// Take a control off the panel. Wi-Fi stays: it is how a desktop without a network gets online
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    HideControl {
        /// The control's name, as the panel lists it
        #[param("The control, as Show control names it")]
        control: String,
    },
    /// Move a control to another place on the panel
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    MoveControl {
        /// The control's name, as the panel lists it
        #[param("The control, as Show control names it")]
        control: String,
        /// The control it stands in front of; none for the last place
        #[param("The control it stands in front of; none for the last place")]
        before: Option<String>,
    },
    /// Put the panel's controls back as they were at first
    #[effect(kind = "outside")]
    #[effect(own_device)]
    ResetControls,
}

/// The Quick Settings resource: which controls the panel shows, read by the panel and by anyone else
/// who asks.
fn quick_settings_resource() -> tilo_actions::ResourceSpec {
    tilo_service::offer::<LayoutAction>(
        tilo_resource::LiveRow::typed::<QuickSettings>(
            QUICK_SETTINGS,
            crate::i18n::t("session-quick-settings"),
        )
        .into_resource()
        .with_personal_reading(),
    )
}

/// Which controls the panel shows, as a view of another language reads it
/// (`device.quick_settings`): the controls, in the order they stand, each named as the actions
/// below name them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct QuickSettingsView {
    #[serde(default)]
    pub controls: Vec<String>,
}

/// The panel's controls as a view of another language reads them (`device.quick_settings`).
#[must_use]
pub(crate) fn controls_surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<QuickSettingsView, LayoutAction>(
        "quick-settings",
        "Quick settings",
    )
}

/// Which controls the panel shows, as the panel reads them: the person's choice of controls on this
/// device, kept in its settings document.
#[derive(Default)]
pub(crate) struct QuickSettingsService {
    /// The controls as last read, published as the reading.
    shown: std::cell::RefCell<settings_store::hardware::QuickSettingsConfig>,
    /// Whether the settings document was read at all, and the version that was read: one that did
    /// not move is not read again.
    settings: settings_store::SettingsFollower,
    /// Whether an action this service took changed the controls, so the reading it answers with
    /// and the one it published are told apart once: what a press changed is published too.
    changed: std::cell::Cell<bool>,
}

impl QuickSettingsService {
    /// The service, holding the person's controls as their settings document has them.
    pub(crate) fn new() -> Self {
        let mut service = Self::default();
        service.follow();
        service
    }

    /// Read the person's controls, and say the resource changed when they are not what was
    /// published.
    fn follow(&mut self) -> bool {
        if !self.settings.changed(settings_store::stamp()) {
            return false;
        }
        let config = settings_store::hardware::QuickSettingsConfig::load().normalised();
        let changed = *self.shown.borrow() != config;
        *self.shown.borrow_mut() = config;
        changed
    }

    /// Carry `action` out on the person's controls, and keep it: whether they changed.
    fn carry(&mut self, action: &LayoutAction) -> Result<bool, tilo_actions::ApiError> {
        let mut config = self.shown.borrow().clone();
        let changed = match action {
            LayoutAction::ShowControl { control } => {
                tilo_quick_settings::host::show_control(&mut config, control)
            }
            LayoutAction::HideControl { control } => {
                tilo_quick_settings::host::hide_control(&mut config, control)
            }
            LayoutAction::MoveControl { control, before } => {
                tilo_quick_settings::host::move_control(&mut config, control, before.as_deref())
            }
            LayoutAction::ResetControls => tilo_quick_settings::host::reset_controls(&mut config),
        };
        if !changed {
            return Ok(false);
        }
        config.save().map_err(|error| {
            tilo_actions::ApiError::Failed(format!(
                "the Quick Settings config was not kept: {error}"
            ))
        })?;
        *self.shown.borrow_mut() = config;
        self.settings.changed(settings_store::stamp());
        self.changed.set(true);
        Ok(true)
    }
}

impl tilo_service::Service for QuickSettingsService {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![quick_settings_resource()]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == QUICK_SETTINGS).then(|| {
            tilo_service::reading(&QuickSettingsView {
                controls: self
                    .shown
                    .borrow()
                    .controls
                    .iter()
                    .map(|control| tilo_quick_settings::control_id(*control).to_owned())
                    .collect(),
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        let action =
            tilo_service::decode::<LayoutAction>(&self.resources(), resource, action, arguments)?;
        self.carry(&action)?;
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn poll(&mut self) -> bool {
        let changed = self.changed.replace(false);
        changed | self.follow()
    }
}

#[cfg(test)]
mod tests {
    use tilo_service::Service as _;

    use super::*;

    /// Ask the config resource `action` with `arguments`, as the API decodes it.
    fn asked(service: &mut QuickSettingsService, action: &str, arguments: &serde_json::Value) {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        service
            .invoke(QUICK_SETTINGS, action, &arguments)
            .expect("the config takes it");
    }

    /// The controls the config publishes now.
    fn shown(service: &QuickSettingsService) -> String {
        service
            .read(QUICK_SETTINGS)
            .expect("the config is published")["controls"]
            .to_string()
    }

    tilo_test_env::scenario! {
        fn the_quick_settings_panel_keeps_its_config_and_comes_from_its_corner;

        phase the_config_is_published_by_the_session_and_kept;
        // The config is the person's own setting, so it is read, changed and kept whether the
        // panel is drawn or not, and a control moves in front of the one it was dropped on.
        {
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

        phase the_panel_arrives_from_its_corner_and_leaves_back_into_it;
        // The panel grows from its corner when its first frame is drawn, stands as it stands, and
        // shrinks back into the corner while it leaves, until it asks for nothing at all.
        {
            use ui::time::testing::with_monotonic_time;
            use ui::tokens::MotionSource;
            let t0 = ui::time::now();
            let at = |ms: u64| t0 + ui::time::Duration::from_millis(ms);
            let mut panel = PanelMotion::new(EffectiveMotion::default());
            // The place the stack gives it: 380 x 640 at the display's right, under the bar's status
            // area it was opened from.
            let place = Some((
                Rect::new(892.0, 32.0, 1272.0, 672.0),
                Point::new(1272.0, 0.0),
            ));
            // Every read of the motion is pinned at the instant it is read at.
            let look = |panel: &PanelMotion, ms: u64| {
                with_monotonic_time(at(ms), || (panel.is_drawn(), panel.pose()))
            };
            let asks = |panel: &PanelMotion, ms: u64| {
                with_monotonic_time(at(ms), || panel.next_pass(at(ms)).is_some())
            };

            // Up while the person does not see its app's frame yet: out of sight, not moving.
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
                coming.scale < 1.0 && coming.offset.0 > 0.0,
                "from small, toward the corner it was opened from: {coming:?}"
            );
            assert!(
                (coming.opacity - 1.0).abs() < f32::EPSILON,
                "opaque as it arrives: the sharp workspace under it never shows through the \
                 floating chrome it is made of, which read as the cards drawn over it: {coming:?}"
            );
            assert!(
                coming.offset.0 > 0.0 && coming.offset.1 < 0.0 && coming.anchor == (1.0, 0.0),
                "and from the corner the status area is in, up and to the right: {coming:?}"
            );
            assert!(
                asks(&panel, 0),
                "an arriving panel asks for the pass that moves it"
            );
            // Part way through its arrival it is still wholly opaque: no frame of the motion shows
            // the workspace under it through the panel.
            let partway = look(&panel, 60).1.expect("its arrival is drawn");
            assert!(
                (partway.opacity - 1.0).abs() < f32::EPSILON,
                "opaque all the way in: {partway:?}"
            );
            assert!(partway.scale < 1.0, "and still small: {partway:?}");

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
}
