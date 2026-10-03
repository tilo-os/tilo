//! Content-first forms on the ordinary retained widget tree.
//!
//! [`FormPage`] owns scrolling and outer gutters. [`Form`], [`FormSection`] and
//! [`Field`] hug their content and also work inside manually composed screens.
//! All optional layout setters accept `None` to restore inheritance. Explicit
//! dimensions on standard children remain owned by those children.
use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, fr, length, minmax, percent};
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{Actions, Element, FormControl, Scrollable, Text, Widget};

use super::composition_parts::{Parts, retained_parts};
mod label;
mod page;
use label::FieldLabel;
pub use page::FormPage;
#[cfg(test)]
mod tests;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

use super::layout_info::{
    Density, Effective, FieldLayout, FormLayout, LabelPlacement, LayoutInfo, LayoutOrigin,
    PageLayout, SectionLayout,
};

fn spacing(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
}
fn field_gap(theme: &Theme, density: Effective<Density>) -> Effective<f32> {
    Effective::new(
        match density.value {
            Density::Compact => theme.tokens.spacing.s3,
            Density::Comfortable => theme.tokens.spacing.s6,
        },
        if density.origin == LayoutOrigin::Pattern {
            LayoutOrigin::Theme
        } else {
            density.origin
        },
    )
}
fn column_style(gap: f32) -> Style {
    Style {
        display: taffy::Display::Grid,
        grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
        grid_auto_rows: vec![taffy::style_helpers::min_content()],
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        min_size: Size {
            width: length(0.0_f32),
            height: length(0.0_f32),
        },
        flex_shrink: 0.0,
        align_content: Some(taffy::AlignContent::Start),
        gap: Size {
            width: length(0.0_f32),
            height: length(gap),
        },
        ..Default::default()
    }
}

/// Intrinsic ordered sections with a trailing standard action region.
#[derive(Default)]
pub struct Form {
    parts: Parts,
    density: Option<Density>,
    section_spacing: Option<f32>,
    field_spacing: Option<f32>,
    label_placement: Option<LabelPlacement>,
    effective: Option<FormLayout>,
    has_actions: bool,
}
impl Form {
    pub fn new() -> Self {
        Self::default()
    }
    #[must_use]
    pub fn section(mut self, section: FormSection) -> Self {
        let index = self
            .parts
            .children
            .len()
            .saturating_sub(usize::from(self.has_actions));
        self.parts.children.insert(index, Box::new(section));
        self
    }
    #[must_use]
    pub fn with_actions(mut self, actions: Actions) -> Self {
        if self.has_actions {
            self.parts.children.pop();
        }
        self.parts.children.push(Box::new(actions));
        self.has_actions = true;
        self
    }
    #[must_use]
    pub fn with_density(mut self, value: Option<Density>) -> Self {
        self.density = value;
        self
    }
    #[must_use]
    pub fn with_section_spacing(mut self, value: Option<f32>) -> Self {
        self.section_spacing = spacing(value);
        self
    }
    #[must_use]
    pub fn with_field_spacing(mut self, value: Option<f32>) -> Self {
        self.field_spacing = spacing(value);
        self
    }
    #[must_use]
    pub fn with_label_placement(mut self, value: Option<LabelPlacement>) -> Self {
        self.label_placement = value;
        self
    }
    pub fn effective_layout(&self) -> Option<FormLayout> {
        self.effective
    }
}

