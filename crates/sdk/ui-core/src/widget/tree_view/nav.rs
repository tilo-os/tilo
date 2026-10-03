//! The navigation presentation of a tree: the rows `NavList` shows.
//!
//! A tree in this presentation keeps its whole engine (keyboard, disclosure,
//! selection, semantics, rename, reorder, row actions) and paints every row
//! through [`crate::chrome::paint_nav_row`] and every section header through
//! [`crate::chrome::paint_nav_section`], so a `NavList` and a painted app
//! calling those painters are the same pixels. Hit-testing reads the same
//! geometry ([`crate::chrome::nav_row_layout`],
//! [`crate::chrome::nav_section_layout`]).

use super::{NodeId, Point, Rect, Scene, TextRenderer, Theme, TreeModel, TreeRowKey, TreeView};
use crate::chrome::{
    NavCount, NavRowFace, NavRowStyle, NavSectionFace, SelectionState, nav_rail_row_height,
    nav_row_height, nav_row_layout, nav_section_height, nav_section_layout, paint_nav_row,
    paint_nav_section,
};
use crate::icons::Icon;
use crate::virtual_viewport::SparseVirtualViewport;
use vello::kurbo::Affine;

/// What one row of a navigation tree is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NavRowSpec {
    /// A section header. `first` is the first row of the list, which has
    /// no space above it.
    Section {
        title: String,
        collapsible: bool,
        action: Option<Icon>,
        first: bool,
    },
    /// A destination. `depth` is its nesting below the top level of its
    /// section.
    Item {
        icon: Option<Icon>,
        count: Option<NavCount>,
        depth: usize,
    },
}

type SpecFn<M> = Box<dyn Fn(&M, NodeId) -> Option<NavRowSpec>>;

/// The navigation presentation a `NavList` installs on its tree.
pub(crate) struct NavPresentation<M> {
    pub rail: bool,
    pub spec: SpecFn<M>,
    /// A section header's trailing action was pressed.
    pub on_section_action: Option<Box<dyn FnMut(NodeId)>>,
    /// The theme the list was last laid out with, for geometry read
    /// outside the paint pass (hit-testing).
    pub theme: Theme,
}

impl<M> NavPresentation<M> {
    pub(crate) fn new(rail: bool, spec: SpecFn<M>) -> Self {
        Self {
            rail,
            spec,
            on_section_action: None,
            theme: Theme::dark(),
        }
    }

    fn style(&self) -> NavRowStyle {
        if self.rail {
            NavRowStyle::Rail
        } else {
            NavRowStyle::List
        }
    }
}

impl<T, M: TreeModel> TreeView<T, M> {
    /// Paint this tree as a navigation list.
    pub(crate) fn set_nav_presentation(&mut self, nav: NavPresentation<M>) {
        self.nav = Some(nav);
        // A list stands still when it first appears.
        self.transitions.clear();
        self.refresh_transition_snapshot();
        self.paint_dirty = true;
    }

    pub(crate) fn is_nav(&self) -> bool {
        self.nav.is_some()
    }

    pub(crate) fn nav_spec(&self, id: NodeId) -> Option<NavRowSpec> {
        let nav = self.nav.as_ref()?;
        (nav.spec)(&self.model, id)
    }

    /// Size every row for the navigation presentation: rows at the nav row
    /// height (or the rail tile), section headers with their leading space.
    /// Navigation lists are short, so every row is measured.
    pub(super) fn apply_nav_metrics(&mut self, theme: &Theme) {
        let Some(nav) = self.nav.as_mut() else {
            return;
        };
        nav.theme = theme.clone();
        let row = if nav.rail {
            nav_rail_row_height(theme)
        } else {
            nav_row_height(theme)
        };
        self.viewport.set_estimated_height(row);
        self.refresh_nav_heights();
    }

