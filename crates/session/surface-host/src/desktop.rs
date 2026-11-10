//! The desktop under the windows, and the window policy it answers.
//!
//! [`desktop`] makes the pair the compositor runs a session with: a
//! [`DesktopHost`] drawing the portable desktop surface (its strips, window
//! bars and edges) under every window, and a [`StripPolicy`] placing each
//! window where that surface lays its tab out ([`tilo_desktop_policy`]).
//! Both share one desktop, so a window opened, focused, closed or panned in
//! one is where the other shows it.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::gestures::{
    GestureAction, GestureConfig, GestureTracker, SWIPE_THRESHOLD, claims,
};
use tilo_compositor::{
    Arrangement, GestureEvent, LayoutPoint, LayoutRect, TouchSample, WindowInfo, WindowPolicy,
    WindowRequest,
};
use tilo_session_core::surface::{HostIme, HostKey, HostLayer, HostTextInput, SurfaceHost};

use tilo_desktop_policy::displays::Output;
use tilo_desktop_policy::floating::centred;
use tilo_desktop_policy::{DesktopPolicy, Window};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};

use crate::{Placement, WidgetHost};

/// Where a window not shown waits: left of every output, out of sight and
/// out of the pointer's reach.
const OFFSTAGE: i32 = -(1 << 20);

/// What a touchpad gesture the desktop took asks of it, once the gesture
/// has said what it means: the same steps the desktop's chords take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeskGesture {
    /// Three fingers carrying the shown strip: `travel` is their total
    /// signed travel since they landed, positive toward later windows.
    StripDrag { travel: f64, began: bool },
    /// Those fingers lifted (or the hand was taken back): the strip coasts
    /// on the velocity they let go at, or stays where it was.
    StripDragEnd { cancelled: bool },
    /// Three fingers up or down: the strip `steps` on.
    StepStrip(i32),
    /// Three fingers landed on the deck: it follows them from here. `travel`
    /// is their signed travel in pixels since they landed, positive toward
    /// the strips below.
    StripDeckDrag { travel: f64 },
    /// Those fingers lifted, having carried the deck `travel`: the strip
    /// `steps` on from it, or none when they came back.
    StepStripFrom { travel: f64, steps: i32 },
    /// Four fingers up or down: the workspace `delta` on.
    SwitchWorkspace(i32),
    /// A pinch in opens the overview (`true`), out closes it.
    Overview(bool),
}

/// What reading one touchpad event came to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Reading {
    /// Not the desk's: the client under the pointer keeps it.
    Clients,
    /// The desk took it; the fingers say nothing yet.
    Taken,
    /// The desk took it and it asks this.
    Asked(DeskGesture),
}

/// The desk's reading of the touchpad's gestures: the three- and four-finger
/// streams the person's settings give the desk, followed until the fingers say
/// what they mean (`tilo_compositor::gestures` turns the stream into those
/// actions). Both window policies read them — the strips' and the shell's
/// wells — so a gesture means the same on either desk.
#[derive(Clone, Debug, Default)]
pub(crate) struct GestureReader {
    tracker: GestureTracker,
    /// How far the fingers have carried the strip, or the strip deck, in the
    /// gesture in flight.
    travel: f64,
}

impl GestureReader {
    /// `gesture` as the desk's, when `config` reads gestures (the person asked
    /// for them) and it has three or more fingers.
    pub(crate) fn read(
        &mut self,
        gesture: &GestureEvent,
        config: Option<GestureConfig>,
    ) -> Reading {
        let action = match *gesture {
            GestureEvent::SwipeBegin { fingers } | GestureEvent::PinchBegin { fingers } => {
                let Some(config) = config.filter(|_| claims(fingers)) else {
                    return Reading::Clients;
                };
                self.tracker.set_config(config);
                self.tracker.swipe_begin(fingers);
                self.travel = 0.0;
                None
            }
            GestureEvent::SwipeUpdate { dx, dy } => self.tracker.swipe_update(dx, dy),
            GestureEvent::SwipeEnd { cancelled } | GestureEvent::PinchEnd { cancelled } => {
                self.tracker.swipe_end_with_cancel(cancelled)
            }
            GestureEvent::PinchUpdate { scale, .. } => self.tracker.pinch_update(scale),
            // A hold asks nothing of the desk: the client keeps it.
            GestureEvent::HoldBegin { .. } | GestureEvent::HoldEnd { .. } => {
                return Reading::Clients;
            }
        };
        let step = |travel: f64| {
            if travel >= SWIPE_THRESHOLD {
                Some(1)
            } else if travel <= -SWIPE_THRESHOLD {
                Some(-1)
            } else {
                None
            }
        };
        let asked = match action {
            // A vertical deck drag hands its travel to the desk on every
            // update, so the strips are under the fingers; so does the
            // horizontal one, which carries the shown strip with them.
            Some(GestureAction::StripDeckUpdate { travel, began }) => {
                self.travel = travel;
                if began {
                    return Reading::Asked(DeskGesture::StripDeckDrag { travel });
                }
                Some(DeskGesture::StripDeckDrag { travel })
            }
            Some(GestureAction::StripPanUpdate { travel, began }) => {
                self.travel = travel;
                Some(DeskGesture::StripDrag { travel, began })
            }
            Some(GestureAction::StripPanEnd { cancelled }) => {
                Some(DeskGesture::StripDragEnd { cancelled })
            }
            Some(GestureAction::StripDeckEnd { cancelled: false }) => {
                Some(DeskGesture::StepStripFrom {
                    travel: self.travel,
                    steps: step(self.travel).unwrap_or(0),
                })
            }
            Some(GestureAction::SwitchWorkspace(delta)) => {
                Some(DeskGesture::SwitchWorkspace(delta))
            }
            Some(GestureAction::ShowOverview) => Some(DeskGesture::Overview(true)),
            Some(GestureAction::HideOverview) => Some(DeskGesture::Overview(false)),
            // A cancelled deck drag is the hand coming back: the strip
            // returns the way it went.
            Some(GestureAction::StripDeckEnd { cancelled: true }) => {
                Some(DeskGesture::StepStripFrom {
                    travel: self.travel,
                    steps: 0,
                })
            }
            None => None,
        };
        match asked {
            Some(asked) => Reading::Asked(asked),
            None => Reading::Taken,
        }
    }
}

