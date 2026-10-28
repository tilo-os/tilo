//! The imager's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): Rescan while the wizard asks for a drive. The
//! steps' titles, Back and Continue are the wizard's own.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use super::{Command, ImagerApp};
use crate::model::Step;
use crate::resource::ImagerEffect;

pub(crate) const RESCAN: &str = "imager.rescan";

impl ImagerApp {
    /// The window bar's slots for the step shown now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar::default();
        if self.step == Step::Drive && !self.prepared_delivery.needs_source_review() {
            toolbar
                .actions
                .push(ToolbarAction::new(RESCAN, "refresh", "Rescan drives"));
        }
        toolbar
    }

    /// Run what the person did in the window's bar through the published
    /// effect. Returns whether anything was asked.
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
            match &self.dispatcher {
                Some(dispatcher) => dispatcher.effect(&ImagerEffect::Rescan),
                None => self.commands.borrow_mut().push(Command::Rescan),
            }
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
    use crate::resource::Imager;
    use ui::prelude::TestApp;

    /// The wizard on the sample drives and images, live: what the bar
    /// asks reaches the published actions as in a window someone uses.
    struct SampleImager;

    impl ui::framework::ResourceApp for SampleImager {
        type Resource = Imager;
        const ID: &'static str = "so.tilo.imager";
        const TITLE: &'static str = "Tilo Installer";

        fn view(cx: &mut ui::framework::ViewCx<Imager>) -> ui::widget::Element {
            cx.surface(ImagerApp::sample())
        }
    }

    fn at(step: Step) -> TestApp<SampleImager> {
        let mut host = TestApp::<SampleImager>::new().expect("the imager opens");
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

    /// Rescan is the bar's while the wizard asks for a drive, and runs the
    /// published rescan; the step draws its title but no Rescan of its own.
    #[test]
    fn rescan_is_the_window_bars_while_the_wizard_asks_for_a_drive() {
        let mut host = at(Step::Drive);
        assert_eq!(action_ids(), [RESCAN]);
        assert!(
            host.harness().a11y_node_named("Choose a drive").is_some(),
            "the step keeps its title"
        );
        assert!(
            host.harness().a11y_node_named("Rescan drives").is_none(),
            "the step draws no Rescan of its own"
        );

        host.with_widget::<ImagerApp, _>(|app| app.status.clear());
        host.harness()
            .press_window_slot(ToolbarEvent::Action { id: RESCAN.into() });
        host.settle();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|app| app.status.clone())
                .as_deref(),
            Some("Rescanned drives")
        );

        let _ = at(Step::Source);
        assert!(
            action_ids().is_empty(),
            "choosing an image has nothing to rescan"
        );
    }
}
