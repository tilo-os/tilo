//! The document scanner: scan settings on the leading side (or on top in a
//! narrow window), the selected page beside them. Which page it is and what
//! can be done with it are the window bar's ([`super::window_bar`]).

use std::rc::Rc;

use taffy::NodeId;
use ui::LocalState;
use ui::TextRole;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, Dropdown, DropdownFace, EmptyState,
    EmptyStateContent, Image, ImageFit, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::hud::HudMetrics;
use super::settings_sheet::ActionQueue;
use super::{Action, CameraApp, DocumentChoice, DocumentJobKind, OcrLanguage};
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// The visible choices of one document setting, in index order.
pub(super) fn choice_labels(choice: DocumentChoice) -> &'static [&'static str] {
    match choice {
        // Until a scanner answers the field says so, never its raw index.
        DocumentChoice::Scanner => &[NO_SCANNER],
        DocumentChoice::Resolution => &["150 DPI", "300 DPI", "600 DPI"],
        DocumentChoice::Color => &["Color", "Grayscale", "Black and white"],
        DocumentChoice::Source => &["Flatbed", "Feeder", "Duplex feeder"],
        DocumentChoice::Language => &["English", "German", "French", "Spanish"],
    }
}

/// The scanner field while no scanner is found.
const NO_SCANNER: &str = "No scanner found";

const CHOICES: [DocumentChoice; 5] = [
    DocumentChoice::Scanner,
    DocumentChoice::Resolution,
    DocumentChoice::Color,
    DocumentChoice::Source,
    DocumentChoice::Language,
];

fn caption(choice: DocumentChoice) -> &'static str {
    match choice {
        DocumentChoice::Scanner => "Scanner",
        DocumentChoice::Resolution => "Resolution",
        DocumentChoice::Color => "Color",
        DocumentChoice::Source => "Paper source",
        DocumentChoice::Language => "Text language",
    }
}

fn indexed(labels: impl IntoIterator<Item = String>) -> Vec<(String, String)> {
    labels
        .into_iter()
        .enumerate()
        .map(|(i, label)| (i.to_string(), label))
        .collect()
}

/// The scan settings as standard dropdowns the app hosts in its own layout.
pub(super) struct DocumentControls {
    pub(super) fields: Vec<Dropdown>,
    states: Vec<LocalState<String>>,
    pub(super) nodes: Vec<NodeId>,
    scanners: Vec<String>,
}

impl DocumentControls {
    pub(super) fn new(queue: &ActionQueue) -> Self {
        let mut fields = Vec::new();
        let mut states = Vec::new();
        for choice in CHOICES {
            let state = LocalState::new("0".to_owned());
            let queue = Rc::clone(queue);
            let mut dropdown = Dropdown::from_state(Vec::new(), &state)
                .caption(caption(choice))
                .on_change(move |value| {
                    if let Ok(index) = value.parse() {
                        queue
                            .borrow_mut()
                            .push(Action::SetDocumentChoice(choice, index));
                    }
                });
            dropdown.set_value_options(indexed(
                choice_labels(choice)
                    .iter()
                    .map(|label| (*label).to_owned()),
            ));
            fields.push(dropdown);
            states.push(state);
        }
        Self {
            fields,
            states,
            nodes: Vec::new(),
            scanners: Vec::new(),
        }
    }
}

/// Where the document screen's parts sit for one window size.
pub(super) struct DocumentLayout {
    pub fields: [Rect; 5],
    pub scan: Rect,
    pub page: Rect,
}

impl CameraApp {
    /// Show the current scanners and settings in the dropdowns.
    pub(super) fn present_document_controls(&mut self) {
        let scanners: Vec<String> = self
            .scanner_devices
            .iter()
            .map(|scanner| scanner.description.clone())
            .collect();
        let controls = &mut self.document_controls;
        if controls.scanners != scanners {
            if let Some(field) = controls.fields.first_mut() {
                if scanners.is_empty() {
                    field.set_value_options(indexed([NO_SCANNER.to_owned()]));
                } else {
                    field.set_value_options(indexed(scanners.iter().cloned()));
                }
            }
            controls.scanners = scanners;
        }
        let scan = &self.document_settings.scan;
        let selected = [
            self.selected_scanner
                .as_ref()
                .and_then(|selected| {
                    self.scanner_devices
                        .iter()
                        .position(|scanner| scanner.device == *selected)
                })
                .unwrap_or(0),
            match scan.resolution_dpi {
                150 => 0,
                600 => 2,
                _ => 1,
            },
            match scan.color {
                tilo_capture::scanner::ScanColor::Color => 0,
                tilo_capture::scanner::ScanColor::Grayscale => 1,
                tilo_capture::scanner::ScanColor::LineArt => 2,
            },
            match scan.source {
                tilo_capture::scanner::ScanSource::Flatbed => 0,
                tilo_capture::scanner::ScanSource::Feeder => 1,
                tilo_capture::scanner::ScanSource::FeederDuplex => 2,
            },
            match self.document_settings.ocr_language {
                OcrLanguage::English => 0,
                OcrLanguage::German => 1,
                OcrLanguage::French => 2,
                OcrLanguage::Spanish => 3,
            },
        ];
        let busy = self.document_job.is_some();
        for ((state, index), field) in controls
            .states
            .iter()
            .zip(selected)
            .zip(controls.fields.iter_mut())
        {
            let value = index.to_string();
            if state.get() != value {
                state.set(value);
            }
            field.set_enabled(!busy);
        }
        if let Some(scanner) = controls.fields.first_mut() {
            scanner.set_enabled(!busy && !self.scanner_devices.is_empty());
        }
    }

