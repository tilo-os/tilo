//! Print Centre's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): Back to the printers from every other page, the
//! page shown as the location, and the page's actions: Add printer, Print a
//! document and Refresh with the printers, Check again in device help. Each
//! runs the published effect its control in the page ran.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

use super::{Page, PrintCentre, UiCommand, display_name};

pub(crate) const ADD_PRINTER: &str = "print-centre.add";
pub(crate) const PRINT: &str = "print-centre.print";
pub(crate) const REFRESH: &str = "print-centre.refresh";
pub(crate) const CHECK_AGAIN: &str = "print-centre.diagnostics.refresh";
pub(crate) const NEEDS_YOU: &str = "print-centre.needs";

impl PrintCentre {
    /// The window bar's slots for the page shown now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar::default();
        if let Some(title) = self.page_title() {
            toolbar.navigation = Some(ToolbarNavigation {
                back: true,
                forward: false,
            });
            toolbar.location = Some(ToolbarLocation {
                label: title,
                ..ToolbarLocation::default()
            });
        }
        let has_printers = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.queues.as_ref().ok())
            .is_some_and(|queues| !queues.is_empty());
        match self.page {
            Page::Dashboard => {
                // Adding a printer and printing both need printers: with
                // none, the page's own empty state offers to find one.
                if has_printers {
                    toolbar
                        .actions
                        .push(ToolbarAction::new(PRINT, "printer", "Print a document"));
                    toolbar
                        .actions
                        .push(ToolbarAction::new(ADD_PRINTER, "plus", "Add printer"));
                }
                toolbar
                    .actions
                    .push(ToolbarAction::new(REFRESH, "refresh", "Refresh"));
            }
            Page::Diagnostics => {
                toolbar
                    .actions
                    .push(ToolbarAction::new(CHECK_AGAIN, "refresh", "Check again"));
            }
            _ => {}
        }
        if let Some(label) = ui::attention::pill() {
            toolbar
                .actions
                .push(ToolbarAction::new(NEEDS_YOU, "bell", label).labelled());
        }
        toolbar
    }

    /// What the window is on, for every page but the printers.
    fn page_title(&self) -> Option<String> {
        Some(match &self.page {
            Page::Loading | Page::Dashboard => return None,
            Page::Queue => "Print queue".to_owned(),
            Page::History => "Print history".to_owned(),
            Page::Diagnostics => "Device help".to_owned(),
            Page::Device(name) => display_name(name),
            Page::Discovery => "Add a printer".to_owned(),
            Page::Print => "Print a document".to_owned(),
            Page::Needs => "Needs you".to_owned(),
            Page::Scan if self.scan.result.is_some() => "Scanned document".to_owned(),
            Page::Scan => "Scan a document".to_owned(),
        })
    }

    /// Run what the person did in the window's bar through the effect its
    /// control in the page ran. Returns whether anything was asked.
    pub(super) fn take_window_toolbar_events(&self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if let Some(command) = command(&event) {
                let _ = self.door().send(command);
            }
        }
        asked
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

