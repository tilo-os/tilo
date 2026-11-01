//! The resource's declared actions and effects as entries of the window's
//! bar.
//!
//! An app puts one of its record's verbs, field edits or effects in its
//! window's bar ([`ui_core::window_toolbar`]) by naming it, the way a bound
//! button names it (`bar.action(Snippets::DELETE.with(id))`,
//! `bar.effect(&View::ZoomIn)`): the entry takes its name, icon and
//! destructive role from the declaration, and is disabled where the
//! participant may not take it. Where it sits (the row or the menu, a menu
//! button's items) and whether a toggle is on stay the app's, set on the
//! entry it gets back like on any [`ToolbarAction`].
//!
//! The app hands each event it takes from the bar to [`BarActions::press`]
//! first, as it hands them to the navigation shell: a press on one of these
//! entries is submitted through the window's dispatcher, as a bound
//! control's is, and never reaches the app's own slot handling. No id to
//! keep in step and no command to map it back to.
//!
//! What the window does that is no action of its record (a panel it opens,
//! a view state it switches) is an entry of [`BarCommands`]: it carries the
//! app's own command, and a press on it hands the command back.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use tilo_actions::{ActionNeed, RiskLevel};
use tilo_resource::{Effect, Invoke, Resource};
use ui_core::window_toolbar::{ToolbarAction, ToolbarEvent};

use super::{Dispatcher, Submit};

/// Starts the id of every entry a [`BarActions`] runs, so a press on one is
/// told from the app's own slots by its id alone.
const PREFIX: &str = "invoke:";

/// The most entries kept for presses. An entry's id is what it invokes, so
/// a bar declared anew keeps the same ids; this only bounds the ones a
/// changing bar (a toggle's argument flipping) leaves behind.
const KEPT: usize = 256;

/// The window bar's entries that invoke the resource's actions and effects.
pub struct BarActions<R: Resource> {
    dispatcher: Dispatcher<R>,
    /// What each entry declared since the bar last forgot invokes, by id.
    shown: RefCell<HashMap<String, Invoke<R>>>,
}

impl<R: Resource> BarActions<R> {
    /// Entries submitted through `dispatcher`: the window's own.
    #[must_use]
    pub fn new(dispatcher: Dispatcher<R>) -> Self {
        Self {
            dispatcher,
            shown: RefCell::new(HashMap::new()),
        }
    }

    /// The window's dispatcher, which these entries are submitted through.
    #[must_use]
    pub fn dispatcher(&self) -> &Dispatcher<R> {
        &self.dispatcher
    }

    /// The entry that submits `submit` (a verb, a field edit), named and
    /// pictured as its action is declared; a field edit, declared by no
    /// verb, has neither and is named by the app. Disabled where the
    /// participant may not take it.
    pub fn action(&self, submit: impl Submit<R>) -> ToolbarAction {
        let id = submit.action_id();
        let declarations = tilo_resource::vocabulary::<R>().declarations();
        let declared = declarations.iter().find(|decl| decl.id == id);
        let (name, icon) = declared.map_or_else(Default::default, |decl| {
            (decl.label.clone(), decl.icon.clone().unwrap_or_default())
        });
        let mut entry = match submit.into_invoke() {
            Ok(invoke) => {
                let enabled = self.dispatcher.may_invoke(&invoke);
                self.entry(invoke, icon, name).enabled(enabled)
            }
            Err(error) => {
                super::controls::unbound::<R>(&format!("`{id}`"), &error);
                ToolbarAction::new(String::new(), icon, name).enabled(false)
            }
        };
        entry.destructive = declared.is_some_and(|decl| decl.destructive);
        entry
    }

