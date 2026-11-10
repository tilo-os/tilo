//! Alt+Tab's panel: what the desk's walk shows, kept in step with the chrome that draws it and the
//! windows it pictures.
//! The walk itself is the desk's ([`crate::overlays`]): Alt+Tab's presses ask its verbs, and it
//! says which window it reached. This reads that once a pass, as the panel shows it, so the chrome
//! draws what the desk decided and nothing of its own.
//! A host without the desk's door, the page, moves the same walk through [`WindowWalk`].

use std::rc::Rc;

use tilo_desktop::window_switcher::Switcher;
use tilo_session_core::surface::KeyModifiers;
use ui::winit::event::{ElementState, KeyEvent};
use ui::winit::keyboard::{Key, ModifiersState};

use crate::keys::{Press, SwitcherKey, WalkKeys};

impl crate::Session {
    /// Alt+Tab's panel: what it shows and where its cards stand, as the chrome last drew it.
    #[must_use]
    pub fn window_switcher(&self) -> tilo_desktop::window_switcher::Switcher {
        self.shared.switcher.clone()
    }
}

/// Show on `panel` what `walk` reached over `handle`'s windows this pass, or take it down: whether
/// the panel changed. A walk held less than [`crate::overlays::REVEAL_DELAY`] shows nothing: a quick
/// Alt+Tab switches windows without a panel flashing, and the window it reached is still where
/// Alt's release goes.
pub(crate) fn show(
    handle: &tilo_desktop_surface::DesktopHandle,
    walk: &crate::overlays::Overlays,
    panel: &Switcher,
    locked: bool,
    sizes: &std::collections::HashMap<u64, (u32, u32)>,
) -> bool {
    if locked {
        walk.close(crate::overlays::Overlay::Switcher.name());
        walk.forget_walk();
    }
    // A flick shorter than the panel's reveal delay shows nothing, and costs no reading of the
    // desk's windows.
    let view = (!locked && walk.revealed(tilo_clock::Instant::now()))
        .then(|| crate::desk_windows::switcher_view(handle, walk, sizes))
        .flatten();
    panel.set(view)
}

/// Alt+Tab on a host that hands the session its keys itself, as the page does: the desk's walk over
/// its windows, the panel the chrome draws of it, and the keys that move it. The walk is the one the
/// desk's verbs move on a native desktop; with no door to the desk here, its keys move it directly.
pub struct WindowWalk {
    handle: tilo_desktop_surface::DesktopHandle,
    walk: Rc<crate::overlays::Overlays>,
    keys: WalkKeys,
    panel: Switcher,
}

impl WindowWalk {
    /// The walk over `handle`'s windows, with no panel up.
    #[must_use]
    pub fn new(handle: tilo_desktop_surface::DesktopHandle) -> Self {
        let walk = Rc::new(crate::overlays::Overlays::new());
        Self {
            handle,
            keys: WalkKeys::new(Rc::clone(&walk)),
            walk,
            panel: Switcher::default(),
        }
    }

    /// The panel, for the chrome to draw.
    #[must_use]
    pub fn panel(&self) -> Switcher {
        self.panel.clone()
    }

    /// Take the page's key `event`, with `modifiers` held: whether it was the walk's. Off a Mac the
    /// system keeps Alt+Tab, so the page's stand-in for Super (Alt+Shift) holds the walk's Alt there
    /// and Alt+Shift+Tab walks; a Mac leaves Option+Tab to the page, and Shift walks back.
    pub fn key(&self, event: &KeyEvent, modifiers: ModifiersState, mac: bool) -> bool {
        self.take(
            &event.logical_key,
            event.state == ElementState::Pressed,
            event.repeat,
            modifiers,
            mac,
        )
    }

    fn take(
        &self,
        key: &Key,
        pressed: bool,
        repeat: bool,
        modifiers: ModifiersState,
        mac: bool,
    ) -> bool {
        let alt = modifiers.alt_key();
        let press = Press {
            key: key.clone(),
            bare: key.clone(),
            modifiers: KeyModifiers {
                ctrl: modifiers.control_key(),
                alt,
                shift: modifiers.shift_key() && (mac || !alt),
                logo: modifiers.super_key(),
                caps_lock: false,
            },
            pressed,
            repeat,
            code: 0,
            device: None,
            app_shortcut: None,
        };
        if self.keys.claim(&press, false, false).is_none() {
            return false;
        }
        self.keys.walk(&press, &self.handle)
    }

