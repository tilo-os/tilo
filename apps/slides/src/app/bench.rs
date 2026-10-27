//! The editor under test, as it runs: the framework app on a private
//! profile, its deck edited only through the resource's dispatcher. A test
//! reaches into the editor's view state with [`Bench::with`], sends it input
//! as a window does with [`Bench::send`] and the harness, and reads the deck
//! the document holds with [`Bench::deck`].

use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use vello::kurbo::Point;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::SlidesApp;
use crate::SlidesShell;
use crate::actions::replace_deck_action;
use crate::model::{Deck, DeckLook, PaletteKind, Slide};

/// The window most tests use.
pub(super) const WINDOW: (u32, u32) = (1280, 800);

pub(super) struct Bench {
    pub(super) app: TestApp<SlidesShell>,
}

impl Bench {
    /// The editor at `window` over `deck`, opened on its first slide with
    /// nothing selected.
    pub(super) fn open(deck: &Deck, window: (u32, u32)) -> Self {
        Self::open_as(deck, window, ui::permissions::Role::Owner)
    }

    /// [`Self::open`] for a participant holding `role`.
    pub(super) fn open_as(deck: &Deck, window: (u32, u32), role: ui::permissions::Role) -> Self {
        let mut app = TestApp::<SlidesShell>::new().expect("the app opens");
        app.invoke(Invoke::apply(&replace_deck_action(deck)).expect("the deck encodes"))
            .expect("the deck opens");
        app.set_role(role);
        app.harness().resize(window).expect("the window resizes");
        let mut bench = Self { app };
        bench.with(|editor| {
            editor.bootstrapped = true;
            editor.cur_slide = 0;
            editor.sel = None;
        });
        bench.frame();
        bench
    }

    /// The editor over the sample deck at the usual window, opened on its
    /// first slide.
    pub(super) fn sample() -> Self {
        Self::open(&crate::model::seed_deck(), WINDOW)
    }

    pub(super) fn one_slide(slide: Slide) -> Self {
        Self::open(&deck_of(vec![slide]), WINDOW)
    }

    /// Run `visit` on the editor, then a frame: whatever it submitted is
    /// applied, and the deck it shows follows.
    pub(super) fn with<O>(&mut self, visit: impl FnOnce(&mut SlidesApp) -> O) -> O {
        self.app
            .harness()
            .with_widget_mut::<SlidesApp, _>(visit)
            .expect("the editor is mounted")
    }

    /// Read the editor.
    pub(super) fn read<O>(&mut self, read: impl FnOnce(&SlidesApp) -> O) -> O {
        self.with(|editor| read(editor))
    }

    /// One frame with no input: the host applies what was submitted and the
    /// editor syncs and draws.
    pub(super) fn frame(&mut self) {
        self.with(|_| ());
    }

    /// Give the editor keyboard focus, as clicking into it does.
    pub(super) fn focus(&mut self) {
        self.app
            .harness()
            .perform_named("slides-editor", &ui::a11y::Action::Focus)
            .expect("the editor takes focus");
    }

    /// Hand the window `event`, as the event loop does.
    pub(super) fn send(&mut self, event: &WindowEvent) {
        self.app.harness().send(event);
    }

    pub(super) fn move_to(&mut self, p: Point) {
        self.send(&cursor_ev(p));
    }

    pub(super) fn press(&mut self) {
        self.send(&left_press_ev());
    }

    pub(super) fn release(&mut self) {
        self.send(&release_ev());
    }

    pub(super) fn click(&mut self, p: Point) {
        self.app.harness().click_at(p.x, p.y);
    }

    /// Press `key` (a named key like `"Enter"`, `"ArrowLeft"`, or a
    /// character), as [`ui::headless::FrameHarness::key`] spells it.
    pub(super) fn key(&mut self, key: &str) {
        self.app.harness().key(key);
    }

    /// Press and release the named key `key`.
    pub(super) fn named(&mut self, key: NamedKey) {
        self.send(&key_event(Key::Named(key), None, ElementState::Pressed));
        self.send(&key_event(Key::Named(key), None, ElementState::Released));
    }

    /// Press `key` while holding `modifiers` (`"ctrl"`, `"shift"`, ...).
    pub(super) fn chord(&mut self, modifiers: &[&str], key: &str) {
        let held: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
        self.app.harness().set_modifiers(&held);
        self.app.harness().key(key);
        self.app.harness().set_modifiers(&[]);
    }

    pub(super) fn type_text(&mut self, text: &str) {
        self.app.harness().type_text(text);
    }

    pub(super) fn deck(&self) -> Deck {
        crate::deck::mirror(&self.app.state())
    }

    pub(super) fn undo(&mut self) -> bool {
        self.app.undo()
    }
}

pub(super) fn deck_of(slides: Vec<Slide>) -> Deck {
    Deck {
        title: "t".into(),
        palette: PaletteKind::Indigo,
        look: DeckLook::default(),
        slides,
    }
}

pub(super) fn cursor_ev(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

pub(super) fn left_press_ev() -> WindowEvent {
    ui::synthetic_input::mouse_press()
}

pub(super) fn release_ev() -> WindowEvent {
    ui::synthetic_input::mouse_release()
}

/// A key event as the window delivers it: `logical` pressed or released,
/// typing `text`.
pub(super) fn key_event(logical: Key, text: Option<&str>, state: ElementState) -> WindowEvent {
    let event =
        ui::synthetic_input::key_event(logical, text.map(winit::keyboard::SmolStr::new), state);
    ui::synthetic_input::keyboard_input(event)
}