/// The desktop both halves share.
struct Desk {
    policy: DesktopPolicy,
    host: WidgetHost,
    /// What the gestures the desktop took asked, for its host to carry out.
    gestures: Vec<DeskGesture>,
    /// How the desktop reads touchpad gestures; `None` leaves every one to
    /// the clients.
    gesture_config: Option<GestureConfig>,
    /// The monitor on each output, as the compositor knows it.
    monitors: Vec<(LayoutRect, String)>,
    /// The monitor the person made primary, while it is lit.
    primary: Option<String>,
}

impl Desk {
    /// Cover `outputs`' bounding box, the surface laid out against it.
    fn cover(&mut self, outputs: &[LayoutRect]) {
        let Some(bounds) = outputs.iter().copied().reduce(LayoutRect::merge) else {
            return;
        };
        let (Ok(width), Ok(height)) = (u32::try_from(bounds.size.w), u32::try_from(bounds.size.h))
        else {
            return;
        };
        let placement = Placement {
            origin: (bounds.loc.x, bounds.loc.y),
            size: (width.max(1), height.max(1)),
            scale: self.host.placement().scale,
        };
        if placement != self.host.placement() {
            self.policy.set_viewport_width(f64::from(placement.size.0));
            self.host.set_placement(placement);
        }
        // Each display holds its own windows. A monitor is known by where
        // it sits, as the policy is told it, and the primary one is the
        // display the person chose for their bar, windows and sheets.
        let primary = self.primary.clone();
        let displays: Vec<Output> = outputs
            .iter()
            .map(|output| {
                let x = f64::from(output.loc.x.saturating_sub(bounds.loc.x));
                let y = f64::from(output.loc.y.saturating_sub(bounds.loc.y));
                let monitor = self
                    .monitors
                    .iter()
                    .find(|(rect, _)| rect == output)
                    .map_or_else(
                        || format!("output:{},{}", output.loc.x, output.loc.y),
                        |(_, monitor)| monitor.clone(),
                    );
                Output {
                    primary: primary.as_deref() == Some(monitor.as_str()),
                    monitor,
                    rect: ui::kurbo::Rect::new(
                        x,
                        y,
                        x + f64::from(output.size.w),
                        y + f64::from(output.size.h),
                    ),
                }
            })
            .collect();
        if self.policy.set_displays(&displays) {
            self.host.relayout();
        }
    }

    /// Where a dialog of `size` floats: centred over its parent, or the
    /// display the person works on, in the logical layout. `None` while it
    /// is hidden with its parent.
    fn floating(&self, window: u64, size: (i32, i32)) -> Option<LayoutPoint> {
        let (ox, oy) = self.host.placement().origin;
        // A card stands exactly where the floating layer put it, and nowhere
        // while it is suspended.
        if self.policy.draws_card(window) {
            let at = self.policy.card_place(window)?;
            return Some(LayoutPoint::from((
                ox.saturating_add(round(at.x0)),
                oy.saturating_add(round(at.y0)),
            )));
        }
        let (_, area) = self
            .policy
            .layout()
            .floating
            .into_iter()
            .find(|(id, _)| *id == window)?;
        let at = centred(area, (f64::from(size.0), f64::from(size.1)));
        Some(LayoutPoint::from((
            ox.saturating_add(round(at.x)),
            oy.saturating_add(round(at.y)),
        )))
    }

    /// Where `window`'s content is in the logical layout, as last laid out.
    fn shown(&self, window: u64) -> Option<LayoutRect> {
        let (ox, oy) = self.host.placement().origin;
        self.policy
            .layout()
            .shown
            .into_iter()
            .find(|(id, _)| *id == window)
            .map(|(_, rect)| {
                LayoutRect::new(
                    LayoutPoint::from((
                        ox.saturating_add(round(rect.x0)),
                        oy.saturating_add(round(rect.y0)),
                    )),
                    (round(rect.width()).max(1), round(rect.height()).max(1)).into(),
                )
            })
    }
}

/// `value` rounded to a whole logical pixel.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a window's place on a screen"
)]
fn round(value: f64) -> i32 {
    value.round() as i32
}

/// `rect` of the surface, as it stands in the logical layout where the desk's origin is
/// `(ox, oy)`.
fn placed(rect: ui::kurbo::Rect, (ox, oy): (i32, i32)) -> (i32, i32, i32, i32) {
    (
        ox.saturating_add(round(rect.x0)),
        oy.saturating_add(round(rect.y0)),
        round(rect.width()).max(1),
        round(rect.height()).max(1),
    )
}