/// What the page's own control for `event` sent.
fn command(event: &ToolbarEvent) -> Option<UiCommand> {
    Some(match event {
        ToolbarEvent::Back => UiCommand::Dashboard,
        ToolbarEvent::Action { id } => match id.as_str() {
            ADD_PRINTER => UiCommand::Discover,
            PRINT => UiCommand::ChooseDocument,
            REFRESH | CHECK_AGAIN => UiCommand::Refresh,
            NEEDS_YOU => UiCommand::Needs,
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::PrintCentreShell;
    use ui::prelude::TestApp;

    fn bar() -> WindowToolbar {
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    fn action_ids(bar: &WindowToolbar) -> Vec<&str> {
        bar.actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    fn press(app: &mut TestApp<PrintCentreShell>, event: ToolbarEvent) {
        app.harness().press_window_slot(event);
        app.settle();
    }

    fn drawn(node: &ui::headless::InspectionNode, id: &str) -> bool {
        node.test_id.as_deref() == Some(id) || node.children.iter().any(|child| drawn(child, id))
    }

    fn page(app: &mut TestApp<PrintCentreShell>) -> Page {
        app.surface_mut::<PrintCentre>().page.clone()
    }

    /// Wait until the printers show again after the refresh a press
    /// started, with what `asked` says the refresh changed.
    fn refreshed(
        app: &mut TestApp<PrintCentreShell>,
        asked: &str,
        changed: impl Fn(&PrintCentre) -> bool,
    ) {
        tilo_test_env::wait_until(asked, || {
            app.settle();
            let centre = app.surface_mut::<PrintCentre>();
            centre.refresh.is_none() && centre.page == Page::Dashboard && changed(centre)
        });
    }

    /// The printers' actions are the bar's; no page draws a way back, a
    /// title or those actions of its own.
    #[test]
    fn the_window_bar_holds_the_way_back_the_page_and_its_actions() {
        let _serial = crate::surface::serial();
        let mut app = super::super::tests::live_with_studio();
        app.harness().present_in_window_bar();
        app.settle();

        // What wants a person may already be waiting in this process, so the
        // page's own actions come first and the count follows them.
        let declared = bar();
        let ids = action_ids(&declared);
        assert!(
            ids.starts_with(&[PRINT, ADD_PRINTER, REFRESH]),
            "the page's actions, in order: {ids:?}"
        );
        assert_eq!(bar().navigation, None, "the printers are where Back leads");
        assert_eq!(bar().location, None);
        let root = app.harness().inspection().root;
        for own in [ADD_PRINTER, PRINT, REFRESH, "print-centre.back"] {
            assert!(!drawn(&root, own), "the page draws no {own} of its own");
        }

        app.surface_mut::<PrintCentre>()
            .tell(super::super::OperationNotice::success("Printer added"));
        press(&mut app, ToolbarEvent::Action { id: REFRESH.into() });
        refreshed(
            &mut app,
            "Refresh to ask again, as the printers' own refresh did",
            |_| {
                !ui::attention::items()
                    .iter()
                    .any(|item| item.title().contains("Printer"))
            },
        );

        app.effect(&crate::resource::PrintCentreEffect::RunDiagnostics)
            .expect("device help opens");
        app.settle();
        assert_eq!(page(&mut app), Page::Diagnostics);
        let help = bar();
        assert_eq!(
            help.location.map(|location| location.label).as_deref(),
            Some("Device help")
        );
        assert!(help.navigation.is_some_and(|navigation| navigation.back));
        let help_bar = bar();
        let help_ids = action_ids(&help_bar);
        assert!(
            help_ids.starts_with(&[CHECK_AGAIN]),
            "the page's own action first: {help_ids:?}"
        );
        let root = app.harness().inspection().root;
        for own in [CHECK_AGAIN, "print-centre.back"] {
            assert!(!drawn(&root, own), "device help draws no {own} of its own");
        }
        press(
            &mut app,
            ToolbarEvent::Action {
                id: CHECK_AGAIN.into(),
            },
        );
        refreshed(
            &mut app,
            "Check again to ask again and show the printers it found",
            |_| true,
        );

        app.effect(&crate::resource::PrintCentreEffect::OpenHistory)
            .expect("the history opens");
        app.settle();
        assert_eq!(
            bar().location.map(|location| location.label).as_deref(),
            Some("Print history")
        );
        press(&mut app, ToolbarEvent::Back);
        assert_eq!(
            page(&mut app),
            Page::Dashboard,
            "Back leads to the printers"
        );
    }

    /// Add printer and Print a document send what the page's own buttons
    /// sent: finding printers and choosing a document.
    #[test]
    fn the_bars_actions_send_the_pages_own_commands() {
        let action = |id: &str| ToolbarEvent::Action { id: id.into() };
        assert!(matches!(
            command(&action(ADD_PRINTER)),
            Some(UiCommand::Discover)
        ));
        assert!(matches!(
            command(&action(PRINT)),
            Some(UiCommand::ChooseDocument)
        ));
        assert!(matches!(
            command(&ToolbarEvent::Back),
            Some(UiCommand::Dashboard)
        ));
    }
}
