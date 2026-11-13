//! The shortcut guide in a page: the session's own sheet
//! ([`tilo_sheets::shortcut_guide`]) over the desktop, explaining the table
//! this page's desk answers keys with. What the person changes here is kept in
//! their settings, so it reaches their other devices; the chords pressed for a
//! shortcut are taken from the page's keys, with the page's stand-in for Super.

use std::cell::Cell;
use std::rc::Rc;

use settings_store::hardware::ShortcutConfig;
use tilo_desktop::shortcuts::{Shared, page_chord, shortcut_chord_from_event};
use tilo_sheets::Presentation;
use tilo_sheets::shortcut_guide::{self, Ask, Caught, Guide, GuideEffect, State};

use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::over_page::OverSheet;

/// The shortcut guide over the page.
pub(crate) struct PageGuide {
    /// The table the desk answers and the guide explains, kept in the
    /// person's settings.
    table: Shared,
    state: Rc<State>,
    presentation: Presentation,
    /// The guide as it was last built into a sheet.
    built: Option<Guide>,
}

impl PageGuide {
    pub(crate) fn new() -> Self {
        let table: Shared = Rc::new(std::cell::RefCell::new(ShortcutConfig::load()));
        let state = Rc::new(State::new(
            Rc::clone(&table),
            None,
            Rc::new(Cell::new(None)),
        ));
        Self {
            table,
            state,
            presentation: Presentation::default(),
            built: None,
        }
    }

    /// The table the desk answers keys with: the person's, as the guide shows
    /// and changes it.
    pub(crate) fn shortcuts(&self) -> Shared {
        Rc::clone(&self.table)
    }

    /// Whether the guide is up, taking the keyboard and what it covers.
    pub(crate) fn is_open(&self) -> bool {
        self.presentation.is_open()
    }

    /// The person asked for the guide, or asked for it to go.
    pub(crate) fn toggle(&self) {
        if self.state.up() {
            self.close();
        } else {
            self.state.set_up(true);
            self.presentation.open(true);
        }
    }

    fn close(&self) {
        self.state.set_up(false);
        self.presentation.close();
    }

    /// Take what the keyboard asked, with the modifiers held.
    pub(crate) fn heard(
        &self,
        event: &WindowEvent,
        modifiers: ui::winit::event::Modifiers,
        mac: bool,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed || self.state.listening().is_none() {
            return false;
        }
        if key.logical_key == Key::Named(NamedKey::Escape) {
            self.state.stop_listening();
            self.state.catch(Caught::Cancelled);
        } else if let Some(chord) = page_chord(&key.logical_key, key.physical_key, modifiers, mac)
            .and_then(|(key, modifiers)| shortcut_chord_from_event(&key, modifiers))
        {
            self.state.catch(Caught::Chord(chord));
        }
        true
    }

    /// Answer what the guide asked, and show it as it stands.
    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = false;
        self.state.follow();
        if self.state.up() {
            let guide = self.state.shaped();
            if missing || self.built.as_ref() != Some(&guide) {
                self.built = Some(guide.clone());
                let state = Rc::clone(&self.state);
                let presentation = self.presentation.clone();
                let ask: Ask = Rc::new(move |effect: GuideEffect| {
                    if effect == GuideEffect::Done {
                        // Its own close: the card leaves as the sheet's does.
                        presentation.close();
                    }
                    state.effect(effect);
                });
                show(Some(shortcut_guide::sheet(
                    &guide,
                    &ask,
                    Some(&self.presentation),
                )));
                changed = true;
            }
        } else if self.built.take().is_some() {
            show(None);
            changed = true;
        }
        changed
    }
}

/// Build the guide's sheet as it stands: it loads with the page's other
/// sheets ([`crate::over_page::SHEETS`]), which nothing before the person's
/// first look at one needs.
pub(crate) fn follow(
    guide: &mut PageGuide,
    show: &mut dyn FnMut(Option<ui::widget::Element>),
    missing: bool,
) -> bool {
    guide.follow(show, missing)
}

impl OverSheet for PageGuide {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        // Down and never drawn, the guide has nothing to show and nothing
        // was asked of it: keys are only caught while it is up. Asked up
        // before its code is in, it shows once the code is, as the page lays
        // out again.
        if !self.state.up() && self.built.is_none() {
            return false;
        }
        crate::over_page::SHEETS
            .get()
            .is_some_and(|code| (code.guide)(self, show, missing))
    }

    fn drawn(&self) -> bool {
        self.presentation.is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageGuide"
    }
}
