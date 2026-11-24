//! The imager's controls as slots in the window's bar: Rescan while the wizard asks for
//! a drive.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use super::ImagerApp;
use crate::model::Step;
use crate::resource::ImagerEffect;

pub(crate) const RESCAN: &str = "imager.rescan";

impl ImagerApp {
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar::default();
        if self.step == Step::Drive {
            toolbar.actions.push(ToolbarAction::new(
                RESCAN,
                "refresh",
                crate::resource::CATALOG.t("imager-rescan-drives"),
            ));
        }
        toolbar
    }

    pub(super) fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            if id != RESCAN {
                continue;
            }
            self.port.presses().press(ImagerEffect::Rescan);
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Step;

    use crate::hosting::testing::TestHost;

    fn at(step: Step) -> TestHost {
        let mut host = crate::app::sample_host();
        host.harness().present_in_window_bar();
        host.with_widget::<ImagerApp, _>(|app| {
            app.selected = Some(0);
            app.step = step;
            app.dirty = true;
        })
        .expect("the wizard");
        host.settle();
        host
    }

    fn action_ids() -> Vec<String> {
        ui::window_toolbar::declared()
            .map(|bar| bar.actions.into_iter().map(|action| action.id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn rescan_is_the_window_bars_while_the_wizard_asks_for_a_drive() {
        let mut host = at(Step::Drive);
        assert_eq!(action_ids(), [RESCAN]);
        assert!(
            host.harness().a11y_node_named("Choose a drive").is_some(),
            "the step keeps its title"
        );
        // The window's tree carries its bar first; Rescan may stand only there.
        fn named(nodes: &[ui::a11y::AccessNode], name: &str) -> usize {
            nodes
                .iter()
                .map(|node| {
                    usize::from(node.name.as_deref() == Some(name)) + named(&node.children, name)
                })
                .sum()
        }
        let harness = host.harness();
        assert_eq!(
            harness.a11y_nodes_named("Rescan drives").len(),
            named(&harness.window_parts(), "Rescan drives"),
            "the step draws no Rescan of its own"
        );

        host.with_widget::<ImagerApp, _>(|app| app.status.clear());
        host.harness()
            .press_window_part(ToolbarEvent::Action { id: RESCAN.into() });
        host.settle();
        let rescanned = crate::setup::strings::t("imager-rescan-done");
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|app| app.status.clone())
                .as_deref(),
            Some(rescanned.as_str())
        );

        let _ = at(Step::Source);
        assert!(
            action_ids().is_empty(),
            "choosing an image has nothing to rescan"
        );
    }
}