    /// Carry out what a press on the panel asked.
    pub fn ask(&self, ask: tilo_desktop::window_switcher::SwitcherAsk) {
        let walking = self.walk.is_up().then(|| self.walk.scope());
        for key in SwitcherKey::of_panel(ask) {
            key.walk(&self.handle, &self.walk, walking);
        }
    }

    /// Show what the walk reached this pass: whether the panel changed. The page has no window's
    /// picture to give a card, so each card shows its app's mark.
    pub fn follow(&self) -> bool {
        show(
            &self.handle,
            &self.walk,
            &self.panel,
            false,
            &std::collections::HashMap::new(),
        )
    }

    /// When the panel of a walk held down comes up, with nothing pressed meanwhile.
    #[must_use]
    pub fn deadline(&self) -> Option<tilo_clock::Instant> {
        self.walk.deadline(tilo_clock::Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::winit::keyboard::NamedKey;

    struct Blank;

    impl tilo_desktop_surface::TileSource for Blank {
        fn tile(
            &mut self,
            _: &tilo_desktop_model::Workspace,
            tab: &tilo_desktop_model::Tab,
        ) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// A page's desk with three windows, and the walk over them.
    fn walk() -> (WindowWalk, tilo_desktop_surface::DesktopSurface) {
        let mut minter = tilo_desktop_model::IdMinter::new(5);
        let tabs = ["Mail", "Notes", "Budget"].map(|title| tilo_desktop_model::Tab {
            id: minter.mint(),
            title: title.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.{}", title.to_lowercase()),
                label: title.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        });
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Desk", tabs.into());
        let (surface, handle) = tilo_desktop_surface::DesktopSurface::new(tree, 5, Box::new(Blank));
        (WindowWalk::new(handle), surface)
    }

    /// The windows the walk crosses, in its order, and the place of the one it reached.
    fn walked(walk: &WindowWalk) -> (Vec<String>, Option<usize>, Option<usize>) {
        walk.handle.read(|tree| {
            let windows = crate::desk_windows::windows(tree, &walk.handle);
            let (walked, front) = crate::desk_windows::walked(&windows, walk.walk.scope());
            let reached = crate::desk_windows::reaching(&walk.walk, &windows)
                .and_then(|at| walked.iter().position(|place| *place == at));
            let titles = walked
                .iter()
                .filter_map(|at| windows.get(*at).map(|window| window.title.clone()))
                .collect();
            (titles, front, reached)
        })
    }

    /// The page's stand-in for Super holds the walk's Alt off a Mac: Alt+Shift+Tab walks on, and
    /// letting Alt go settles it. A Mac's Option+Tab walks as Alt+Tab does, Shift walking back.
    #[test]
    fn alt_shift_tab_walks_the_pages_windows() {
        let alt_shift = ModifiersState::ALT | ModifiersState::SHIFT;
        let tab = Key::Named(NamedKey::Tab);
        for (mac, held, back) in [
            (false, alt_shift, false),
            (true, ModifiersState::ALT, false),
            (true, alt_shift, true),
        ] {
            let (walk, _surface) = walk();
            assert!(
                !walk.take(&tab, true, false, ModifiersState::SHIFT, mac),
                "Tab alone types"
            );
            assert!(
                walk.take(&tab, true, false, held, mac),
                "mac {mac}: the chord walks"
            );
            let (titles, front, reached) = walked(&walk);
            let front = front.unwrap_or_default();
            let count = titles.len();
            let expected = if back {
                (front + count - 1) % count
            } else {
                (front + 1) % count
            };
            assert_eq!(
                reached,
                Some(expected),
                "mac {mac}: back {back} in {titles:?}"
            );
            assert!(
                walk.take(&Key::Named(NamedKey::Alt), false, false, held, mac),
                "mac {mac}: letting Alt go is the walk's"
            );
            assert!(!walk.walk.is_up(), "mac {mac}: the walk went to the window");
        }
    }

    /// The panel's cards are pressed as the native panel's are: the card under the pointer is the
    /// one reached, and the card chosen is gone to.
    #[test]
    fn a_card_on_the_panel_is_reached_and_chosen() {
        let (walk, _surface) = walk();
        let alt_shift = ModifiersState::ALT | ModifiersState::SHIFT;
        assert!(walk.take(&Key::Named(NamedKey::Tab), true, false, alt_shift, false));
        walk.ask(tilo_desktop::window_switcher::SwitcherAsk::Hover(2));
        assert_eq!(walked(&walk).2, Some(2));
        walk.ask(tilo_desktop::window_switcher::SwitcherAsk::Choose(0));
        assert!(!walk.walk.is_up());
    }
}