/// The desktop drawn under every window; a clone is the same desktop.
#[derive(Clone)]
pub struct DesktopHost {
    desk: Rc<RefCell<Desk>>,
}

/// Told about the windows a program opens and closes, by the policy: what
/// finds the program behind a window that named itself.
pub trait WindowObserver: Send + Sync {
    /// `window` named itself and is about to map.
    fn opened(&self, window: &WindowInfo);
    /// The window `id` is gone.
    fn closed(&self, id: u64);
}

/// The window policy answered by the desktop's strips.
pub struct StripPolicy {
    desk: Rc<RefCell<Desk>>,
    /// The windows given a place, so one gone from the compositor closes
    /// its tab.
    mapped: Vec<u64>,
    /// The touchpad gestures the desk took.
    gestures: GestureReader,
    /// Those of them that have not shown a buffer yet: they wait in their
    /// place, which the compositor does not list among its windows.
    pending: Vec<u64>,
    /// Who is told of the windows the desktop gives a place and of the
    /// ones gone.
    followers: WindowFollowers,
    /// Where the pointer and fingers go as well as to the windows: the
    /// chrome that follows them beyond where it draws.
    samples: Option<Rc<RefCell<dyn PointerSamples>>>,
}

/// What the desk's chrome follows beyond the pixels it draws: the pointer
/// wherever it goes and fingers on the screen, which reach a hosted surface
/// only while it is what the pointer is over. The chrome hides parts of
/// itself and brings them back when the person reaches for them, so it
/// reads every sample, not only the ones it is the target of.
pub trait PointerSamples {
    /// The pointer moved to this point in the logical layout.
    fn moved(&mut self, x: f64, y: f64);
    /// A pointer button went down at this point in the logical layout.
    fn pressed(&mut self, x: f64, y: f64);
    /// A finger touched down at this point, over anything: whether the
    /// desk takes the contact, so nothing under it sees it. A taken
    /// contact's motion and lift come here too.
    fn touch(&mut self, touch: TouchSample) -> bool;
}

/// A desktop of one workspace with one empty strip (`salt` seeds its ids),
/// drawn at `scale`: the surface under the windows and the policy placing
/// them. The compositor hosts the one and runs with the other.
#[must_use]
pub fn desktop(salt: u16, scale: f64) -> (DesktopHost, StripPolicy) {
    desktop_with(salt, scale, |surface, _| Box::new(surface))
}

/// As [`desktop`], with the surface drawn inside the tree `root` makes of
/// it (the desk's backdrop and launchpad around the strips), given the
/// policy's host side of the surface.
#[must_use]
pub fn desktop_with(
    salt: u16,
    scale: f64,
    root: impl FnOnce(DesktopSurface, &DesktopHandle) -> Box<dyn ui::Widget>,
) -> (DesktopHost, StripPolicy) {
    let (policy, surface) = DesktopPolicy::new(salt);
    let handle = policy.handle().clone();
    let root = root(surface, policy.handle());
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale,
    };
    // What the desk itself types into (its search, its notification centre)
    // takes the keys; everything else on it passes them on to the window the
    // person is in.
    let waking_handle = handle.clone();
    let keyboard = Rc::new(move || handle.wants_keyboard());
    let desk = Rc::new(RefCell::new(Desk {
        policy,
        host: WidgetHost::new(root, placement, true)
            .named(crate::i18n::t("desktop"))
            .with_keyboard(keyboard),
        gestures: Vec::new(),
        gesture_config: Some(GestureConfig::default()),
        monitors: Vec::new(),
        primary: None,
    }));
    // What another surface commands of the desk — the chrome's own actions, an
    // agent's — is queued on its handle and taken up by the desk's next pass:
    // the handle asks for it, so a desk that draws nothing of its own still
    // keeps its windows where the person put them.
    let asked = desk.borrow().host.asking();
    waking_handle.set_wake(Rc::new(move || asked.set(true)));
    (
        DesktopHost {
            desk: Rc::clone(&desk),
        },
        StripPolicy {
            desk,
            mapped: Vec::new(),
            gestures: GestureReader::default(),
            pending: Vec::new(),
            followers: WindowFollowers::default(),
            samples: None,
        },
    )
}