    /// Re-measure the rows whose height differs from the estimate (section
    /// headers). Called after any change of the visible order.
    pub(super) fn refresh_nav_heights(&mut self) {
        let Some(nav) = self.nav.as_ref() else {
            return;
        };
        let theme = &nav.theme;
        let rail = nav.rail;
        let measurements: Vec<(usize, TreeRowKey, f64)> = (0..self.index.len())
            .filter_map(|index| {
                let (key, _) = self.index.row_at(&self.model, index)?;
                let TreeRowKey::Node(id) = key else {
                    return None;
                };
                match (nav.spec)(&self.model, id)? {
                    NavRowSpec::Section { first, .. } => {
                        let height = if rail {
                            f64::from(theme.tokens.spacing.s3)
                        } else {
                            nav_section_height(theme, first)
                        };
                        Some((index, key, height))
                    }
                    NavRowSpec::Item { .. } => None,
                }
            })
            .collect();
        for (index, key, height) in measurements {
            SparseVirtualViewport::measure(&mut self.viewport, index, key, height.max(1.0));
        }
    }

    /// The face of row `id` as it paints now.
    fn nav_item_face<'a>(
        &self,
        id: NodeId,
        row: Rect,
        spec: &NavRowSpec,
        label: &'a str,
        actions: &'a [Icon],
    ) -> Option<NavRowFace<'a>> {
        let nav = self.nav.as_ref()?;
        let NavRowSpec::Item { icon, count, depth } = spec else {
            return None;
        };
        let key = TreeRowKey::Node(id);
        let mut face = NavRowFace::new(row, label)
            .depth(*depth)
            .state(SelectionState::new(
                self.selection.is_selected(&id),
                self.hovered == Some(key) && !self.reorder.is_tracking(),
                false,
            ))
            .focused(self.is_focused && self.focused == Some(key));
        face.style = nav.style();
        face.icon = *icon;
        face.count = *count;
        if face.style == NavRowStyle::List && self.model.is_branch(id) {
            face = face.disclosure(self.expanded.contains(&id));
        }
        if self.row_actions_visible(key) {
            let hover = self.last_cursor.and_then(|point| {
                self.nav_action_rects(row, actions)
                    .iter()
                    .position(|rect| rect.contains(point))
            });
            face = face.actions(actions, hover);
        }
        Some(face)
    }

    fn nav_section_face<'a>(
        &self,
        id: NodeId,
        row: Rect,
        spec: &'a NavRowSpec,
    ) -> Option<NavSectionFace<'a>> {
        let NavRowSpec::Section {
            title,
            collapsible,
            action,
            ..
        } = spec
        else {
            return None;
        };
        let key = TreeRowKey::Node(id);
        let mut face = NavSectionFace::new(row, title)
            .hovered(self.hovered == Some(key))
            .focused(self.is_focused && self.focused == Some(key));
        if *collapsible {
            face = face.collapsible(self.expanded.contains(&id));
        }
        if let Some(icon) = action {
            face = face.action(*icon);
            if let Some(theme) = self.nav.as_ref().map(|nav| &nav.theme) {
                let layout = nav_section_layout(theme, &face);
                face.action_hover = layout
                    .action
                    .zip(self.last_cursor)
                    .is_some_and(|(rect, point)| rect.contains(point));
            }
        }
        Some(face)
    }

    /// Paint one row of the navigation presentation. Returns `false` when
    /// the row is not a navigation row (the regular painter then runs).
    pub(super) fn draw_nav_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        id: NodeId,
        row: Rect,
    ) -> bool {
        let Some(spec) = self.nav_spec(id) else {
            return false;
        };
        let rail = self.nav.as_ref().is_some_and(|nav| nav.rail);
        match &spec {
            NavRowSpec::Section { .. } => {
                if !rail && let Some(face) = self.nav_section_face(id, row, &spec) {
                    paint_nav_section(scene, transform, text_renderer, theme, &face);
                }
            }
            NavRowSpec::Item { .. } => {
                let label = self.model.label(id).unwrap_or_default();
                let actions: Vec<Icon> =
                    self.row_actions.iter().map(|action| action.icon).collect();
                if let Some(face) = self.nav_item_face(id, row, &spec, label, &actions) {
                    paint_nav_row(scene, transform, text_renderer, theme, &face);
                }
            }
        }
        true
    }

    /// Trailing action squares of a navigation row, as
    /// [`crate::chrome::nav_row_layout`] places them.
    pub(super) fn nav_action_rects(&self, row: Rect, actions: &[Icon]) -> Vec<Rect> {
        let Some(nav) = self.nav.as_ref() else {
            return Vec::new();
        };
        if nav.rail {
            return Vec::new();
        }
        let face = NavRowFace::new(row, "").actions(actions, None);
        // Action geometry does not depend on measured text.
        crate::text_render::with_layout_renderer(nav.theme.text_scale, |text| {
            nav_row_layout(&nav.theme, text, &face).actions
        })
    }

    /// The disclosure of a navigation row: the trailing chevron of an item
    /// with children, the whole header of a collapsible section. `None`
    /// when the tree is not a navigation list (its own caret column
    /// applies); `Some(None)` for a navigation row without one.
    #[expect(
        clippy::option_option,
        reason = "not a navigation row, a navigation row without a disclosure, and one with it are three answers"
    )]
    pub(super) fn nav_disclosure_rect(&self, id: NodeId, row: Rect) -> Option<Option<Rect>> {
        let spec = self.nav_spec(id)?;
        let nav = self.nav.as_ref()?;
        Some(match spec {
            NavRowSpec::Section { collapsible, .. } => (collapsible && !nav.rail).then(|| {
                let face = NavSectionFace::new(row, "");
                nav_section_layout(&nav.theme, &face).band
            }),
            NavRowSpec::Item { .. } if nav.rail || !self.model.is_branch(id) => None,
            NavRowSpec::Item { .. } => {
                let face = NavRowFace::new(row, "").disclosure(self.expanded.contains(&id));
                crate::text_render::with_layout_renderer(nav.theme.text_scale, |text| {
                    nav_row_layout(&nav.theme, text, &face).disclosure
                })
            }
        })
    }

    /// Lay the open rename field over a navigation row's label.
    pub(super) fn place_nav_rename_field(&mut self, text_renderer: &mut TextRenderer) {
        let Some(id) = self.rename.as_ref().map(|field| field.id) else {
            return;
        };
        let (Some(row), Some(spec)) = (self.row_rect(id), self.nav_spec(id)) else {
            return;
        };
        let label = self.model.label(id).unwrap_or_default().to_owned();
        let Some(face) = self.nav_item_face(id, row, &spec, &label, &[]) else {
            return;
        };
        let Some(theme) = self.nav.as_ref().map(|nav| nav.theme.clone()) else {
            return;
        };
        let layout = nav_row_layout(&theme, text_renderer, &face);
        if let Some(field) = self.rename.as_mut() {
            field.input.set_text_role(super::LABEL_ROLE);
            field.input.apply_layout_external(Rect::new(
                layout.label_x0,
                layout.selection.y0,
                layout.label_x1.max(layout.label_x0),
                layout.selection.y1,
            ));
        }
    }

    /// A press on a section header's trailing action.
    pub(super) fn press_nav_section_action(&mut self, point: Point) -> bool {
        let Some(TreeRowKey::Node(id)) = self.row_key_at(point) else {
            return false;
        };
        let Some(
            spec @ NavRowSpec::Section {
                action: Some(_), ..
            },
        ) = self.nav_spec(id)
        else {
            return false;
        };
        let Some(row) = self.row_rect(id) else {
            return false;
        };
        let Some(face) = self.nav_section_face(id, row, &spec) else {
            return false;
        };
        let Some(theme) = self.nav.as_ref().map(|nav| &nav.theme) else {
            return false;
        };
        if !nav_section_layout(theme, &face)
            .action
            .is_some_and(|rect| rect.contains(point))
        {
            return false;
        }
        if let Some(callback) = self
            .nav
            .as_mut()
            .and_then(|nav| nav.on_section_action.as_mut())
        {
            callback(id);
        }
        true
    }
}
