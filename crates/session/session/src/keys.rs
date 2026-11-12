//! The keys the session answers wherever the keyboard is: the desktop's chords, Alt+Tab's walk and
//! the shortcut guide. A host offers this every press before the focused window or panel gets it,
//! as a [`Press`] in the toolkit's names; the device's own keys (levels, media, power, Print) and
//! the apps' shortcuts arrive already read by that host ([`Press::device`], [`Press::app_shortcut`]).
//! A press that is a chord of the desktop's one shortcut table ([`tilo_desktop::shortcuts`]) or a
//! device key becomes an invocation of a published action, run through the same door every other
//! caller uses; nothing here performs anything itself.

#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page runs only the walk's keys; its other chords are its own"
    )
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::ActionAddress;
use tilo_session_core::surface::{KeyClaim, KeyModifiers};

use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop::desk_widgets::DeskFold;
use tilo_desktop::osd::OsdKind;
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_session_core::shell::OVERLAYS;
use tilo_sheets::shortcut_guide::{Caught, State as GuideState};
use ui::shortcut_table::{FixedAction, KeyChord, ShortcutAction, ShortcutConfig};
use ui::winit::keyboard::{Key, NamedKey};

/// A press as the session's chords read it, whichever host's keyboard it came from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Press {
    /// The key as a chord names it: under Ctrl, Alt or Super, a letter outside the Latin script is
    /// the Latin key the same key types, so Ctrl+C copies whichever layout is typing.
    pub(crate) key: Key,
    /// The key with no modifier applied: what Escape is read by, whatever is held with it.
    pub(crate) bare: Key,
    pub(crate) modifiers: KeyModifiers,
    /// Pressed or released.
    pub(crate) pressed: bool,
    /// A press repeated while the key is held, not a new one.
    pub(crate) repeat: bool,
    /// The physical key: an app's shortcut is released by the key that held it.
    pub(crate) code: u32,
    /// The device's own key this is, as its host reads it: a level, the media, power or Print.
    pub(crate) device: Option<DeviceKey>,
    /// The app shortcut bound to this press, as its published resource.
    pub(crate) app_shortcut: Option<String>,
}

impl Press {
    /// The key and the modifiers held, as the shortcut tables and the desktop's chords match them.
    fn chord(&self) -> (Key, ui::winit::event::Modifiers) {
        (self.key.clone(), self.modifiers.state().into())
    }
}

/// A key the device has and the desktop answers wherever the keyboard is: what it runs, whether
/// holding it repeats it, and whether it works on the lock screen (it changes a level and reveals
/// nothing behind the lock).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeviceKey {
    pub(crate) invocation: Invocation,
    pub(crate) repeats: bool,
    pub(crate) while_locked: bool,
}

/// One published action to invoke, with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The readout to show once it ran (a hardware key's answer on screen).
    pub readout: Option<OsdKind>,
}

impl Invocation {
    /// `action` of `resource` , without arguments.
    #[must_use]
    pub fn new(resource: &str, action: &str) -> Self {
        Self {
            address: ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
            readout: None,
        }
    }

    /// The same, showing the readout `kind` once it ran.
    #[must_use]
    pub const fn showing(mut self, kind: OsdKind) -> Self {
        self.readout = Some(kind);
        self
    }

    /// The same, with `name` set to `value` .
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.arguments.insert(name.to_owned(), value.into());
        self
    }

    /// The chrome's shortcut action for `action` .
    #[must_use]
    pub fn shortcut(action: ShortcutAction) -> Self {
        let id = serde_json::to_value(action)
            .ok()
            .and_then(|id| id.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self::new(CHROME_RESOURCE, "shortcut").with("action", id)
    }
}

/// Invocations waiting for the session's door.
pub type Invocations = Rc<RefCell<Vec<Invocation>>>;

/// Chords pressed that run a saved shortcut's keyboard trigger, waiting for the automation host to
/// fire them.
pub type PressedChords = Rc<RefCell<Vec<KeyChord>>>;

/// Quick settings' resource, whose effects the notification and paste keys run.
pub const QUICK_SETTINGS: &str = "quick-settings.panel";

/// What the keys hand the rest of the session: the shortcuts the person's presses invoked, and the
/// chords an armed shortcut answers, which the automation host starts its run on.
#[derive(Clone, Default)]
pub(crate) struct Presses {
    pub invocations: Invocations,
    pub triggered: PressedChords,
}

/// The controller the person last drove the desktop with, and whether the keys being routed are
/// that controller's own rather than their fingers on a keyboard.
#[derive(Clone, Default)]
pub(crate) struct ControllerKeys {
    /// The controller the person last drove the desktop with, which a press of their own replaces:
    /// the guide then leads with the keyboard.
    pub(crate) last: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed are a controller's own, standing for its presses rather than
    /// the person's fingers on a keyboard.
    pub(crate) own: Rc<Cell<bool>>,
}

/// What stands over the desk and what the keys owe it: Escape folds the card a desk widget has
/// open, and closes the topmost surface of the stack that answers Escape. Neither hears keys
/// itself, so both say here what theirs are worth.
#[derive(Clone, Default)]
pub(crate) struct DeskKeys {
    fold: DeskFold,
    overlays: Overlays,
    /// The desk's own overlays, where Alt+Tab's walk lives.
    switcher: Option<Rc<crate::overlays::Overlays>>,
    /// What the keys ask the panel's own app, when this device has one and draws it: Escape asks it
    /// to go back a page, and the panel answers whether it had one.
    panel: Option<Rc<RefCell<Vec<&'static str>>>>,
}

impl DeskKeys {
    /// The desk's fold and the surfaces over it, as the session holds them.
    #[must_use]
    pub(crate) fn new(fold: DeskFold, overlays: Overlays) -> Self {
        Self {
            fold,
            overlays,
            switcher: None,
            panel: None,
        }
    }

    /// The same, with the desk's own overlays: Alt+Tab's walk.
    #[must_use]
    pub(crate) fn with_switcher(mut self, switcher: Rc<crate::overlays::Overlays>) -> Self {
        self.switcher = Some(switcher);
        self
    }

    /// The same, with the queue the panel's own app is asked through, when this device has one and
    /// draws it.
    #[must_use]
    pub(crate) fn with_panel(mut self, asks: Option<Rc<RefCell<Vec<&'static str>>>>) -> Self {
        self.panel = asks;
        self
    }
}

/// Whether `key` is the key above Tab: `` ` ``, and `~` with Shift held.
fn above_tab(key: &Key) -> bool {
    matches!(key, Key::Character(typed) if typed == "`" || typed == "~")
}

/// What a press is to the window switcher Alt+Tab opened, or to the chord that opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwitcherKey {
    /// Walk on (or back, with Shift), opening the walk over `opened` when it is not up yet.
    /// Everything else steps inside the scope already walking.
    Walk {
        back: bool,
        opened: crate::overlays::SwitcherScope,
    },
    /// Home and End: the window `at` places along the walk, `-1` the last.
    Place(i64),
    /// Enter, Space, or letting Alt go: go to the window the walk reached.
    Commit,
    /// Escape: close the panel and stay where the person was.
    Cancel,
    /// The walk takes the key and does nothing with it.
    Hold,
}