impl DesktopHost {
    /// The desktop's policy, for everything else a session drives on it
    /// (strips and workspaces, the tree it keeps).
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desk.borrow_mut().policy)
    }

    /// The apps whose windows keep what they show to themselves, as the
    /// session last learnt them: nothing the desk draws, or names for the
    /// compositor to draw, shows one of their windows.
    pub fn set_withheld_apps(&self, apps: Vec<String>) {
        self.desk.borrow().policy.handle().set_withheld_apps(apps);
    }

    /// Answer `look` and `read` of a window this session does not run, which
    /// the desk publishes on the window's own resource: the tree holds what
    /// they say, and the session's [`crate::Look`] makes the picture. Any
    /// other address is left to the desk's tree.
    fn read_window(
        &self,
        address: &ui::tilo_actions::ActionAddress,
        look: &mut crate::Look<'_>,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, ui::tilo_actions::DoorError>
    {
        use tilo_desktop_surface::window;
        let reads = address.action == "look" || address.action == "read";
        let Some(tab) = window::resource_tab(&address.resource).filter(|_| reads) else {
            return Ok(None);
        };
        let desk = self.desk.borrow();
        let handle = desk.policy.handle();
        let Some(shown) = handle.read(|tree| window::shown(tree, tab)) else {
            // Not a window this session does not run: the tree publishes no
            // read action of it, so the address names nothing.
            return Err(ui::tilo_actions::DoorError::Unknown(address.clone()));
        };
        if address.action == "look" {
            // A window whose app keeps what it shows to itself is never
            // pictured, however it was asked: the address is not offered
            // while it is withheld, and a catalogue read before then does not
            // outlive it.
            if handle.withholds(&shown.app_id) {
                return Err(ui::tilo_actions::DoorError::Unavailable {
                    code: "withheld".into(),
                    reason: "this app's window keeps what it shows to itself".into(),
                });
            }
            let shot = look
                .window(shown.window, &shown.workspace)
                .map_err(|reason| ui::tilo_actions::DoorError::Unavailable {
                    code: "no_picture".into(),
                    reason,
                })?;
            return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "title": shown.title,
                        "app": shown.app_id,
                        "width": shot.width,
                        "height": shot.height,
                        "content": shot.content,
                    })),
                    resources: vec![shot.content],
                },
            )));
        }
        Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
            ui::tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "title": shown.title,
                    "app": shown.app_id,
                })),
                resources: Vec::new(),
            },
        )))
    }

    /// Answer the platform search door (`tilo.host:search`'s `open`) with the
    /// desk's own search: a window's search control, its toolbar's magnifier,
    /// Ctrl+K and an agent all ask the platform to search, and the platform's
    /// search is the one that knows the person's apps, their files, what apps
    /// hold and the desk's windows. The desk's palette is drawn over the
    /// windows by the chrome ([`crate::desktop::DesktopHost`]'s
    /// `SearchOverlay`), which is the one surface that may cover a window in
    /// front; opening it here means the same search every way into search
    /// reaches.
    ///
    /// `None` when `address` is not the platform search door.
    fn open_platform_search(
        &self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<ui::host_operation::ResourceOperationStart> {
        if !ui::search::is_search_address(address) {
            return None;
        }
        let request = ui::search::SearchRequest::from_arguments(arguments);
        let handle = self.desk.borrow().policy.handle().clone();
        handle.open_search(ui::search::Scope::Workspace, request.query);
        Some(ui::host_operation::ResourceOperationStart::Completed(
            ui::tilo_actions::ActionOutput::default(),
        ))
    }

    /// Read touchpad gestures by `config` from the next one on; `None`
    /// leaves them all to the clients.
    pub fn set_gestures(&self, config: Option<GestureConfig>) {
        self.desk.borrow_mut().gesture_config = config;
    }

    /// The policy that places this desktop's windows where a tilolang shell's wells stand
    /// (`docs/designs/app-language/shell.md` §8.3), in place of the strips': what a session
    /// runs with when the shell draws the whole screen. The windows keep their tabs in this
    /// desktop, so a place in the person's arrangement names the window's well.
    #[must_use]
    pub fn well_policy(&self) -> crate::WellPolicy {
        crate::WellPolicy::over(Rc::new(self.clone()))
    }

    /// Draw the desk with `theme`: the person's palette, tokens and fonts.
    pub fn set_theme(&self, theme: ui::Theme) {
        self.desk.borrow_mut().host.set_theme(theme);
    }

    /// What the touchpad gestures the desktop took asked since the last
    /// call, oldest first.
    pub fn take_gestures(&self) -> Vec<DeskGesture> {
        std::mem::take(&mut self.desk.borrow_mut().gestures)
    }

    /// The windows whose bar asked for its menu since the last call, oldest
    /// first: a secondary click on a bar, and the place on the desk it was
    /// pressed.
    pub fn take_tab_menus(&self) -> Vec<(u64, f64, f64)> {
        self.desk
            .borrow()
            .policy
            .handle()
            .take_tab_menus()
            .into_iter()
            .map(|(tab, at)| (tab, at.x, at.y))
            .collect()
    }

    /// What the desktop asked to open since the last call, oldest first:
    /// the apps the session plans and starts, and the tab each window the
    /// desk held for them will take.
    pub fn take_launches(&self) -> Vec<tilo_desktop_policy::LaunchAsk> {
        self.with_policy(DesktopPolicy::take_launches)
    }

    /// The tabs the desk made for windows apps asked it for since the last
    /// call, oldest first: the session starts each window beside its tab.
    pub fn take_held_launches(&self) -> Vec<tilo_desktop_policy::HeldLaunch> {
        self.with_policy(DesktopPolicy::take_held_launches)
    }

    /// The desk's widget tree as hosted: what it publishes, its
    /// accessibility tree, the actions it accepts.
    pub fn with_widgets<R>(&self, read: impl FnOnce(&mut WidgetHost) -> R) -> R {
        read(&mut self.desk.borrow_mut().host)
    }
}

impl crate::well_policy::WellDesk for DesktopHost {
    fn policy(&self) -> std::cell::RefMut<'_, DesktopPolicy> {
        std::cell::RefMut::map(self.desk.borrow_mut(), |desk| &mut desk.policy)
    }

    fn relayout(&self) {
        self.desk.borrow_mut().host.relayout();
    }

    fn cover(&self, outputs: &[LayoutRect]) {
        self.desk.borrow_mut().cover(outputs);
    }

    fn gesture_config(&self) -> Option<GestureConfig> {
        self.desk.borrow().gesture_config
    }

    fn took_gesture(&self, gesture: DeskGesture) {
        self.desk.borrow_mut().gestures.push(gesture);
    }
}

