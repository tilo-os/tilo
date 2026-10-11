//! Content-first forms on the ordinary retained widget tree.
//!
//! [`FormPage`] owns scrolling and outer gutters. [`Form`], [`FormSection`] and
//! [`Field`] hug their content and also work inside manually composed screens.
//! All optional layout setters accept `None` to restore inheritance. Explicit
//! dimensions on standard children remain owned by those children.
use std::collections::HashMap;

use automerge::ObjId;
use taffy::style_helpers::{auto, fr, length, minmax, percent};
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{Actions, Element, FormControl, Scrollable, Text, Widget};
use taffy::style_helpers::{line, span};

use super::composition_parts::{Parts, retained_parts};
mod label;
mod page;
use label::FieldLabel;
pub use page::{FormPage, Page};
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
        // An automatic block minimum: inside a scroll view a form keeps its
        // content height instead of being squeezed to the viewport.
        min_size: Size {
            width: length(0.0_f32),
            height: auto(),
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
    /// Whether `parts.children[0]` is the section's heading.
    has_title: bool,
    /// Settings rows: label leading, control trailing, inset separators.
    grouped: bool,
    density: Option<Density>,
    field_spacing: Option<f32>,
    label_placement: Option<LabelPlacement>,
    inherited: Option<FormLayout>,
    inherited_field_spacing: Option<f32>,
    effective: Option<SectionLayout>,
    /// The label and control needs of every field of the enclosing
    /// [`Form`], so all its sections share one label column.
    form_needs: Option<(f32, f32)>,
    /// Whether the section's rows currently stand beside their labels.
    beside: bool,
    /// The effective field spacing, the gap between stacked grouped rows.
    stacked_gap: f32,
    /// The width the section's dropdowns share.
    choosers: super::dropdown::ChooserColumn,
}
impl FormSection {
    pub fn new(title: impl Into<String>) -> Self {
        let mut section = Self::untitled();
        section.parts.children.push(Box::new(
            Text::new(&title.into())
                .with_role(TextRole::Heading)
                .with_heading_level(2),
        ));
        section.has_title = true;
        section
    }
    /// A titled section whose heading line also carries `action` (a master
    /// switch, a status badge, an "Add" button) on its trailing edge.
    pub fn with_action(title: impl Into<String>, action: Element) -> Self {
        let mut section = Self::untitled();
        let header = super::Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(
                Text::new(&title.into())
                    .with_role(TextRole::Heading)
                    .with_heading_level(2),
            ))
            .push(Box::new(super::Spacer::new()))
            .push(action);
        section.parts.children.push(Box::new(header));
        section.has_title = true;
        section
    }
    /// A section without a heading: the first group of a page whose title
    /// already says what it is, or a group that continues the one above.
    /// It emits no heading, visible or accessible.
    pub fn untitled() -> Self {
        Self {
            parts: Parts::new(Vec::new()),
            has_title: false,
            grouped: false,
            density: None,
            field_spacing: None,
            label_placement: None,
            inherited: None,
            inherited_field_spacing: None,
            effective: None,
            form_needs: None,
            beside: true,
            stacked_gap: 0.0,
            choosers: super::dropdown::ChooserColumn::default(),
        }
    }
    /// Lay the fields out as a settings list: each a row at least one
    /// grouped-row high, its label leading and its control trailing, with an
    /// inset separator between rows. The rows still stack their label above
    /// the control when the section is too narrow; stacked rows are spaced
    /// like plain fields and their controls take the full width.
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
    /// A row of the section with no label of its own: a `Row` of buttons
    /// ("Export", "Import"), a full-width control. It spans the label and
    /// control columns, and in a grouped section it is one of the rows,
    /// with their height and separators.
    #[must_use]
    pub fn control_row(mut self, content: Element) -> Self {
        self.parts.children.push(Box::new(Field::row(content)));
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

    fn fields_mut(&mut self) -> impl Iterator<Item = &mut Field> {
        self.parts
            .children
            .iter_mut()
            .filter_map(|child| child.as_any_mut()?.downcast_mut::<Field>())
    }

    /// The widest label and control need of this section's fields that may
    /// stand beside their labels.
    fn own_needs(&mut self, tree: &TaffyTree<()>, text: &mut TextRenderer) -> (f32, f32) {
        let mut needs = (0.0_f32, 0.0_f32);
        for field in self.fields_mut() {
            if !field.shares_label_column() {
                continue;
            }
            let measured = field.needs(tree, text);
            needs.0 = needs.0.max(measured.0);
            needs.1 = needs.1.max(measured.1);
        }
        needs
    }
}