    /// Place the settings, the scan button and the page for `full`.
    pub(super) fn document_layout(full: Rect, theme: &Theme) -> DocumentLayout {
        let m = HudMetrics::of(theme);
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let gutter = f64::from(spacing.s6);
        let gap = f64::from(spacing.s4);
        // A captioned dropdown, its caption stacked above its field.
        let field_h = DropdownFace::new(Rect::ZERO, "")
            .caption(caption(CHOICES[0]))
            .outer_height(theme);
        let control = ControlSize::Regular.height(theme);
        let content = Rect::new(
            full.x0 + gutter,
            full.y0 + m.edge,
            full.x1 - gutter,
            full.y1 - m.edge,
        );
        let column = f64::from(metrics.panel_width_utility);
        let wide = content.width() >= column + gutter + f64::from(metrics.detail_min_width);
        let mut fields = [Rect::ZERO; 5];
        if wide {
            // The settings and the page stay one composition, centred,
            // with the page no wider than a sheet.
            let page_w =
                (content.width() - column - gutter).min(f64::from(metrics.panel_width_sheet));
            let x0 = content.center().x - (column + gutter + page_w) / 2.0;
            let content = Rect::new(x0, content.y0, x0 + column + gutter + page_w, content.y1);
            let mut y = content.y0;
            for field in &mut fields {
                *field = Rect::new(content.x0, y, content.x0 + column, y + field_h);
                y += field_h + gap;
            }
            let scan_top = y - gap + gutter;
            let scan = Rect::new(
                content.x0,
                scan_top,
                content.x0 + column,
                scan_top + control,
            );
            let page = Rect::new(
                content.x0 + column + gutter,
                content.y0,
                content.x1,
                content.y1,
            );
            DocumentLayout { fields, scan, page }
        } else {
            let half = (content.width() - gap) / 2.0;
            let row = |y: f64| {
                (
                    Rect::new(content.x0, y, content.x0 + half, y + field_h),
                    Rect::new(content.x1 - half, y, content.x1, y + field_h),
                )
            };
            fields[0] = Rect::new(content.x0, content.y0, content.x1, content.y0 + field_h);
            let second = content.y0 + field_h + gap;
            (fields[1], fields[2]) = row(second);
            let third = second + field_h + gap;
            (fields[3], fields[4]) = row(third);
            let scan_top = third + field_h + gap;
            let scan = Rect::new(content.x0, scan_top, content.x1, scan_top + control);
            let page = Rect::new(content.x0, scan.y1 + gutter, content.x1, content.y1);
            DocumentLayout { fields, scan, page }
        }
    }

