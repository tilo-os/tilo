//! One row of a [`NavList`](super::NavList) as the list scrolls it: a
//! destination, a section header, an empty section's caption or a row still
//! being read.
//!
//! Every row paints through [`crate::chrome::paint_nav_row`] and every
//! section header through [`crate::chrome::paint_nav_section`], so a
//! `NavList` and a painted app calling those painters are the same pixels.
//! The list hit-tests its rows with the same geometry
//! ([`crate::chrome::nav_row_layout`], [`crate::chrome::nav_section_layout`]),
//! through the functions here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::percent;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::model::{NavItemSpec, NavRowData, NavRowKind, NavStatus};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::chrome::{
    NavRowFace, NavRowStyle, NavSectionFace, SelectionState, nav_badge_column, nav_item_height,
    nav_rail_item_height, nav_rail_row_height, nav_row_height, nav_row_layout, nav_section_height,
    nav_section_layout, paint_nav_row, paint_nav_section,
};
use crate::icons::Icon;
use crate::interaction::InteractionEnvironment;
use crate::text_render::TextRenderer;
use crate::tokens::TextRole;
use crate::widget::row_selection::{RowSelection, RowSelectionSink};
use crate::widget::{RowActionRects, RowRenameField, TaffyTreeExt, Theme, Widget};

/// What every row of one list paints from beyond its own data: kept by the
/// list, read by its rows at paint time.
pub(crate) struct NavPaint {
    /// The rows paint as a rail's tiles (icon over caption).
    pub rail: bool,
    /// The item actions' icons and what each is called.
    pub actions: Vec<(Icon, String)>,
    /// The theme the list was last laid out with, for geometry read outside
    /// the paint pass (hit-testing, semantics).
    pub theme: Theme,
    /// The list's width, which a rail tile's caption wraps in.
    pub width: f64,
    /// The row under the pointer, and the pointer.
    pub hovered: Option<(String, Point)>,
    /// The row renamed in place: its label gives way to the field.
    pub renaming: Option<String>,
    /// The row holding the keyboard while the list has it.
    pub focused: Option<String>,
    /// The row's own control holding the keyboard (its semantic target:
    /// `section-action:…`, `row-action:…`, `check:…`), which Enter and Space
    /// press.
    pub focused_control: Option<String>,
    /// A row is being dragged: no row shows its hover.
    pub dragging: bool,
    /// The input the list was last laid out for: a trailing action's hit
    /// target grows to the smallest target that input takes, as every
    /// compact control's does, while its painted square keeps its size.
    pub interaction: InteractionEnvironment,
    /// The check marks as they move.
    pub faces: crate::widget::FaceMotions,
}

impl Default for NavPaint {
    fn default() -> Self {
        Self {
            rail: false,
            actions: Vec::new(),
            theme: Theme::dark(),
            width: 0.0,
            hovered: None,
            renaming: None,
            focused: None,
            focused_control: None,
            dragging: false,
            interaction: InteractionEnvironment::default(),
            faces: crate::widget::FaceMotions::default(),
        }
    }
}

impl NavPaint {
    fn style(&self) -> NavRowStyle {
        if self.rail {
            NavRowStyle::Rail
        } else {
            NavRowStyle::List
        }
    }

    /// The pointer, while it is over the row of `key` and no drag runs.
    fn pointer_on(&self, key: &str) -> Option<Point> {
        if self.dragging {
            return None;
        }
        self.hovered
            .as_ref()
            .filter(|(hovered, _)| hovered == key)
            .map(|(_, point)| *point)
    }

