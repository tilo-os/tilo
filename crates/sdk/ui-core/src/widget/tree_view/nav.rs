//! The navigation presentation of a tree: the rows `NavList` shows.
//!
//! A tree in this presentation keeps its whole engine (keyboard, disclosure,
//! selection, semantics, rename, reorder, row actions) and paints every row
//! through [`crate::chrome::paint_nav_row`] and every section header through
//! [`crate::chrome::paint_nav_section`], so a `NavList` and a painted app
//! calling those painters are the same pixels. Hit-testing reads the same
//! geometry ([`crate::chrome::nav_row_layout`],
//! [`crate::chrome::nav_section_layout`]).

use std::rc::Rc;

use super::{NodeId, Point, Rect, Scene, TextRenderer, Theme, TreeModel, TreeRowKey, TreeView};
use crate::chrome::{
    NavCount, NavRowFace, NavRowStyle, NavSectionFace, SelectionState, nav_badge_column,
    nav_item_height, nav_rail_item_height, nav_rail_row_height, nav_row_height, nav_row_layout,
    nav_section_height, nav_section_layout, paint_nav_row, paint_nav_section,
};
use crate::icons::Icon;
use crate::tokens::TextRole;
use crate::virtual_viewport::SparseVirtualViewport;
use crate::widget::{RowStatus, Tone};
use vello::kurbo::{Affine, Vec2};
use vello::peniko::Color;

/// An owned [`RowStatus`], as a navigation item keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavStatus {
    Active,
    Warning,
    Error,
    Idle,
    Presence(String),
}

impl NavStatus {
    pub(crate) fn of(status: RowStatus<'_>) -> Self {
        match status {
            RowStatus::Active => Self::Active,
            RowStatus::Warning => Self::Warning,
            RowStatus::Error => Self::Error,
            RowStatus::Idle => Self::Idle,
            RowStatus::Presence(id) => Self::Presence(id.to_owned()),
        }
    }

    pub(crate) fn row_status(&self) -> RowStatus<'_> {
        match self {
            Self::Active => RowStatus::Active,
            Self::Warning => RowStatus::Warning,
            Self::Error => RowStatus::Error,
            Self::Idle => RowStatus::Idle,
            Self::Presence(id) => RowStatus::Presence(id),
        }
    }
}

/// Every part of one destination's row, owned.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NavItemSpec {
    pub icon: Option<Icon>,
    pub count: Option<NavCount>,
    pub depth: usize,
    pub under_section: bool,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub status: Option<NavStatus>,
    pub status_label: Option<(String, Tone)>,
    pub badge: Option<String>,
    /// Every badge code of the row's section, which share one column.
    pub section_badges: Rc<Vec<String>>,
    pub swatch: Option<Color>,
    pub checked: Option<bool>,
    pub level: Option<f32>,
    pub presence: Vec<String>,
}

impl NavItemSpec {
    /// The face of this row over `row`, labelled `label`, without state.
    pub(crate) fn face<'a>(&'a self, row: Rect, label: &'a str, theme: &Theme) -> NavRowFace<'a> {
        let mut face = NavRowFace::new(row, label).depth(self.depth);
        face.under_section = self.under_section;
        face.icon = self.icon;
        face.count = self.count;
        face.secondary = self.secondary.as_deref();
        face.meta = self.meta.as_deref();
        face.status = self.status.as_ref().map(NavStatus::row_status);
        face.status_label = self
            .status_label
            .as_ref()
            .map(|(label, tone)| (label.as_str(), *tone));
        face.badge = self.badge.as_deref();
        if self.badge.is_some() {
            let codes: Vec<&str> = self.section_badges.iter().map(String::as_str).collect();
            face.badge_column = Some(crate::text_render::with_layout_renderer(
                theme.text_scale,
                |text| nav_badge_column(theme, text, &codes),
            ));
        }
        face.swatch = self.swatch;
        face.checked = self.checked;
        face.level = self.level;
        face.presence = &self.presence;
        face
    }
}

/// What one row of a navigation tree is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NavRowSpec {
    /// A section header. `first` is the first row of the list, which has
    /// no space above it.
    Section {
        title: String,
        collapsible: bool,
        action: Option<Icon>,
        /// What the trailing action is announced as.
        action_label: Option<String>,
        first: bool,
    },
    /// A destination.
    Item(Box<NavItemSpec>),
    /// The one quiet line an empty section shows ("No drafts").
    Caption,
}

type SpecFn<M> = Box<dyn Fn(&M, NodeId) -> Option<NavRowSpec>>;

