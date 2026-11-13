//! The bar as one list of controls: every control it shows, where it stands and what it is
//! called, for a capture, an inspector and a layout check to read.

use super::{TabBar, WINDOW_HISTORY_TARGET, icons, new_tab_control};

impl TabBar {
    /// The bar as it lays itself out now: every control it shows, in reading order (the active
    /// tab's own slots — the sidebar toggle, Back and Forward, the leading actions, the views
    /// and the location's picker, the actions and the overflow button — then the window's
    /// History control and own controls), and the room it leaves empty beside its location's
    /// label.
    #[must_use]
    pub fn bar_layout(&self) -> crate::window_toolbar::BarLayout {
        use crate::widget::window_controls::WindowControl;
        use crate::window_toolbar::{BarControl, BarLayout};
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let mut spare = 0.0;
        let mut out = Vec::new();
        let mut push = |control: BarControl| {
            if control.rect[2] > 0.0 && control.rect[3] > 0.0 {
                out.push(control);
            }
        };
        if self.new_tab_rect.area() > 0.0 {
            push(new_tab_control(self.new_tab_rect));
        }
        if let Some(toolbar) = self.active_toolbar() {
            let slots = self.toolbar_row.slots();
            spare = slots.spare;
            if toolbar.sidebar.is_some() && !slots.sidebar_folded(toolbar) {
                let key = if toolbar.sidebar == Some(true) {
                    "window-toolbar-hide-sidebar"
                } else {
                    "window-toolbar-show-sidebar"
                };
                push(
                    BarControl::new(
                        "sidebar",
                        crate::widget::window_toolbar_row::SIDEBAR_TARGET,
                        slots.sidebar,
                    )
                    .named(lookup(key)),
                );
            }
            if toolbar.navigation.is_some() {
                push(
                    BarControl::new(
                        "back",
                        crate::widget::window_toolbar_row::BACK_TARGET,
                        slots.back,
                    )
                    .named(lookup("window-toolbar-back")),
                );
                if !slots.forward_folded(toolbar) {
                    push(
                        BarControl::new(
                            "forward",
                            crate::widget::window_toolbar_row::FORWARD_TARGET,
                            slots.forward,
                        )
                        .named(lookup("window-toolbar-forward")),
                    );
                }
            }
            if let Some(location) = toolbar.location.as_ref() {
                push(
                    BarControl::new(
                        "location",
                        crate::widget::window_toolbar_row::LOCATION_TARGET,
                        slots.location,
                    )
                    .named(location.label.clone()),
                );
            }
            for (index, (action, rect)) in toolbar.leading.iter().zip(&slots.leading).enumerate() {
                let labelled = action.labelled && slots.labelled;
                let icon = (!action.icon.is_empty()).then(|| action.icon.clone());
                if icon.is_none() && !labelled {
                    continue;
                }
                let control = BarControl::new("leading", action.id.clone(), *rect)
                    .named(action.name.clone())
                    .words(slots.leading_words.get(index).copied().unwrap_or(0.0));
                let control = if action.primary {
                    control.primary()
                } else {
                    control
                };
                push(if labelled {
                    control.icon(icon).labelled()
                } else {
                    control.icon(icon)
                });
            }
            if let (Some(views), Some(first), Some(last)) = (
                toolbar.views.as_ref(),
                slots.views.first(),
                slots.views.last(),
            ) {
                push(
                    BarControl::new(
                        "views",
                        crate::widget::window_toolbar_row::VIEWS_MENU_TARGET,
                        first.union(*last),
                    )
                    .named(views.name.clone()),
                );
            }
            if let Some(views) = toolbar.views.as_ref()
                && slots.views_menu.area() > 0.0
            {
                let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
                let label = chosen.map_or(views.name.as_str(), |view| view.label.as_str());
                push(
                    BarControl::new(
                        "views-menu",
                        crate::widget::window_toolbar_row::VIEWS_MENU_TARGET,
                        slots.views_menu,
                    )
                    .named(label.to_owned())
                    .icon(Some(icons::Icon::ChevronDown.name().to_owned()))
                    .menu(),
                );
            }
            for (index, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
                let mut control = BarControl::new("action", action.id.clone(), *rect)
                    .named(action.name.clone())
                    .words(slots.action_words.get(index).copied().unwrap_or(0.0));
                if !action.icon.is_empty() {
                    control = control.icon(Some(action.icon.clone()));
                }
                if action.menu || !action.items.is_empty() {
                    control = control.menu();
                }
                if action.primary {
                    control = control.primary();
                }
                push(if action.labelled && slots.actions_labelled {
                    control.labelled()
                } else {
                    control
                });
            }
            if slots.overflow.area() > 0.0 {
                push(
                    BarControl::new(
                        "overflow",
                        crate::widget::window_toolbar_row::OVERFLOW_TARGET,
                        slots.overflow,
                    )
                    .named(lookup("window-toolbar-more"))
                    .icon(Some(icons::Icon::MoreHorizontal.name().to_owned()))
                    .menu(),
                );
            }
        }
        if self.history_action.area() > 0.0 {
            push(
                BarControl::new("history", WINDOW_HISTORY_TARGET, self.history_action)
                    .named(lookup("history-mode-title")),
            );
        }
        for (slot, control, rect) in [
            ("minimize", WindowControl::Minimize, self.controls.minimize),
            ("maximize", WindowControl::Maximize, self.controls.maximize),
            ("close", WindowControl::Close, self.controls.close),
        ] {
            push(
                BarControl::new(slot, control.target(), rect)
                    .named(lookup(control.name_key(self.resize)))
                    .icon(Some(control.icon(self.resize).name().to_owned())),
            );
        }
        BarLayout {
            spare,
            controls: out,
            labels: self.painted_labels(),
        }
    }
}