impl crate::Published for DesktopHost {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        crate::Published::resources(&mut self.desk.borrow_mut().host)
    }

    fn revision(&self) -> Option<u64> {
        // What the desk publishes — the windows it holds and what can be done
        // with them — is the desk model's, so it is the model's revision that
        // says it changed. The hosted tree's own revision counts the frames it
        // drew, and the desk draws every frame of a drag or a resize: read as
        // that, everything publishing here was read again every frame of
        // every gesture.
        Some(self.desk.borrow().policy.handle().revision())
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut crate::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        if let Some(answered) = self.read_window(address, look)? {
            return Ok(answered);
        }
        if let Some(answered) = self.open_platform_search(address, arguments) {
            return Ok(answered);
        }
        crate::Published::invoke(&mut self.desk.borrow_mut().host, address, arguments, look)
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut crate::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        if let Some(answered) = self.read_window(address, look)? {
            return Ok(answered);
        }
        if let Some(answered) = self.open_platform_search(address, arguments) {
            return Ok(answered);
        }
        crate::Published::invoke_as(
            &mut self.desk.borrow_mut().host,
            actor,
            address,
            arguments,
            look,
        )
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        crate::Published::accessible_labels(&mut self.desk.borrow_mut().host)
    }

    /// The desk's own controls — its strips, its windows' bars — and, behind
    /// them, the controls the apps that draw the windows describe of their
    /// own content ([`ui::window_targets`]): a window's app keeps its widget
    /// tree to itself, so this is the one place they are named.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        let mut shown = crate::Published::controls(&mut self.desk.borrow_mut().host);
        let chrome: std::collections::BTreeSet<String> =
            shown.iter().map(|(name, _)| name.clone()).collect();
        for (name, rect) in self.desk.borrow().policy.handle().window_controls() {
            // A name the desk's own chrome shows keeps meaning there: what a
            // window adds are the names the desk does not have. Two windows of
            // an app describe controls by the same names, and each is listed
            // where it stands, so a window floating over the desk is as
            // readable as one in its strip: a press by name reaches the first
            // window that shows it ([`Self::press_action`]).
            if !chrome.contains(&name) {
                shown.push((name, rect));
            }
        }
        shown
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        self.press_action(name, ui::window_targets::TargetAction::Activate)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let mut desk = self.desk.borrow_mut();
        // A name the desk's own chrome shows keeps meaning there, as
        // `controls` says.
        if crate::Published::controls(&mut desk.host)
            .iter()
            .any(|(shown, _)| shown == name)
        {
            return crate::Published::press_action(&mut desk.host, name, action);
        }
        // The control is one a window's app draws: the press reaches it as its
        // own assistive technology's activation does.
        let handle = desk.policy.handle();
        if handle
            .window_controls()
            .iter()
            .any(|(shown, _)| shown == name)
        {
            return handle.press_window_control(name, action);
        }
        Err(format!("no control named {name}"))
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        crate::Published::a11y_tree(&mut self.desk.borrow_mut().host)
    }

    fn a11y_name(&self) -> Option<String> {
        crate::Published::a11y_name(&self.desk.borrow().host)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        crate::Published::a11y_action(&mut self.desk.borrow_mut().host, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
    }
}

impl SurfaceHost for DesktopHost {
    fn layers(&self) -> Vec<HostLayer> {
        self.desk.borrow().host.layers()
    }

    fn withholds_captures(&self) -> bool {
        self.desk.borrow().host.withholds_captures()
    }

    fn below_windows(&self) -> bool {
        true
    }

    fn drawn_over_windows(&self) -> Vec<(u64, (i32, i32, i32, i32))> {
        // The desk draws a floating window's bar and lift itself, under the windows, so the windows
        // it covers are drawn around it.
        let desk = self.desk.borrow();
        let (ox, oy) = desk.host.placement().origin;
        desk.policy
            .float_covers()
            .into_iter()
            .map(|(window, cover)| {
                let (x, y, width, height) = placed(cover, (ox, oy));
                (window, (x, y, width, height))
            })
            .collect()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.desk.borrow().host.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.desk.borrow_mut().host.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.desk.borrow_mut().host.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.desk.borrow_mut().host.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.desk
            .borrow_mut()
            .host
            .pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(&self.desk.borrow().host)
    }

    fn key(&mut self, key: &HostKey) {
        self.desk.borrow_mut().host.key(key);
    }

    fn text_input(&self) -> Option<HostTextInput> {
        self.desk.borrow().host.text_input()
    }

    fn ime(&mut self, event: HostIme) {
        self.desk.borrow_mut().host.ime(event);
    }

    fn prepare_frame(&mut self) -> bool {
        {
            let mut desk = self.desk.borrow_mut();
            // Something outside the desk's tree asked for a pass — an app
            // described the controls of its window's content, a service
            // published what the desk shows. The desk reads what it changed in
            // that frame, as it does standing under the shell
            // ([`crate::shell::Aside`]), rather than whenever the next frame
            // comes for another reason. A desk nobody asked draws nothing.
            if desk.host.asking().get() {
                desk.host.relayout();
            }
        }
        self.desk.borrow_mut().host.prepare_frame()
    }