    /// The height row `row` stands at.
    pub(crate) fn height(&self, row: &NavRowData) -> f64 {
        let theme = &self.theme;
        let rail = self.rail;
        let height = match &row.kind {
            NavRowKind::Section { first, .. } => {
                if rail {
                    f64::from(theme.tokens.spacing.s3)
                } else {
                    nav_section_height(theme, *first)
                }
            }
            NavRowKind::Item { label, .. } if rail => {
                let width = if self.width > 0.0 {
                    self.width
                } else {
                    f64::from(theme.tokens.metrics.rail_width)
                };
                crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                    nav_rail_item_height(theme, text, label, width)
                })
            }
            NavRowKind::Item { spec, .. } => {
                nav_item_height(theme, spec.secondary.is_some(), spec.level.is_some())
            }
            NavRowKind::Caption { .. } if rail => f64::from(theme.tokens.spacing.s3),
            NavRowKind::Caption { .. } => nav_row_height(theme),
            NavRowKind::Pending { .. } if rail => nav_rail_row_height(theme),
            NavRowKind::Pending { .. } => nav_item_height(theme, false, false),
        };
        // A destination is pressed with a finger where the input is a finger's: the row is as
        // tall as the target that input takes, as every compact control of the toolkit is.
        let height = if matches!(&row.kind, NavRowKind::Item { .. }) {
            height.max(f64::from(self.interaction.minimum_target()))
        } else {
            height
        };
        height.max(1.0)
    }

    /// The face of item row `row` at `rect`, without state.
    pub(crate) fn item_face<'a>(
        &self,
        spec: &'a NavItemSpec,
        rect: Rect,
        label: &'a str,
    ) -> NavRowFace<'a> {
        let mut face = plain_face(spec, rect, label);
        if spec.badge.is_some() {
            let codes: Vec<&str> = spec.section_badges.iter().map(String::as_str).collect();
            face.badge_column = Some(crate::text_render::with_layout_renderer(
                self.theme.text_scale,
                |text| nav_badge_column(&self.theme, text, &codes),
            ));
        }
        face.style = self.style();
        face
    }

    /// The trailing action squares of an item row at `rect`, in order, each
    /// grown to the smallest target the input takes.
    pub(crate) fn action_rects(&self, row: &NavRowData, rect: Rect) -> Vec<Rect> {
        if self.rail || !matches!(row.kind, NavRowKind::Item { actions: true, .. }) {
            return Vec::new();
        }
        let icons: Vec<Icon> = self.actions.iter().map(|(icon, _)| *icon).collect();
        let face = NavRowFace::new(rect, "").actions(&icons, None);
        // Action geometry does not depend on measured text.
        crate::text_render::with_layout_renderer(self.theme.text_scale, |text| {
            let painted = nav_row_layout(&self.theme, text, &face).actions;
            painted.into_iter().map(|rect| self.target(rect)).collect()
        })
    }

    /// The smallest target the input takes, about `rect`'s centre: a finger
    /// needs 48 where a mouse needs 32. A control paints at its own size and
    /// is pressed at this one, as [`InteractionEnvironment::minimum_target_rect`]
    /// has it for every compact control.
    fn target(&self, rect: Rect) -> Rect {
        self.interaction.minimum_target_rect(rect)
    }

    /// The disclosure of a row at `rect`: the trailing chevron of an item
    /// with children, the whole band of a section that folds.
    pub(crate) fn disclosure_rect(&self, row: &NavRowData, rect: Rect) -> Option<Rect> {
        if self.rail {
            return None;
        }
        match &row.kind {
            NavRowKind::Section {
                collapsible: true, ..
            } => Some(nav_section_layout(&self.theme, &NavSectionFace::new(rect, "")).band),
            NavRowKind::Item {
                branch: true, open, ..
            } => {
                let face = NavRowFace::new(rect, "").disclosure(*open);
                crate::text_render::with_layout_renderer(self.theme.text_scale, |text| {
                    nav_row_layout(&self.theme, text, &face).disclosure
                })
            }
            _ => None,
        }
    }

    /// Where a section header's trailing action is pressed at `rect`: its
    /// painted square grown to the smallest target the input takes.
    pub(crate) fn section_action_rect(&self, row: &NavRowData, rect: Rect) -> Option<Rect> {
        let NavRowKind::Section {
            action: Some(icon), ..
        } = &row.kind
        else {
            return None;
        };
        if self.rail {
            return None;
        }
        nav_section_layout(&self.theme, &NavSectionFace::new(rect, "").action(*icon))
            .action
            .map(|rect| self.target(rect))
    }

    /// The lift of row `row` at `rect`: where its hover and selection paint,
    /// and what a menu acting on it marks.
    pub(crate) fn lift_rect(&self, row: &NavRowData, rect: Rect) -> Option<Rect> {
        match &row.kind {
            NavRowKind::Item { label, spec, .. } => {
                let face = self.item_face(spec, rect, label);
                Some(crate::text_render::with_layout_renderer(
                    self.theme.text_scale,
                    |text| nav_row_layout(&self.theme, text, &face).selection,
                ))
            }
            NavRowKind::Section { menu: true, .. } => {
                Some(nav_section_layout(&self.theme, &NavSectionFace::new(rect, "")).band)
            }
            _ => None,
        }
    }
}