impl SwitcherKey {
    /// The desk action it asks for: the walk, the place or the commit, as the desk's own published
    /// action. `walking` is the scope of the walk already up: a walk going keeps it, so Alt+grave
    /// opens over the app in front's windows and Alt+Tab steps inside it.
    pub(crate) fn asks(
        self,
        walking: Option<crate::overlays::SwitcherScope>,
    ) -> Option<Invocation> {
        match self {
            Self::Walk { back, opened } => {
                let scope = walking.unwrap_or(opened);
                Some(
                    Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "walk-switcher")
                        .with("steps", if back { -1 } else { 1 })
                        .with("scope", scope.name()),
                )
            }
            Self::Place(at) => Some(
                Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "select-switcher")
                    .with("at", at),
            ),
            Self::Commit => Some(Invocation::new(
                tilo_session_core::shell::DESK_RESOURCE,
                "commit-switcher",
            )),
            Self::Cancel => Some(
                Invocation::new(tilo_session_core::shell::DESK_RESOURCE, "close-surface")
                    .with("surface", "switcher"),
            ),
            Self::Hold => None,
        }
    }

    /// Carry it out on `walk` over `handle`'s windows, as the desk's verb [`Self::asks`] names does.
    pub(crate) fn walk(
        self,
        handle: &tilo_desktop_surface::DesktopHandle,
        walk: &crate::overlays::Overlays,
        walking: Option<crate::overlays::SwitcherScope>,
    ) {
        use crate::desk_windows::{commit_switcher, select_switcher, walk_switcher};
        match self {
            Self::Walk { back, opened } => walk_switcher(
                handle,
                walk,
                Some(walking.unwrap_or(opened).name()),
                if back { -1 } else { 1 },
            ),
            Self::Place(at) => select_switcher(handle, walk, at),
            Self::Commit => commit_switcher(handle, walk),
            Self::Cancel => walk.close(crate::overlays::Overlay::Switcher.name()),
            Self::Hold => {}
        }
    }

    /// What a press on the walk's panel asks: the card under the pointer chosen, or gone to, or the
    /// panel dismissed.
    pub(crate) fn of_panel(ask: tilo_desktop::window_switcher::SwitcherAsk) -> Vec<Self> {
        use tilo_desktop::window_switcher::SwitcherAsk;
        let place = |at: usize| Self::Place(i64::try_from(at).unwrap_or(i64::MAX));
        match ask {
            SwitcherAsk::Hover(at) => vec![place(at)],
            SwitcherAsk::Choose(at) => vec![place(at), Self::Commit],
            SwitcherAsk::Dismiss => vec![Self::Cancel],
        }
    }
}

/// Alt+Tab's keys: the chord that opens the desk's walk, and every key it answers while it is up.
#[derive(Clone)]
pub(crate) struct WalkKeys {
    walk: Rc<crate::overlays::Overlays>,
}

impl WalkKeys {
    pub(crate) const fn new(walk: Rc<crate::overlays::Overlays>) -> Self {
        Self { walk }
    }

    /// What `press` is to Alt+Tab's walk: the chord that opens it, what it does while it is up, or
    /// nothing when it is not the walk's at all. While the walk is up it is modal, as every
    /// desktop's switcher is: the keys are its own, and none of them reaches the window behind it.
    pub(crate) fn key(&self, press: &Press) -> Option<SwitcherKey> {
        let desk = &*self.walk;
        let held = press.modifiers;
        if !desk.is_up() {
            // Alt+Tab walks the workspace's windows, and Alt with the key above Tab the ones the
            // app in front shows: fixed chords, as no shortcut table holds a modifier.
            if !press.pressed || !held.alt {
                return None;
            }
            if press.key == Key::Named(NamedKey::Tab) {
                return Some(SwitcherKey::Walk {
                    back: held.shift,
                    opened: crate::overlays::SwitcherScope::Workspace,
                });
            }
            if above_tab(&press.key) && !held.ctrl && !held.logo {
                return Some(SwitcherKey::Walk {
                    back: held.shift,
                    opened: crate::overlays::SwitcherScope::App,
                });
            }
            return None;
        }
        if !press.pressed {
            // Letting Alt go goes to the window the walk reached, whether the press was Alt's or
            // was lost to another seat: every other release is the walk's, and does nothing.
            let alt = press.key == Key::Named(NamedKey::Alt);
            return Some(if alt || !held.alt {
                SwitcherKey::Commit
            } else {
                SwitcherKey::Hold
            });
        }
        // A key still repeating after Alt came up does not walk: the walk ends on that release, and
        // a repeat must not choose another window.
        if press.repeat && !held.alt {
            return Some(SwitcherKey::Hold);
        }
        let walked = desk.scope();
        let back = held.shift;
        Some(match &press.key {
            Key::Named(NamedKey::Tab) => SwitcherKey::Walk {
                back,
                opened: walked,
            },
            key if above_tab(key) => SwitcherKey::Walk {
                back,
                opened: walked,
            },
            Key::Named(NamedKey::Escape) => SwitcherKey::Cancel,
            Key::Named(NamedKey::Enter | NamedKey::Space) => SwitcherKey::Commit,
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => SwitcherKey::Walk {
                back: true,
                opened: walked,
            },
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => SwitcherKey::Walk {
                back: false,
                opened: walked,
            },
            Key::Named(NamedKey::Home) => SwitcherKey::Place(0),
            Key::Named(NamedKey::End) => SwitcherKey::Place(-1),
            // A press without Alt settles on the window the walk reached: Alt came up without the
            // walk hearing it. It does not type into a window nobody chose yet.
            _ if !held.alt => SwitcherKey::Commit,
            _ => SwitcherKey::Hold,
        })
    }

    /// The scope of the walk already up: a walk going keeps it.
    fn walking(&self) -> Option<crate::overlays::SwitcherScope> {
        self.walk.is_up().then(|| self.walk.scope())
    }

    /// How the walk claims `press`: the chord that opens it, and, while it is up, every key — its
    /// own Alt's release among them. A locked session, or one whose focused client holds the
    /// shortcuts, opens no walk. `None` when the press is not the walk's.
    pub(crate) fn claim(&self, press: &Press, locked: bool, inhibited: bool) -> Option<KeyClaim> {
        let action = self.key(press)?;
        let claim = if press.pressed && matches!(action, SwitcherKey::Walk { .. }) {
            KeyClaim::TakeRepeating
        } else {
            KeyClaim::Take
        };
        (self.walk.is_up() || (!locked && !inhibited)).then_some(claim)
    }

    /// Move the walk as `press` asks, on `handle`'s desk itself: what a host without the desk's
    /// door does with the walk's keys. Whether the press was the walk's.
    pub(crate) fn walk(&self, press: &Press, handle: &tilo_desktop_surface::DesktopHandle) -> bool {
        let Some(key) = self.key(press) else {
            return false;
        };
        key.walk(handle, &self.walk, self.walking());
        true
    }
}