/// A meaningful group. Fields align together; `content` retains an arbitrary
/// standard/manual region at the section edge and in declaration order.
///
/// The title is a `Heading`. A section is never a card: it sits on the
/// ground, separated from the next section by space.
pub struct FormSection {
    parts: Parts,
    /// Settings rows: label leading, control trailing, inset separators.
    grouped: bool,
    density: Option<Density>,
    field_spacing: Option<f32>,
    label_placement: Option<LabelPlacement>,
    inherited: Option<FormLayout>,
    inherited_field_spacing: Option<f32>,
    effective: Option<SectionLayout>,
}
impl FormSection {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            parts: Parts::new(vec![Box::new(
                Text::new(&title.into())
                    .with_role(TextRole::Heading)
                    .with_heading_level(2),
            )]),
            grouped: false,
            density: None,
            field_spacing: None,
            label_placement: None,
            inherited: None,
            inherited_field_spacing: None,
            effective: None,
        }
    }
    /// Lay the fields out as a settings list: each a row at least one
    /// grouped-row high, its label leading and its control trailing, with an
    /// inset separator between rows. The rows still stack their label above
    /// the control when the section is too narrow.
    #[must_use]
    pub fn grouped(mut self) -> Self {
        self.grouped = true;
        self
    }
    #[must_use]
    pub fn field(mut self, field: Field) -> Self {
        self.parts.children.push(Box::new(field));
        self
    }
    #[must_use]
    pub fn content(mut self, content: Element) -> Self {
        self.parts.children.push(content);
        self
    }
    #[must_use]
    pub fn with_density(mut self, value: Option<Density>) -> Self {
        self.density = value;
        self
    }
    #[must_use]
    pub fn with_field_spacing(mut self, value: Option<f32>) -> Self {
        self.field_spacing = spacing(value);
        self
    }
    #[must_use]
    pub fn with_label_placement(mut self, value: Option<LabelPlacement>) -> Self {
        self.label_placement = value;
        self
    }
    pub fn effective_layout(&self) -> Option<SectionLayout> {
        self.effective
    }
}

/// One visible label and its ordinarily retained standard control. The same
/// localization key names both; help and validation are ordinary readable Text.
/// This does not create label/description relationships in AccessKit.
pub struct Field {
    parts: Parts,
    label_placement: Option<LabelPlacement>,
    /// The control keeps its own width and sits at the trailing edge when
    /// the label is beside it (a settings chooser), instead of filling the
    /// control column.
    trailing_control: bool,
    inherited: Option<SectionLayout>,
    effective: Option<FieldLayout>,
    group_needs: Option<(f32, f32)>,
    pair_gap: f32,
    comfortable_control: f32,
}
impl Field {
    pub fn new<C: FormControl + 'static>(label: impl Into<String>, control: C) -> Self {
        let label = label.into();
        Self {
            parts: Parts::new(vec![
                Box::new(FieldLabel {
                    parts: Parts::new(vec![Box::new(Text::new(&label).with_role(TextRole::Label))]),
                    top_inset: 0.0,
                }),
                Box::new(FieldBody {
                    parts: Parts::new(vec![Box::new(control.with_form_accessible_name(&label))]),
                    help: false,
                    error: false,
                    gap: 0.0,
                }),
            ]),
            label_placement: None,
            trailing_control: false,
            inherited: None,
            effective: None,
            group_needs: None,
            pair_gap: 0.0,
            comfortable_control: 0.0,
        }
    }
    #[must_use]
    pub fn with_help(mut self, text: impl Into<String>) -> Self {
        self.set_copy(&text.into(), false);
        self
    }
    #[must_use]
    pub fn with_error(mut self, text: impl Into<String>) -> Self {
        self.set_copy(&text.into(), true);
        self
    }
    fn set_copy(&mut self, text: &str, error: bool) {
        if let Some(body) = self
            .parts
            .children
            .get_mut(1)
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<FieldBody>())
        {
            body.set_copy(text, error);
        }
    }
    #[must_use]
    pub fn with_label_placement(mut self, value: Option<LabelPlacement>) -> Self {
        self.label_placement = value;
        self
    }
    /// A compact chooser (dropdown, segmented switch) that keeps its own
    /// width: it sits at the trailing edge of the row while the label is
    /// beside it, and under the label's leading edge once the section stacks.
    /// The beside/above decision is still the section's measured one, so
    /// every row of a settings card switches together instead of each
    /// deciding for itself. Help and validation copy stay with the control.
    #[must_use]
    pub fn with_trailing_control(mut self) -> Self {
        self.trailing_control = true;
        self
    }
    pub fn effective_layout(&self) -> Option<FieldLayout> {
        self.effective
    }

    fn needs(&mut self, tree: &TaffyTree<()>, text: &mut TextRenderer) -> (f32, f32) {
        let label = self.parts.children.first_mut().map_or(0.0, |child| {
            measure_owned(child.as_mut(), AvailableSpace::MaxContent, text).width
        });
        // The body owns its control directly; help/error do not set its minimum.
        let control_node = self
            .parts
            .nodes
            .get(1)
            .and_then(|&body| tree.children(body).ok())
            .and_then(|children| children.first().copied());
        let Some(control_node) = control_node else {
            return (label, self.comfortable_control);
        };
        let Some(body) = self.parts.children.get_mut(1) else {
            return (label, self.comfortable_control);
        };
        let mut controls = body.children_mut();
        let Some(control) = controls.first_mut() else {
            return (label, self.comfortable_control);
        };
        let measured = measure_owned(*control, AvailableSpace::MinContent, text).width;
        let (declared, expandable) = tree.style(control_node).map_or((0.0, true), |style| {
            let fixed = style.size.width.into_option();
            let minimum = style.min_size.width.into_option().unwrap_or(0.0);
            (fixed.unwrap_or(0.0).max(minimum), fixed.is_none())
        });
        let minimum = measured.max(declared);
        (
            label,
            if expandable {
                minimum.max(self.comfortable_control)
            } else {
                minimum
            },
        )
    }
}

