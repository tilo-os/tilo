//! The Store's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the page as the window's location; on the catalogue, its
//! four parts as the view switcher (Discover, Updates, On this device, Bots)
//! and its actions: review a package file, the Flatpak remotes and trust,
//! and repairing the runtime while it needs it; on the repositories,
//! refreshing them. Back is the shell's slot, and search the bar's own,
//! which searches the catalogue in place (the Store is its
//! [`ui::widget::Queryable`]). Each slot runs the same published action a
//! button bound to it ran, through the window's controls.

use ui::tilo_actions::ControlInvocation;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::{StoreApp, VIEWS, View};
use crate::bindings;
use crate::resource::StoreEffect;
use ui::widget::Widget as _;

const REVIEW_PACKAGE: &str = "review-package";
const REMOTES: &str = "remotes";
const TRUST: &str = "trust";
const REPAIR_RUNTIME: &str = "repair-runtime";
const REFRESH_REMOTES: &str = "refresh-remotes";
/// A view's slot id: `view:` and its name in [`VIEWS`].
const VIEW_PREFIX: &str = "view:";

impl StoreApp {
    /// Whether the window shows the catalogue (no page, list or runtime over
    /// it).
    pub(super) fn shows_catalogue(&self) -> bool {
        !self.runtime_visible()
            && !self.safety_open
            && !self.remotes_open
            && self.focused_detail.is_none()
    }

    /// The window bar's slots, the shell's own (`base`) first.
    pub(super) fn window_toolbar(&self, mut base: WindowToolbar) -> WindowToolbar {
        base.location = Some(ToolbarLocation {
            label: if self.runtime_visible() {
                ui::i18n!("store-runtime-title")
            } else {
                self.page_title()
            },
            ..ToolbarLocation::default()
        });
        if self.remotes_open && !self.runtime_visible() {
            ui::widget::NavShell::push_window_actions(
                &mut base,
                vec![ToolbarAction::new(
                    REFRESH_REMOTES,
                    "refresh",
                    ui::i18n!("store-remote-refresh"),
                )],
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
        let mut actions = vec![
            ToolbarAction::new(REVIEW_PACKAGE, "box", ui::i18n!("store-package-review")).in_menu(),
            ToolbarAction::new(REMOTES, "cloud", ui::i18n!("store-remotes-title")).in_menu(),
            ToolbarAction::new(TRUST, "shield", ui::i18n!("store-trust-title")).in_menu(),
        ];
        if self.runtime_recovery.is_some() {
            actions.insert(
                0,
                ToolbarAction::new(
                    REPAIR_RUNTIME,
                    "triangle-alert",
                    ui::i18n!("store-runtime-show"),
                ),
            );
        }
        ui::widget::NavShell::push_window_actions(&mut base, actions);
        base
    }

    /// Declare the window bar's slots and queue what the person did there
    /// as the controls bound to the same actions queue it. Returns whether
    /// anything was queued.
    pub(super) fn sync_window_toolbar(&mut self) -> bool {
        let mut acted = false;
        for event in ui::window_toolbar::take_events() {
            if let Some(root) = self.root.as_mut()
                && root.window_toolbar_event(&event)
            {
                acted = true;
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            if let Some(invocation) = Self::bar_invocation(&id) {
                self.bar_controls.push_back(invocation);
                acted = true;
            }
        }
        let base = self
            .root
            .as_ref()
            .map(|root| root.window_toolbar())
            .unwrap_or_default();
        ui::window_toolbar::declare(Some(self.window_toolbar(base)));
        acted
    }

    /// The invocation of the published action slot `id` stands for.
    fn bar_invocation(id: &str) -> Option<ControlInvocation> {
        if let Some(view) = id.strip_prefix(VIEW_PREFIX) {
            let index = VIEWS.iter().position(|name| *name == view)?;
            return bindings::view()?.invoke(index).ok();
        }
        let effect = match id {
            REVIEW_PACKAGE => StoreEffect::ImportPackage {
                path: None,
                content: None,
                reported_size: None,
            },
            REMOTES => StoreEffect::ShowRemotes,
            TRUST => StoreEffect::ShowTrust,
            REPAIR_RUNTIME => StoreEffect::ShowRuntime,
            REFRESH_REMOTES => StoreEffect::RefreshRemotes,
            _ => return None,
        };
        bindings::effect(&effect)?.invoke(()).ok()
    }
}

impl ui::widget::controls::ActionControls for StoreApp {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.bar_controls.pop_front().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }

    fn has_pending_control_invocation(&self) -> bool {
        !self.bar_controls.is_empty()
            || self
                .children()
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }
}
