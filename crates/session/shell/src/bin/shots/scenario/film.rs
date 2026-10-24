//! Films: every frame of a shell motion at 60 Hz, from the input that starts
//! it until nothing on screen asks for another frame, and two frames more.
//!
//! A [`Film`] applies its cue and paints frame after frame on the toolkit's
//! clock, pinned to exact 60 Hz instants, while the wall clock is held in
//! step with it: what still reads the wall clock directly moves at the same
//! pace. Frames are only painted while the motion runs; the sink renders
//! them afterwards, so however long a software render takes, it never
//! stretches the motion between two frames. Each frame records who still
//! asks for animation frames, so a motion that looks settled while
//! something keeps the loop busy shows in the film's JSON.
//!
//! A [`Step`] is a shell-side cue or setup a film needs beyond the
//! scenario's [`Action`]s: a notification arriving, an app's uninstall
//! sheet, a player playing.

use std::sync::Arc;
use std::time::Duration;

use winit::keyboard::{Key, ModifiersState};

use super::{Action, NullSink, ShellDriver, SnapshotSink};

/// The rate films are recorded at: a 60 Hz display.
pub const HZ: u32 = 60;
/// One frame of the 60 Hz display films are recorded at.
#[cfg(feature = "test-support")]
const FRAME: Duration = Duration::from_nanos(16_666_667);
/// The longest a film runs when its motion never settles: three seconds.
const DEFAULT_MAX_FRAMES: u32 = 180;
/// Frames every film records at least: what changes on screen without
/// asking for frames shows too.
const DEFAULT_MIN_FRAMES: u32 = 20;
/// Frames recorded after the first frame nothing asks for animation in.
#[cfg(feature = "test-support")]
const FRAMES_AFTER_SETTLED: u32 = 2;

/// Record every frame of the motion `cue` starts. See the module docs.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(feature = "test-support"),
    allow(
        dead_code,
        reason = "only a test-support build pins the clock a film records on"
    )
)]
pub struct Film {
    /// Frames are written as `<prefix>-000.png`, `<prefix>-001.png`, … and
    /// described in `<prefix>.json`.
    pub prefix: &'static str,
    /// Applied at frame 0, in order, on the pinned clock.
    pub cue: Vec<Step>,
    /// Record at least this many frames: a motion that waits before it
    /// starts (a hover's dwell) asks for no frames while it waits.
    pub min_frames: u32,
    /// Stop here even if something still asks for frames.
    pub max_frames: u32,
}

impl Film {
    pub fn new(prefix: &'static str, cue: Vec<Step>) -> Self {
        Self {
            prefix,
            cue,
            min_frames: DEFAULT_MIN_FRAMES,
            max_frames: DEFAULT_MAX_FRAMES,
        }
    }

    /// Record at least `frames` frames.
    pub const fn at_least(mut self, frames: u32) -> Self {
        self.min_frames = frames;
        self
    }
}

/// A film's frame 0 pressing `key` with `modifiers` held.
pub fn key_film(prefix: &'static str, modifiers: ModifiersState, key: Key) -> Film {
    Film::new(prefix, vec![Step::Key(modifiers, key)])
}

/// A film's frame 0 applying `action`.
pub fn act_film(prefix: &'static str, action: Action) -> Film {
    Film::new(prefix, vec![Step::Act(Box::new(action))])
}

/// A cue or setup step of the motion films.
#[derive(Debug, Clone)]
pub enum Step {
    /// One of the scenario's own actions.
    Act(Box<Action>),
    /// Press `key` with these modifiers held, then let them go.
    Key(ModifiersState, Key),
    /// A secondary click on the named target.
    RightClickNamed(&'static str),
    /// An app posts a notification.
    Notify {
        app: &'static str,
        title: &'static str,
        body: &'static str,
    },
    /// The newest notification banner is dismissed.
    DismissNewestNotification,
    /// "Uninstall Notes?", as the app list asks it.
    AskUninstall,
    /// A music player is playing, as the now-playing card reads it.
    NowPlaying,
    /// A laptop's Quick Settings (Wi-Fi with networks around, Bluetooth,
    /// battery, power modes), closed.
    LaptopQuickSettings,
    /// The pointer onto the strip rail's segment for window group `index`,
    /// and a click there if `click`.
    RailSegment { index: usize, click: bool },
    /// Print every named target on screen and its rect to stderr: the names
    /// a scenario can click or hover.
    LogTargets(&'static str),
    /// A collaborator's pointer is heard at this point of the desktop.
    PeerPointer {
        who: &'static str,
        at: vello::kurbo::Point,
    },
}

/// One painted frame of a film.
#[cfg_attr(
    not(feature = "test-support"),
    allow(
        dead_code,
        reason = "only a test-support build pins the clock a film records on"
    )
)]
pub struct FilmFrame {
    /// `None` for the scene just before the cue.
    pub index: Option<u32>,
    /// Since the cue, on the pinned clock.
    pub at: Duration,
    /// How far the wall clock had run past `at` when the frame was painted.
    pub late: Duration,
    pub scene: vello::Scene,
    pub backdrop: Option<Arc<ui::backdrop::BackdropFrame>>,
    /// Who asked for another animation frame after this one; empty once the
    /// loop may idle.
    pub owners: Vec<String>,
}