/// Where a row's menu was asked for: the row, its lift, and the point the
/// menu opens at.
pub(crate) type NavMenuFn = Box<dyn FnMut(NodeId, Rect, Point)>;

/// The navigation presentation a `NavList` installs on its tree.
pub(crate) struct NavPresentation<M> {
    pub rail: bool,
    pub spec: SpecFn<M>,
    /// A section header's trailing action was pressed.
    pub on_section_action: Option<Box<dyn FnMut(NodeId)>>,
    /// A checkable row was toggled.
    pub on_check: Option<Box<dyn FnMut(NodeId)>>,
    /// A row's menu was asked for (right click, long press, the Menu key, a
    /// row action that opens it).
    pub on_menu: Option<NavMenuFn>,
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
            on_check: None,
            on_menu: None,
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
    /// Paint this tree as a navigation list. The rows take the navigation
    /// metrics at once, so sections handed over before the first layout
    /// already stand at their own heights.
    pub(crate) fn set_nav_presentation(&mut self, nav: NavPresentation<M>) {
        let theme = nav.theme.clone();
        self.nav = Some(nav);
        self.apply_nav_metrics(&theme);
        // A list stands still when it first appears.
        self.transitions.clear();
        self.refresh_transition_snapshot();
        self.paint_dirty = true;
    }

    pub(crate) fn nav_mut(&mut self) -> Option<&mut NavPresentation<M>> {
        self.nav.as_mut()
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
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        self.refresh_nav_heights();
    }

