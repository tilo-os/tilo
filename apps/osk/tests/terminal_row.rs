//! A terminal field gets a row of the keys a terminal needs that the letters
//! lack: Esc, Tab, Ctrl, Alt and the arrows. Each is a real key, and a finger
//! alone can reach and use every one.

use std::cell::RefCell;
use std::rc::Rc;

use osk::{InputPurpose, KeyboardLayout, Keypress, Latch, Modifiers, OskOutput, OskWidget, Symbol};
use ui::State;
use ui::headless::FrameHarness;
use vello::kurbo::Point;
use winit::event::TouchPhase;

/// A phone-width keyboard serving a terminal.
struct Terminal {
    driver: FrameHarness,
    sent: Rc<RefCell<Vec<OskOutput>>>,
    finger: u64,
}

impl Terminal {
    fn new() -> Self {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let sink = sent.clone();
        let widget = OskWidget::new()
            .with_purpose(InputPurpose::Terminal)
            .on_output(move |output| sink.borrow_mut().push(output));
        let driver = FrameHarness::for_root(State::new(), widget, (390, 320), ui::Theme::dark());
        Self {
            driver,
            sent,
            finger: 0,
        }
    }

    fn keyboard(&self) -> &OskWidget {
        self.driver.root::<OskWidget>()
    }

    /// Centre of the row item `id`.
    fn row(&self, id: &str) -> Point {
        let osk = self.keyboard();
        let index = osk
            .toolbar_items()
            .iter()
            .position(|item| item.id == id)
            .unwrap_or_else(|| panic!("no row item {id}"));
        osk.toolbar_geometry()
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, rect)| rect.center())
            .unwrap()
    }

    /// Centre of the letter key `label`.
    fn letter(&self, label: &str) -> Point {
        let layout = KeyboardLayout::qwerty();
        self.keyboard()
            .key_geometry()
            .iter()
            .find(|(id, _)| layout.key(*id).is_some_and(|key| key.label == label))
            .map_or_else(|| panic!("no key {label}"), |(_, rect)| rect.center())
    }

    fn tap(&mut self, at: Point) {
        self.finger = self.finger.saturating_add(1);
        self.driver
            .touch(self.finger, TouchPhase::Started, at.x, at.y);
        self.driver
            .touch(self.finger, TouchPhase::Ended, at.x, at.y);
        self.driver.recompose().unwrap();
    }

    fn tap_row(&mut self, id: &str) {
        self.tap(self.row(id));
    }

    fn tap_letter(&mut self, label: &str) {
        self.tap(self.letter(label));
    }

    fn take(&self) -> Vec<OskOutput> {
        std::mem::take(&mut *self.sent.borrow_mut())
    }
}

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
};
const ALT: Modifiers = Modifiers {
    ctrl: false,
    alt: true,
};

fn chord(symbol: Symbol, modifiers: Modifiers) -> OskOutput {
    OskOutput::Key(Keypress { symbol, modifiers })
}

#[test]
fn every_row_key_is_a_real_key_a_finger_reaches() {
    let mut terminal = Terminal::new();
    let letter_width = {
        let layout = KeyboardLayout::qwerty();
        terminal
            .keyboard()
            .key_geometry()
            .iter()
            .find(|(id, _)| layout.key(*id).is_some_and(|key| key.label == "q"))
            .unwrap()
            .1
            .width()
    };
    assert!(
        terminal
            .keyboard()
            .toolbar_geometry()
            .iter()
            .all(|(_, rect)| rect.width() >= letter_width && rect.height() >= 44.0),
        "each row key is at least as easy to hit as a letter"
    );
    for (id, symbol) in [
        ("esc", Symbol::Escape),
        ("tab", Symbol::Tab),
        ("left", Symbol::Left),
        ("down", Symbol::Down),
        ("up", Symbol::Up),
        ("right", Symbol::Right),
    ] {
        terminal.tap_row(id);
        assert_eq!(terminal.take(), [OskOutput::key(symbol)], "{id}");
    }
}

#[test]
fn ctrl_holds_for_the_next_key_only() {
    let mut terminal = Terminal::new();
    terminal.tap_row("ctrl");
    assert_eq!(terminal.keyboard().modifier_latches().ctrl, Latch::Once);
    assert!(terminal.take().is_empty(), "Ctrl alone sends nothing");
    terminal.tap_letter("c");
    assert_eq!(terminal.take(), [chord(Symbol::Unicode('c'), CTRL)]);
    assert_eq!(terminal.keyboard().modifier_latches().ctrl, Latch::Off);
    terminal.tap_letter("c");
    assert_eq!(terminal.take(), [OskOutput::SetPreedit("c".into())]);
}

#[test]
fn a_held_modifier_finishes_the_word_before_its_chord() {
    let mut terminal = Terminal::new();
    terminal.tap_letter("l");
    terminal.tap_letter("s");
    terminal.take();
    terminal.tap_row("alt");
    terminal.tap_row("left");
    assert_eq!(
        terminal.take(),
        [OskOutput::Commit("ls".into()), chord(Symbol::Left, ALT),]
    );
    assert_eq!(terminal.keyboard().modifier_latches().alt, Latch::Off);
}

#[test]
fn a_double_tap_locks_a_modifier_until_tapped_again() {
    let mut terminal = Terminal::new();
    terminal.tap_row("ctrl");
    terminal.tap_row("ctrl");
    assert_eq!(terminal.keyboard().modifier_latches().ctrl, Latch::Locked);
    terminal.tap_letter("a");
    terminal.tap_letter("e");
    terminal.tap_row("tab");
    assert_eq!(
        terminal.take(),
        [
            chord(Symbol::Unicode('a'), CTRL),
            chord(Symbol::Unicode('e'), CTRL),
            chord(Symbol::Tab, CTRL),
        ]
    );
    assert_eq!(terminal.keyboard().modifier_latches().ctrl, Latch::Locked);
    terminal.tap_row("ctrl");
    assert_eq!(terminal.keyboard().modifier_latches().ctrl, Latch::Off);
    terminal.tap_row("esc");
    assert_eq!(terminal.take(), [OskOutput::key(Symbol::Escape)]);
}

#[test]
fn ctrl_and_alt_combine_on_one_key() {
    let mut terminal = Terminal::new();
    terminal.tap_row("ctrl");
    terminal.tap_row("alt");
    terminal.tap_letter("d");
    assert_eq!(
        terminal.take(),
        [chord(
            Symbol::Unicode('d'),
            Modifiers {
                ctrl: true,
                alt: true
            }
        )]
    );
    assert!(terminal.keyboard().modifier_latches().held().is_empty());
}