    pub(super) fn draw_document_scanner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        full: Rect,
    ) {
        let layout = Self::document_layout(full, theme);
        for (field, rect) in self.document_controls.fields.iter_mut().zip(layout.fields) {
            field.apply_layout_external(rect);
            field.draw(scene, t, tr, theme, presence);
        }
        let busy = self.document_job.is_some();
        let can_scan = self.selected_scanner.is_some() && !busy;
        let label = if self.document_job_kind == Some(DocumentJobKind::Capture) {
            "Scanning…"
        } else {
            "Scan page"
        };
        let scan = ButtonFace::new(layout.scan, label).role(ButtonRole::Primary);
        let hit = scan.hit_rect(theme);
        let hovered = scan.contains(theme, self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &scan.hovered(hovered).disabled(!can_scan),
        );
        if can_scan {
            self.push_region(hit, Action::CaptureDocument);
        }

        if self.document_scan.pages.is_empty() {
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let content = EmptyStateContent {
                icon: Some(Icon::FileText),
                message: Some("Scanned pages appear here."),
                ..EmptyStateContent::new("No pages yet")
            };
            let _ = EmptyState::paint(&mut ctx, layout.page, &content, Point::new(-1.0, -1.0));
        } else {
            self.draw_document_page(scene, t, tr, theme, layout.page);
        }
    }

    /// The selected page: its text status, the page itself, and while a
    /// crop is in progress the crop's Cancel and Apply under it.
    fn draw_document_page(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let status = self.draw_page_status(scene, t, tr, theme, area);
        let footer_y = if self.document_crop_editor.is_some() {
            self.draw_crop_actions(scene, t, tr, theme, area) - f64::from(theme.tokens.spacing.s4)
        } else {
            area.y1
        };
        let room = Rect::new(
            area.x0,
            status.y1 + f64::from(theme.tokens.spacing.s4),
            area.x1,
            footer_y,
        );
        self.draw_page_image(scene, t, theme, room);
    }

    /// Whether the page's text is recognized, at the top of `area`. Returns
    /// the band it takes.
    fn draw_page_status(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> Rect {
        let colors = &theme.tokens.colors;
        let caption_h = f64::from(theme.text(TextRole::Caption).line_height_px());
        let band = Rect::new(area.x0, area.y0, area.x1, area.y0 + caption_h);
        let stale = self
            .document_scan
            .pages
            .get(self.document_scan.selected)
            .and_then(|page| page.ocr.as_ref())
            .is_some_and(|document| {
                document.language != self.document_settings.ocr_language.code()
            });
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let room = ui::num::f32_of_f64(band.width().max(1.0));
        let status = ctx.text.elide_to_width_style(
            &self.document_text_status(),
            room,
            &theme.text(TextRole::Caption),
        );
        ctx.text_role(
            &status,
            TextRole::Caption,
            if stale {
                colors.semantic.warning
            } else {
                colors.text_dim
            },
            Point::new(band.x0, band.y0),
            None,
        );
        band
    }

    /// The crop's Cancel and Apply, centred under the page. Returns the top
    /// of their row.
    fn draw_crop_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> f64 {
        let control = ControlSize::Regular.height(theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let actions = [
            (
                "Cancel",
                Icon::X,
                Action::CancelDocumentCrop,
                ButtonRole::Secondary,
            ),
            (
                "Apply crop",
                Icon::Check,
                Action::ApplyDocumentCrop,
                ButtonRole::Primary,
            ),
        ];
        let widths: Vec<f64> = actions
            .iter()
            .map(|(label, icon, _, role)| {
                ButtonFace::new(Rect::ZERO, label)
                    .icon(*icon)
                    .role(*role)
                    .intrinsic_width(tr, theme)
            })
            .collect();
        let total = widths.iter().sum::<f64>() + gap * (widths.len() as f64 - 1.0);
        let top = area.y1 - control;
        let mut x = area.center().x - total / 2.0;
        for ((label, icon, action, role), w) in actions.into_iter().zip(widths) {
            let rect = Rect::new(x, top, x + w, top + control);
            let face = ButtonFace::new(rect, label).icon(icon).role(role);
            let hit = face.hit_rect(theme);
            let hovered = face.contains(theme, self.last_mouse);
            Button::paint(scene, t, tr, theme, &face.hovered(hovered));
            self.push_region(hit, action);
            x = rect.x1 + gap;
        }
        top
    }

    /// The page, as large as `room` allows, or the crop editor over it.
    fn draw_page_image(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, room: Rect) {
        let colors = theme.tokens.colors.clone();
        if let Some(editor) = self.document_crop_editor.as_mut() {
            editor.paint_at(scene, t, theme, room);
            return;
        }
        let selected = self.document_scan.selected;
        let key = self
            .document_scan
            .pages
            .get(selected)
            .map(|page| (page.crop, page.rotation));
        let current =
            self.document_preview
                .as_ref()
                .is_some_and(|(index, crop, rotation, _, _)| {
                    *index == selected && Some((*crop, *rotation)) == key
                });
        if !current && let Some(page) = self.document_scan.pages.get(selected) {
            let rendered = page.rendered().to_rgba8();
            let (width, height) = rendered.dimensions();
            self.document_preview = Some((
                selected,
                page.crop,
                page.rotation,
                Image::from_rgba(std::sync::Arc::from(rendered.into_raw()), width, height)
                    .with_fit(ImageFit::Contain),
                (width, height),
            ));
        }
        if let Some((_, _, _, image, (width, height))) = self.document_preview.as_mut() {
            let aspect = f64::from(*width) / f64::from((*height).max(1));
            let (w, h) = if room.width() / room.height().max(1.0) > aspect {
                (room.height() * aspect, room.height())
            } else {
                (room.width(), room.width() / aspect.max(0.01))
            };
            let paper = Rect::from_center_size(room.center(), (w, h));
            image.paint_at(scene, t, paper);
            ui::paint::stroke_rect(
                scene,
                t,
                paper,
                ui::surface::edge_on(colors.surfaces.content),
                f64::from(theme.tokens.metrics.chrome_rule_px),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A staged page was scanned with the sample's scanner, and the
    /// settings name that scanner.
    #[test]
    fn a_staged_page_names_the_scanner_it_came_from() {
        let mut app = CameraApp::sample();
        app.stage_scene(Some("document-page"));
        assert_eq!(app.document_controls.scanners, ["Document scanner"]);
    }
}
