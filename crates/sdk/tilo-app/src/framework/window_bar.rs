//! An app's window bar, declared as bindings.
//!
//! The bar is the window's own row (`ui_core::window_toolbar`): the app says
//! what its slots are, the desktop draws them and the app hears what the
//! person did in them. This module is that declaration in the framework's
//! terms: an app builds one [`WindowBar`] out of what the window shows and
//! does — the location, the views, the actions, toggles and menus — and the
//! framework makes the slots of it.
//!
//! A slot is a *binding*, never an id the app builds and later takes apart,
//! and never a callback: what pressing it does is a change the framework
//! makes itself, through the same door an assistive technology, a script or
//! an agent uses.
//!
//! - an **action** ([`WindowBar::action`]) submits a verb of the record, with
//!   its arguments, named and pictured as the action is declared, and disabled
//!   where the participant may not take it;
//! - an **effect** ([`WindowBar::effect`]) invokes an effect the app declares
//!   (what the window shows, what it asks the host for), answered wherever the
//!   app already answers it;
//! - a **flag** ([`WindowBar::flag`]) is a toggle bound to a bool of the
//!   record: pressing it submits the opposite;
//! - a **view switcher** ([`WindowBar::views`]) is a list of labels, each
//!   bound to an action or an effect that shows that view, with the one shown
//!   now named by its place in the list;
//! - the **location** ([`WindowBar::label`], [`WindowBar::place`]) is what the
//!   window shows, from the value the app read.
//!
//! Every id is the framework's, made from the invocation it dispatches; the
//! app never writes one, never reads one back and never matches one by
//! prefix. It hands the bar's events to [`WindowBar::take_events`], which
//! submits each into what declared it.
//!
//! ```ignore
//! fn window_bar(&self) -> WindowBar<Tracker> {
//!     let place = self.nav.place.with(|place| *place);
//!     WindowBar::new(self.dispatcher.clone())
//!         .under(&self.link)
//!         .label(self.location())
//!         .views(
//!             tr("tasks-views"),
//!             Place::ALL.iter().position(|each| *each == place).unwrap_or_default(),
//!             Place::ALL.map(|each| Choice::new(each.title(), &each.show())),
//!         )
//! }
//! ```
//!
//! # The shell's own slots
//!
//! A window with a [`NavShell`] shows the shell's sidebar toggle, Back and
//! inspector toggle in the bar. [`WindowBar::over`] (or [`WindowBar::under`]
//! with the shell's [`WindowBarLink`], for an app painted inside the shell's
//! content) starts the declaration from those slots and hands the shell the
//! presses on them; the shell draws no header row of its own
//! ([`NavShell::with_window_toolbar`]).
//!
//! # Declaring and hearing
//!
//! A window declares its bar for what it shows now, and hears the bar's
//! events through it: a press names a slot of the slots on screen, so the
//! declaration that made it is the one that dispatches it. A widget that
//! settles each frame keeps the bar it declared, hears it, then declares it
//! again:
//!
//! ```ignore
//! fn settle(&mut self, ..) {
//!     self.bar.take_events();
//!     self.bar = self.window_bar();
//!     self.bar.declare();
//! }
//! ```
//!
//! Outside a Tilo session (a window of its own, a test) nothing hosts the
//! slots, and the app's own row — if it keeps one — stays as it was.

use tilo_resource::{Effect, Path, Resource};
use ui_core::widget::{NavShell, WindowBarLink};
use ui_core::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::{BarActions, Dispatcher, Submit};

/// The most entries one bar keeps: the app's own, past what any row and menu
/// show together.
const MAX_ENTRIES: usize = 128;

/// What a slot of the bar does when pressed: a verb of the record with its
/// arguments, a field edit, an effect the app answers. A binding is submitted
/// through the window's dispatcher, never run by the app's own code, so the
/// window's bar and an assistive technology take the same door.
pub trait Binding<R: Resource> {
    /// What a press submits; `None` where the app names something the record
    /// does not publish (a control the framework reports unbound).
    fn bound(self) -> Option<tilo_resource::Invoke<R>>;
}

impl<R: Resource> Binding<R> for tilo_resource::Invoke<R> {
    fn bound(self) -> Option<tilo_resource::Invoke<R>> {
        Some(self)
    }
}

impl<R: Resource, E: Effect> Binding<R> for &E {
    fn bound(self) -> Option<tilo_resource::Invoke<R>> {
        Some(tilo_resource::Invoke::effect(self))
    }
}