/// [`NavPaint::item_face`] without the badge column, which only painting
/// and hit-testing read: what the row says to assistive technology is not
/// made of the width every badge of its section lays out at.
fn plain_face<'a>(spec: &'a NavItemSpec, row: Rect, label: &'a str) -> NavRowFace<'a> {
    let mut face = NavRowFace::new(row, label).depth(spec.depth);
    face.under_section = spec.under_section;
    face.icon = spec.icon;
    face.count = spec.count;
    face.secondary = spec.secondary.as_deref();
    face.meta = spec.meta.as_deref();
    face.status = spec.status.as_ref().map(NavStatus::row_status);
    face.status_label = spec
        .status_label
        .as_ref()
        .map(|(label, tone)| (label.as_str(), *tone));
    face.badge = spec.badge.as_deref();
    face.swatch = spec.swatch;
    face.avatar = spec.art.as_ref().map(|art| {
        crate::widget::AvatarFace::new(&art.name, &art.name)
            .square()
            .neutral()
            .image(art.image.as_ref())
    });
    if let Some(person) = &spec.person {
        let name = person.name.as_deref().unwrap_or(label);
        let mut avatar = crate::widget::AvatarFace::new(name, &person.identity)
            .image(person.image.as_ref())
            .presence_ring();
        if let Some(colour) = person.colour {
            avatar = avatar.presence(colour);
        }
        if person.speaking {
            avatar = avatar.speaking(1.0);
        }
        face.avatar = Some(avatar);
    }
    face.checked = spec.checked;
    face.level = spec.level;
    face.presence = &spec.presence;
    face.unread = spec.unread;
    face
}

/// One mounted row of a navigation list.
pub(crate) struct NavRow {
    row: NavRowData,
    paint: Rc<RefCell<NavPaint>>,
    selection: RowSelection,
    rect: Rect,
    /// What the row last drew of the list's shared state: a change of it
    /// (the pointer, the keyboard) repaints this row alone.
    drawn: std::cell::Cell<Option<NavLook>>,
}

/// What a row shows of the list's shared state.
#[derive(Clone, Copy, PartialEq)]
struct NavLook {
    /// The pointer is over the row: over which of its actions, and over a
    /// header's action.
    pointer: Option<(Option<usize>, bool)>,
    selected: bool,
    focused: bool,
    renaming: bool,
    dragging: bool,
    rail: bool,
}

impl NavRow {
    pub(crate) fn new(
        row: NavRowData,
        paint: Rc<RefCell<NavPaint>>,
        selection: RowSelection,
    ) -> Self {
        Self {
            row,
            paint,
            selection,
            rect: Rect::ZERO,
            drawn: std::cell::Cell::new(None),
        }
    }

    fn look(&self, paint: &NavPaint) -> NavLook {
        let pointer = paint.pointer_on(&self.row.key).map(|point| {
            let action = paint
                .action_rects(&self.row, self.rect)
                .iter()
                .position(|rect| rect.contains(point));
            let section = paint
                .section_action_rect(&self.row, self.rect)
                .is_some_and(|rect| rect.contains(point));
            (action, section)
        });
        NavLook {
            pointer,
            selected: self.selection.selected,
            focused: paint.focused.as_deref() == Some(self.row.key.as_str()),
            renaming: self.renaming(paint),
            dragging: paint.dragging,
            rail: paint.rail,
        }
    }

    fn renaming(&self, paint: &NavPaint) -> bool {
        paint.renaming.as_deref() == Some(self.row.key.as_str())
    }