/// The surface that takes the session's keys; it shows nothing.
pub(crate) struct SessionKeys {
    /// The desktop's shortcut table, shared with the shortcut guide so the guide explains the
    /// chords the keys resolve against.
    shortcuts: Rc<RefCell<ShortcutConfig>>,
    /// The display sheet's presentation: Super+P puts it up while it is not there, and once it is,
    /// the chord moves inside it instead.
    displays: Option<tilo_sheets::Presentation>,
    pub(crate) invocations: Invocations,
    /// The chords pressed that run a saved shortcut's keyboard trigger: the automation host starts
    /// them, on the pass that follows.
    triggered: PressedChords,
    /// The shortcut guide's keys while it is up: what it holds back from the desktop behind it, and
    /// the chord it waits for.
    guide: Rc<GuideState>,
    /// Whether a desk widget has its card open, which Escape folds. The desk is under the windows
    /// and hears no keys itself, so it says here what its keys are worth.
    fold: DeskFold,
    /// What stands over the desk: Escape closes the topmost surface that answers it, and while one
    /// holds the keyboard a press is that surface's and not the window's.
    overlays: Overlays,
    /// Alt+Tab's keys: the chord that opens the desk's walk, and the keys it answers while it is up.
    switcher: Option<WalkKeys>,
    /// What the keys ask the panel's own app, when this device draws it.
    panel: Option<Rc<RefCell<Vec<&'static str>>>>,
    /// The shortcut resources whose chords are held down, by key.
    held: RefCell<BTreeMap<u32, String>>,
    /// The controller the person last drove the desktop with, which a press of their own replaces:
    /// the guide then leads with the keyboard.
    controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed are a controller's own, standing for its presses rather than
    /// the person's fingers on a keyboard.
    controller_keys: Rc<Cell<bool>>,
    /// When each display's bar shows over the window covering it: Super asks for it, and the app
    /// under it has nothing of that press.
    reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
}

impl SessionKeys {
    pub(crate) fn new(
        shortcuts: Rc<RefCell<ShortcutConfig>>,
        presses: Presses,
        guide: Rc<GuideState>,
        desk: DeskKeys,
        displays: Option<tilo_sheets::Presentation>,
        controller: ControllerKeys,
        reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    ) -> Self {
        let Presses {
            invocations,
            triggered,
        } = presses;
        let ControllerKeys {
            last: controller,
            own: controller_keys,
        } = controller;
        Self {
            shortcuts,
            displays,
            invocations,
            triggered,
            guide,
            fold: desk.fold,
            overlays: desk.overlays,
            switcher: desk.switcher.map(WalkKeys::new),
            panel: desk.panel,
            held: RefCell::new(BTreeMap::new()),
            controller,
            controller_keys,
            reveals,
        }
    }

    /// Whether `press` is the desk's own: Escape folds the card a desk widget has open, and what
    /// sits behind the desk does not hear it.
    fn folds_the_desk(&self, press: &Press) -> bool {
        press.bare == Key::Named(NamedKey::Escape) && self.fold.is_open()
    }

    /// Ask the desk for what a key of the walk's is worth: whether it was the walk's.
    fn walk_the_switcher(&self, press: &Press) -> bool {
        let Some(walk) = &self.switcher else {
            return false;
        };
        let Some(key) = walk.key(press) else {
            return false;
        };
        if let Some(invocation) = key.asks(walk.walking()) {
            self.invocations.borrow_mut().push(invocation);
        }
        true
    }

    /// The surface `press` closes, by the stack's dismissal table: Escape, for the topmost surface
    /// that answers it. The surface is drawn elsewhere, so this surface is the one that hears it.
    fn closes_the_top_surface(&self, press: &Press) -> Option<Slot> {
        (press.bare == Key::Named(NamedKey::Escape))
            .then(|| self.overlays.escape())
            .flatten()
    }

    /// Whether `press` is the display sheet's chord, which puts the sheet up: Super+P, as the
    /// desktop's fixed table names it. A sheet that is up is not opened again: what is drawn takes
    /// the chord and moves the highlight inside it.
    fn opens_the_display_sheet(&self, press: &Press) -> bool {
        let Some(displays) = &self.displays else {
            return false;
        };
        if displays.is_drawn() {
            return false;
        }
        let Key::Character(letter) = &press.key else {
            return false;
        };
        let held = press.modifiers;
        letter.eq_ignore_ascii_case("p") && held.logo && !held.shift && !held.ctrl && !held.alt
    }

    /// The desktop's shortcut `press` is bound to, as the table names it.
    fn configured_action(&self, press: &Press) -> Option<ShortcutAction> {
        let (logical, modifiers) = press.chord();
        let shortcuts = self.shortcuts.borrow();
        tilo_desktop::shortcuts::configured_shortcut_action(&shortcuts, &logical, modifiers)
    }

    /// The fixed action `press` is: a chord Settings cannot rebind, answered before any app's.
    /// `None` for a chord the keys answer where they are resolved (the window walk, the workspace
    /// steps, the display sheet, Print) and for the accessibility chords, which have no native
    /// dispatcher yet.
    fn fixed_action(press: &Press) -> Option<FixedAction> {
        let (logical, modifiers) = press.chord();
        tilo_desktop::shortcuts::fixed_shortcut_action(&logical, modifiers)
    }

    /// The chord of this press that runs a saved shortcut's keyboard trigger, which the session
    /// claims and the automation host fires. The chord is the table's because the automation put it
    /// there when it armed the shortcut, and it stays the session's until the next arming drops it.
    fn trigger_chord(&self, press: &Press) -> Option<KeyChord> {
        let (logical, modifiers) = press.chord();
        let chord = tilo_desktop::shortcuts::shortcut_chord_from_event(&logical, modifiers)?;
        let shortcuts = self.shortcuts.borrow();
        shortcuts
            .action_for(&chord)
            .filter(|bound| {
                tilo_automation::runs_shortcut(&bound.target.resource, &bound.target.action)
            })
            .map(|_| chord)
    }

    /// The key of the device's own or the desktop's that `press` is: what its host read, or F1,
    /// which names every key the desktop answers. F1 is the desktop's own key, which nothing may be
    /// bound to, so the guide opens whatever modifiers are held with it.
    fn device_key(press: &Press) -> Option<DeviceKey> {
        press.device.clone().or_else(|| {
            (press.key == Key::Named(NamedKey::F1)).then(|| DeviceKey {
                invocation: Invocation::shortcut(ShortcutAction::ShortcutHelp),
                repeats: false,
                while_locked: false,
            })
        })
    }

