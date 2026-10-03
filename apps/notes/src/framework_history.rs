//! Notebook History presentation. Only frozen bytes and local navigation enter here.
use crate::{MainPanel, PageId, Workspace, doc::NotesDoc};
use automerge::{AutoCommit, ObjType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
    restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType},
};
use ui::surface::{edge_on, edge_outline};
use ui::{TextRenderer, Theme, Widget};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect, RoundedRect, Stroke},
    peniko::Fill,
};

/// Page-tree geometry for the current text scale, every value a theme token
/// (HIG §7b, §9.6): the pane is the theme's sidebar width and docks beside
/// the page from the compact breakpoint up; the list starts a section pad
/// (`s4`) down, a selected pill keeps `s2` from the pane sides and half an
/// `s1` from its neighbours, the label sits `s3` inside the pill and each
/// nesting level indents `s4`. A row is the theme's selector height or its
/// measured label line plus `s2`, whole pixels at every scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TreeRowMetrics {
    /// Window width from which the tree docks beside the page.
    min_panel_w: f64,
    /// Width of the tree pane.
    pane_w: f64,
    /// Base size of a row label (the renderer applies the text scale).
    text: f32,
    top: f64,
    row_h: f64,
    line_h: f64,
    pill_inset_x: f64,
    pill_gap: f64,
    label_pad_x: f64,
    indent: f64,
}

impl TreeRowMetrics {
    fn new(theme: &Theme, text_scale: f32) -> Self {
        let sp = &theme.tokens.spacing;
        let mx = &theme.tokens.metrics;
        let text = theme.tokens.typography.scale.base;
        let line_h = ui::text_metrics::line_height_px(text, text_scale);
        Self {
            min_panel_w: f64::from(mx.compact_breakpoint),
            pane_w: f64::from(mx.sidebar_width),
            text,
            top: f64::from(sp.s4),
            row_h: ui::text_metrics::at_least_whole(
                f64::from(mx.selector_height),
                line_h + f64::from(sp.s2),
            ),
            line_h,
            pill_inset_x: f64::from(sp.s2),
            pill_gap: f64::from(sp.s1),
            label_pad_x: f64::from(sp.s3),
            indent: f64::from(sp.s4),
        }
    }

    /// The tree pane, or `None` when the window is too narrow to dock it.
    fn pane(&self, bounds: Rect) -> Option<Rect> {
        (bounds.width() >= self.min_panel_w)
            .then(|| Rect::new(bounds.x0, bounds.y0, bounds.x0 + self.pane_w, bounds.y1))
    }

    /// The full-width row band of the `index`-th visible page.
    fn row(&self, pane: Rect, index: usize) -> Rect {
        let y0 = pane.y0 + self.top + index as f64 * self.row_h;
        Rect::new(pane.x0, y0, pane.x1, y0 + self.row_h)
    }

    /// The selected row's pill inside its band.
    fn pill(&self, row: Rect) -> Rect {
        Rect::new(
            row.x0 + self.pill_inset_x,
            row.y0 + self.pill_gap / 2.0,
            row.x1 - self.pill_inset_x,
            row.y1 - self.pill_gap / 2.0,
        )
    }

    /// Where a label of nesting `depth` starts, and the top of its line box
    /// centred on the row.
    fn label_origin(&self, row: Rect, depth: usize) -> Point {
        Point::new(
            row.x0 + self.pill_inset_x + self.label_pad_x + depth as f64 * self.indent,
            row.y0 + (row.height() - self.line_h) / 2.0,
        )
    }
}

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.notes",
    scopes: &[HistoryScopeDefinition {
        key: "notebook",
        title: "Notebook",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "notes.notebook",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "pages",
            required: true,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "tags",
            required: true,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "tag_assignments",
            required: true,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
    ],
    selectors: &[],
    // Schema and import provenance describe current custody, not user content.
    // Retained legacy workspace bytes never become the canonical restore target.
    preserved_roots: &["notes_schema_version", "notes_import_v1", "workspace_json"],
    preserved: &[],
    validate: crate::notebook::validate,
    validate_result: crate::notebook::validate,
};

fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    fn visit(
        workspace: &Workspace,
        parent: Option<PageId>,
        depth: usize,
        out: &mut Vec<(PageId, usize)>,
    ) {
        for page in workspace.children_of(parent) {
            out.push((page.id, depth));
            visit(workspace, Some(page.id), depth + 1, out);
        }
    }
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let workspace = crate::notebook::project(&doc, None)?;
    let mut pages = Vec::new();
    visit(&workspace, None, 0, &mut pages);
    let selected = input
        .initial_view
        .page
        .as_deref()
        .and_then(|id| id.parse().ok())
        .filter(|id| workspace.get(*id).is_some())
        .or_else(|| pages.first().map(|(id, _)| *id));
    let mut result = NotebookPreview {
        doc,
        workspace,
        pages,
        panel: MainPanel::new(),
        bounds: Rect::ZERO,
        view: input.initial_view,
    };
    result.select(selected)?;
    Ok(Box::new(result))
}

