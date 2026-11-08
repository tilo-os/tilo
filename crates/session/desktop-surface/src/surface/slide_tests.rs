//! A strip or workspace the person goes to is drawn moving: the view they left stands where they
//! left it and the one they chose arrives from the side it lies on, so the change is a slide across
//! frames and never a cut.
//! Only a hand moves the view. A key or an action lands, as
//! `docs/architecture/window-management.md` says; a pointer and a finger slide, and while a finger
//! is down the deck is under it.

use super::*;
use ui::widget::TileContent;

struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

fn tab(minter: &mut IdMinter, title: &str) -> Tab {
    Tab {
        id: minter.mint(),
        title: title.into(),
        tile: tilo_desktop_model::TileContent::LocalApp {
            app_id: format!("so.tilo.{}", title.to_lowercase()),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

fn window_group(minter: &mut IdMinter, title: &str) -> tilo_desktop_model::WindowGroup {
    let tab = tab(minter, title);
    tilo_desktop_model::WindowGroup {
        id: minter.mint(),
        active_tab: tab.id,
        tabs: vec![tab],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A desk of three strips, each holding one window, on a held clock.
struct Desk {
    frame: ui::headless::FrameHarness,
    handle: DesktopHandle,
    now: Instant,
    strips: Vec<StripId>,
}

impl Desk {
    fn new() -> Self {
        let mut minter = IdMinter::new(9);
        let notes = tab(&mut minter, "Notes");
        let mut tree = seed_tree(&mut minter, "Desk", vec![notes]);
        let workspace = &mut tree.workspaces[0];
        let mut strips = vec![workspace.strips[0].id];
        for name in ["Research", "Reading"] {
            let id = StripId(minter.mint());
            let group = window_group(&mut minter, name);
            workspace.strips.push(tilo_desktop_model::Strip {
                id,
                name: name.into(),
                window_groups: vec![group],
            });
            strips.push(id);
        }
        // A second workspace with a strip of its own, so a switch one level out has somewhere to
        // go.
        let mut elsewhere = Workspace::create("Elsewhere");
        let elsewhere_strip = tilo_desktop_model::Strip {
            id: StripId(minter.mint()),
            name: "Main".into(),
            window_groups: Vec::new(),
        };
        elsewhere.strips = vec![elsewhere_strip];
        tree.workspaces.push(elsewhere);
        let (surface, handle) = DesktopSurface::new(tree, 9, Box::new(Apps));
        handle.set_viewport_width(1200.0);
        let frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (1200, 800),
            ui::headless::FrameOptions::default(),
        );
        let mut desk = Self {
            frame,
            handle,
            now: ui::time::now(),
            strips,
        };
        desk.frames(4);
        desk
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.now = self.now.checked_add(Duration::from_millis(16)).unwrap();
            ui::time::testing::with_monotonic_time(self.now, || self.frame.relayout());
        }
    }

    /// Go to `strip` with `source` , as the person did.
    fn go_to(&mut self, strip: StripId, source: ui::tokens::MotionSource) {
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.slot.0.to_string()))
            .expect("a workspace");
        ui::motion::note_source(source);
        self.handle
            .dispatch(navigation::ChromeEffect::ShowStrip {
                workspace,
                strip: strip.0.to_string(),
            })
            .expect("the strip is there");
        // The layer a host draws for the chrome names what asked before the change is shown: the
        // frame states it, as a chord or a press does.
        self.frame.root_mut::<DesktopSurface>().note_source(source);
    }

    /// Where the leaving strip is drawn this frame, in pixels from where it stood.
    fn leaving(&self) -> Option<f64> {
        self.frame
            .root::<DesktopSurface>()
            .slides
            .borrow()
            .leaving_offset(self.now)
    }

    /// The surface, for a test that moves the deck as its host does.
    fn surface(&mut self) -> ui::headless::RootMut<'_, DesktopSurface> {
        self.frame.root_mut::<DesktopSurface>()
    }

    fn height(&self) -> f64 {
        self.frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .map_or(0.0, |row| row.get_rect().height())
    }

    /// The strip the surface shows.
    fn shown(&self) -> Option<StripId> {
        self.frame.root::<DesktopSurface>().shown
    }

    /// What the surface says a move from `before` to `after` is.
    fn change(
        &self,
        before: Option<StripId>,
        after: Option<StripId>,
    ) -> (super::slide::SlideKind, f64, f64) {
        let tree = self.handle.read(Clone::clone);
        DesktopSurface::change_of(before, after, &tree)
    }

    fn moving(&self) -> bool {
        self.frame
            .root::<DesktopSurface>()
            .slides
            .borrow()
            .is_moving(self.now)
    }
}