    fn advance(&mut self) -> bool {
        self.desk.borrow_mut().host.advance()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.desk.borrow().host.next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.desk.borrow_mut().host.set_frame_interval(interval);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::HostedOutput]) {
        // The desk draws at the scale of the first display there is, the one
        // it covers: its strips and the windows' bars paint the pixels that
        // display shows, rather than one scale's worth stretched over a
        // denser panel (which is what made everything grainy on a HiDPI
        // laptop). A desk spanning displays of different scales draws at the
        // first's, as its layout is the bounding box of all of them.
        let scale = outputs.first().map_or(1.0, |output| output.scale);
        let mut desk = self.desk.borrow_mut();
        let placement = desk.host.placement();
        if (placement.scale - scale).abs() > f64::EPSILON {
            desk.host.set_placement(Placement { scale, ..placement });
        }
    }

    fn window_pictures(&self) -> Vec<tilo_session_core::surface::WindowPicture> {
        self.desk.borrow().host.window_pictures()
    }

    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        self.desk.borrow().host.backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        self.desk.borrow_mut().host.set_backdrops(images);
    }
}

/// Who a window policy tells of the windows the desktop gives a place, and
/// of the ones gone: the same for every policy placing the desktop's
/// windows ([`StripPolicy`], [`crate::WellPolicy`]).
#[derive(Default)]
pub(crate) struct WindowFollowers {
    /// Told the process of each window the compositor maps, the moment the
    /// desktop gives it a place.
    map_observer: Option<Rc<dyn Fn(u32)>>,
    /// Who follows the windows, when the session has someone that does.
    observer: Option<std::sync::Arc<dyn WindowObserver>>,
    /// The windows it was told about, so none is told of twice.
    announced: std::collections::HashSet<u64>,
}

impl WindowFollowers {
    /// Tell `observer` the process of each window the desktop gives a place.
    pub(crate) fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.map_observer = Some(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`.
    pub(crate) fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.observer = Some(observer);
    }

    /// `window` was given its place: tell its process's observer, and the
    /// window's observer once.
    pub(crate) fn mapped(&mut self, window: &WindowInfo) {
        if let (Some(observer), Some(pid)) = (self.map_observer.as_ref(), window.pid) {
            observer(pid);
        }
        let Some(observer) = self.observer.clone() else {
            return;
        };
        if !self.announced.insert(window.id) {
            return;
        }
        observer.opened(window);
    }

    /// Tell the observer that `id` is gone, once.
    pub(crate) fn gone(&mut self, id: u64) {
        if !self.announced.remove(&id) {
            return;
        }
        if let Some(observer) = &self.observer {
            observer.closed(id);
        }
    }
}

impl StripPolicy {
    /// Tell `observer` the process of each window the compositor maps, as the
    /// desktop gives it a place: the app's door opens to it from then on.
    pub fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.followers.set_window_mapped(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`:
    /// how a session learns which programs the person runs.
    pub fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.followers.observe_windows(observer);
    }

    /// Send every pointer sample and touch to `samples` too, wherever they
    /// go: the chrome that keeps out of the way until reached for.
    pub fn with_samples(&mut self, samples: Rc<RefCell<dyn PointerSamples>>) {
        self.samples = Some(samples);
    }

    /// Name apps with `names`, the session's: the installed package's
    /// localised name, else the machine's desktop entry, else the registry's.
    /// A tab opens with the name of its app, never with the app's id.
    pub fn reads_app_names(&mut self, names: Rc<dyn Fn(&str) -> String>) {
        self.desk.borrow_mut().policy.reads_app_names(names);
    }

    /// Give `window` its tab, once, and lay the desktop out so its place
    /// is known.
    fn map(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) {
        {
            let mut desk = self.desk.borrow_mut();
            desk.cover(outputs);
            if self.mapped.contains(&window.id) {
                return;
            }
            let mapped = desk.policy.map(Window {
                id: window.id,
                app_id: window.app_id.clone(),
                title: window.title.clone(),
                parent: window.parent,
                min_size: window.min_size,
                max_size: window.max_size,
                pid: window.pid,
            });
            if let Err(error) = mapped {
                tracing::warn!(%error, window = window.id, "the desktop has no strip to open a window in");
                return;
            }
            self.mapped.push(window.id);
            desk.host.relayout();
            // A window launched into a workspace not in front goes there once its tab is made.
            desk.policy.carry_launched();
        }
        self.followers.mapped(window);
    }
}

impl WindowPolicy for StripPolicy {
    /// Focus follows the pointer onto another display: to the window last
    /// focused there, else to none, so the next window opens there.
    fn pointer_moved(&mut self, x: f64, y: f64) {
        if let Some(samples) = &self.samples {
            samples.borrow_mut().moved(x, y);
        }
        let mut desk = self.desk.borrow_mut();
        let (ox, oy) = desk.host.placement().origin;
        let (x, y) = (x - f64::from(ox), y - f64::from(oy));
        let handle = desk.policy.handle().clone();
        let under = handle.read(|tree| {
            if !tree.is_partitioned() {
                return None;
            }
            tree.outputs
                .displays
                .iter()
                .find(|d| x >= d.x && x < d.x + d.width && y >= d.y && y < d.y + d.height)
                .map(|d| d.id)
        });
        if let Some(display) = under
            && handle.focused_display() != Some(display)
            && handle.focus_display(display)
        {
            desk.host.relayout();
        }
    }

    fn pointer_pressed(&mut self, x: f64, y: f64) {
        if let Some(samples) = &self.samples {
            samples.borrow_mut().pressed(x, y);
        }
    }

