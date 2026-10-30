//! The Store's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the page as the window's location; on the catalogue, its
//! four parts as the view switcher (Discover, Updates, On this device, Bots)
//! and its actions: review a package file, the Flatpak remotes and trust;
//! on the repositories,
//! refreshing them. Back is the shell's slot, and search the bar's own,
//! which searches the catalogue in place (the Store is its
//! [`ui::widget::Queryable`]). Each action runs the same published effect
//! a button bound to it ran, through the window's dispatcher; a view, the
//! published view action, through the window's controls.

use ui::tilo_actions::ControlInvocation;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::{StoreApp, StoreState, VIEWS, View};
use crate::bindings;
use crate::resource::StoreEffect;
use ui::widget::Widget as _;

/// A view's slot id: `view:` and its name in [`VIEWS`].
const VIEW_PREFIX: &str = "view:";

impl StoreState {
    /// Whether the window shows the catalogue (no page or list over it).
    pub(super) fn shows_catalogue(&self) -> bool {
        !self.safety_open && !self.remotes_open && self.focused_detail.is_none()
    }

    /// The window bar's slots, the shell's own (`base`) first.
    pub(super) fn window_toolbar(&self, mut base: WindowToolbar) -> WindowToolbar {
        let entry = |effect: StoreEffect, icon: &str, name: String| {
            self.bar.as_ref().map(|bar| ToolbarAction {
                icon: icon.into(),
                name,
                ..bar.effect(&effect)
            })
        };
        base.location = Some(ToolbarLocation {
            label: self.page_title(),
            ..ToolbarLocation::default()
        });
        if self.remotes_open {
            ui::widget::NavShell::push_window_actions(
                &mut base,
                entry(
                    StoreEffect::RefreshRemotes,
                    "refresh",
                    ui::i18n!("store-remote-refresh"),
                ),
            );
        }
        if !self.shows_catalogue() {
            return base;
        }
        let updates = self.update_ids().len();
        let updates_label = if updates == 0 {
            ui::i18n!("store-view-updates")
        } else {
            ui::i18n!("store-view-updates-count", "count" => ui::format::count(updates as u64))
        };
        let chosen = match &self.view {
            View::Home | View::Category(_) | View::Repository(_) => "home",
            View::Updates => "updates",
            View::Installed => "installed",
            View::Bots => "bots",
        };
        let labels = [
            ui::i18n!("store-view-home"),
            updates_label,
            ui::i18n!("store-view-installed"),
            ui::i18n!("store-view-bots"),
        ];
        base.views = Some(ToolbarViews {
            name: ui::i18n!("store-views-label"),
            choices: VIEWS
                .iter()
                .zip(labels)
                .map(|(view, label)| ToolbarView {
                    id: format!("{VIEW_PREFIX}{view}"),
                    label,
                })
                .collect(),
            chosen: format!("{VIEW_PREFIX}{chosen}"),
        });
        // Opening a package file and managing where apps come from are
        // occasional: they stand named in the More menu, not as bare icons.
        let occasional = [
            entry(
                StoreEffect::ImportPackage {
                    path: None,
                    content: None,
                    reported_size: None,
                },
                "box",
                ui::i18n!("store-package-review"),
            ),
            entry(
                StoreEffect::ShowRemotes,
                "cloud",
                ui::i18n!("store-remotes-title"),
            ),
            entry(
                StoreEffect::ShowTrust,
                "shield",
                ui::i18n!("store-trust-title"),
            ),
        ];
        let actions = occasional.into_iter().flatten().map(ToolbarAction::in_menu);
        ui::widget::NavShell::push_window_actions(&mut base, actions);
        base
    }

    /// The invocation of the view action the view slot `id` stands for.
    fn view_invocation(id: &str) -> Option<ControlInvocation> {
        let view = id.strip_prefix(VIEW_PREFIX)?;
        let index = VIEWS.iter().position(|name| *name == view)?;
        bindings::view()?.invoke(index).ok()
    }
}

impl StoreApp {
    /// Declare the window bar's slots and queue what the person did there
    /// as the controls bound to the same actions queue it. Returns whether
    /// anything was queued.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        let mut store = self.state.borrow_mut();
        let mut acted = false;
        for event in ui::window_toolbar::take_events() {
            if let Some(root) = self.root.as_mut()
                && root.window_toolbar_event(&event)
            {
                acted = true;
                continue;
            }
            if store.bar.as_ref().is_some_and(|bar| bar.press(&event)) {
                acted = true;
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            if let Some(invocation) = StoreState::view_invocation(&id) {
                store.bar_controls.push_back(invocation);
                acted = true;
            }
        }
        let base = self
            .root
            .as_ref()
            .map(|root| root.window_toolbar())
            .unwrap_or_default();
        ui::window_toolbar::declare(Some(store.window_toolbar(base)));
        acted
    }
}

impl ui::widget::controls::ActionControls for StoreApp {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        let queued = self.state().bar_controls.pop_front();
        queued.or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }

    fn has_pending_control_invocation(&self) -> bool {
        !self.state.borrow().bar_controls.is_empty()
            || self
                .children()
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }
}