/// What a [`Field`] labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldKind {
    /// A standard control that carries the label as its accessible name.
    Control,
    /// Any element (a read-only value, a row of controls): the label is
    /// announced as a label of its own.
    Custom,
    /// No label: a row that spans both columns.
    Row,
}

/// One visible label and what it labels: a standard control (which takes
/// the label as its accessible name, so it is read once), a custom element,
/// or nothing for a control row. Help copy reads under the label while the
/// label stands beside the control, and under the control once stacked;
/// validation copy always stays with the control.
pub struct Field {
    /// `[label, body, help?]`.
    parts: Parts,
    kind: FieldKind,
    has_help: bool,
    label_placement: Option<LabelPlacement>,
    /// The control keeps its own width while the label is beside it (a
    /// chooser), instead of filling the control column: on the trailing edge
    /// of a grouped row, on the column's leading edge in a plain form.
    trailing_control: bool,
    /// The control takes the room the label leaves instead of its own
    /// width, even in a grouped section (a read-only value that wraps only
    /// when the row is full).
    filling_control: bool,
    /// The field is a row of a grouped section.
    grouped: bool,
    inherited: Option<SectionLayout>,
    effective: Option<FieldLayout>,
    group_needs: Option<(f32, f32)>,
    pair_gap: f32,
    stacked_gap: f32,
    help_gap: f32,
    first_line: f32,
    comfortable_control: f32,
}
impl Field {
    pub fn new<C: FormControl + 'static>(label: impl Into<String>, control: C) -> Self {
        let label = label.into();
        Self::with_parts(
            FieldKind::Control,
            Box::new(
                Text::new(&label)
                    .with_role(TextRole::Label)
                    .naming_its_control(),
            ),
            Box::new(control.with_form_accessible_name(&label)),
        )
    }
    /// A label for any element: a read-only value (a `Text`), a `Row` of
    /// controls or buttons. It lines up with the other fields of its form
    /// and follows the same placement rules; the label is announced as a
    /// label, since the element cannot carry it as its name.
    pub fn custom(label: impl Into<String>, content: Element) -> Self {
        Self::with_parts(
            FieldKind::Custom,
            Box::new(
                Text::new(&label.into())
                    .with_role(TextRole::Label)
                    .announced_as_label(),
            ),
            content,
        )
    }
    fn row(content: Element) -> Self {
        Self::with_parts(FieldKind::Row, Box::new(super::Absent::new()), content)
    }
    fn with_parts(kind: FieldKind, label: Element, content: Element) -> Self {
        Self {
            parts: Parts::new(vec![
                Box::new(FieldLabel {
                    parts: Parts::new(vec![label]),
                    top_inset: 0.0,
                }),
                Box::new(FieldBody {
                    parts: Parts::new(vec![content]),
                    error: false,
                    gap: 0.0,
                }),
            ]),
            kind,
            has_help: false,
            label_placement: None,
            trailing_control: false,
            filling_control: false,
            grouped: false,
            inherited: None,
            effective: None,
            group_needs: None,
            pair_gap: 0.0,
            stacked_gap: 0.0,
            help_gap: 0.0,
            first_line: 0.0,
            comfortable_control: 0.0,
        }
    }
    #[must_use]
    pub fn with_help(mut self, text: impl Into<String>) -> Self {
        let copy: Element = Box::new(
            Text::new(&text.into())
                .with_role(TextRole::Caption)
                .dimmed(),
        );
        if self.has_help {
            if let Some(slot) = self.parts.children.get_mut(2) {
                *slot = copy;
            }
        } else {
            self.parts.children.push(copy);
            self.has_help = true;
        }
        self
    }
    #[must_use]
    pub fn with_error(self, text: impl Into<String>) -> Self {
        self.with_error_live(text, crate::a11y::Politeness::Polite)
    }
    /// Announce validation with the urgency of the interaction: polite for
    /// live checking, assertive when an explicit submission is refused.
    #[must_use]
    pub fn with_error_live(
        mut self,
        text: impl Into<String>,
        politeness: crate::a11y::Politeness,
    ) -> Self {
        if let Some(body) = self
            .parts
            .children
            .get_mut(1)
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<FieldBody>())
        {
            body.set_error(&text.into(), politeness);
        }
        self
    }
    #[must_use]
    pub fn with_label_placement(mut self, value: Option<LabelPlacement>) -> Self {
        self.label_placement = value;
        self
    }
    /// A compact chooser (dropdown, segmented switch, switch) that keeps its
    /// own width while the label is beside it: it ends on the trailing edge
    /// of a grouped row (a settings card), and starts on the control
    /// column's leading edge in a plain form, where the fields around it
    /// start too. It takes the full width under the label once the section
    /// stacks. The beside/above decision is still the
    /// section's measured one, so every row of a settings card switches
    /// together instead of each deciding for itself. Help copy stays under
    /// the label and validation copy with the control.
    #[must_use]
    pub fn with_trailing_control(mut self) -> Self {
        self.trailing_control = true;
        self
    }
    /// The control takes all the room the label leaves, instead of the
    /// narrow column a chooser gets, even in a grouped (settings) row: for a
    /// read-only value, which then wraps only when the row is really full.
    #[must_use]
    pub fn with_filling_control(mut self) -> Self {
        self.filling_control = true;
        self
    }
    pub fn effective_layout(&self) -> Option<FieldLayout> {
        self.effective
    }

    /// Whether this field's label takes part in the shared label column.
    fn shares_label_column(&self) -> bool {
        self.kind != FieldKind::Row
            && !self
                .effective
                .is_some_and(|layout| layout.label_placement.value == LabelPlacement::Above)
    }

    fn needs(&mut self, tree: &TaffyTree<()>, text: &mut TextRenderer) -> (f32, f32) {
        let mut label = if self.kind == FieldKind::Row {
            0.0
        } else {
            self.parts.children.first_mut().map_or(0.0, |child| {
                measure_owned(child.as_mut(), AvailableSpace::MaxContent, text).width
            })
        };
        // Help reads under a beside label, so the label column keeps room
        // for it to wrap into a few lines, not a word a line: as wide as the
        // help wants, up to a chooser's measure.
        if self.has_help
            && let Some(help) = self.parts.children.get_mut(2)
        {
            let help = measure_owned(help.as_mut(), AvailableSpace::MaxContent, text).width;
            label = label.max(help.min(self.comfortable_control));
        }
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
            if expandable && (self.kind == FieldKind::Control || self.trailing_control) {
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

/// One placement of a field part in the field's grid.
fn place(
    row: i16,
    rows: u16,
    column: i16,
) -> (
    taffy::Line<taffy::GridPlacement>,
    taffy::Line<taffy::GridPlacement>,
) {
    (
        taffy::Line {
            start: line(row),
            end: span(rows),
        },
        taffy::Line {
            start: line(column),
            end: span(1),
        },
    )
}

// The control and its validation copy. This is ordinary host layout, not a
// private measurement tree; its children appear in focus/accessibility traversal.
struct FieldBody {
    parts: Parts,
    error: bool,
    gap: f32,
}
impl FieldBody {
    fn set_error(&mut self, text: &str, politeness: crate::a11y::Politeness) {
        // Validation is status: the message in danger ink, and the control
        // itself edged in it, so the error reads at a glance.
        let copy: Element = Box::new(
            Text::new(text)
                .with_role(TextRole::Caption)
                .with_live(politeness)
                .danger(),
        );
        if let Some(control) = self.parts.children.first_mut() {
            control.set_invalid(true);
        }
        if self.error {
            if let Some(slot) = self.parts.children.get_mut(1) {
                *slot = copy;
            }
        } else {
            self.parts.children.push(copy);
            self.error = true;
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
                section.form_needs = None;
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
    /// One label column for the whole form: every section lines its
    /// controls up on the widest label of any of them.
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let mut needs = (0.0_f32, 0.0_f32);
        for child in &mut self.parts.children {
            if let Some(section) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FormSection>())
            {
                let own = section.own_needs(tree, text);
                needs.0 = needs.0.max(own.0);
                needs.1 = needs.1.max(own.1);
            }
        }
        let mut changed = false;
        for child in &mut self.parts.children {
            if let Some(section) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FormSection>())
            {
                changed |= section.form_needs != Some(needs);
                section.form_needs = Some(needs);
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
    fn is_layout_dirty(&self) -> bool {
        // A collapsed section can contain no rows. It is still a layout
        // container, whose intrinsic size changes only with its children.
        self.parts
            .children
            .iter()
            .any(|child| child.is_layout_dirty())
    }
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
        let grouped = self.grouped;
        for field in self.fields_mut() {
            field.inherited = Some(effective);
            field.group_needs = None;
            field.grouped = grouped;
            if grouped && !field.filling_control {
                field.trailing_control = true;
            }
        }
        self.stacked_gap = effective.field_spacing.value;
        let gap = if self.grouped && self.field_spacing.is_none() {
            0.0
        } else {
            effective.field_spacing.value
        };
        self.beside = true;
        // The section's dropdowns share one width.
        self.choosers.reset();
        for child in &mut self.parts.children {
            self.choosers.join(child.as_mut());
        }
        let node = self.parts.build(column_style(gap), tree, theme, map);
        if self.grouped {
            // Each field row is at least a grouped row high, its content on
            // the row's centre line; the title keeps its own height and sits
            // a related step above the first row.
            let row = theme.tokens.metrics.panel_row_height + theme.tokens.spacing.s2;
            let has_title = self.has_title;
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
                    } else if index == 0 && has_title {
                        style.margin.bottom = length(theme.tokens.spacing.rhythm.related);
                    }
                    let _ = tree.set_style(child_node, style);
                }
            }
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let own = self.own_needs(tree, text);
        let needs = self
            .form_needs
            .map_or(own, |form| (form.0.max(own.0), form.1.max(own.1)));
        let mut changed = false;
        for field in self.fields_mut() {
            changed |= field.group_needs != Some(needs);
            field.group_needs = Some(needs);
        }
        // Stacked grouped rows are spaced like plain fields: a row height
        // and no gap would run each label into the control above it.
        if self.grouped && self.field_spacing.is_none() {
            let width = tree.layout(node).map_or(0.0, |layout| layout.size.width);
            let pair = self.fields_mut().next().map_or(0.0, |field| field.pair_gap);
            let beside = match self.effective.map(|e| e.label_placement.value) {
                Some(LabelPlacement::Above) => false,
                Some(LabelPlacement::Beside) => true,
                _ => width >= needs.0 + pair + needs.1,
            };
            if beside != self.beside {
                self.beside = beside;
                if let Ok(current) = tree.style(node) {
                    let mut style = current.clone();
                    style.gap.height = length(if beside { 0.0 } else { self.stacked_gap });
                    changed |= tree.set_style(node, style).is_ok();
                }
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
        if self.grouped && self.beside {
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
        self.stacked_gap = theme.tokens.spacing.s2;
        self.help_gap = theme.tokens.spacing.rhythm.tight;
        // A beside label sits on the control's first line: a one-line
        // control is at most a large control high, so a taller one (a
        // multi-line entry, a list) keeps its label on its first line.
        self.first_line = crate::num::f32_of_f64(super::ControlSize::Large.height(theme));
        // Single-line entry widgets intentionally return zero intrinsic width.
        // Reserve the chooser measure, grown with the text, in that case.
        self.comfortable_control = theme.tokens.metrics.chooser_width
            * crate::tokens::Metrics::text_growth(theme.text_scale);
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
            .build(column_style(self.stacked_gap), tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn adjust_layout_style(
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
        let beside = self.kind != FieldKind::Row
            && match effective.label_placement.value {
                LabelPlacement::Automatic => width >= required,
                LabelPlacement::Above => false,
                LabelPlacement::Beside => true,
            };
        let mut changed = self.place_parts(tree, node, beside, label, control);
        let control_height = self
            .parts
            .nodes
            .get(1)
            .and_then(|&body| tree.children(body).ok())
            .and_then(|children| children.first().copied())
            .and_then(|control| tree.layout(control).ok())
            .map_or(0.0, |layout| layout.size.height);
        let first_line = control_height.min(self.first_line.max(1.0));
        if let Some(label) = self.parts.children.first_mut() {
            let height = measure_owned(label.as_mut(), AvailableSpace::MaxContent, text).height;
            // A trailing control with help copy is centred on the whole
            // row instead (see `place_parts`), so its label starts the row.
            let top = if beside && !(self.trailing_control && self.has_help) {
                ((first_line - height) / 2.0).max(0.0)
            } else {
                0.0
            };
            if let Some(label) = label
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FieldLabel>())
            {
                changed |= (label.top_inset - top).abs() > f32::EPSILON;
                label.top_inset = top;
            }
        }
        if let Some(effective) = self.effective.as_mut() {
            effective.resolved_placement = if beside {
                LabelPlacement::Beside
            } else {
                LabelPlacement::Above
            };
            effective.available_width = width;
            effective.beside_required_width = required;
            effective.label_width = label;
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
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
impl Field {
    /// Place the label, body and help copy in the field's grid for the
    /// resolved arrangement. Beside: the label and its help stack in the
    /// label column and the body spans both rows. Stacked: label, body and
    /// help follow each other. Returns whether any style changed.
    fn place_parts(
        &self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        beside: bool,
        label: f32,
        control: f32,
    ) -> bool {
        fn restyle(tree: &mut TaffyTree<()>, part: NodeId, edit: impl FnOnce(&mut Style)) -> bool {
            let Ok(current) = tree.style(part) else {
                return false;
            };
            let mut style = current.clone();
            edit(&mut style);
            *current != style && tree.set_style(part, style).is_ok()
        }
        let help = self.has_help;
        let mut changed = false;
        if let Some(&part) = self.parts.nodes.first() {
            let display = if self.kind == FieldKind::Row {
                taffy::Display::None
            } else {
                taffy::Display::Grid
            };
            changed |= restyle(tree, part, |style| {
                (style.grid_row, style.grid_column) = place(1, 1, 1);
                style.display = display;
            });
        }
        if let Some(&part) = self.parts.nodes.get(1) {
            let trailing = self.trailing_control;
            let edge = if self.grouped {
                taffy::AlignItems::End
            } else {
                taffy::AlignItems::Start
            };
            let stacked_row = if self.kind == FieldKind::Row { 1 } else { 2 };
            changed |= restyle(tree, part, |style| {
                (style.grid_row, style.grid_column) = if beside {
                    place(1, if help { 2 } else { 1 }, 2)
                } else {
                    place(stacked_row, 1, 1)
                };
                // A chooser keeps the shared control need as its column (so
                // sibling choosers line up): beside its label it ends on a
                // grouped row's trailing edge, or starts on a plain form's
                // control edge with the fields; stacked, it takes the full
                // width under the label.
                if trailing && beside {
                    style.size.width = length(control);
                    style.justify_self = Some(edge);
                    style.justify_items = Some(edge);
                } else {
                    style.size.width = auto();
                    style.justify_self = None;
                    style.justify_items = None;
                }
                // Beside a label with help copy, a trailing control (a
                // switch, a chooser) is centred on the whole row, label and
                // help together, as a settings list reads.
                style.align_self =
                    (trailing && beside && help).then_some(taffy::AlignItems::Center);
            });
        }
        if let Some(&part) = self.parts.nodes.get(2) {
            changed |= restyle(tree, part, |style| {
                (style.grid_row, style.grid_column) = place(if beside { 2 } else { 3 }, 1, 1);
            });
        }
        let pair_gap = self.pair_gap;
        let row_gap = if beside {
            self.help_gap
        } else {
            self.stacked_gap
        };
        // A grouped row with help copy beside its control keeps a related
        // step above and below, so a row grown past the list's row height by
        // wrapped help never runs into its separators; stacked, it keeps a
        // tight step under the help.
        // A row that always stacks (a field, a slider) keeps the same step
        // above and below, so the rows around it do not run into its label.
        let always_above = self.grouped
            && self.kind != FieldKind::Row
            && self.label_placement == Some(LabelPlacement::Above);
        let (top, bottom) = match (self.grouped && help, beside) {
            (true, true) => (self.stacked_gap, self.stacked_gap),
            (true, false) => (0.0, self.help_gap),
            (false, _) if always_above => (self.stacked_gap, self.stacked_gap),
            (false, _) => (0.0, 0.0),
        };
        changed |= restyle(tree, node, |style| {
            style.grid_template_columns = if beside {
                vec![length(label), minmax(length(0.0_f32), fr(1.0_f32))]
            } else {
                vec![minmax(length(0.0_f32), fr(1.0_f32))]
            };
            style.gap.width = length(pair_gap);
            style.gap.height = length(row_gap);
            style.padding.top = length(top);
            style.padding.bottom = length(bottom);
        });
        changed
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