fn measure_owned(
    child: &mut dyn Widget,
    width: AvailableSpace,
    text: &mut TextRenderer,
) -> Size<f32> {
    let binding = child.callback_binding();
    let Ok(_entry) = binding
        .as_ref()
        .map(crate::bound_dispatch::CallbackBinding::enter)
        .transpose()
    else {
        return Size::ZERO;
    };
    child.measure(
        Size {
            width: None,
            height: None,
        },
        Size {
            width,
            height: AvailableSpace::MaxContent,
        },
        text,
    )
}

// A small retained support-copy stack. This is ordinary host layout, not a
// private measurement tree; its children appear in focus/accessibility traversal.
struct FieldBody {
    parts: Parts,
    help: bool,
    error: bool,
    gap: f32,
}
impl FieldBody {
    fn set_copy(&mut self, text: &str, error: bool) {
        let index = if error {
            1_usize.saturating_add(usize::from(self.help))
        } else {
            1
        };
        let copy = Text::new(text).with_role(TextRole::Caption);
        let copy: Element = Box::new(if error {
            copy.with_live(crate::a11y::Politeness::Polite)
        } else {
            copy.dimmed()
        });
        if (error && self.error) || (!error && self.help) {
            if let Some(slot) = self.parts.children.get_mut(index) {
                *slot = copy;
            }
        } else {
            self.parts
                .children
                .insert(index.min(self.parts.children.len()), copy);
        }
        if error {
            self.error = true;
        } else {
            self.help = true;
        }
    }
}