/// `Action::Film` and `Action::Step`.
pub(super) fn apply<S: SnapshotSink>(driver: &mut ShellDriver, action: &Action, sink: &mut S) {
    match action {
        Action::Step(step) => apply_step(driver, step),
        Action::Film(film) => record(driver, film, sink),
        _ => {}
    }
}

fn apply_step(driver: &mut ShellDriver, step: &Step) {
    match step {
        Step::Act(action) => super::apply(driver, action, &mut NullSink),
        Step::Key(modifiers, key) => {
            driver.set_modifiers(*modifiers);
            driver.press_key(key);
            driver.set_modifiers(ModifiersState::empty());
        }
        Step::RightClickNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => {
                use ui::synthetic_input::PointerAction;
                use winit::event::MouseButton;
                driver.move_cursor(p);
                driver.dispatch(&PointerAction::Press(MouseButton::Right).event(1.0));
                driver.dispatch(&PointerAction::Release(MouseButton::Right).event(1.0));
            }
            None => eprintln!("scenario: could not resolve named target '{name}'"),
        },
        Step::Notify { app, title, body } => driver.with_chrome(|st| {
            st.push_notification_full(
                tilo_desktop::notifications::Notification::new(*title, *body)
                    .with_app_name(*app)
                    .with_dismiss_after(Duration::from_secs(60)),
            );
        }),
        Step::DismissNewestNotification => driver.with_chrome(|st| {
            if let Some(id) = st.notifications().last().map(|n| n.id) {
                st.dismiss_notification(id);
            }
        }),
        Step::AskUninstall => {
            use shell::shell::app_list::{AppListItem, AppSource};
            driver.host_mut().ask_uninstall(AppListItem {
                id: "so.tilo.notes".into(),
                name: "Notes".into(),
                icon: None,
                source: AppSource::Tilo { added: true },
                size: None,
            });
        }
        Step::NowPlaying => driver.seed_now_playing_for_shots(),
        Step::LaptopQuickSettings => driver.with_chrome(|st| {
            ShellDriver::seed_laptop_quick_settings(st);
            st.overlays.quick_settings = false;
        }),
        Step::RailSegment { index, click } => {
            let rail = driver.host().strip_rail_for_tests();
            match rail.hit_rect(*index) {
                Some(r) if *click => driver.click_at(r.center()),
                Some(r) => driver.move_cursor(r.center()),
                None => eprintln!("scenario: the strip rail has no segment {index}"),
            }
        }
        Step::PeerPointer { who, at } => driver.peer_pointer_for_shots(who, *at),
        Step::LogTargets(label) => {
            let mut targets: Vec<_> = driver.widget_registry().into_iter().collect();
            targets.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, r) in targets {
                eprintln!(
                    "targets[{label}] {name} ({:.0},{:.0} {:.0}x{:.0})",
                    r.x0,
                    r.y0,
                    r.width(),
                    r.height()
                );
            }
        }
    }
}

#[cfg(not(feature = "test-support"))]
fn record<S: SnapshotSink>(_driver: &mut ShellDriver, film: &Film, _sink: &mut S) {
    eprintln!(
        "scenario: film '{}' pins the toolkit's clock, which needs `--features test-support`; skipping",
        film.prefix
    );
}

#[cfg(feature = "test-support")]
fn record<S: SnapshotSink>(driver: &mut ShellDriver, film: &Film, sink: &mut S) {
    // Whatever the set-up left pending is laid out and painted before the
    // cue and recorded, so frame 0 differs from the scene before only by
    // the cue.
    driver.relayout();
    let before = FilmFrame {
        index: None,
        at: Duration::ZERO,
        late: Duration::ZERO,
        scene: driver.paint(),
        backdrop: driver.take_backdrop(),
        owners: driver.animation_owners_at(ui::time::now()),
    };
    let wall_start = std::time::Instant::now();
    let start = ui::time::now();
    let mut frames = vec![before];
    let mut idle_since: Option<u32> = None;
    for index in 0..film.max_frames {
        let at = FRAME.saturating_mul(index);
        // Hold the wall clock in step with the pinned one.
        if let Some(ahead) = at.checked_sub(wall_start.elapsed()) {
            std::thread::sleep(ahead);
        }
        let now = start.checked_add(at).unwrap_or(start);
        let frame = ui::time::testing::with_monotonic_time(now, || {
            if index == 0 {
                for step in &film.cue {
                    apply_step(driver, step);
                }
            }
            driver.relayout();
            let scene = driver.paint();
            FilmFrame {
                index: Some(index),
                at,
                late: wall_start.elapsed().saturating_sub(at),
                scene,
                backdrop: driver.take_backdrop(),
                owners: driver.animation_owners_at(now),
            }
        });
        idle_since = match (frame.owners.is_empty(), idle_since) {
            (false, _) => None,
            (true, None) => Some(index),
            (true, since) => since,
        };
        frames.push(frame);
        let done =
            idle_since.is_some_and(|since| index >= since.saturating_add(FRAMES_AFTER_SETTLED));
        if done && index.saturating_add(1) >= film.min_frames {
            break;
        }
    }
    sink.film(driver, film.prefix, frames);
}
