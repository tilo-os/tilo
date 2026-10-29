//! The reader's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the bar's sidebar toggle shows and hides the
//! pages, its location names the open document, and its actions carry the
//! annotation tools, the marker colour, the review panel, zoom, page
//! stepping, Open and Save. Each slot runs the reader's published
//! [`PdfEffect`] through the same handler the palette and agents reach. The window's search finds in
//! the pages (the reader is its [`ui::widget::Findable`]) and its History
//! control opens the record's History; the reader draws no toolbar row.

use ui::icons::Icon;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{FitMode, PdfApp, swatch_label, tool_icon};
use crate::actions::PdfEffect;
use crate::model::{Swatch, Tool};

impl PdfApp {
    /// The window bar's slots for what the reader shows now. Open stands in
    /// the bar's menu whether or not a document is open, so it is always in
    /// the same place; the rest act on the open document.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let bar = &self.bar;
        let open = bar
            .entry(
                Icon::FolderOpen.name(),
                "Open PDF",
                PdfEffect::OpenPdf { path: None },
            )
            .enabled(!self.opening())
            .in_menu();
        if !self.has_document {
            return WindowToolbar {
                actions: vec![open],
                ..WindowToolbar::default()
            };
        }
        let tools = Tool::ALL
            .into_iter()
            .map(|tool| {
                bar.entry(
                    tool_icon(tool).name(),
                    tool.label(),
                    PdfEffect::UseTool { tool },
                )
                .on(tool == self.tool)
            })
            .collect();
        let colours = Swatch::ALL
            .into_iter()
            .map(|colour| {
                let effect = PdfEffect::SetMarkerColour { colour };
                bar.entry(Icon::Highlighter.name(), swatch_label(colour), effect)
                    .on(colour == self.swatch)
            })
            .collect();
        let review = if self.show_sidebar {
            "Hide review panel"
        } else {
            "Show review panel"
        };
        let zoom = vec![
            bar.entry(Icon::Plus.name(), "Zoom in", PdfEffect::ZoomIn),
            bar.entry(Icon::Minus.name(), "Zoom out", PdfEffect::ZoomOut),
            bar.entry(Icon::File.name(), "Fit page", PdfEffect::FitPage)
                .on(self.fit == FitMode::Page),
            bar.entry(Icon::Columns2.name(), "Fit width", PdfEffect::FitWidth)
                .on(self.fit == FitMode::Width),
        ];
        // A menu button's own id is never sent: its items are.
        WindowToolbar {
            sidebar: Some(self.show_rail),
            location: Some(ToolbarLocation {
                label: self.doc_name.clone(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new("pdf.tools", tool_icon(self.tool).name(), "Tools")
                    .with_items(tools),
                ToolbarAction::new("pdf.marker-colour", Icon::Palette.name(), "Marker colour")
                    .with_items(colours),
                bar.entry(Icon::PanelRight.name(), review, PdfEffect::ToggleSidebar)
                    .on(self.show_sidebar),
                ToolbarAction::new("pdf.zoom", Icon::Plus.name(), "Zoom")
                    .with_items(zoom)
                    .in_menu(),
                bar.entry(
                    Icon::ChevronLeft.name(),
                    "Previous page",
                    PdfEffect::PreviousPage,
                )
                .enabled(self.current_page > 1)
                .in_menu(),
                bar.entry(Icon::ChevronRight.name(), "Next page", PdfEffect::NextPage)
                    .enabled(self.current_page < self.doc.page_count())
                    .in_menu(),
                open,
                bar.entry(
                    Icon::Download.name(),
                    "Save annotated PDF",
                    PdfEffect::SaveAs { path: None },
                )
                .enabled(self.raster_active())
                .in_menu(),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the reader shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        self.take_note_action();
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar: the published
    /// effect its slot stands for.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        let effect = match (event, self.bar.pressed(event)) {
            (_, Some(effect @ PdfEffect::OpenPdf { .. })) => effect,
            _ if !self.has_document => return,
            (ToolbarEvent::ToggleSidebar, _) => PdfEffect::TogglePages,
            (_, Some(effect)) => effect,
            _ => return,
        };
        self.run_effect(&effect);
    }

    /// Run `effect` as the person's own press: the view effects through
    /// [`Self::show`], Open and Save through the pickers, as
    /// [`Self::answer_effect`] runs them for the palette and agents.
    pub(super) fn run_effect(&mut self, effect: &PdfEffect) {
        if self.show(effect) {
            return;
        }
        match effect {
            PdfEffect::OpenPdf { path: None } if !self.opening() => {
                self.request_open_via_portal();
            }
            PdfEffect::SaveAs { path: None } => self.request_save_via_portal(),
            _ => {}
        }
    }
}

#[cfg(test)]
impl PdfApp {
    /// The bar's entry named `name`, a menu's item too.
    pub(crate) fn bar_entry(&self, name: &str) -> ToolbarAction {
        let toolbar = self.window_toolbar();
        toolbar
            .actions
            .iter()
            .flat_map(|action| std::iter::once(action).chain(&action.items))
            .find(|action| action.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("the bar has {name}: {toolbar:?}"))
    }

    /// Press the bar's entry named `name`, as the desktop hands it over.
    pub(crate) fn press_bar(&mut self, name: &str) {
        let id = self.bar_entry(name).id;
        self.run_window_bar_event(&ToolbarEvent::Action { id });
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
