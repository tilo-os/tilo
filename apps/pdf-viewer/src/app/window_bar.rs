//! The reader's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the bar's sidebar toggle shows and hides the
//! pages, its location names the open document, and its actions carry the
//! annotation tools, the marker colour, the review panel, zoom, page
//! stepping, Open and Save. Each slot runs the reader's published [`PdfEffect`] through the
//! same handler the palette and agents reach. The window's search finds in
//! the pages (the reader is its [`ui::widget::Findable`]) and its History
//! control opens the record's History; the reader draws no toolbar row.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{FitMode, PdfApp, swatch_label, tool_icon};
use crate::actions::PdfEffect;
use crate::model::{Swatch, Tool};

/// The tools' menu button (never sent: its items are).
pub(crate) const TOOLS: &str = "pdf.tools";
/// The marker colours' menu button (never sent: its items are).
pub(crate) const MARKER_COLOUR: &str = "pdf.marker-colour";
/// The action that shows and hides the review panel.
pub(crate) const REVIEW_PANEL: &str = "pdf.review-panel";
/// The zoom menu button (never sent: its items are).
pub(crate) const ZOOM: &str = "pdf.zoom";
pub(crate) const ZOOM_IN: &str = "pdf.zoom-in";
pub(crate) const ZOOM_OUT: &str = "pdf.zoom-out";
pub(crate) const FIT_PAGE: &str = "pdf.fit-page";
pub(crate) const FIT_WIDTH: &str = "pdf.fit-width";
pub(crate) const PREVIOUS_PAGE: &str = "pdf.previous-page";
pub(crate) const NEXT_PAGE: &str = "pdf.next-page";
pub(crate) const OPEN: &str = "pdf.open";
pub(crate) const SAVE: &str = "pdf.save";

/// The bar id of `tool`'s item.
pub(crate) fn tool_id(tool: Tool) -> String {
    format!("pdf.tool:{}", tool_key(tool))
}

/// The bar id of `swatch`'s item.
pub(crate) fn colour_id(swatch: Swatch) -> String {
    format!("pdf.colour:{}", swatch.key())
}

/// How `tool` is spelled in its published effect.
fn tool_key(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "select",
        Tool::Highlight => "highlight",
        Tool::Ink => "ink",
        Tool::Note => "note",
        Tool::Text => "text",
        Tool::Sign => "sign",
    }
}

/// The published effect a bar slot runs.
fn bar_effect(id: &str) -> Option<PdfEffect> {
    let effect = match id {
        REVIEW_PANEL => PdfEffect::ToggleSidebar,
        ZOOM_IN => PdfEffect::ZoomIn,
        ZOOM_OUT => PdfEffect::ZoomOut,
        FIT_PAGE => PdfEffect::FitPage,
        FIT_WIDTH => PdfEffect::FitWidth,
        PREVIOUS_PAGE => PdfEffect::PreviousPage,
        NEXT_PAGE => PdfEffect::NextPage,
        OPEN => PdfEffect::OpenPdf { path: None },
        SAVE => PdfEffect::SaveAs { path: None },
        _ => {
            if let Some(tool) = Tool::ALL.into_iter().find(|tool| tool_id(*tool) == id) {
                PdfEffect::UseTool { tool }
            } else {
                let colour = Swatch::ALL
                    .into_iter()
                    .find(|swatch| colour_id(*swatch) == id)?;
                PdfEffect::SetMarkerColour { colour }
            }
        }
    };
    Some(effect)
}

impl PdfApp {
    /// The window bar's slots for what the reader shows now. Open stands in
    /// the bar's menu whether or not a document is open, so it is always in
    /// the same place; the rest act on the open document.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let open = ToolbarAction::new(OPEN, ui::icons::Icon::FolderOpen.name(), "Open PDF")
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
                ToolbarAction::new(tool_id(tool), tool_icon(tool).name(), tool.label())
                    .on(tool == self.tool)
            })
            .collect();
        let colours = Swatch::ALL
            .into_iter()
            .map(|swatch| {
                ToolbarAction::new(
                    colour_id(swatch),
                    ui::icons::Icon::Highlighter.name(),
                    swatch_label(swatch),
                )
                .on(swatch == self.swatch)
            })
            .collect();
        let review = if self.show_sidebar {
            "Hide review panel"
        } else {
            "Show review panel"
        };
        let zoom = vec![
            ToolbarAction::new(ZOOM_IN, ui::icons::Icon::Plus.name(), "Zoom in"),
            ToolbarAction::new(ZOOM_OUT, ui::icons::Icon::Minus.name(), "Zoom out"),
            ToolbarAction::new(FIT_PAGE, ui::icons::Icon::File.name(), "Fit page")
                .on(self.fit == FitMode::Page),
            ToolbarAction::new(FIT_WIDTH, ui::icons::Icon::Columns2.name(), "Fit width")
                .on(self.fit == FitMode::Width),
        ];
        WindowToolbar {
            sidebar: Some(self.show_rail),
            location: Some(ToolbarLocation {
                label: self.doc_name.clone(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new(TOOLS, tool_icon(self.tool).name(), "Tools").with_items(tools),
                ToolbarAction::new(
                    MARKER_COLOUR,
                    ui::icons::Icon::Palette.name(),
                    "Marker colour",
                )
                .with_items(colours),
                ToolbarAction::new(REVIEW_PANEL, ui::icons::Icon::PanelRight.name(), review)
                    .on(self.show_sidebar),
                ToolbarAction::new(ZOOM, ui::icons::Icon::Plus.name(), "Zoom")
                    .with_items(zoom)
                    .in_menu(),
                ToolbarAction::new(
                    PREVIOUS_PAGE,
                    ui::icons::Icon::ChevronLeft.name(),
                    "Previous page",
                )
                .enabled(self.current_page > 1)
                .in_menu(),
                ToolbarAction::new(NEXT_PAGE, ui::icons::Icon::ChevronRight.name(), "Next page")
                    .enabled(self.current_page < self.doc.page_count())
                    .in_menu(),
                open,
                ToolbarAction::new(SAVE, ui::icons::Icon::Download.name(), "Save annotated PDF")
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
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar: the published
    /// effect its slot stands for.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        let effect = match event {
            ToolbarEvent::Action { id } if id == OPEN => PdfEffect::OpenPdf { path: None },
            _ if !self.has_document => return,
            ToolbarEvent::ToggleSidebar => PdfEffect::TogglePages,
            ToolbarEvent::Action { id } => {
                let Some(effect) = bar_effect(id) else {
                    return;
                };
                effect
            }
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
#[path = "window_bar_tests.rs"]
mod tests;