    fn paint_item(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        paint: &NavPaint,
    ) {
        let NavRowKind::Item {
            label,
            spec,
            branch,
            open,
            actions,
            ..
        } = &self.row.kind
        else {
            return;
        };
        let label = if self.renaming(paint) { "" } else { label };
        let pointer = paint.pointer_on(&self.row.key);
        let mut face = paint
            .item_face(spec, self.rect, label)
            .state(SelectionState::new(
                self.selection.selected,
                pointer.is_some(),
                false,
            ));
        face.check_pose = spec.checked.map(|checked| {
            paint.faces.check(
                &self.row.key,
                checked,
                crate::widget::CheckGlyph::Check,
                theme.motion(),
            )
        });
        if face.style == NavRowStyle::List && *branch {
            face = face.disclosure(*open);
        }
        let icons: Vec<Icon> = paint.actions.iter().map(|(icon, _)| *icon).collect();
        let focused = paint.focused.as_deref() == Some(self.row.key.as_str());
        if *actions
            && !paint.rail
            && !paint.dragging
            && paint.renaming.is_none()
            && (pointer.is_some() || focused)
        {
            let hover = pointer.and_then(|point| {
                paint
                    .action_rects(&self.row, self.rect)
                    .iter()
                    .position(|rect| rect.contains(point))
            });
            face = face.actions(&icons, hover);
        }
        paint_nav_row(scene, transform, text, theme, &face);
    }

    fn paint_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        paint: &NavPaint,
    ) {
        let NavRowKind::Section {
            title,
            collapsible,
            open,
            action,
            ..
        } = &self.row.kind
        else {
            return;
        };
        // The rename field stands where the title was.
        let title = if self.renaming(paint) { "" } else { title };
        let pointer = paint.pointer_on(&self.row.key);
        let mut face = NavSectionFace::new(self.rect, title).hovered(pointer.is_some());
        if *collapsible {
            face = face.collapsible(*open);
        }
        if let Some(icon) = action {
            face = face.action(*icon);
            face.action_hover = paint
                .section_action_rect(&self.row, self.rect)
                .zip(pointer)
                .is_some_and(|(target, point)| target.contains(point));
        }
        paint_nav_section(scene, transform, text, theme, &face);
    }

    /// The row's buttons as semantic children: a section header's trailing
    /// action ("New playlist Library"), an item's actions, its check.
    fn semantic_children(&self, paint: &NavPaint) -> Vec<AccessNode> {
        let key = &self.row.key;
        let focusable = |target: &str| {
            AccessState::new()
                .focusable(true)
                .focused(paint.focused_control.as_deref() == Some(target))
        };
        let mut children = Vec::new();
        match &self.row.kind {
            NavRowKind::Section {
                title,
                action: Some(_),
                action_label,
                ..
            } => {
                let name = match action_label {
                    Some(label) if !title.is_empty() => format!("{label} {title}"),
                    Some(label) => label.clone(),
                    None => title.clone(),
                };
                let target = format!("section-action:{key}");
                let mut node = AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_state(focusable(&target))
                    .with_action_target(target);
                if let Some(rect) = paint.section_action_rect(&self.row, self.rect) {
                    node = node.with_bounds(rect);
                }
                children.push(node);
            }
            NavRowKind::Item { label, spec, .. } => {
                if let Some(checked) = spec.checked {
                    // The box the row paints, as the control it is: a reader
                    // hears it, and a toggle reaches the app's own callback.
                    let target = format!("check:{key}");
                    children.push(
                        AccessNode::new(Role::Checkbox)
                            .with_name(label.clone())
                            .with_state(focusable(&target).checked(checked))
                            .with_action_target(target)
                            .with_bounds(paint.target(self.rect)),
                    );
                }
                for (index, ((_, name), rect)) in paint
                    .actions
                    .iter()
                    .zip(paint.action_rects(&self.row, self.rect))
                    .enumerate()
                {
                    let target = format!("row-action:{index}:{key}");
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("{name} {label}"))
                            .with_state(focusable(&target))
                            .with_action_target(target)
                            .with_bounds(rect),
                    );
                }
            }
            _ => {}
        }
        children
    }

    /// The full title of a row or section header that is elided where it
    /// paints.
    fn elided_label(&self, paint: &NavPaint) -> Option<String> {
        let theme = &paint.theme;
        let row = self.rect;
        crate::text_render::with_layout_renderer(theme.text_scale, |text| match &self.row.kind {
            NavRowKind::Section { title, .. } if !paint.rail => {
                let layout = nav_section_layout(theme, &NavSectionFace::new(row, title));
                let style = theme.text(TextRole::Subheading);
                let (width, _) = text.measure_style(title, None, &style);
                (f64::from(width) > layout.title_x1 - layout.title_x0 + 0.5).then(|| title.clone())
            }
            NavRowKind::Item { label, spec, .. } => {
                let face = paint.item_face(spec, row, label);
                let layout = nav_row_layout(theme, text, &face);
                let room = layout.label_x1 - layout.label_x0;
                let elided = if paint.rail {
                    crate::chrome::nav_row::rail_caption_lines(theme, text, label, room)
                        .last()
                        .is_some_and(|line| line.ends_with('\u{2026}'))
                } else {
                    let style = crate::chrome::nav_row::label_measure_style(theme, face.style);
                    let (width, _) = text.measure_style(label, None, &style);
                    f64::from(width) > room + 0.5
                };
                elided.then(|| label.clone())
            }
            _ => None,
        })
    }
}