/// A strip the person clicked to is drawn on its way across the frames after the click, not cut to
/// its place: the strip they left stands at its place and moves by more with every frame until it
/// is gone.
#[test]
fn a_clicked_strip_slides_across_frames_instead_of_cutting() {
    let mut desk = Desk::new();
    let second = desk.strips[1];
    desk.go_to(second, ui::tokens::MotionSource::Pointer);
    desk.frames(1);
    let first = desk.leaving().expect("the strip left is drawn");
    assert_eq!(
        first, 0.0,
        "the slide starts from what the person saw: {first}"
    );
    desk.frames(3);
    let later = desk.leaving().expect("still on its way");
    assert!(
        later < 0.0,
        "going down the deck, the strip left goes up: {later}"
    );
    desk.frames(6);
    let further = desk.leaving().expect("still on its way");
    assert!(
        further < later,
        "the strips move on every frame, not in one step: {further} after {later}"
    );
    desk.frames(60);
    assert_eq!(
        desk.leaving(),
        None,
        "a slide comes to rest and stops drawing"
    );
    assert!(!desk.moving());
}

/// A strip the person went back to slides the other way.
#[test]
fn going_back_slides_the_other_way() {
    let mut desk = Desk::new();
    let (second, first) = (desk.strips[1], desk.strips[0]);
    desk.go_to(second, ui::tokens::MotionSource::Pointer);
    desk.frames(2);
    assert!(desk.moving(), "the first change is on its way");
    desk.frames(80);
    assert!(!desk.moving());
    desk.go_to(first, ui::tokens::MotionSource::Pointer);
    desk.frames(4);
    let shift = desk.leaving().expect("the strip left is drawn");
    assert!(
        shift > 0.0,
        "going up the deck, the strip left goes down: {shift}"
    );
}

/// The move the strip change is: from one strip to the next along the deck,
/// which way it goes, and how many screens the deck travels. The source it
/// is made from says whether anything is drawn moving at all.
#[test]
fn a_strip_change_is_a_step_along_the_deck() {
    let desk = Desk::new();
    let (first, second, third) = (desk.strips[0], desk.strips[1], desk.strips[2]);
    let (kind, toward, steps) = desk.change(Some(first), Some(second));
    assert_eq!(kind, super::slide::SlideKind::Strip);
    assert_eq!((toward, steps), (1.0, 1.0), "down the deck, one screen");
    let (_, toward, steps) = desk.change(Some(third), Some(first));
    assert_eq!((toward, steps), (-1.0, 2.0), "up the deck, two screens");
}

/// A chord's strip change moves within `fast` , as the design note says: spatial context may move,
/// briefly, and it never takes the deck's full settle.
#[test]
fn a_key_slide_is_within_fast() {
    let mut desk = Desk::new();
    let second = desk.strips[1];
    desk.go_to(second, ui::tokens::MotionSource::Keyboard);
    desk.frames(1);
    assert_eq!(desk.shown(), Some(second), "the strip it went to is shown");
    assert_eq!(
        desk.leaving(),
        Some(0.0),
        "it starts from what the person saw"
    );
    // `fast` is 130 ms: within a tenth of a second the strips have arrived.
    desk.frames(10);
    assert!(!desk.moving(), "a chord's change is over within `fast`");
}

/// A change nobody's hand made (an action, a script, an agent) is a shortcut's too: it moves within
/// `fast` and never waits on a hand.
#[test]
fn an_action_slide_is_within_fast() {
    let mut desk = Desk::new();
    let second = desk.strips[1];
    desk.go_to(second, ui::tokens::MotionSource::Action);
    desk.frames(1);
    assert_eq!(desk.shown(), Some(second));
    desk.frames(10);
    assert!(!desk.moving());
}

/// A shortcut's slide is shorter than a hand's: the same change made with a pointer is still moving
/// when a chord's has arrived.
#[test]
fn a_chord_s_slide_is_shorter_than_a_click_s() {
    let mut by_key = Desk::new();
    let second = by_key.strips[1];
    by_key.go_to(second, ui::tokens::MotionSource::Keyboard);
    by_key.frames(10);
    let mut by_click = Desk::new();
    let second = by_click.strips[1];
    by_click.go_to(second, ui::tokens::MotionSource::Pointer);
    by_click.frames(10);
    assert!(!by_key.moving(), "the chord's change is over");
    assert!(by_click.moving(), "the click's is still moving");
}