    /// The invocation a press is, and how it is claimed. `inhibited` while a focused client holds
    /// the desktop's shortcuts (a virtual machine, a remote desktop): only a device key and
    /// locking the session stay the desktop's then.
    pub(crate) fn resolve(
        &self,
        press: &Press,
        locked: bool,
        inhibited: bool,
    ) -> Option<(Invocation, KeyClaim)> {
        if let Some(device) = Self::device_key(press) {
            if locked && !device.while_locked {
                return None;
            }
            let claim = if device.repeats {
                KeyClaim::TakeRepeating
            } else {
                KeyClaim::Take
            };
            return Some((device.invocation, claim));
        }
        if locked {
            return None;
        }
        if inhibited {
            // The client showing another machine answers its own chords; locking this one is never
            // its to keep, or it could hide the way out of itself.
            let action = self.configured_action(press)?;
            return (action == ShortcutAction::Lock)
                .then(|| (Invocation::shortcut(action), KeyClaim::Take));
        }
        // Super+P is the display sheet's, as it is on every desktop: the keys the fixed table names
        // come before any app's chord.
        if self.opens_the_display_sheet(press) {
            return Some((
                Invocation::new(tilo_session_core::shell::DISPLAYS_RESOURCE, "open-sheet"),
                KeyClaim::Take,
            ));
        }
        // The fixed chords Settings cannot rebind are the desktop's before any app's, and run the
        // same verb their menu rows and an agent's invocation name.
        if let Some(action) = Self::fixed_action(press) {
            return Some((
                Invocation::new(CHROME_RESOURCE, "fixed-shortcut").with("action", action.id()),
                KeyClaim::Take,
            ));
        }
        // An app's bound shortcut is the person's answer to its sheet: it is pressed like the
        // desktop's own chords are.
        if let Some(resource) = &press.app_shortcut {
            return Some((Invocation::new(resource, "activate"), KeyClaim::Take));
        }
        let action = self.configured_action(press)?;
        Some((Invocation::shortcut(action), KeyClaim::Take))
    }

    /// Whether `invocation` is the desktop's way to the shortcut guide, which stays its own key
    /// while the guide is up.
    fn shows_the_guide(invocation: &Invocation) -> bool {
        *invocation == Invocation::shortcut(ShortcutAction::ShortcutHelp)
    }

    /// A press the guide is up over: it answers only what is its own, and while it waits for a new
    /// chord, that press is the chord.
    fn guide_key(&self, press: &Press) {
        let resolved = self.resolve(press, false, false);
        // F1 and the desktop's help chord stay the guide's own way out, waiting for a chord or not.
        if let Some((invocation, _)) = &resolved
            && Self::shows_the_guide(invocation)
        {
            self.invocations.borrow_mut().push(invocation.clone());
            return;
        }
        // A device key changes a level or takes the screen: it is no chord of the desktop's table,
        // so it answers as it does without the guide.
        if Self::device_key(press).is_some() {
            if let Some((invocation, _)) = resolved {
                self.invocations.borrow_mut().push(invocation);
            }
            return;
        }
        if self.guide.listening().is_none() || press.repeat {
            // The guide is modal for everything else: a chord acts once it is gone.
            return;
        }
        if press.bare == Key::Named(NamedKey::Escape) {
            self.guide.stop_listening();
            self.guide.catch(Caught::Cancelled);
            return;
        }
        let (logical, modifiers) = press.chord();
        // A modifier held on its own is no chord, and no modifier is either.
        if let Some(chord) = tilo_desktop::shortcuts::shortcut_chord_from_event(&logical, modifiers)
        {
            self.guide.catch(Caught::Chord(chord));
        }
    }

    /// How the session claims `press`, offered before the focused window or panel gets it.
    pub(crate) fn claim(&self, press: &Press, locked: bool, inhibited: bool) -> KeyClaim {
        // Super going down asks for the bar over the window that covers the display, as reaching
        // for the top edge does. The app under it has nothing of a modifier that asked for the
        // desktop's own bar.
        if !locked && !inhibited && is_super(press) && self.reveals.borrow().covers_display(None) {
            self.invocations
                .borrow_mut()
                .push(Invocation::new(CHROME_RESOURCE, "reveal-top-bar"));
            return KeyClaim::Take;
        }
        if self.guide.up() {
            if self.guide.listening().is_some() {
                // The guide waits for a shortcut's new keys: every press is that chord, and none of
                // them reaches what is behind it.
                return KeyClaim::Take;
            }
            // The guide is modal: a chord of the desktop's table or an app's shortcut must not act
            // on what it hides. A device key (the volume keys, F1) is no chord and answers as it
            // does without the guide.
            if Self::device_key(press).is_none() {
                if self.trigger_chord(press).is_some() {
                    return KeyClaim::Take;
                }
                return self
                    .resolve(press, locked, inhibited)
                    .map_or(KeyClaim::Pass, |(_, claim)| claim);
            }
        }
        if let Some(claim) = self
            .switcher
            .as_ref()
            .and_then(|walk| walk.claim(press, locked, inhibited))
        {
            return claim;
        }
        if self.closes_the_top_surface(press).is_some() || self.folds_the_desk(press) {
            return KeyClaim::Take;
        }
        // A chord that runs a saved shortcut is the session's, as a chord of the desktop's own
        // table is: the shortcut fires rather than the window behind it. A locked session, or one
        // whose focused client holds the shortcuts, answers neither.
        if !locked && !inhibited && self.trigger_chord(press).is_some() {
            return KeyClaim::Take;
        }
        self.resolve(press, locked, inhibited)
            .map_or(KeyClaim::Pass, |(_, claim)| claim)
    }

    /// Answer a press the session claimed, or the release of one.
    pub(crate) fn answer(&self, press: &Press) {
        if !press.pressed {
            // The app hears its shortcut released wherever the modifiers are by then: the chord is
            // remembered by the key that held it.
            if let Some(resource) = self.held.borrow_mut().remove(&press.code) {
                self.invocations
                    .borrow_mut()
                    .push(Invocation::new(&resource, "deactivate"));
            }
            // Letting Alt go is the walk's, wherever the press that opened it was recorded: the
            // switcher goes to the window it reached.
            self.walk_the_switcher(press);
            return;
        }
        // A press of the person's own: the guide leads with the keyboard again. The controller's
        // own presses reach here too, standing for its buttons, and say nothing about a keyboard.
        if !self.controller_keys.get() {
            self.controller.set(None);
        }
        if self.guide.up() {
            self.guide_key(press);
            return;
        }
        // Alt+Tab's walk answers the keys the desk's switcher shows: the walk, its places, and the
        // press that settles it.
        if self.walk_the_switcher(press) {
            return;
        }
        if let Some(slot) = self.closes_the_top_surface(press) {
            if slot == Slot::QuickSettings
                && let Some(asks) = &self.panel
            {
                // The panel's own pages are its own: Escape goes back one of them, and the panel
                // answers whether it had one to go back to (`Shared::panel_back`). Its main page
                // has none, and the session closes the panel there.
                asks.borrow_mut().push("back");
                return;
            }
            self.invocations
                .borrow_mut()
                .push(Invocation::new(OVERLAYS, "close").with("slot", slot.as_str()));
            return;
        }
        if self.folds_the_desk(press) {
            self.fold.ask();
            return;
        }
        // A chord of a saved shortcut's keyboard trigger starts the shortcut: the automation host
        // is handed the press on this pass.
        if let Some(chord) = self.trigger_chord(press) {
            self.triggered.borrow_mut().push(chord);
            return;
        }
        // A repeat of a key taken once is not run again (only a level key repeats); the host
        // repeats only keys claimed repeating.
        if let Some(resource) = &press.app_shortcut {
            self.held.borrow_mut().insert(press.code, resource.clone());
            self.invocations
                .borrow_mut()
                .push(Invocation::new(resource, "activate"));
            return;
        }
        if let Some((invocation, _)) = self.resolve(press, false, false) {
            self.invocations.borrow_mut().push(invocation);
        }
    }
}