impl Widget for Form {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Form)
    }
    retained_parts!("Form");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let density = self.density.map_or_else(
            || Effective::new(Density::Comfortable, LayoutOrigin::Pattern),
            |value| Effective::new(value, LayoutOrigin::Form),
        );
        let effective = FormLayout {
            density,
            section_spacing: self.section_spacing.map_or_else(
                || {
                    Effective::new(
                        match density.value {
                            Density::Compact => theme.tokens.spacing.s6,
                            Density::Comfortable => theme.tokens.spacing.s8,
                        },
                        if self.density.is_some() {
                            LayoutOrigin::Form
                        } else {
                            LayoutOrigin::Theme
                        },
                    )
                },
                |value| Effective::new(value, LayoutOrigin::Form),
            ),
            field_spacing: self.field_spacing.map_or_else(
                || field_gap(theme, density),
                |value| Effective::new(value, LayoutOrigin::Form),
            ),
            label_placement: self.label_placement.map_or_else(
                || Effective::new(LabelPlacement::Automatic, LayoutOrigin::Pattern),
                |value| Effective::new(value, LayoutOrigin::Form),
            ),
        };
        self.effective = Some(effective);
        for child in &mut self.parts.children {
            if let Some(section) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FormSection>())
            {
                section.inherited = Some(effective);
                section.inherited_field_spacing = self.field_spacing;
            }
        }
        let node = self.parts.build(
            column_style(effective.section_spacing.value),
            tree,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
impl Widget for FormSection {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Section)
    }
    retained_parts!("FormSection");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let density = self.density.map_or_else(
            || {
                self.inherited.map_or_else(
                    || Effective::new(Density::Comfortable, LayoutOrigin::Pattern),
                    |parent| parent.density,
                )
            },
            |value| Effective::new(value, LayoutOrigin::Section),
        );
        let effective = SectionLayout {
            density,
            field_spacing: self.field_spacing.map_or_else(
                || {
                    self.inherited_field_spacing.map_or_else(
                        || {
                            if self.density.is_some() {
                                field_gap(theme, density)
                            } else {
                                self.inherited.map_or_else(
                                    || field_gap(theme, density),
                                    |parent| parent.field_spacing,
                                )
                            }
                        },
                        |value| Effective::new(value, LayoutOrigin::Form),
                    )
                },
                |value| Effective::new(value, LayoutOrigin::Section),
            ),
            label_placement: self.label_placement.map_or_else(
                || {
                    self.inherited.map_or_else(
                        || Effective::new(LabelPlacement::Automatic, LayoutOrigin::Pattern),
                        |parent| parent.label_placement,
                    )
                },
                |value| Effective::new(value, LayoutOrigin::Section),
            ),
        };
        self.effective = Some(effective);
        for child in &mut self.parts.children {
            if let Some(field) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Field>())
            {
                field.inherited = Some(effective);
                field.group_needs = None;
                if self.grouped {
                    field.trailing_control = true;
                }
            }
        }
        let gap = if self.grouped && self.field_spacing.is_none() {
            0.0
        } else {
            effective.field_spacing.value
        };
        let node = self.parts.build(column_style(gap), tree, theme, map);
        if self.grouped {
            // Each field row is at least a grouped row high, its content on
            // the row's centre line; the title keeps its own height and sits
            // a related step above the first row.
            let row = theme.tokens.metrics.panel_row_height + theme.tokens.spacing.s2;
            for (index, (child, &child_node)) in self
                .parts
                .children
                .iter_mut()
                .zip(&self.parts.nodes)
                .enumerate()
            {
                let is_field = child
                    .as_any_mut()
                    .is_some_and(|any| any.downcast_mut::<Field>().is_some());
                if let Ok(style) = tree.style(child_node) {
                    let mut style = style.clone();
                    if is_field {
                        style.min_size.height = length(row);
                        style.align_content = Some(taffy::AlignContent::Center);
                    } else if index == 0 {
                        style.margin.bottom = length(theme.tokens.spacing.rhythm.related);
                    }
                    let _ = tree.set_style(child_node, style);
                }
            }
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn adjust_layout_with_measure(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let mut needs = (0.0_f32, 0.0_f32);
        for child in &mut self.parts.children {
            if let Some(field) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Field>())
            {
                if field
                    .effective
                    .is_some_and(|layout| layout.label_placement.value == LabelPlacement::Above)
                {
                    continue;
                }
                let measured = field.needs(tree, text);
                needs.0 = needs.0.max(measured.0);
                needs.1 = needs.1.max(measured.1);
            }
        }
        let mut changed = false;
        for child in &mut self.parts.children {
            if let Some(field) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Field>())
            {
                changed |= field.group_needs != Some(needs);
                field.group_needs = Some(needs);
            }
        }
        changed
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.grouped {
            // A separator between consecutive field rows, never above the
            // first or below the last.
            let ground = theme.tokens.colors.surfaces.content;
            let rows: Vec<Rect> = self
                .parts
                .children
                .iter_mut()
                .filter_map(|child| {
                    let rect = child.get_rect();
                    child
                        .as_any_mut()
                        .and_then(|any| any.downcast_mut::<Field>())
                        .map(|_| rect)
                })
                .collect();
            for pair in rows.windows(2) {
                if let [above, below] = pair {
                    crate::surface::separator(
                        scene,
                        transform,
                        below.x0,
                        below.x1,
                        f64::midpoint(above.y1, below.y0),
                        0.0,
                        ground,
                    );
                }
            }
        }
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
impl Widget for Field {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Field)
    }
    retained_parts!("Field");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.pair_gap = theme.tokens.spacing.s3;
        // Single-line entry widgets intentionally return zero intrinsic width.
        // Reserve a readable entry measure at the active type size in that case.
        self.comfortable_control =
            theme.tokens.typography.size(crate::tokens::TextRole::Body) * 16.0;
        let placement = self.label_placement.map_or_else(
            || {
                self.inherited.map_or_else(
                    || Effective::new(LabelPlacement::Automatic, LayoutOrigin::Pattern),
                    |parent| parent.label_placement,
                )
            },
            |value| Effective::new(value, LayoutOrigin::Field),
        );
        self.effective = Some(FieldLayout {
            label_placement: placement,
            resolved_placement: LabelPlacement::Above,
            available_width: 0.0,
            beside_required_width: 0.0,
            label_width: 0.0,
        });
        let node = self
            .parts
            .build(column_style(theme.tokens.spacing.s2), tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn adjust_layout_with_measure(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Ok(layout) = tree.layout(node) else {
            return false;
        };
        let width = layout.size.width;
        let (label, control) = self.group_needs.unwrap_or_else(|| self.needs(tree, text));
        let required = label + self.pair_gap + control;
        let Some(effective) = self.effective else {
            return false;
        };
        let beside = match effective.label_placement.value {
            LabelPlacement::Automatic => width >= required,
            LabelPlacement::Above => false,
            LabelPlacement::Beside => true,
        };
        // A trailing control keeps the measured control need as its column
        // (the section's shared need, so sibling choosers line up) and sits
        // at the trailing edge beside its label, or leading once stacked.
        let mut body_changed = false;
        if self.trailing_control
            && let Some(&body) = self.parts.nodes.get(1)
            && let Ok(current) = tree.style(body)
        {
            let mut style = current.clone();
            style.size.width = length(control);
            style.justify_self = Some(if beside {
                taffy::AlignItems::End
            } else {
                taffy::AlignItems::Start
            });
            if *current != style {
                body_changed = tree.set_style(body, style).is_ok();
            }
        }
        let control_height = self
            .parts
            .nodes
            .get(1)
            .and_then(|&body| tree.children(body).ok())
            .and_then(|children| children.first().copied())
            .and_then(|control| tree.layout(control).ok())
            .map_or(0.0, |layout| layout.size.height);
        let mut inset_changed = false;
        if let Some(label) = self.parts.children.first_mut() {
            let height = measure_owned(label.as_mut(), AvailableSpace::MaxContent, text).height;
            let top = if beside {
                ((control_height - height) / 2.0).max(0.0)
            } else {
                0.0
            };
            if let Some(label) = label
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FieldLabel>())
            {
                inset_changed = (label.top_inset - top).abs() > f32::EPSILON;
                label.top_inset = top;
            }
        }
        let Some(effective) = self.effective.as_mut() else {
            return inset_changed;
        };
        effective.resolved_placement = if beside {
            LabelPlacement::Beside
        } else {
            LabelPlacement::Above
        };
        effective.available_width = width;
        effective.beside_required_width = required;
        effective.label_width = label;
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.grid_template_columns = if beside {
            vec![length(label), minmax(length(0.0_f32), fr(1.0_f32))]
        } else {
            vec![minmax(length(0.0_f32), fr(1.0_f32))]
        };
        style.gap.width = length(self.pair_gap);
        if *current == style {
            return inset_changed || body_changed;
        }
        tree.set_style(node, style).is_ok() || inset_changed || body_changed
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
impl Widget for FieldBody {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    retained_parts!("FieldBody");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.gap = theme.tokens.spacing.s1;
        let node = self.parts.build(column_style(self.gap), tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