/// The fingers carry the deck: every update puts the strip they landed on exactly that far the
/// other way, so the strips are under the hand.
#[test]
fn a_finger_carries_the_deck_with_it() {
    let mut desk = Desk::new();
    let height = desk.height();
    assert!(height > 0.0);
    desk.surface().note_laid_strip();
    desk.surface().begin_strip_slide();
    let mut last = 0.0;
    for travel in [10.0f64, 60.0, 120.0] {
        desk.surface().drag_strip_slide(travel);
        let drawn = desk.leaving().expect("the deck follows the fingers");
        assert!(
            (drawn - travel).abs() < 0.5 && drawn > last,
            "the strips follow the fingers one to one: {drawn} for {travel}"
        );
        last = drawn;
    }
    assert!(
        desk.moving(),
        "the strips are drawn moving for as long as the hand is on them"
    );
}

/// A slide starts from what the person sees: the strip it leaves is drawn at its own place at
/// first, and the whole motion asks the clock for frames until it settles.
#[test]
fn a_slide_asks_for_frames_and_starts_from_rest() {
    let mut desk = Desk::new();
    let second = desk.strips[1];
    desk.go_to(second, ui::tokens::MotionSource::Pointer);
    desk.frames(1);
    assert_eq!(desk.leaving(), Some(0.0));
    assert!(
        desk.moving(),
        "the slide is what makes the desk draw the frames after it"
    );
    desk.frames(120);
    assert!(!desk.moving());
}

/// A change asked for mid-slide keeps its place and carries on as one motion instead of starting
/// over from the top.
#[test]
fn a_second_change_mid_slide_carries_on_from_where_it_is_drawn() {
    let mut desk = Desk::new();
    let (first, second, third) = (desk.strips[0], desk.strips[1], desk.strips[2]);
    desk.go_to(second, ui::tokens::MotionSource::Pointer);
    desk.frames(3);
    let partway = desk.leaving().expect("on its way");
    desk.go_to(third, ui::tokens::MotionSource::Pointer);
    desk.frames(1);
    let after = desk.leaving().expect("still drawn");
    // The slide restarts its progress but keeps its place: the strip left is still the first one,
    // drawn from where it stood, so the second change carries on out instead of cutting.
    assert!(
        after < 0.0 && after > -desk.height(),
        "the strips carry on from the place they were left in: {after} after {partway}"
    );
    let _ = first;
}

/// A workspace switch is a whole-view change one level out: it moves a direction's worth of the
/// view, not a whole screen, as the old shell drew it.
#[test]
fn a_workspace_switch_is_a_whole_view_change() {
    let mut desk = Desk::new();
    let workspace = desk
        .handle
        .read(|tree| {
            tree.workspaces
                .get(1)
                .map(|w| (w.slot.0.to_string(), w.id()))
        })
        .expect("a second workspace");
    ui::motion::note_source(ui::tokens::MotionSource::Pointer);
    desk.handle
        .dispatch(navigation::ChromeEffect::SwitchWorkspace {
            workspace: Some(workspace.0),
            delta: None,
        })
        .expect("the workspace is there");
    desk.frames(2);
    let shift = desk.leaving().expect("the workspace left is drawn");
    assert!(
        shift < 0.0 && shift.abs() <= 28.0,
        "a shared axis, not a full-width push: {shift}"
    );
}

/// The slide is not started when nothing was left behind: a request for the strip already shown
/// paints no motion.
#[test]
fn asking_for_the_strip_already_shown_moves_nothing() {
    let mut desk = Desk::new();
    let first = desk.strips[0];
    desk.go_to(first, ui::tokens::MotionSource::Pointer);
    desk.frames(2);
    assert_eq!(desk.leaving(), None);
    assert!(!desk.moving());
}

/// A hand's change is drawn from where the person was; a chord's is over within `fast` . Both start
/// from what was on screen, so neither cuts.
#[test]
fn every_strip_change_starts_from_what_the_person_saw() {
    for source in [
        ui::tokens::MotionSource::Pointer,
        ui::tokens::MotionSource::Gesture,
        ui::tokens::MotionSource::Keyboard,
        ui::tokens::MotionSource::Action,
    ] {
        let mut desk = Desk::new();
        let second = desk.strips[1];
        desk.go_to(second, source);
        desk.frames(1);
        assert_eq!(
            desk.leaving(),
            Some(0.0),
            "{source:?}: the change starts from the place the person saw"
        );
    }
}