/// One view of a view switcher ([`WindowBar::views`]): what it says, and the
/// binding that shows it.
pub struct Choice<R: Resource> {
    label: String,
    /// The action or effect that shows this view; `None` where the app named
    /// something the record does not publish.
    press: Option<tilo_resource::Invoke<R>>,
}

impl<R: Resource> Choice<R> {
    /// The view `label`, shown by `binding` — a verb with its arguments, a
    /// field edit, an effect.
    #[must_use]
    pub fn new(label: impl Into<String>, binding: impl Binding<R>) -> Self {
        Self {
            label: label.into(),
            press: binding.bound(),
        }
    }
}

/// The window's bar: the slots the app declares now, and what pressing each
/// of them submits.
pub struct WindowBar<R: Resource> {
    /// The shell's own slots, and the app's location, views and leading
    /// actions.
    slots: WindowToolbar,
    /// The app's own entries, in the order declared: the row's buttons and
    /// the overflow menu's.
    entries: Vec<ToolbarAction>,
    /// The actions and effects of the app's record, each submitted through
    /// the window's dispatcher when pressed.
    actions: BarActions<R>,
    /// The shell the bar starts from ([`Self::over`]): its slots' presses go
    /// to it.
    shell: Option<WindowBarLink>,
    /// The entry the sidebar toggle presses, for a window whose app shows the
    /// sidebar itself ([`Self::sidebar`]).
    sidebar: Option<String>,
}

impl<R: Resource> WindowBar<R> {
    /// The bar of the window running `dispatcher`'s record: nothing declared
    /// yet.
    #[must_use]
    pub fn new(dispatcher: Dispatcher<R>) -> Self {
        Self {
            slots: WindowToolbar::default(),
            entries: Vec::new(),
            actions: BarActions::new(dispatcher),
            shell: None,
            sidebar: None,
        }
    }