impl RowActionRects for NavRow {
    fn row_action_rects(&self) -> Vec<Rect> {
        self.paint.borrow().action_rects(&self.row, self.rect)
    }
}

impl RowSelectionSink for NavRow {
    fn take_selection(&mut self, selection: RowSelection) {
        self.selection = selection;
    }
}

impl RowRenameField for NavRow {
    fn rename_field(&self) -> Option<(Rect, TextRole)> {
        let paint = self.paint.borrow();
        let theme = &paint.theme;
        match &self.row.kind {
            NavRowKind::Item { label, spec, .. } => {
                let face = paint.item_face(spec, self.rect, label);
                let layout = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                    nav_row_layout(theme, text, &face)
                });
                Some((
                    Rect::new(
                        layout.label_x0,
                        layout.selection.y0,
                        layout.label_x1.max(layout.label_x0),
                        layout.selection.y1,
                    ),
                    TextRole::Body,
                ))
            }
            NavRowKind::Section { title, action, .. } => {
                let mut face = NavSectionFace::new(self.rect, title);
                if let Some(icon) = action {
                    face = face.action(*icon);
                }
                let layout = nav_section_layout(theme, &face);
                let x1 = layout
                    .action
                    .map_or(layout.band.x1, |action| action.x0)
                    .max(layout.title_x0);
                Some((
                    Rect::new(layout.title_x0, layout.band.y0, x1, layout.band.y1),
                    TextRole::Subheading,
                ))
            }
            _ => None,
        }
    }
}

impl Widget for NavRow {
    crate::provides!(dyn RowActionRects, dyn RowSelectionSink, dyn RowRenameField);