    /// The entry that invokes `effect`, named and pictured as it is
    /// declared, and answered by whoever answers the effect (a view effect
    /// by the window's surface). Disabled where the participant may not
    /// take it.
    pub fn effect<E: Effect>(&self, effect: &E) -> ToolbarAction {
        let declared = E::declaration(effect.id());
        let (name, icon) = declared.as_ref().map_or_else(Default::default, |decl| {
            (
                decl.label.to_owned(),
                decl.icon.unwrap_or_default().to_owned(),
            )
        });
        let enabled = declared.as_ref().is_some_and(|decl| {
            self.dispatcher.may(&ActionNeed {
                kind: decl.action_kind,
                field: None,
                privileged: false,
            })
        });
        let mut entry = self
            .entry(Invoke::effect(effect), icon, name)
            .enabled(enabled);
        entry.destructive = declared.is_some_and(|decl| decl.risk == RiskLevel::Destructive);
        entry
    }

    /// Submit what `event` presses when it presses one of these entries.
    /// Returns whether it was a press on one (then it is handled, even when
    /// the entry is gone), so the app's own slots never see it.
    pub fn press(&self, event: &ToolbarEvent) -> bool {
        let ToolbarEvent::Action { id } = event else {
            return false;
        };
        if !id.starts_with(PREFIX) {
            return false;
        }
        let invoke = self.shown.borrow().get(id).cloned();
        if let Some(invoke) = invoke {
            self.dispatcher.submit(invoke);
        }
        true
    }

    fn entry(&self, invoke: Invoke<R>, icon: String, name: String) -> ToolbarAction {
        let id = format!(
            "{PREFIX}{}{}:{}",
            if invoke.is_effect() { "effect:" } else { "" },
            invoke.id,
            serde_json::Value::Object(invoke.arguments.clone())
        );
        let mut shown = self.shown.borrow_mut();
        if shown.len() >= KEPT && !shown.contains_key(&id) {
            shown.clear();
        }
        shown.insert(id.clone(), invoke);
        ToolbarAction::new(id, icon, name)
    }
}

/// Starts the id of every entry a [`BarCommands`] runs.
const COMMAND_PREFIX: &str = "command:";

thread_local! {
    /// The serial of the next command entry, on any [`BarCommands`] of the
    /// thread: an id never names two commands, even of two windows.
    static NEXT_COMMAND: Cell<u64> = const { Cell::new(0) };
}

/// The window bar's entries that run the app's own commands `C`.
pub struct BarCommands<C> {
    /// The commands of the entries declared since the bar last forgot, by
    /// serial.
    shown: RefCell<Vec<(u64, C)>>,
}

impl<C> Default for BarCommands<C> {
    fn default() -> Self {
        Self {
            shown: RefCell::new(Vec::new()),
        }
    }
}

impl<C: Clone + PartialEq> BarCommands<C> {
    /// No entries yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The entry, pictured by `icon` and named `name`, that hands back
    /// `command` when pressed ([`Self::pressed`]). An equal command keeps
    /// its entry's id, so a bar declared anew is the same bar.
    pub fn entry(
        &self,
        icon: impl Into<String>,
        name: impl Into<String>,
        command: C,
    ) -> ToolbarAction {
        let mut shown = self.shown.borrow_mut();
        let serial = if let Some((serial, _)) = shown.iter().find(|(_, shown)| *shown == command) {
            *serial
        } else {
            if shown.len() >= KEPT {
                shown.clear();
            }
            let serial = NEXT_COMMAND.with(|next| next.replace(next.get().wrapping_add(1)));
            shown.push((serial, command));
            serial
        };
        ToolbarAction::new(format!("{COMMAND_PREFIX}{serial}"), icon, name)
    }

    /// The command of the entry `event` presses, if it presses one of
    /// these: the app runs it, and its own slots never see the press.
    #[must_use]
    pub fn pressed(&self, event: &ToolbarEvent) -> Option<C> {
        let ToolbarEvent::Action { id } = event else {
            return None;
        };
        let serial = id.strip_prefix(COMMAND_PREFIX)?.parse::<u64>().ok()?;
        self.shown
            .borrow()
            .iter()
            .find(|(shown, _)| *shown == serial)
            .map(|(_, command)| command.clone())
    }
}
