//! The reader's retained header controls.
//!
//! Header actions are the toolkit's icon buttons in the shell's header row.
//! Each follows the library: a command with nothing to act on is disabled
//! (no feeds to refresh, no open article to star), a control whose meaning
//! flips with state is announced by what it does now, and a control that
//! opens a menu or panel records where it sits so the menu opens under it.

use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::widget::{AppModel, Button, ModelPath, Widget};
use vello::kurbo::Point;

use crate::app::{Core, Shared};

/// A header control that anchors a menu or a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Anchor {
    ListMenu,
    ArticleMenu,
    ReaderSettings,
}

/// An icon button in the shell's header that follows the reader's state.
pub(crate) struct HeaderButton {
    button: Button,
    core: Shared,
    anchor: Option<Anchor>,
    enabled: fn(&Core) -> bool,
    named: Option<fn(&Core) -> &'static str>,
    toggled: Option<fn(&Core) -> bool>,
}

impl HeaderButton {
    pub(crate) fn new(button: Button, core: &Shared) -> Self {
        Self {
            button,
            core: Rc::clone(core),
            anchor: None,
            enabled: |_| true,
            named: None,
            toggled: None,
        }
    }

    /// Enabled only while `f` holds.
    pub(crate) fn enabled_when(mut self, f: fn(&Core) -> bool) -> Self {
        self.enabled = f;
        self
    }

    /// Announced (and titled) by what `f` says it does now.
    pub(crate) fn named_by(mut self, f: fn(&Core) -> &'static str) -> Self {
        self.named = Some(f);
        self
    }

    /// A toggle that reads as on (the selection lift) while `f` holds.
    pub(crate) fn toggled_by(mut self, f: fn(&Core) -> bool) -> Self {
        self.toggled = Some(f);
        self
    }

    /// Record where the button sits, for the menu or panel it opens.
    pub(crate) fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    fn follow(&mut self) {
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let enabled = (self.enabled)(&core);
        let name = self.named.map(|f| f(&core));
        let toggled = self.toggled.map(|f| f(&core));
        drop(core);
        if let Some(on) = toggled
            && self.button.is_toggled() != on
        {
            self.button.set_toggled(on);
        }
        if self.button.is_enabled() != enabled {
            self.button.set_enabled(enabled);
        }
        if let Some(name) = name {
            self.button.set_accessible_name(name);
            self.button.set_tooltip(Some(name.to_string()));
        }
    }
}

impl ui::widget::Decorator for HeaderButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow();
        Widget::sync(&mut self.button, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.follow();
        Widget::update(&mut self.button, state, path)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.button, taffy, node, origin);
        if let Some(anchor) = self.anchor
            && let Ok(mut core) = self.core.try_borrow_mut()
        {
            core.anchors.insert(anchor, Widget::get_rect(&self.button));
        }
    }

    fn name(&self) -> &'static str {
        "FeedsHeaderButton"
    }
}