// MainPanel owns only its local block/title presentation. No NotesApp, search
// provider, live Widget tree, HostContext, callbacks, savers or service handle.
// Its Widget event/sync/semantic interfaces are deliberately never forwarded.
struct NotebookPreview {
    doc: AutoCommit,
    workspace: Workspace,
    pages: Vec<(PageId, usize)>,
    panel: MainPanel,
    bounds: Rect,
    view: PreviewView,
}
impl NotebookPreview {
    fn select(&mut self, selected: Option<PageId>) -> Result<(), String> {
        self.workspace.set_selected(selected);
        self.panel = self.panel.project_canonical(&self.doc, &self.workspace)?;
        self.panel
            .finish_local_focus(crate::local_view::NotesFocus::None);
        self.panel.set_tags(
            selected
                .map(|id| {
                    self.workspace
                        .tags_for_page(id)
                        .into_iter()
                        .map(|tag| crate::main_panel::TagChip {
                            id: tag.id,
                            name: tag.name.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        self.view.page = selected.map(|id| id.to_string());
        self.panel.restore_scroll(self.view.scroll.1);
        Ok(())
    }
}
impl HistoryPreview for NotebookPreview {
    fn layout(
        &mut self,
        bounds: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        let tree_width = TreeRowMetrics::new(theme, text.text_scale())
            .pane(bounds)
            .map_or(0.0, |pane| pane.width());
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let root = self.panel.build_layout(&mut tree, theme, &mut nodes);
        let mut style = tree.style(root).map_err(|e| e.to_string())?.clone();
        style.size = taffy::Size {
            width: taffy::Dimension::length((bounds.width() - tree_width) as f32),
            height: taffy::Dimension::length(bounds.height() as f32),
        };
        tree.set_style(root, style).map_err(|e| e.to_string())?;
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: maps live only in this call; the exclusively borrowed panel
        // is not moved or rebuilt until both maps are dropped.
        unsafe {
            bindings.rebuild(&self.panel, &tree, root, &nodes);
            ui::widget::compute_widget_layout(
                &mut tree,
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite((bounds.width() - tree_width) as f32),
                    height: taffy::AvailableSpace::Definite(bounds.height() as f32),
                },
                &nodes,
                &bindings,
                text,
            )
            .map_err(|e| e.to_string())?;
        }
        self.panel
            .apply_layout(&tree, root, Point::new(bounds.x0 + tree_width, bounds.y0));
        self.view.scroll.1 = self.panel.local_scroll();
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.bg_primary,
            None,
            &self.bounds,
        );
        let m = TreeRowMetrics::new(theme, text.text_scale());
        if let Some(pane) = m.pane(self.bounds) {
            let colors = &theme.tokens.colors;
            // The page tree is its own raised pane beside the page, with the
            // edge derived against that pane (HIG §9.5) — a bare list of
            // titles on the page ground read as part of the page.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &pane,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                edge_on(colors.surfaces.raised),
                None,
                &Rect::new(pane.x1 - 1.0, pane.y0, pane.x1, pane.y1),
            );
            let radius = f64::from(theme.tokens.radii.md);
            for (index, (id, depth)) in self.pages.iter().enumerate() {
                let Some(page) = self.workspace.get(*id) else {
                    continue;
                };
                let row = m.row(pane, index);
                if row.y0 >= pane.y1 {
                    break;
                }
                if self.workspace.selected() == Some(*id) {
                    // The shown page is a lift (HIG §9.3), one `s1` clear
                    // of the rows above and below it.
                    let pill = m.pill(row);
                    let lifted = crate::sidebar::selection_fill_on_raised(theme);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        lifted,
                        None,
                        &RoundedRect::from_rect(pill, radius),
                    );
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge_on(lifted),
                        None,
                        &edge_outline(pill, radius),
                    );
                }
                let origin = m.label_origin(row, *depth);
                let max_w = (pane.x1 - m.pill_inset_x - m.label_pad_x - origin.x).max(0.0);
                text.draw_text(
                    scene,
                    &page.title,
                    m.text,
                    theme.text_primary,
                    transform * Affine::translate((origin.x, origin.y)),
                    Some(max_w as f32),
                );
            }
        }
        self.panel.draw(
            scene,
            transform,
            text,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        let selected = match command {
            PreviewNavigation::Scroll { dy, .. } if dy.is_finite() => {
                self.view.scroll.1 = (self.view.scroll.1 - dy).max(0.0);
                self.panel.restore_scroll(self.view.scroll.1);
                return true;
            }
            PreviewNavigation::SelectPage(id) | PreviewNavigation::Reveal(id) => id.parse().ok(),
            PreviewNavigation::NextPage => self
                .pages
                .iter()
                .position(|(id, _)| Some(*id) == self.workspace.selected())
                .and_then(|i| self.pages.get(i + 1))
                .map(|(id, _)| *id),
            PreviewNavigation::PreviousPage => self
                .pages
                .iter()
                .position(|(id, _)| Some(*id) == self.workspace.selected())
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| self.pages.get(i))
                .map(|(id, _)| *id),
            _ => return false,
        };
        let Some(id) = selected.filter(|id| self.workspace.get(*id).is_some()) else {
            return false;
        };
        self.view.scroll = (0.0, 0.0);
        self.select(Some(id)).is_ok()
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let pages = self
            .pages
            .iter()
            .filter_map(|(id, depth)| {
                self.workspace.get(*id).map(|p| {
                    (
                        id.to_string(),
                        format!("{}{}", "  ".repeat(*depth), p.title),
                    )
                })
            })
            .collect();
        let description = self
            .workspace
            .selected()
            .and_then(|id| self.workspace.get(id))
            .map_or_else(
                || "Empty notebook".into(),
                |page| {
                    let blocks = NotesDoc::page_block_snapshots(&self.doc, page.id)
                        .into_iter()
                        .map(|(_, text)| text)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let tags = self
                        .workspace
                        .tags_for_page(page.id)
                        .into_iter()
                        .map(|tag| tag.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}\nTags: {tags}\n{blocks}", page.title)
                },
            );
        PreviewAccessibility { description, pages }
    }
}

