//! Notebook History presentation. Only frozen bytes and local navigation enter here.
use crate::{MainPanel, PageId, Workspace, doc::NotesDoc};
use automerge::{AutoCommit, ObjType};
use ui::chrome::{NavRowFace, SelectionState, nav_row_height, paint_nav_row};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
    restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType},
};
use ui::surface::Region;
use ui::{TextRenderer, Theme, Widget};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// The page tree beside the previewed page. It is the navigation region of
/// the preview, as in the live window: the theme's sidebar width, docked from
/// the compact breakpoint up, with the same rows the live page tree has.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TreeRowMetrics {
    /// Window width from which the tree docks beside the page.
    min_panel_w: f64,
    /// Width of the tree pane.
    pane_w: f64,
    /// Where the first row starts below the top of the pane.
    top: f64,
    /// One row, as a navigation row is high in the sidebar's theme.
    row_h: f64,
}

impl TreeRowMetrics {
    fn new(theme: &Theme) -> Self {
        let sidebar = theme.in_region(Region::Sidebar);
        Self {
            min_panel_w: f64::from(theme.tokens.metrics.compact_breakpoint),
            pane_w: f64::from(theme.tokens.metrics.sidebar_width),
            top: f64::from(theme.tokens.spacing.s2),
            row_h: nav_row_height(&sidebar),
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
        let tree_width = TreeRowMetrics::new(theme)
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
        let m = TreeRowMetrics::new(theme);
        if let Some(pane) = m.pane(self.bounds) {
            // The page tree is the navigation region of the preview, the
            // same ground and rows as the live sidebar.
            ui::surface::region(scene, transform, pane, Region::Sidebar, theme);
            let sidebar = theme.in_region(Region::Sidebar);
            for (index, (id, depth)) in self.pages.iter().enumerate() {
                let Some(page) = self.workspace.get(*id) else {
                    continue;
                };
                let row = m.row(pane, index);
                if row.y0 >= pane.y1 {
                    break;
                }
                let selected = self.workspace.selected() == Some(*id);
                paint_nav_row(
                    scene,
                    transform,
                    text,
                    &sidebar,
                    &NavRowFace::new(row, &page.title)
                        .depth(*depth)
                        .state(SelectionState::new(selected, false, false)),
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
    fn history_tree_pane_docks_from_the_compact_breakpoint_and_rows_stack() {
        let theme = Theme::dark();
        let m = TreeRowMetrics::new(&theme);
        let pane = m.pane(Rect::new(0.0, 0.0, 1280.0, 400.0)).unwrap();
        assert!((pane.width() - f64::from(theme.tokens.metrics.sidebar_width)).abs() < 1e-9);
        assert!(
            m.pane(Rect::new(0.0, 0.0, m.min_panel_w - 1.0, 400.0))
                .is_none(),
            "below the compact breakpoint the tree yields to the page"
        );
        let (a, b) = (m.row(pane, 0), m.row(pane, 1));
        assert!((a.y1 - b.y0).abs() < 1e-9, "rows touch");
        assert!(a.y0 >= pane.y0 + f64::from(theme.tokens.spacing.s2));
        assert!(
            (a.width() - pane.width()).abs() < 1e-9,
            "a row is the pane's full band"
        );
    }

    #[test]
    fn history_tree_rows_grow_with_the_text() {
        let regular = TreeRowMetrics::new(&Theme::dark());
        let large = TreeRowMetrics::new(&Theme::dark().with_text_scale(2.0));
        assert!(large.row_h > regular.row_h);
    }
}