    /// The dispatcher this bar's actions are submitted through: what a window
    /// keeps to declare its bar anew for what it shows now.
    #[must_use]
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.actions.dispatcher().clone()
    }

    /// Start from `shell`'s own slots — its sidebar toggle, Back, the
    /// inspector toggle — and hand it the presses on them. The shell draws
    /// no header row of its own ([`NavShell::with_window_toolbar`]).
    #[must_use]
    pub fn over(mut self, shell: &NavShell) -> Self {
        self.slots = shell.window_toolbar();
        self.shell = Some(shell.window_bar_link());
        self
    }

    /// Start from the slots the shell of `link` last laid out, for an app
    /// painted inside the shell's content column.
    #[must_use]
    pub fn under(mut self, link: &WindowBarLink) -> Self {
        self.slots = link.slots();
        self.shell = Some(link.clone());
        self
    }

    /// The window shows `label`: its mode, the open document, the place.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.slots
            .location
            .get_or_insert_with(ToolbarLocation::default)
            .label = label.into();
        self
    }

    /// The window's location whole: its label, its choices, its breadcrumb,
    /// the anchor it reopens at ([`ToolbarLocation`]); `None` where the window
    /// shows no location (its sidebar stands in front of the content).
    #[must_use]
    pub fn place(mut self, location: Option<ToolbarLocation>) -> Self {
        self.slots.location = location;
        self
    }

    /// The window's sidebar toggle and whether it is open, for a window whose
    /// app shows the sidebar itself: pressing it submits `binding`.
    #[must_use]
    pub fn sidebar(mut self, open: bool, binding: impl Binding<R>) -> Self {
        self.slots.sidebar = Some(open);
        if let Some(invoke) = binding.bound() {
            self.sidebar = Some(self.actions.action(invoke).id).filter(|id| !id.is_empty());
        }
        self
    }

    /// The app's views of what the window shows — Issues and Board, Month and
    /// Week — as its view switcher: `chosen` is the place in `choices` of the
    /// one shown now, and choosing one submits what it carries (the effect
    /// that shows it).
    #[must_use]
    pub fn views(
        mut self,
        name: impl Into<String>,
        chosen: usize,
        choices: impl IntoIterator<Item = Choice<R>>,
    ) -> Self {
        let mut views = Vec::new();
        for choice in choices {
            let Some(invoke) = choice.press else {
                continue;
            };
            let entry = self.actions.action(invoke);
            views.push(ToolbarView {
                id: entry.id,
                label: choice.label,
            });
        }
        let Some(last) = views.len().checked_sub(1) else {
            self.slots.views = None;
            return self;
        };
        let Some(chosen) = views.get(chosen.min(last)).map(|view| view.id.clone()) else {
            self.slots.views = None;
            return self;
        };
        self.slots.views = Some(ToolbarViews {
            name: name.into(),
            chosen,
            choices: views,
        });
        self
    }

    /// The button that submits `submit` — a verb with its arguments, a field
    /// edit, a form — named and pictured as the action is declared, disabled
    /// where the participant may not take it. The framework submits it
    /// through the window's dispatcher when it is pressed.
    #[must_use]
    pub fn action(mut self, submit: impl Submit<R>) -> Self {
        let entry = self.actions.action(submit);
        self.push(entry);
        self
    }

    /// The button that invokes `effect`, named and pictured as it is declared,
    /// and disabled where the participant may not take it. The effect is
    /// answered wherever the app answers that effect: the window's own view,
    /// or the host.
    #[must_use]
    pub fn effect<E: Effect>(mut self, effect: &E) -> Self {
        let entry = self.actions.effect(effect);
        self.push(entry);
        self
    }

    /// The action `submit` as a leading action: on where the window is
    /// (previous, next, today), right after the navigation.
    #[must_use]
    pub fn leading(mut self, submit: impl Submit<R>) -> Self {
        let entry = self.actions.action(submit);
        self.slots.leading.push(entry);
        self
    }

    /// The record's flag at `field` as a toggle: it shows `on`, and pressing
    /// it submits the opposite, disabled where the participant may not edit
    /// the record.
    #[must_use]
    pub fn flag(
        mut self,
        icon: impl Into<String>,
        name: impl Into<String>,
        field: &Path<R, bool>,
        on: bool,
    ) -> Self {
        let mut entry = self.actions.action(field.set(!on));
        entry.icon = icon.into();
        entry.name = name.into();
        entry.toggles = true;
        entry.on = on;
        self.push(entry);
        self
    }

    /// The entry declared last shows as pressed when `on` and as a toggle: an
    /// effect that sets one of a window's own states (a unit, a panel) reads
    /// as the state it sets.
    #[must_use]
    pub fn on(mut self, on: bool) -> Self {
        if let Some(entry) = self.entries.last_mut() {
            entry.toggles = true;
            entry.on = on;
        }
        self
    }

    /// The entry declared last goes to the bar's overflowing menu, never the
    /// row: an occasional action (import, export, the app's settings).
    #[must_use]
    pub fn in_menu(mut self) -> Self {
        if let Some(entry) = self.entries.last_mut() {
            entry.menu = true;
        }
        self
    }

    /// The entry declared last is pictured by `icon` and named `name`: the
    /// window's own words for what the record's action or effect calls
    /// itself, where the bar names it differently.
    #[must_use]
    pub fn named(mut self, icon: impl Into<String>, name: impl Into<String>) -> Self {
        if let Some(entry) = self.entries.last_mut() {
            entry.icon = icon.into();
            entry.name = name.into();
        }
        self
    }

    /// The entry declared last is enabled only where `enabled` too: what it
    /// would do is possible only sometimes (a theme that must be legible
    /// before it applies), and never where the participant may not take it.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        if let Some(entry) = self.entries.last_mut() {
            entry.enabled &= enabled;
        }
        self
    }

    /// The slots as the app declared them: the shell's own, then the app's,
    /// in the row and in the menu.
    #[must_use]
    pub fn slots(&self) -> WindowToolbar {
        let mut toolbar = self.slots.clone();
        NavShell::push_window_actions(&mut toolbar, self.entries.clone());
        toolbar
    }

    /// Declare these slots as the window's.
    pub fn declare(&self) {
        ui_core::window_toolbar::declare(Some(self.slots()));
    }

    /// Submit what the person pressed in the window's bar since the last
    /// call: on the shell's own slots, the sidebar toggle, then the record's
    /// actions and effects. Returns whether anything was asked.
    pub fn take_events(&self) -> bool {
        let mut asked = false;
        for event in ui_core::window_toolbar::take_events() {
            asked = true;
            self.press(&event);
        }
        asked
    }

    /// Submit one event the bar sent. Returns whether it was one of these
    /// slots (then it is handled, even where the slot is gone).
    pub fn press(&self, event: &ToolbarEvent) -> bool {
        if let Some(shell) = &self.shell
            && shell.event(event)
        {
            return true;
        }
        if matches!(event, ToolbarEvent::ToggleSidebar)
            && let Some(id) = &self.sidebar
        {
            return self.actions.press(&ToolbarEvent::Action { id: id.clone() });
        }
        self.actions.press(event)
    }

    fn push(&mut self, entry: ToolbarAction) {
        if self.entries.len() < MAX_ENTRIES {
            self.entries.push(entry);
        }
    }
}