    fn touch(&mut self, touch: TouchSample) -> bool {
        self.samples
            .as_ref()
            .is_some_and(|samples| samples.borrow_mut().touch(touch))
    }

    fn monitors(&mut self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        let mut desk = self.desk.borrow_mut();
        monitors.clone_into(&mut desk.monitors);
        desk.primary = primary.map(str::to_owned);
    }

    fn overlay(&self, window: &WindowInfo) -> bool {
        self.desk.borrow().policy.draws_card(window.id)
    }

    /// A card's window takes no focus: the person works in the workspace it
    /// floats over, and the card is a view of the activity, not a place to
    /// type.
    fn focus_on_map(&mut self, window: &WindowInfo) -> bool {
        !self.desk.borrow().policy.draws_card(window.id)
    }

    fn initial_size(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) -> Option<(i32, i32)> {
        self.map(window, outputs);
        if !self.pending.contains(&window.id) {
            self.pending.push(window.id);
        }
        // A dialog picks its own size, and a card the size its app asked for.
        if let Some(size) = self.desk.borrow().policy.card_size(window.id) {
            return Some(size);
        }
        let rect = self.desk.borrow().shown(window.id)?;
        Some((rect.size.w, rect.size.h))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        self.map(window, outputs);
        self.pending.retain(|id| *id != window.id);
        let desk = self.desk.borrow();
        desk.shown(window.id)
            .map(|rect| rect.loc)
            .or_else(|| desk.floating(window.id, size))
            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, LayoutRect)],
        outputs: &[LayoutRect],
    ) -> Option<Vec<Arrangement>> {
        // A launched window whose tab came with the last layout goes to its own workspace.
        self.desk.borrow_mut().policy.carry_launched();
        // A window the compositor lists has shown its buffer.
        self.pending
            .retain(|id| !windows.iter().any(|(window, _)| window.id == *id));
        let gone: Vec<u64> = self
            .mapped
            .iter()
            .copied()
            .filter(|id| {
                !self.pending.contains(id) && !windows.iter().any(|(window, _)| window.id == *id)
            })
            .collect();
        {
            let mut desk = self.desk.borrow_mut();
            desk.cover(outputs);
            for id in &gone {
                desk.policy.unmap(*id);
            }
            for (window, _) in windows {
                if let Some(title) = &window.title {
                    desk.policy.retitle(window.id, title);
                }
                if let Some(app_id) = &window.app_id {
                    desk.policy.set_app_id(window.id, app_id);
                }
            }
            if !gone.is_empty() {
                desk.host.relayout();
            }
        }
        self.mapped.retain(|id| !gone.contains(id));
        for id in &gone {
            self.followers.gone(*id);
        }
        for (window, _) in windows {
            self.map(window, outputs);
        }
        let desk = self.desk.borrow();
        Some(
            windows
                .iter()
                .map(|(window, current)| {
                    if let Some(rect) = desk.shown(window.id) {
                        return Arrangement {
                            id: window.id,
                            location: rect.loc,
                            size: Some((rect.size.w, rect.size.h)),
                        };
                    }
                    let size = (current.size.w, current.size.h);
                    Arrangement {
                        id: window.id,
                        location: desk
                            .floating(window.id, size)
                            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0))),
                        size: None,
                    }
                })
                .collect(),
        )
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
        self.followers.gone(window.id);
        let mut desk = self.desk.borrow_mut();
        desk.policy.unmap(window.id);
        desk.host.relayout();
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        let mut desk = self.desk.borrow_mut();
        // Focus moves on the surface's next pass: run it, so the window's
        // place and focus are known when the compositor asks.
        if desk.policy.focus(window.id) {
            desk.host.relayout();
        }
        true
    }

    fn focused(&mut self) -> Option<u64> {
        self.desk.borrow().policy.focused()
    }

    fn activate(&mut self, window: &WindowInfo, fresh: bool) -> bool {
        let mut desk = self.desk.borrow_mut();
        if fresh {
            // What the person just did opened it: bring it forward.
            if desk.policy.focus(window.id) {
                desk.host.relayout();
            }
        } else if desk.policy.ask_attention(window.id) {
            desk.host.relayout();
        }
        fresh
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        let config = self.desk.borrow().gesture_config;
        match self.gestures.read(gesture, config) {
            Reading::Clients => false,
            Reading::Taken => true,
            Reading::Asked(asked) => {
                self.desk.borrow_mut().gestures.push(asked);
                true
            }
        }
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desk.borrow_mut().policy.take_closed()
    }

    /// A window's own asking. A tab has no free geometry: the strip lays it
    /// out, so a move or a resize asks for nothing. Covering the display is
    /// the desktop's either way, the strip filling or the whole display, and
    /// the compositor is told the desktop took it rather than covering the
    /// window itself.
    fn request(&mut self, window: &WindowInfo, request: &WindowRequest) -> bool {
        let mut desk = self.desk.borrow_mut();
        let answered = match request {
            WindowRequest::Maximize(on) => desk.policy.set_maximized(window.id, *on),
            WindowRequest::Fullscreen { on, .. } => desk.policy.set_fullscreen(window.id, *on),
            WindowRequest::Move | WindowRequest::Resize(_) | WindowRequest::Minimize => {
                return false;
            }
        };
        if answered {
            desk.host.relayout();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display the test places windows on.
    fn output() -> LayoutRect {
        LayoutRect::new(LayoutPoint::from((0, 0)), (1280, 800).into())
    }

    /// A window of `app` made by the process `pid`.
    fn window(id: u64, app: &str, pid: Option<u32>) -> WindowInfo {
        WindowInfo {
            id,
            app_id: Some(app.to_owned()),
            pid,
            ..WindowInfo::default()
        }
    }

    /// Three fingers moving sideways carry the shown strip: the desk reads
    /// every report as the hand's total travel, from the first one on, and
    /// the lift as its release, rather than stepping the strip once the
    /// travel passes a threshold.
    #[test]
    fn three_fingers_sideways_carry_the_strip() {
        let mut reader = GestureReader::default();
        // Natural scrolling, the touchpad default: the strip goes with the
        // fingers, so their leftward travel goes toward later windows.
        let config = Some(GestureConfig {
            natural_scroll: true,
            ..GestureConfig::default()
        });
        assert_eq!(
            reader.read(&GestureEvent::SwipeBegin { fingers: 3 }, config),
            Reading::Taken,
            "the desk takes three fingers"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeUpdate { dx: -20.0, dy: 0.0 }, config),
            Reading::Asked(DeskGesture::StripDrag {
                travel: 20.0,
                began: true,
            }),
            "the first report already carries the strip"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeUpdate { dx: -10.0, dy: 0.0 }, config),
            Reading::Asked(DeskGesture::StripDrag {
                travel: 30.0,
                began: false,
            }),
            "the gesture reports its total"
        );
        assert_eq!(
            reader.read(&GestureEvent::SwipeEnd { cancelled: false }, config),
            Reading::Asked(DeskGesture::StripDragEnd { cancelled: false })
        );
    }

    /// The policy is told the process of each window the compositor maps, as
    /// the desktop gives it a place, so the app behind it opens its later
    /// windows there. A window without a process, or one already placed, has
    /// nothing to tell.
    #[test]
    fn a_mapped_window_tells_the_policy_its_process() {
        let (_desk, mut policy) = desktop(1, 1.0);
        let told = Rc::new(RefCell::new(Vec::new()));
        let heard = Rc::clone(&told);
        policy.set_window_mapped(Rc::new(move |pid| heard.borrow_mut().push(pid)));

        policy.map(&window(1, "so.tilo.notes", Some(4242)), &[output()]);
        assert_eq!(*told.borrow(), vec![4242], "the mapped window's process");

        policy.map(&window(1, "so.tilo.notes", Some(4242)), &[output()]);
        assert_eq!(*told.borrow(), vec![4242], "a window is placed once");

        policy.map(&window(2, "so.tilo.notes", None), &[output()]);
        assert_eq!(
            *told.borrow(),
            vec![4242],
            "a window without a process has none to tell"
        );
    }

    /// A window's app describes the controls of its own content, and nothing
    /// else on the desk changes. The desk takes them up in the frame that
    /// follows: what a person can reach does not wait for the next thing that
    /// happens to draw.
    #[test]
    fn a_windows_declared_controls_are_taken_up_in_the_next_frame() {
        use ui::window_targets::{TargetAction, WindowTarget};

        let (mut desk, mut policy) = desktop(7, 1.0);
        SurfaceHost::outputs_changed(
            &mut desk,
            &[tilo_session_core::surface::HostedOutput {
                rect: (0, 0, 1280, 800),
                scale: 1.0,
            }],
        );
        policy.map(&window(1, "so.tilo.file-explorer", Some(4242)), &[output()]);
        for _ in 0..8 {
            SurfaceHost::prepare_frame(&mut desk);
        }
        assert!(
            !SurfaceHost::prepare_frame(&mut desk),
            "the desk is at rest before the app describes its content"
        );
        let handle = desk.with_policy(|policy| policy.handle().clone());
        let tab = handle
            .shown_tabs()
            .first()
            .map(|(tab, _)| *tab)
            .expect("the window is drawn");
        assert!(handle.window_controls().is_empty());

        handle.declare_targets(
            tab,
            41,
            vec![WindowTarget {
                role: "Row".to_owned(),
                name: "Documents".to_owned(),
                rect: [10.0, 20.0, 110.0, 40.0],
                actions: vec![TargetAction::Activate],
            }],
        );
        assert!(
            handle.window_controls().is_empty(),
            "nothing of it is read before the frame that reads it"
        );

        SurfaceHost::prepare_frame(&mut desk);

        let listed = handle.window_controls();
        assert_eq!(
            listed
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["Documents"],
            "the frame after the declaration carries the window's controls"
        );
    }
}

#[cfg(test)]
mod hidpi {
    use super::*;

    /// The desk draws at the scale of the display it covers: its strips and
    /// the windows' bars paint the pixels a `HiDPI` panel shows, rather than
    /// one scale's worth stretched over twice the area.
    #[test]
    fn the_desk_draws_at_its_displays_scale() {
        let (mut desk, _policy) = desktop(7, 1.0);
        SurfaceHost::outputs_changed(
            &mut desk,
            &[tilo_session_core::surface::HostedOutput {
                rect: (0, 0, 1472, 920),
                scale: 2.0,
            }],
        );
        let scale = desk.with_widgets(|widgets| widgets.placement().scale);
        assert!(
            (scale - 2.0).abs() < f64::EPSILON,
            "the desk on a 2 display draws at 2: it drew at {scale}"
        );
    }
}
