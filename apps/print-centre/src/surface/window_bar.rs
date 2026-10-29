//! Print Centre's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): Back to the printers from every other page, the
//! page shown as the location, and the page's actions: Add printer, Print a
//! document and Refresh with the printers, Check again in device help. Each
//! runs the published effect its control in the page ran.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

use super::{Page, PrintCentre, UiCommand, display_name};
use crate::resource::PrintCentreEffect;

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
        if let Some(bar) = &self.bar {
            let entry = |effect: PrintCentreEffect, icon: &str, name: &str| ToolbarAction {
                icon: icon.into(),
                name: name.into(),
                ..bar.effect(&effect)
            };
            match self.page {
                Page::Dashboard => {
                    // Adding a printer and printing both need printers: with
                    // none, the page's own empty state offers to find one.
                    if has_printers {
                        toolbar.actions.push(entry(
                            PrintCentreEffect::PrintDocument,
                            "printer",
                            "Print a document",
                        ));
                        toolbar.actions.push(entry(
                            PrintCentreEffect::FindPrinters,
                            "plus",
                            "Add printer",
                        ));
                    }
                    toolbar
                        .actions
                        .push(entry(PrintCentreEffect::Refresh, "refresh", "Refresh"));
                }
                Page::Diagnostics => {
                    toolbar.actions.push(entry(
                        PrintCentreEffect::Refresh,
                        "refresh",
                        "Check again",
                    ));
                }
                _ => {}
            }
            if let Some(label) = ui::attention::pill() {
                toolbar
                    .actions
                    .push(entry(PrintCentreEffect::OpenNeeds, "bell", &label).labelled());
            }
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

    /// Run what the person did in the window's bar: its entries run their
    /// effects, Back leads to the printers. Returns whether anything was
    /// asked.
    pub(super) fn take_window_toolbar_events(&self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.bar.as_ref().is_some_and(|bar| bar.press(&event)) {
                continue;
            }
            if event == ToolbarEvent::Back {
                let _ = self.door().send(UiCommand::Dashboard);
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
    use crate::resource::PrintCentreShell;
    use ui::prelude::TestApp;

    fn bar() -> WindowToolbar {
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    fn action_names(bar: &WindowToolbar) -> Vec<&str> {
        bar.actions
            .iter()
            .map(|action| action.name.as_str())
            .collect()
    }

    fn press(app: &mut TestApp<PrintCentreShell>, name: &str) {
        app.harness().press_window_action(name).unwrap();
        app.settle();
    }

    fn page(app: &mut TestApp<PrintCentreShell>) -> Page {
        crate::surface::tests::centre(app).borrow().page.clone()
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
            let shared = crate::surface::tests::centre(app);
            let centre = shared.borrow();
            centre.refresh.is_none() && centre.page == Page::Dashboard && changed(&centre)
        });
    }

    /// The bar holds the way back, the page shown and the page's actions,
    /// which run what the page's own controls ran.
    #[test]
    fn the_window_bar_holds_the_way_back_the_page_and_its_actions() {
        let _serial = crate::surface::serial();
        let mut app = super::super::tests::live_with_studio();
        app.harness().present_in_window_bar();
        app.settle();

        // What wants a person may already be waiting in this process, so the
        // page's own actions come first and the count follows them.
        let declared = bar();
        let names = action_names(&declared);
        assert!(
            names.starts_with(&["Print a document", "Add printer", "Refresh"]),
            "the page's actions, in order: {names:?}"
        );
        assert_eq!(bar().navigation, None, "the printers are where Back leads");
        assert_eq!(bar().location, None);

        crate::surface::tests::centre(&mut app)
            .borrow_mut()
            .tell(super::super::OperationNotice::success("Printer added"));
        press(&mut app, "Refresh");
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
        let help_names = action_names(&help_bar);
        assert!(
            help_names.starts_with(&["Check again"]),
            "the page's own action first: {help_names:?}"
        );
        press(&mut app, "Check again");
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
        app.harness().press_window_slot(ToolbarEvent::Back);
        app.settle();
        assert_eq!(
            page(&mut app),
            Page::Dashboard,
            "Back leads to the printers"
        );
    }
}