    /// The input the list lays its rows out for, kept where the list reads
    /// its hit targets: a finger's target is larger than a mouse's, and the
    /// rows that mount from now on are pressed at the same one.
    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        let mut paint = self.paint.borrow_mut();
        if paint.interaction != environment {
            paint.interaction = environment;
            // Which action the pointer is over is a target, not a face.
            self.drawn.set(None);
        }
    }

    /// A row takes its box from the list and the tokens: the width it stands
    /// in, and the height its kind of row has. What it shows (the pointer, the
    /// keyboard, its own data) repaints it and never resizes it, so a frame
    /// laid out again measures nothing here.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a row height in logical pixels; f32 holds it exactly at every UI extent"
        )]
        let height = self.paint.borrow().height(&self.row) as f32;
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: Dimension::length(height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::PresenceCtx,
    ) {
        let paint = self.paint.borrow();
        self.drawn.set(Some(self.look(&paint)));
        match &self.row.kind {
            NavRowKind::Item { .. } => self.paint_item(scene, transform, text, theme, &paint),
            NavRowKind::Section { .. } if !paint.rail => {
                self.paint_section(scene, transform, text, theme, &paint);
            }
            NavRowKind::Pending { .. } if !paint.rail => {
                paint_nav_pending(scene, transform, text, theme, self.rect);
            }
            NavRowKind::Caption { caption, .. } if !paint.rail => {
                paint_nav_caption(scene, transform, text, theme, self.rect, caption);
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        self.drawn.get() != Some(self.look(&self.paint.borrow()))
    }

    fn needs_animation_frame(&self) -> bool {
        self.paint
            .borrow()
            .faces
            .needs_redraw(crate::motion::frame_now())
    }

    /// The row's name, what it explains of itself (the secondary line, the
    /// state) and its check, with its buttons as children.
    fn accessibility(&self) -> Option<AccessNode> {
        let paint = self.paint.borrow();
        let (name, description, checked) = match &self.row.kind {
            NavRowKind::Section { title, .. } => (title.clone(), None, None),
            NavRowKind::Item { label, spec, .. } => {
                let face = plain_face(spec, Rect::ZERO, "");
                (
                    label.clone(),
                    crate::chrome::nav_row_description(&face),
                    spec.checked,
                )
            }
            NavRowKind::Caption { caption, .. } => (caption.clone(), None, None),
            // A row standing in for an item still being read says so, as every
            // skeleton in a window does: a person who reaches it with a
            // keyboard hears what it waits for, not an unnamed control.
            NavRowKind::Pending { .. } => ("Loading".to_owned(), None, None),
        };
        // A destination is an entry of the tree the list is: it says it is one, and whether it
        // is the one the person picked. A header and a caption group what stands under them.
        let item = matches!(self.row.kind, NavRowKind::Item { .. });
        let mut node = AccessNode::new(if item { Role::TreeItem } else { Role::Group })
            .with_name(name)
            .with_children(self.semantic_children(&paint));
        node.description = description;
        node.state.checked = checked;
        if item {
            node.state.selected = self.selection.selected;
        }
        // Where the row stands in the navigation tree: a header heads its
        // items, nested items stand deeper.
        node.level = Some(match &self.row.kind {
            NavRowKind::Section { .. } => 1,
            NavRowKind::Item { spec, .. } => spec.depth.saturating_add(2),
            NavRowKind::Caption { .. } | NavRowKind::Pending { .. } => 2,
        });
        node.state.expanded = match &self.row.kind {
            NavRowKind::Section {
                collapsible: true,
                open,
                ..
            }
            | NavRowKind::Item {
                branch: true, open, ..
            } => Some(*open),
            _ => None,
        };
        // How many things the place holds, where it draws that number: a
        // reader hears the count the row shows beside it.
        if let NavRowKind::Item { spec, .. } = &self.row.kind {
            node.value = spec.count.map(|count| count.value().to_string());
        }
        node.semantic_key = self.row.kind.semantic_key();
        Some(node)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if !self.rect.contains(point) {
            return None;
        }
        let paint = self.paint.borrow();
        if let Some((rect, (_, name))) = paint
            .action_rects(&self.row, self.rect)
            .into_iter()
            .zip(&paint.actions)
            .find(|(rect, _)| rect.contains(point))
        {
            return Some((rect, name.clone()));
        }
        self.elided_label(&paint).map(|label| (self.rect, label))
    }

    fn name(&self) -> &'static str {
        "NavRow"
    }
}

/// A row standing in for an item still being read: its icon and its label
/// where a row puts them, in the skeleton tone.
fn paint_nav_pending(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
) {
    let face = NavRowFace::new(row, "").icon(Icon::HardDrive);
    let layout = nav_row_layout(theme, text_renderer, &face);
    // The tone every skeleton in the window takes, so the stand-ins read as
    // one loading page.
    let tone = crate::widget::skeleton_tone(theme.tokens.colors.surfaces.content);
    let radius = f64::from(theme.tokens.radii.detail);
    let fill = |scene: &mut Scene, rect: Rect| {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tone,
            None,
            &rect.to_rounded_rect(radius),
        );
    };
    if let Some(icon) = layout.icon {
        fill(scene, icon);
    }
    // The label's bar is the line box's x-height band, as a skeleton's is.
    let line = f64::from(theme.text(TextRole::Body).line_height_px());
    let bar = (line * PENDING_BAR_SHARE).round();
    let top = layout.label_top + (line - bar) / 2.0;
    let width = (layout.label_x1 - layout.label_x0) * PENDING_LABEL_SHARE;
    fill(
        scene,
        Rect::new(layout.label_x0, top, layout.label_x0 + width, top + bar),
    );
}

/// How much of the label's line box a pending row's bar fills.
const PENDING_BAR_SHARE: f64 = 0.6;
/// How far along the label's run a pending row's bar goes.
const PENDING_LABEL_SHARE: f64 = 0.7;

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