/// Whether `press` is a Super key going down for the first time: the key that asks for a bar
/// hidden by a fullscreen window.
fn is_super(press: &Press) -> bool {
    press.pressed
        && !press.repeat
        && matches!(press.key, Key::Named(NamedKey::Super | NamedKey::Meta))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::controls::SessionCommand;
    use crate::platform::keys::{
        CALCULATOR, CAPTURE_STUDIO, KEY_XF86_RFKILL, MEDIA, NETWORK, NativeKeys, SLEEP, SOUND,
    };
    use tilo_session_core::surface::{HostKey, SurfaceHost};
    use xkeysym::key as keysyms;

    /// The session's keys as this device hands them its presses: over `desk`, with `guide`, the
    /// table `shortcuts`, answering into `presses` and `commands`.
    fn on_this_device(
        desk: DeskKeys,
        guide: Rc<GuideState>,
        shortcuts: Rc<RefCell<ShortcutConfig>>,
        presses: Presses,
        commands: Rc<RefCell<Vec<SessionCommand>>>,
    ) -> NativeKeys {
        let keys = SessionKeys::new(
            shortcuts,
            presses,
            guide,
            desk,
            None,
            ControllerKeys::default(),
            Rc::default(),
        );
        NativeKeys::new(keys, None, commands)
    }

    /// The session's keys over `desk`, with nothing else of theirs set.
    fn over(desk: DeskKeys) -> NativeKeys {
        on_this_device(
            desk,
            Rc::default(),
            Rc::default(),
            Presses::default(),
            Rc::default(),
        )
    }

    impl NativeKeys {
        fn resolve(
            &self,
            key: &HostKey,
            locked: bool,
            inhibited: bool,
        ) -> Option<(Invocation, KeyClaim)> {
            self.keys.resolve(&self.press(key), locked, inhibited)
        }
    }

    fn key(keysym: u32, text: Option<char>, modifiers: KeyModifiers) -> HostKey {
        HostKey {
            keycode: 0,
            pressed: true,
            repeat: false,
            keysym,
            unmodified_keysym: keysym,
            latin_keysym: keysym,
            text: text.map(String::from),
            modifiers,
        }
    }

    fn keys() -> NativeKeys {
        over(DeskKeys::default())
    }

    /// The key surface with the shortcut guide up, and the guide's shared keys.
    fn with_guide() -> (NativeKeys, Rc<GuideState>) {
        let guide: Rc<GuideState> = Rc::default();
        guide.set_up(true);
        let keys = on_this_device(
            DeskKeys::default(),
            guide.clone(),
            Rc::default(),
            Presses::default(),
            Rc::default(),
        );
        (keys, guide)
    }

    /// The key surface with the desk's walk to answer to, and that walk.
    fn with_walk() -> (NativeKeys, Rc<crate::overlays::Overlays>) {
        let walk = Rc::new(crate::overlays::Overlays::new());
        let keys = over(DeskKeys::default().with_switcher(walk.clone()));
        (keys, walk)
    }

    /// The modifiers `names` holds: `c` Ctrl, `a` Alt, `s` Shift, `l` the logo key.
    fn held(names: &str) -> KeyModifiers {
        KeyModifiers {
            ctrl: names.contains('c'),
            alt: names.contains('a'),
            shift: names.contains('s'),
            logo: names.contains('l'),
            caps_lock: false,
        }
    }

    /// What the keys asked the desk for since the last call.
    fn asked(keys: &NativeKeys) -> Vec<Invocation> {
        keys.keys.invocations.borrow_mut().drain(..).collect()
    }

    fn desk(action: &str) -> Invocation {
        Invocation::new(tilo_session_core::shell::DESK_RESOURCE, action)
    }

    /// A release of `keysym` with `modifiers` held.
    fn released(keysym: u32, modifiers: KeyModifiers) -> HostKey {
        HostKey {
            pressed: false,
            ..key(keysym, None, modifiers)
        }
    }

    #[test]
    fn alt_tab_opens_the_walk_and_shift_walks_it_back() {
        let (mut keys, _walk) = with_walk();
        let tab = key(keysyms::Tab, None, held("a"));
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::TakeRepeating);
        keys.key(&tab);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", 1)
                    .with("scope", "workspace")
            ],
            "Alt+Tab walks the workspace's windows on from the one in front"
        );

        let backwards = key(keysyms::Tab, None, held("as"));
        keys.key(&backwards);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", -1)
                    .with("scope", "workspace")
            ],
            "Shift walks the other way"
        );
    }

    #[test]
    fn alt_and_the_key_above_tab_walk_the_front_apps_windows() {
        let (mut keys, _walk) = with_walk();
        let grave = key(keysyms::grave, Some('`'), held("a"));
        assert_eq!(
            keys.claim_key(&grave, false, false),
            KeyClaim::TakeRepeating
        );
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![desk("walk-switcher").with("steps", 1).with("scope", "app")]
        );
        // The chord a text field types is nobody's walk: Ctrl or Super with the key above Tab.
        for other in [
            KeyModifiers {
                ctrl: true,
                ..held("a")
            },
            KeyModifiers {
                logo: true,
                ..held("a")
            },
        ] {
            let chord = key(keysyms::grave, Some('`'), other);
            assert_ne!(
                keys.claim_key(&chord, false, false),
                KeyClaim::TakeRepeating,
                "{other:?} is no walk"
            );
            keys.key(&chord);
            assert!(asked(&keys).is_empty(), "{other:?} is no walk");
        }
    }

    #[test]
    fn a_lone_alt_is_no_walk_and_a_locked_session_opens_none() {
        let (mut keys, _walk) = with_walk();
        let alone = key(keysyms::Alt_L, None, held("a"));
        assert_eq!(keys.claim_key(&alone, false, false), KeyClaim::Pass);
        keys.key(&alone);
        assert!(
            asked(&keys).is_empty(),
            "a modifier on its own reaches the window"
        );

        let tab = key(keysyms::Tab, None, held("a"));
        assert_eq!(
            keys.claim_key(&tab, true, false),
            KeyClaim::Pass,
            "the lock takes no chord that walks the windows behind it"
        );
        assert_eq!(
            keys.claim_key(&tab, false, true),
            KeyClaim::Pass,
            "a client that holds the shortcuts opens no walk"
        );
    }

    #[test]
    fn the_walk_is_modal_while_it_is_up() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let letter = key(keysyms::a, Some('a'), held("a"));
        assert_eq!(
            keys.claim_key(&letter, false, false),
            KeyClaim::Take,
            "while the walk is up no key reaches the window behind it"
        );
        keys.key(&letter);
        assert!(
            asked(&keys).is_empty(),
            "an unbound key holds the walk where it is"
        );
        // Locked or not, a walk that is up hears its own keys: it is over the lock too.
        for locked in [false, true] {
            assert_eq!(keys.claim_key(&letter, locked, false), KeyClaim::Take);
        }
    }

    #[test]
    fn letting_alt_go_goes_to_the_window_the_walk_reached() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let let_go = released(keysyms::Alt_L, KeyModifiers::default());
        assert_eq!(keys.claim_key(&let_go, false, false), KeyClaim::Take);
        keys.key(&let_go);
        assert_eq!(asked(&keys), vec![desk("commit-switcher")]);

        // Any other key released with Alt still down is the walk's and does nothing.
        let tab = released(keysyms::Tab, held("a"));
        keys.key(&tab);
        assert!(asked(&keys).is_empty(), "releasing Tab is no answer");
        // A key released once Alt came up commits too: Alt's own release was lost.
        let stray = released(keysyms::a, KeyModifiers::default());
        keys.key(&stray);
        assert_eq!(asked(&keys), vec![desk("commit-switcher")]);
    }

    #[test]
    fn escape_closes_the_walk_and_enter_goes_to_it() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let escape = key(keysyms::Escape, None, held("a"));
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Take);
        keys.key(&escape);
        assert_eq!(
            asked(&keys),
            vec![desk("close-surface").with("surface", "switcher")]
        );

        for commit in [keysyms::Return, keysyms::KP_Enter, keysyms::space] {
            let key = key(commit, None, held("a"));
            keys.key(&key);
            assert_eq!(asked(&keys), vec![desk("commit-switcher")], "{commit:#x}");
        }
    }

    #[test]
    fn escape_asks_the_panel_to_go_back_before_it_closes_it() {
        // The panel's app draws the quick settings on this device: Escape is its own pages' first,
        // and the session waits for its answer.
        let asks = Rc::new(RefCell::new(Vec::new()));
        let overlays = Overlays::new();
        overlays.open(Slot::QuickSettings, None);
        let mut keys = over(
            DeskKeys::new(DeskFold::default(), overlays.clone()).with_panel(Some(Rc::clone(&asks))),
        );
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        keys.key(&escape);
        assert_eq!(*asks.borrow(), ["back"], "the panel goes back a page");
        assert!(
            asked(&keys).is_empty(),
            "nothing closes until the panel says it has nowhere left to go"
        );

        // A device whose shell draws the quick settings has no panel app to ask: Escape closes the
        // surface itself.
        let mut shell = over(DeskKeys::new(DeskFold::default(), overlays));
        shell.key(&escape);
        assert_eq!(
            asked(&shell),
            vec![Invocation::new(OVERLAYS, "close").with("slot", "quick-settings")]
        );
    }

    #[test]
    fn home_end_and_the_arrows_move_inside_the_walk() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let home = key(keysyms::Home, None, held("a"));
        keys.key(&home);
        assert_eq!(asked(&keys), vec![desk("select-switcher").with("at", 0)]);
        let end = key(keysyms::End, None, held("a"));
        keys.key(&end);
        assert_eq!(asked(&keys), vec![desk("select-switcher").with("at", -1)]);
        for (arrow, steps) in [
            (keysyms::Left, -1),
            (keysyms::Up, -1),
            (keysyms::Right, 1),
            (keysyms::Down, 1),
        ] {
            let arrow = key(arrow, None, held("a"));
            keys.key(&arrow);
            assert_eq!(
                asked(&keys),
                vec![
                    desk("walk-switcher")
                        .with("steps", steps)
                        .with("scope", "workspace")
                ],
                "an arrow steps the walk: {steps}"
            );
        }
    }

    #[test]
    fn a_walk_going_keeps_its_scope_and_the_other_chord_starts_it_again() {
        let (mut keys, walk) = with_walk();
        // A walk in the app's scope: Tab and the key above Tab both step inside it.
        walk.walk(Some("app"), 1);
        for keysym in [keysyms::Tab, keysyms::grave] {
            let step = key(keysym, None, held("a"));
            keys.key(&step);
            assert_eq!(
                asked(&keys),
                vec![desk("walk-switcher").with("steps", 1).with("scope", "app")],
                "a walk going keeps the windows it crosses: {keysym:#x}"
            );
        }
        // The other chord does not cross to the other scope in a walk going: it steps, as it always
        // has. The scope is the chord that opened the walk.
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let grave = key(keysyms::grave, Some('`'), held("a"));
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![
                desk("walk-switcher")
                    .with("steps", 1)
                    .with("scope", "workspace")
            ]
        );
        // Alt let go and pressed again with the key above Tab opens the app's walk.
        walk.close("switcher");
        walk.forget_walk();
        keys.key(&grave);
        assert_eq!(
            asked(&keys),
            vec![desk("walk-switcher").with("steps", 1).with("scope", "app")]
        );
    }

    #[test]
    fn a_press_without_alt_settles_the_walk_and_its_repeats_do_not() {
        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let typing = key(keysyms::a, Some('a'), KeyModifiers::default());
        assert_eq!(keys.claim_key(&typing, false, false), KeyClaim::Take);
        keys.key(&typing);
        assert_eq!(
            asked(&keys),
            vec![desk("commit-switcher")],
            "Alt came up without the walk hearing it: the press settles the walk"
        );

        let (mut keys, walk) = with_walk();
        walk.walk(Some("workspace"), 1);
        let repeat = HostKey {
            repeat: true,
            ..key(keysyms::a, Some('a'), KeyModifiers::default())
        };
        assert_eq!(keys.claim_key(&repeat, false, false), KeyClaim::Take);
        keys.key(&repeat);
        assert!(
            asked(&keys).is_empty(),
            "a key still repeating as Alt comes up does not choose another window"
        );
    }

    #[test]
    fn the_key_surface_draws_no_frame() {
        let mut keys = keys();
        assert!(
            !keys.prepare_frame(),
            "the keys take chords and pass the rest on: a frame is never theirs"
        );
    }

    /// The device's keys and F1, unlocked and locked: a level key repeats and works on the lock
    /// screen, a key that opens something or takes the screen waits for the unlock; the sleep key
    /// sleeps whatever the power button is set to; Print opens the studio, or with Shift takes the
    /// whole display and with Alt the window in front, as the studio's own actions take them.
    #[test]
    fn the_device_keys_run_their_actions_and_the_lock_keeps_what_it_hides() {
        let keys = keys();
        let none = KeyModifiers::default();
        let take = |invocation: Invocation| Some((invocation, KeyClaim::Take));
        let volume = Invocation::new(SOUND, "volume-up").showing(OsdKind::Volume);
        let airplane =
            Invocation::new(NETWORK, "toggle-airplane-mode").showing(OsdKind::AirplaneMode);
        let play = Invocation::new(MEDIA, "play-pause").showing(OsdKind::Play);
        let calculator = Invocation::new(CHROME_RESOURCE, "open-app").with("app", CALCULATOR);
        let studio = Invocation::new(CAPTURE_STUDIO, "open").with(
            "mode",
            serde_json::to_value(ui::floating_surface::CaptureStudioProtocolMode::Screenshot)
                .unwrap_or_default(),
        );
        let screenshot = |source: &str| {
            Invocation::new(CAPTURE_STUDIO, "screenshot").with("source", source.to_owned())
        };
        let help = Invocation::shortcut(ShortcutAction::ShortcutHelp);
        assert_eq!(help.arguments["action"], "shortcut_help");
        let sleep = take(Invocation::new(SLEEP, "sleep"));
        let repeating = Some((volume, KeyClaim::TakeRepeating));
        for (name, keysym, held, unlocked, locked) in [
            (
                "volume",
                keysyms::XF86_AudioRaiseVolume,
                none,
                repeating.clone(),
                repeating,
            ),
            ("airplane", KEY_XF86_RFKILL, none, take(airplane), None),
            (
                "play",
                keysyms::XF86_AudioPlay,
                none,
                take(play.clone()),
                take(play),
            ),
            (
                "calculator",
                keysyms::XF86_Calculator,
                none,
                take(calculator),
                None,
            ),
            ("sleep", keysyms::XF86_Sleep, none, sleep.clone(), None),
            ("suspend", keysyms::XF86_Suspend, none, sleep.clone(), None),
            ("standby", keysyms::XF86_Standby, none, sleep, None),
            ("print", keysyms::Print, none, take(studio), None),
            (
                "shift+print",
                keysyms::Print,
                held("s"),
                take(screenshot("display")),
                None,
            ),
            (
                "alt+print",
                keysyms::Print,
                held("a"),
                take(screenshot("window")),
                None,
            ),
            ("F1", keysyms::F1, none, take(help), None),
        ] {
            let press = key(keysym, None, held);
            assert_eq!(keys.resolve(&press, false, false), unlocked, "{name}");
            assert_eq!(keys.resolve(&press, true, false), locked, "{name} locked");
        }
    }

    #[test]
    fn a_chord_on_a_cyrillic_layout_is_the_latin_key_of_that_key() {
        let keys = keys();
        let logo = held("l");
        // The key a US keyboard labels L types `д` on a Russian layout; the chord is the Latin
        // letter the same key carries on US.
        let mut cyrillic = key(keysyms::Cyrillic_de, Some('д'), logo);
        cyrillic.latin_keysym = u32::from('l');
        assert_eq!(
            keys.resolve(&cyrillic, false, false),
            Some((Invocation::shortcut(ShortcutAction::Lock), KeyClaim::Take))
        );
        // Without the Latin key on that layout, it is no chord at all.
        cyrillic.latin_keysym = keysyms::Cyrillic_de;
        assert_eq!(keys.resolve(&cyrillic, false, false), None);
    }

    #[test]
    fn chords_of_the_shortcut_table_become_the_chromes_shortcut_action() {
        let keys = keys();
        let logo = held("l");
        let tab = key(keysyms::Tab, None, logo);
        assert_eq!(
            keys.resolve(&tab, false, false),
            Some((
                Invocation::shortcut(ShortcutAction::WorkspaceOverview),
                KeyClaim::Take
            ))
        );
        assert_eq!(
            Invocation::shortcut(ShortcutAction::WorkspaceOverview).arguments["action"],
            "workspace_overview"
        );
        // Chords wait while the session is locked; plain typing is never one.
        assert_eq!(keys.resolve(&tab, true, false), None);
        let a = key(keysyms::a, Some('a'), KeyModifiers::default());
        assert_eq!(keys.resolve(&a, false, false), None);
    }

    #[test]
    fn the_fixed_chords_run_the_chrome_verb_their_menu_rows_name() {
        let keys = keys();
        let logo = held("l");
        let shift_logo = KeyModifiers {
            shift: true,
            ..logo
        };
        let alt = held("a");
        let fixed = |id: &str| {
            Some((
                Invocation::new(CHROME_RESOURCE, "fixed-shortcut").with("action", id),
                KeyClaim::Take,
            ))
        };
        for (pressed, id) in [
            (key(keysyms::w, Some('w'), logo), "fixed-close-tab"),
            (key(keysyms::F4, None, alt), "fixed-close-tab"),
            (key(keysyms::n, Some('n'), logo), "fixed-new-window"),
            (key(keysyms::t, Some('t'), logo), "fixed-duplicate-tab"),
            (key(keysyms::Return, None, logo), "fixed-open-terminal"),
            (key(keysyms::s, Some('s'), logo), "fixed-strip-switcher"),
            (
                key(keysyms::s, Some('S'), shift_logo),
                "fixed-capture-screenshot",
            ),
            (
                key(keysyms::p, Some('P'), shift_logo),
                "fixed-picture-in-picture",
            ),
            (
                key(keysyms::r, Some('R'), shift_logo),
                "fixed-save-recent-replay",
            ),
            (key(keysyms::h, Some('h'), logo), "fixed-workspace-history"),
            (
                key(keysyms::h, Some('H'), shift_logo),
                "fixed-explore-workspace-history",
            ),
            (key(keysyms::Escape, None, logo), "fixed-stop-remote-input"),
        ] {
            assert_eq!(keys.resolve(&pressed, false, false), fixed(id), "{id}");
        }
        // The chords the keys answer where they resolve keep that answer: the window walk, the
        // workspace steps, the display sheet and Print.
        for answered_before in [
            key(keysyms::Tab, None, held("a")),
            key(keysyms::Page_Up, None, logo),
            key(keysyms::p, Some('p'), logo),
            key(keysyms::Print, None, KeyModifiers::default()),
        ] {
            let resolved = keys.resolve(&answered_before, false, false);
            assert!(
                resolved
                    .as_ref()
                    .is_none_or(|(invocation, _)| invocation.address.action != "fixed-shortcut"),
                "{resolved:?}"
            );
        }
    }

    #[test]
    fn the_shortcut_guide_holds_back_the_desktops_chords_while_it_is_up() {
        let (mut keys, guide) = with_guide();
        let logo = held("l");
        let space = key(keysyms::space, Some(' '), logo);
        assert_eq!(keys.claim_key(&space, false, false), KeyClaim::Take);
        keys.key(&space);
        assert!(
            keys.keys.invocations.borrow().is_empty(),
            "search must not open on the guide: {:?}",
            keys.keys.invocations.borrow()
        );
        // The guide's own key still reaches the desktop, which closes it.
        let f1 = key(keysyms::F1, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&f1, false, false), KeyClaim::Take);
        keys.key(&f1);
        assert_eq!(
            keys.keys.invocations.borrow().as_slice(),
            [Invocation::shortcut(ShortcutAction::ShortcutHelp)]
        );
        // A key that answers nothing at all is not the guide's to take: the sheet behind it moves
        // its focus with Tab.
        let tab = key(keysyms::Tab, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::Pass);
        // A hardware key changes a level whatever is on screen.
        let volume = key(
            keysyms::XF86_AudioRaiseVolume,
            None,
            KeyModifiers::default(),
        );
        assert_eq!(
            keys.claim_key(&volume, false, false),
            KeyClaim::TakeRepeating
        );
        keys.keys.invocations.borrow_mut().clear();
        keys.key(&volume);
        assert_eq!(
            keys.keys.invocations.borrow().as_slice(),
            [Invocation::new(SOUND, "volume-up").showing(OsdKind::Volume)]
        );
        assert_eq!(guide.take_caught(), []);
    }

    #[test]
    fn the_shortcut_guide_takes_the_next_chord_while_it_waits() {
        let (mut keys, guide) = with_guide();
        guide.listen_for("search");
        let control = held("c");
        let j = key(keysyms::j, Some('j'), control);
        assert_eq!(keys.claim_key(&j, false, false), KeyClaim::Take);
        keys.key(&j);
        assert_eq!(
            guide.take_caught(),
            [Caught::Chord(KeyChord::parse("Ctrl+J").expect("a chord"))]
        );
        assert!(
            keys.keys.invocations.borrow().is_empty(),
            "the chord is the guide's, not the desktop's"
        );
        // A modifier on its own is no chord.
        let shift = key(keysyms::Shift_L, None, held("s"));
        keys.key(&shift);
        assert_eq!(guide.take_caught(), []);
        // Escape leaves the waiting to the guide, which stays up.
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Take);
        keys.key(&escape);
        assert_eq!(guide.take_caught(), [Caught::Cancelled]);
        assert_eq!(guide.listening(), None);
        assert!(guide.up(), "the guide itself stays up");
    }

    #[test]
    fn a_client_holding_the_shortcuts_keeps_the_chords_but_not_the_way_to_lock() {
        let mut keys = keys();
        let logo = held("l");
        let tab = key(keysyms::Tab, None, logo);
        assert_eq!(keys.claim_key(&tab, false, false), KeyClaim::Take);
        assert_eq!(
            keys.claim_key(&tab, false, true),
            KeyClaim::Pass,
            "the virtual machine answers its own Tab"
        );
        // Locking the session stays the desktop's: a machine that could hold that chord could hide
        // the way out of itself.
        let lock = key(keysyms::l, Some('l'), logo);
        assert_eq!(keys.claim_key(&lock, false, false), KeyClaim::Take);
        assert_eq!(keys.claim_key(&lock, false, true), KeyClaim::Take);
        keys.keys.invocations.borrow_mut().clear();
        keys.key(&lock);
        assert_eq!(
            keys.keys.invocations.borrow().as_slice(),
            [Invocation::shortcut(ShortcutAction::Lock)]
        );
        // A hardware key changes a level whatever a client holds.
        let volume = key(
            keysyms::XF86_AudioRaiseVolume,
            None,
            KeyModifiers::default(),
        );
        assert_eq!(
            keys.claim_key(&volume, false, true),
            KeyClaim::TakeRepeating
        );
    }

    #[test]
    fn a_shortcuts_chord_is_claimed_and_handed_to_the_automation_host() {
        let shortcuts = Rc::new(RefCell::new(ShortcutConfig::default()));
        let triggered = PressedChords::default();
        let mut keys = on_this_device(
            DeskKeys::default(),
            Rc::default(),
            Rc::clone(&shortcuts),
            Presses {
                invocations: Invocations::default(),
                triggered: Rc::clone(&triggered),
            },
            Rc::default(),
        );
        let chord = KeyChord::parse("ctrl+alt+j").expect("a chord");
        // Nothing armed: the chord is nobody's, and types into what is up.
        assert_eq!(
            keys.claim_key(&key(keysyms::j, Some('j'), held("ca")), false, false),
            KeyClaim::Pass
        );
        // The automation armed a shortcut on that chord: the session answers it, and hands the
        // press on once.
        let mut table = shortcuts.borrow().clone();
        assert!(tilo_action_broker::automation::replace_trigger_chords(
            &mut table,
            vec![settings_store::hardware::ActionChord {
                chord: chord.clone(),
                target: tilo_action_broker::automation::chord_target("workspace", "tidy"),
                label: "Run Tidy".into(),
            }],
        ));
        *shortcuts.borrow_mut() = table;
        let press = key(keysyms::j, Some('j'), held("ca"));
        assert_eq!(keys.claim_key(&press, false, false), KeyClaim::Take);
        assert_eq!(
            keys.claim_key(&press, true, false),
            KeyClaim::Pass,
            "a locked session runs no shortcut"
        );
        keys.key(&press);
        assert_eq!(triggered.borrow().as_slice(), [chord]);
        assert!(
            keys.keys.invocations.borrow().is_empty(),
            "the shortcut starts on the automation host, not as a desktop action"
        );
    }

    #[test]
    fn escape_folds_the_card_a_widget_has_open_and_nothing_behind_it_hears_it() {
        let fold = DeskFold::default();
        let mut keys = over(DeskKeys::new(fold.clone(), Overlays::new()));
        let escape = key(keysyms::Escape, None, KeyModifiers::default());
        // Folded cards want no key: Escape goes on to whatever is up.
        assert_eq!(keys.claim_key(&escape, false, false), KeyClaim::Pass);

        fold.set_open(true);
        assert_eq!(
            keys.claim_key(&escape, false, false),
            KeyClaim::Take,
            "an open card folds on Escape before anything else hears it"
        );
        keys.key(&escape);
        assert!(fold.take_asked(), "the key asks the desk to fold");
        assert!(!fold.take_asked(), "and asks once");
    }
    /// Ctrl+Alt+F3 asks the session to leave for the third virtual terminal,
    /// and a locked session takes no such chord: the way out of a desktop that
    /// cannot give the machine back is not a way around the lock.
    #[test]
    fn control_alt_function_keys_ask_for_a_virtual_terminal() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let mut keys = on_this_device(
            DeskKeys::default(),
            Rc::default(),
            Rc::default(),
            Presses::default(),
            Rc::clone(&commands),
        );
        let chord = held("ca");
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, chord), false, false),
            KeyClaim::Take
        );
        assert_eq!(
            commands.borrow().as_slice(),
            [SessionCommand::SwitchVt { vt: 3 }]
        );
        // A layout that types another character on that key still names the terminal: the chord is
        // the key the US keyboard calls F4.
        let mut other = key(keysyms::Cyrillic_ze, Some('з'), chord);
        other.latin_keysym = keysyms::F4;
        assert_eq!(keys.claim_key(&other, false, false), KeyClaim::Take);
        assert_eq!(
            commands.borrow().as_slice(),
            [
                SessionCommand::SwitchVt { vt: 3 },
                SessionCommand::SwitchVt { vt: 4 }
            ]
        );
        // The same keys without Alt, or on a locked session, are not the session's to take.
        let plain = held("c");
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, plain), false, false),
            KeyClaim::Pass
        );
        assert_eq!(
            keys.claim_key(&key(keysyms::F3, None, chord), true, false),
            KeyClaim::Pass
        );
        assert_eq!(
            commands.borrow().len(),
            2,
            "only the chords the person pressed left the session"
        );
    }
}