#[cfg(test)]
mod tree_geometry_tests {
    use super::*;

    #[test]
    fn history_tree_pane_rows_are_gapped_centred_and_registered_on_every_preset() {
        eprintln!("phase neighbouring_selection_pills_keep_a_token_gap");
        {
            let theme = Theme::dark();
            let m = TreeRowMetrics::new(&theme, 1.0);
            let pane = m.pane(Rect::new(0.0, 0.0, 1280.0, 400.0)).unwrap();
            assert!(
                m.pane(Rect::new(0.0, 0.0, m.min_panel_w - 1.0, 400.0))
                    .is_none(),
                "below the compact breakpoint the tree yields to the page"
            );
            let a = m.pill(m.row(pane, 0));
            let b = m.pill(m.row(pane, 1));
            let gap = b.y0 - a.y1;
            assert!(
                (gap - f64::from(theme.tokens.spacing.s1)).abs() < 1e-9,
                "pill gap {gap} is not s1"
            );
            assert!(
                a.x0 > pane.x0 && a.x1 < pane.x1,
                "pill is inset from the pane sides"
            );
        }

        eprintln!("phase labels_sit_on_their_measured_line_centred_in_the_row");
        {
            let theme = Theme::dark();
            for scale in [1.0_f32, 2.0] {
                let m = TreeRowMetrics::new(&theme, scale);
                let pane = m.pane(Rect::new(0.0, 0.0, 1280.0, 400.0)).unwrap();
                let row = m.row(pane, 3);
                let origin = m.label_origin(row, 1);
                let centre = origin.y + m.line_h / 2.0;
                assert!((centre - row.center().y).abs() < 1e-9, "scale {scale}");
                assert!(m.row_h >= m.line_h + f64::from(theme.tokens.spacing.s2));
                assert!(m.row_h.fract().abs() < 1e-9, "whole-pixel rows at {scale}");
                let depth0 = m.label_origin(row, 0).x;
                assert!((origin.x - depth0 - f64::from(theme.tokens.spacing.s4)).abs() < 1e-9);
            }
        }

        eprintln!("phase tree_pane_edge_and_selection_register_on_every_preset");
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let raised = theme.tokens.colors.surfaces.raised;
                assert!(
                    ui::surface::edge_delta(edge_on(raised), raised)
                        >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}",
                    theme.name
                );
                // The selected page must be a contrast step off the pane, not
                // the pane's own colour with an outline. High contrast paints one
                // flat black tier, so there the fill is the toolkit's own lift
                // fallback (a grey that moves less than a hairline in luminance
                // terms) and the derived edge carries the step.
                let lifted = crate::sidebar::selection_fill_on_raised(&theme);
                let moved = ui::surface::edge_delta(lifted, raised);
                assert!(moved > 0.0, "{} selection fill equals the pane", theme.name);
                if theme.name != "High Contrast" {
                    assert!(
                        moved >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{} selection fill {lifted:?} does not move off raised {raised:?}",
                        theme.name
                    );
                }
                assert!(
                    ui::surface::edge_delta(edge_on(lifted), lifted)
                        >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{} selection edge does not register on its fill",
                    theme.name
                );
            }
        }
    }
}