    /// Measure every row of the visible order at its own height: section
    /// headers with their leading space, rows with a secondary line or a
    /// usage bar taller, rail tiles whose caption wraps a line taller.
    /// Called after any change of the visible order.
    pub(super) fn refresh_nav_heights(&mut self) {
        let Some(nav) = self.nav.as_ref() else {
            return;
        };
        let theme = &nav.theme;
        let rail = nav.rail;
        let rail_width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            f64::from(theme.tokens.metrics.rail_width)
        };
        let measurements: Vec<(usize, TreeRowKey, f64)> =
            crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                (0..self.index.len())
                    .filter_map(|index| {
                        let (key, _) = self.index.row_at(&self.model, index)?;
                        let TreeRowKey::Node(id) = key else {
                            return None;
                        };
                        let height = match (nav.spec)(&self.model, id)? {
                            NavRowSpec::Section { first, .. } => {
                                if rail {
                                    f64::from(theme.tokens.spacing.s3)
                                } else {
                                    nav_section_height(theme, first)
                                }
                            }
                            NavRowSpec::Item(_) if rail => nav_rail_item_height(
                                theme,
                                text,
                                self.model.label(id).unwrap_or_default(),
                                rail_width,
                            ),
                            NavRowSpec::Item(item) => nav_item_height(
                                theme,
                                item.secondary.is_some(),
                                item.level.is_some(),
                            ),
                            NavRowSpec::Caption if rail => f64::from(theme.tokens.spacing.s3),
                            NavRowSpec::Caption => nav_row_height(theme),
                        };
                        Some((index, key, height))
                    })
                    .collect()
            });
        for (index, key, height) in measurements {
            SparseVirtualViewport::measure(&mut self.viewport, index, key, height.max(1.0));
        }
    }

    /// The face of row `id` as it paints now.
    fn nav_item_face<'a>(
        &self,
        id: NodeId,
        row: Rect,
        item: &'a NavItemSpec,
        label: &'a str,
        actions: &'a [Icon],
    ) -> Option<NavRowFace<'a>> {
        let nav = self.nav.as_ref()?;
        let key = TreeRowKey::Node(id);
        let mut face = item.face(row, label, &nav.theme).state(SelectionState::new(
            self.selection.is_selected(&id),
            self.hovered == Some(key) && !self.reorder.is_tracking(),
            false,
        ));
        face = face.focused(self.is_focused && self.focused == Some(key));
        face.style = nav.style();
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
            NavRowSpec::Item(item) => {
                let label = self.model.label(id).unwrap_or_default();
                let actions: Vec<Icon> =
                    self.row_actions.iter().map(|action| action.icon).collect();
                if let Some(face) = self.nav_item_face(id, row, item, label, &actions) {
                    paint_nav_row(scene, transform, text_renderer, theme, &face);
                }
            }
            NavRowSpec::Caption => {
                if !rail {
                    paint_nav_caption(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        row,
                        self.model.label(id).unwrap_or_default(),
                    );
                }
            }
        }
        true
    }

    /// The lift of item `id`'s row as it paints: where its hover and
    /// selection show, and what a menu acting on it marks.
    pub(crate) fn nav_lift_rect(&self, id: NodeId) -> Option<Rect> {
        let nav = self.nav.as_ref()?;
        let row = self.row_rect(id)?;
        let NavRowSpec::Item(item) = self.nav_spec(id)? else {
            return None;
        };
        let label = self.model.label(id).unwrap_or_default();
        let mut face = item.face(row, label, &nav.theme);
        face.style = nav.style();
        Some(crate::text_render::with_layout_renderer(
            nav.theme.text_scale,
            |text| nav_row_layout(&nav.theme, text, &face).selection,
        ))
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
            NavRowSpec::Caption => None,
            NavRowSpec::Item(_) if nav.rail || !self.model.is_branch(id) => None,
            NavRowSpec::Item(_) => {
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
        let (Some(row), Some(NavRowSpec::Item(item))) = (self.row_rect(id), self.nav_spec(id))
        else {
            return;
        };
        let label = self.model.label(id).unwrap_or_default().to_owned();
        let Some(face) = self.nav_item_face(id, row, &item, &label, &[]) else {
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

    /// Section `id`'s trailing action as a semantic button, named with the
    /// section's title ("New playlist Library").
    pub(super) fn nav_section_action_node(
        &self,
        id: NodeId,
        title: &str,
        row: Option<Rect>,
    ) -> Option<crate::a11y::AccessNode> {
        let Some(NavRowSpec::Section {
            action: Some(_),
            action_label,
            ..
        }) = self.nav_spec(id)
        else {
            return None;
        };
        let name = match action_label {
            Some(label) if !title.is_empty() => format!("{label} {title}"),
            Some(label) => label,
            None => title.to_owned(),
        };
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Button)
            .with_name(name)
            .with_state(crate::a11y::AccessState::new().focusable(true))
            .with_action_target(format!("section-action:{id}"));
        if let (Some(row), Some(theme)) = (row, self.nav.as_ref().map(|nav| &nav.theme)) {
            let face = NavSectionFace::new(row, title).action(Icon::Plus);
            if let Some(rect) = nav_section_layout(theme, &face).action {
                node = node.with_bounds(rect);
            }
        }
        Some(node)
    }

    /// Run section `id`'s trailing action (from assistive technology).
    pub(super) fn trigger_nav_section_action(&mut self, id: NodeId) -> bool {
        if !matches!(
            self.nav_spec(id),
            Some(NavRowSpec::Section {
                action: Some(_),
                ..
            })
        ) {
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

    /// The nearest row from `index` on, in the direction of travel, that
    /// is not an empty section's caption: the arrow keys pass over the
    /// captions, which say something but do nothing. Stays at `index` when
    /// there is none that way.
    pub(super) fn skip_nav_captions(&self, index: usize, forward: bool) -> usize {
        if !self.is_nav() {
            return index;
        }
        let is_caption = |at: usize| {
            matches!(
                self.index.row_at(&self.model, at),
                Some((TreeRowKey::Node(id), _)) if matches!(self.nav_spec(id), Some(NavRowSpec::Caption | NavRowSpec::Section { collapsible: false, .. }))
            )
        };
        let mut at = index;
        while is_caption(at) {
            let next = if forward {
                at.checked_add(1).filter(|next| *next < self.index.len())
            } else {
                at.checked_sub(1)
            };
            match next {
                Some(next) => at = next,
                None => return index,
            }
        }
        at
    }

    /// Whether `id` is a checkable navigation row.
    pub(super) fn is_nav_checkable(&self, id: NodeId) -> bool {
        matches!(self.nav_spec(id), Some(NavRowSpec::Item(item)) if item.checked.is_some())
    }

    /// Toggle a checkable navigation row: the whole row is the checkbox's
    /// target. Returns whether `id` is one.
    pub(super) fn toggle_nav_check(&mut self, id: NodeId) -> bool {
        if !self.is_nav_checkable(id) {
            return false;
        }
        if let Some(callback) = self.nav.as_mut().and_then(|nav| nav.on_check.as_mut()) {
            callback(id);
        }
        self.paint_dirty = true;
        true
    }

    /// Whether the navigation list reports row menus.
    pub(super) fn nav_offers_menu(&self) -> bool {
        self.nav.as_ref().is_some_and(|nav| nav.on_menu.is_some())
    }

    /// Ask the host for item `id`'s menu at `at`, the row marked by its
    /// lift. Returns whether the list reports menus for the row.
    pub(super) fn open_nav_menu(&mut self, id: NodeId, at: Point) -> bool {
        if !self.nav_offers_menu() || !matches!(self.nav_spec(id), Some(NavRowSpec::Item(_))) {
            return false;
        }
        let Some(lift) = self.nav_lift_rect(id) else {
            return false;
        };
        self.focused = Some(TreeRowKey::Node(id));
        self.rename = None;
        if let Some(callback) = self.nav.as_mut().and_then(|nav| nav.on_menu.as_mut()) {
            callback(id, lift, at);
        }
        true
    }

    /// The full title of a row or section header that is elided where it
    /// paints, as its tooltip.
    pub(crate) fn nav_tooltip(&self, id: NodeId, row: Rect) -> Option<(Rect, String)> {
        let nav = self.nav.as_ref()?;
        let label = self.model.label(id)?;
        let theme = &nav.theme;
        let elided = crate::text_render::with_layout_renderer(theme.text_scale, |text| match self
            .nav_spec(id)?
        {
            NavRowSpec::Section { .. } if !nav.rail => {
                let face = NavSectionFace::new(row, label);
                let layout = nav_section_layout(theme, &face);
                let style = theme.text(TextRole::Subheading);
                let (width, _) = text.measure_style(label, None, &style);
                Some(f64::from(width) > layout.title_x1 - layout.title_x0 + 0.5)
            }
            NavRowSpec::Item(item) => {
                let mut face = item.face(row, label, theme);
                face.style = nav.style();
                let layout = nav_row_layout(theme, text, &face);
                let style = crate::chrome::nav_row::label_measure_style(theme, face.style);
                let (width, _) = text.measure_style(label, None, &style);
                let room = layout.label_x1 - layout.label_x0;
                Some(if nav.rail {
                    crate::chrome::nav_row::rail_caption_lines(theme, text, label, room)
                        .last()
                        .is_some_and(|line| line.ends_with('\u{2026}'))
                } else {
                    f64::from(width) > room + 0.5
                })
            }
            _ => None,
        })?;
        elided.then(|| (row, label.to_owned()))
    }

    /// What assistive technology hears about item `id` beyond its label
    /// (the secondary line, the state), and its check.
    pub(super) fn nav_semantics(&self, id: NodeId) -> Option<(Option<String>, Option<bool>)> {
        let nav = self.nav.as_ref()?;
        let NavRowSpec::Item(item) = self.nav_spec(id)? else {
            return None;
        };
        let face = item.face(Rect::ZERO, "", &nav.theme);
        Some((crate::chrome::nav_row_description(&face), item.checked))
    }

    /// Paint the dragged row lifted off the list, as the row itself.
    pub(super) fn paint_nav_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        id: NodeId,
        ghost: Rect,
    ) -> bool {
        let Some(nav) = self.nav.as_ref() else {
            return false;
        };
        let Some(NavRowSpec::Item(item)) = self.nav_spec(id) else {
            return false;
        };
        let label = self.model.label(id).unwrap_or_default();
        let floating = theme.on_ground(theme.tokens.colors.surfaces.overlay);
        let mut face = item.face(ghost, label, &floating);
        face.style = nav.style();
        let lift = nav_row_layout(&floating, text_renderer, &face).selection;
        let radius = f64::from(theme.tokens.radii.item);
        crate::surface::shadow(
            scene,
            transform,
            lift,
            radius,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &lift.to_rounded_rect(radius),
        );
        paint_nav_row(scene, transform, text_renderer, &floating, &face);
        true
    }
}

/// The one quiet line of an empty section, where its first row would be.
fn paint_nav_caption(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    caption: &str,
) {
    let s2 = f64::from(theme.tokens.spacing.s2);
    let x0 = row.x0 + 2.0 * s2;
    let max_w = (row.x1 - s2 - x0).max(0.0);
    let style = theme.text(TextRole::Caption);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an on-screen width in logical pixels; f32 holds it exactly at every UI extent"
    )]
    let line = text_renderer.elide_to_width_style(caption, max_w as f32, &style);
    let (_, line_h) = text_renderer.measure_style(&line, None, &style);
    let y = crate::text_metrics::centered_text_top(row.y0, row.height(), f64::from(line_h));
    text_renderer.draw_style(
        scene,
        &line,
        theme.tokens.colors.text_tertiary,
        transform.pre_translate(Vec2::new(x0, y)),
        None,
        &style,
    );
}
