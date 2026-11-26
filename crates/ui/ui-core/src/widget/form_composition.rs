//! Content-first forms on the ordinary retained widget tree.
use std::collections::HashMap;

use taffy::style_helpers::{auto, fr, length, minmax, percent};
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use super::{Actions, FormControl, Scrollable, Text};
use tilo_ui_base::{widget::Element, widget::Widget};
use taffy::style_helpers::{line, span};

use tilo_ui_base::{widget::composition_parts::Parts, widget::composition_parts::retained_parts};
mod label;
mod page;
use label::FieldLabel;
pub use page::{FormPage, Page};
#[cfg(test)]
mod tests;

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;

use tilo_ui_base::{widget::layout_info::Density, widget::layout_info::Effective, widget::layout_info::FieldLayout, widget::layout_info::FormLayout, widget::layout_info::LabelPlacement, widget::layout_info::LayoutInfo, widget::layout_info::LayoutOrigin, widget::layout_info::PageLayout, widget::layout_info::SectionLayout};

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
#[tilo_ui_base::component::not_a_component(layout, "the language's `form`")]
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
#[tilo_ui_base::component::not_a_component(
    layout,
    "the language's `form`, one section per `section` in it"
)]
pub struct FormSection {
    parts: Parts,
    has_title: bool,
    grouped: bool,
    density: Option<Density>,
    field_spacing: Option<f32>,
    label_placement: Option<LabelPlacement>,
    inherited: Option<FormLayout>,
    inherited_field_spacing: Option<f32>,
    effective: Option<SectionLayout>,
    form_needs: Option<(f32, f32)>,
    /// The widest label and control of every plain form on this section's page, so the forms of
    /// a page's sections share one label column.
    page_needs: Option<(f32, f32)>,
    beside: bool,
    stacked_gap: f32,
    choosers: super::dropdown::ChooserColumn,
}
impl FormSection {
    pub fn new(title: impl Into<String>) -> Self {
        Self::new_owned(title.into())
    }

    fn new_owned(title: String) -> Self {
        let mut section = Self::untitled();
        section.parts.children.push(Box::new(
            Text::new(&title)
                .with_role(TextRole::Heading)
                .with_heading_level(2),
        ));
        section.has_title = true;
        section
    }
    /// A titled section whose heading line also carries `action` (a master switch, a status
    /// badge, an "Add" button) on its trailing edge.
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
            .push(Box::new(tilo_ui_base::widget::Spacer::new()))
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
            page_needs: None,
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
    /// A row of the section with no label of its own: a `Row` of buttons ("Export",
    /// "Import"), a full-width control.
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

    /// Share one label column among the plain (not grouped) forms `widget` holds within a few
    /// levels (a page's sections and the forms in them): each is told the widest label and
    /// control of them all.
    pub(super) fn share_label_column(
        widgets: &mut [&mut (dyn Widget + '_)],
        tree: &TaffyTree<()>,
        text: &mut TextRenderer,
    ) {
        /// How deep forms are looked for: a page's part, a section, its pane, the form.
        const DEPTH: usize = 6;
        fn each(widget: &mut dyn Widget, depth: usize, f: &mut dyn FnMut(&mut FormSection)) {
            use tilo_ui_base::{widget::capability::capability, widget::capability::capability_mut};
            use tilo_ui_base::{widget::Decorated, widget::DecoratedMut};
            // A wrapper (a language view's pane round the form it shows) answers as itself, and
            // its children are its inner widget's: the form it wraps is reached through it.
            if capability::<dyn Decorated>(widget).is_some() {
                if let Some(decorated) = capability_mut::<dyn DecoratedMut>(widget) {
                    each(decorated.decorated_mut(), depth, f);
                }
                return;
            }
            if let Some(section) = widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FormSection>())
            {
                if !section.grouped {
                    f(section);
                }
                return;
            }
            // A page inside the page keeps a column of its own.
            if depth == 0
                || widget.readable_column().is_some()
                || widget
                    .layout_info()
                    .is_some_and(|info| matches!(info, LayoutInfo::Page(_)))
            {
                return;
            }
            for child in widget.children_mut() {
                each(child, depth - 1, f);
            }
        }
        let mut needs = (0.0_f32, 0.0_f32);
        let mut count = 0_usize;
        for widget in widgets.iter_mut() {
            each(&mut **widget, DEPTH, &mut |section| {
                let own = section.own_needs(tree, text);
                needs = (needs.0.max(own.0), needs.1.max(own.1));
                count += 1;
            });
        }
        let shared = (count > 1).then_some(needs);
        for widget in widgets.iter_mut() {
            each(&mut **widget, DEPTH, &mut |section| {
                section.page_needs = shared;
                section.share_needs();
            });
        }
    }

    /// The widest label and control of its fields, its form's and its page's, from what each
    /// field measured before the solve.
    fn needs(&mut self) -> (f32, f32) {
        let mut needs = (0.0_f32, 0.0_f32);
        for field in self.fields_mut() {
            if field.shares_label_column() {
                needs = (
                    needs.0.max(field.own_needs.0),
                    needs.1.max(field.own_needs.1),
                );
            }
        }
        [self.form_needs, self.page_needs]
            .into_iter()
            .flatten()
            .fold(needs, |needs, shared| {
                (needs.0.max(shared.0), needs.1.max(shared.1))
            })
    }

    /// Tell its fields the widest label and control of the section, its form and its page.
    fn share_needs(&mut self) {
        let needs = self.needs();
        for field in self.fields_mut() {
            field.group_needs = Some(needs);
        }
    }

    /// Whether a settings list `width` wide sets its labels beside their controls.
    fn beside_at(&mut self, width: AvailableSpace, needs: (f32, f32)) -> bool {
        let pair = self.fields_mut().next().map_or(0.0, |field| field.pair_gap);
        match self.effective.map(|e| e.label_placement.value) {
            Some(LabelPlacement::Above) => false,
            Some(LabelPlacement::Beside) => true,
            _ => fits(width, needs.0 + pair + needs.1),
        }
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldKind {
    Control,
    /// Any element (a read-only value, a row of controls): the label is
    /// announced as a label of its own.
    Custom,
    /// Read-only text: reserve a readable phrase, not only its longest word.
    Value,
    Row,
}

/// One visible label and what it labels: a standard control (which takes the label as its
/// accessible name, so it is read once), a custom element, or nothing for a control row.
#[tilo_ui_base::component::not_a_component(layout, "the language's `field`")]
pub struct Field {
    parts: Parts,
    kind: FieldKind,
    has_help: bool,
    label_placement: Option<LabelPlacement>,
    trailing_control: bool,
    /// The control takes the width the label leaves instead of its own
    /// width, even in a grouped section (a read-only value that wraps only
    /// when the row is full).
    filling_control: bool,
    grouped: bool,
    inherited: Option<SectionLayout>,
    effective: Option<FieldLayout>,
    group_needs: Option<(f32, f32)>,
    /// The widest its label and control are, measured before the solve.
    own_needs: (f32, f32),
    /// How tall its label and its control are on one line, measured before the solve: a beside
    /// label is centred on the control's first line.
    label_height: f32,
    control_height: f32,
    pair_gap: f32,
    stacked_gap: f32,
    help_gap: f32,
    first_line: f32,
    /// The least target the window offers a control: a coarse pointer's controls stand taller
    /// than a large control, and a beside label still sits on their line.
    least_target: f32,
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
        let label = label.into();
        let mut field = Self::with_parts(
            FieldKind::Custom,
            Box::new(
                Text::new(&label)
                    .with_role(TextRole::Label)
                    .announced_as_label(),
            ),
            content,
        );
        if let Some(body) = field
            .parts
            .children
            .get_mut(1)
            .and_then(|body| body.as_any_mut())
            .and_then(|any| any.downcast_mut::<FieldBody>())
        {
            body.names_content = (!label.is_empty()).then_some(label);
        }
        field
    }
    /// A read-only setting that fills the space beside its label. Reserve
    /// a readable phrase width, bounded by the theme's chooser measure.
    /// When that pair cannot fit, the section stacks labels above values.
    pub fn value(label: impl Into<String>, value: impl Into<String>) -> Self {
        let mut field = Self::custom(
            label,
            Box::new(
                Text::new(&value.into())
                    .numeric()
                    .dimmed()
                    .with_align(tilo_ui_base::ParagraphAlign::End),
            ),
        )
        .with_filling_control();
        field.kind = FieldKind::Value;
        // The value is read as itself, after its label.
        if let Some(body) = field
            .parts
            .children
            .get_mut(1)
            .and_then(|body| body.as_any_mut())
            .and_then(|any| any.downcast_mut::<FieldBody>())
        {
            body.names_content = None;
        }
        field
    }
    fn row(content: Element) -> Self {
        Self::with_parts(FieldKind::Row, Box::new(super::Absent::new()), content)
    }
    /// Content with no label of its own, across the field's whole width: a form of one
    /// question says what it asks in its title, so its field carries no second label.
    #[must_use]
    pub fn unlabeled(content: Element) -> Self {
        Self::row(content)
    }
    fn with_parts(kind: FieldKind, label: Element, content: Element) -> Self {
        Self {
            parts: Parts::new(vec![
                Box::new(FieldLabel {
                    parts: Parts::new(vec![label]),
                }),
                Box::new(FieldBody {
                    parts: Parts::new(vec![content]),
                    error: false,
                    gap: 0.0,
                    names_content: None,
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
            own_needs: (0.0, 0.0),
            label_height: 0.0,
            control_height: 0.0,
            pair_gap: 0.0,
            stacked_gap: 0.0,
            help_gap: 0.0,
            first_line: 0.0,
            least_target: 0.0,
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
        self.with_error_live(text, tilo_ui_base::a11y::Politeness::Polite)
    }
    /// Announce validation with the urgency of the interaction: polite for live checking,
    /// assertive when an explicit submission is refused.
    #[must_use]
    pub fn with_error_live(
        mut self,
        text: impl Into<String>,
        politeness: tilo_ui_base::a11y::Politeness,
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
    /// A compact chooser (dropdown, segmented switch, switch) that keeps its own width
    /// while the label is beside it: it ends on the trailing edge of a grouped row (a
    /// settings card), and starts on the control column's leading edge in a plain form,
    /// where the fields around it start too.
    #[must_use]
    pub fn with_trailing_control(mut self) -> Self {
        self.trailing_control = true;
        self
    }
    /// The control takes all the width the label leaves, instead of the
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
        if self.has_help
            && let Some(help) = self.parts.children.get_mut(2)
        {
            let help = measure_owned(help.as_mut(), AvailableSpace::MaxContent, text).width;
            label = label.max(help.min(self.comfortable_control));
        }
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
        let mut measured = measure_owned(*control, AvailableSpace::MinContent, text).width;
        if self.kind == FieldKind::Value {
            let natural = measure_owned(*control, AvailableSpace::MaxContent, text).width;
            measured = measured.max(natural.min(self.comfortable_control));
        }
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

/// Whether `required` fits in `width`: a definite width as it is, the widest a node may be
/// (max-content) always, the narrowest (min-content) never.
fn fits(width: AvailableSpace, required: f32) -> bool {
    match width {
        AvailableSpace::Definite(width) => width >= required,
        AvailableSpace::MaxContent => true,
        AvailableSpace::MinContent => false,
    }
}

fn measure_owned(
    child: &mut dyn Widget,
    width: AvailableSpace,
    text: &mut TextRenderer,
) -> Size<f32> {
    let binding = tilo_ui_base::widget::component_walk::binding_of(child);
    let Some(_entry) = tilo_ui_base::widget::component_walk::enter(binding.as_ref()) else {
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

struct FieldBody {
    parts: Parts,
    error: bool,
    gap: f32,
    /// A custom field's label, which names the control its content is (a chooser, a completing
    /// entry) when that has no name of its own, as a label written for a control does; a row
    /// of several controls keeps their own names.
    names_content: Option<String>,
}
impl FieldBody {
    fn set_error(&mut self, text: &str, politeness: tilo_ui_base::a11y::Politeness) {
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
    /// The form's sections share the widest label and control of them all: what they hold says
    /// how wide those are, before any solve.
    fn size_before_solve(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) {
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
        for child in &mut self.parts.children {
            if let Some(section) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<FormSection>())
            {
                section.form_needs = Some(needs);
                section.share_needs();
            }
        }
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}
impl Widget for FormSection {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Section)
    }
    retained_parts!("FormSection", custom_apply_layout);
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
        self.choosers.reset();
        let grouped_list = self.grouped && self.field_spacing.is_none();
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
        let _ = tree.set_restyled_for_width(node, grouped_list);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    /// Its fields are told the widest label and control of the section, from what they measured
    /// (its form and its page, which are asked after it, tell them theirs), and its choosers
    /// take the width they share.
    fn size_before_solve(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        _text: &mut TextRenderer,
    ) {
        self.share_needs();
        for child in &mut self.parts.children {
            self.choosers.fit(child.as_mut(), tree);
        }
    }
    /// A settings list keeps no gap between its rows while their labels stand beside their
    /// controls.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: AvailableSpace,
        _text: &mut TextRenderer,
    ) {
        let needs = self.needs();
        if self.grouped && self.field_spacing.is_none() {
            let beside = self.beside_at(width, needs);
            let node = styles.node();
            let mut style = styles.style(node).clone();
            style.gap.height = length(if beside { 0.0 } else { self.stacked_gap });
            styles.set_style(node, style);
        }
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let width = tree.layout(node).map_or(0.0, |layout| layout.size.width);
        let needs = self.needs();
        self.beside = self.beside_at(AvailableSpace::Definite(width), needs);
        self.parts.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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
                    tilo_ui_base::surface::separator(
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
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}
impl Widget for Field {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Field)
    }
    retained_parts!("Field", custom_apply_layout);
    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.least_target = environment.minimum_target();
        tilo_ui_base::widget::defaults::set_interaction_environment(self.children_mut(), environment);
    }
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
        self.first_line =
            tilo_ui_base::num::f32_of_f64(super::ControlSize::Large.height(theme)).max(self.least_target);
        self.comfortable_control = theme.tokens.metrics.chooser_width
            * tilo_ui_base::tokens::Metrics::text_growth(theme.text_scale);
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
        let _ = tree.set_restyled_for_width(node, true);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    /// What its label and control need, measured from what they show.
    fn size_before_solve(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) {
        self.own_needs = self.needs(tree, text);
        self.label_height = self.parts.children.first_mut().map_or(0.0, |label| {
            measure_owned(label.as_mut(), AvailableSpace::MaxContent, text).height
        });
        let control_node = self
            .parts
            .nodes
            .get(1)
            .and_then(|&body| tree.children(body).ok())
            .and_then(|children| children.first().copied());
        let declared = control_node
            .and_then(|control| tree.style(control).ok())
            .and_then(|style| style.size.height.into_option());
        self.control_height = declared.unwrap_or_else(|| {
            self.parts
                .children
                .get_mut(1)
                .and_then(|body| body.children_mut().into_iter().next())
                .map_or(0.0, |control| {
                    measure_owned(control, AvailableSpace::MaxContent, text).height
                })
        });
    }
    /// Its label beside its control when the width holds both, else above it.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: AvailableSpace,
        _text: &mut TextRenderer,
    ) {
        let (label, control) = self.group_needs.unwrap_or(self.own_needs);
        let beside = self.beside_at(width, label + self.pair_gap + control);
        self.place_parts(styles, beside, label, control);
        let first_line = self.control_height.min(self.first_line.max(1.0));
        let top = if beside && !(self.trailing_control && self.has_help) {
            ((first_line - self.label_height) / 2.0).max(0.0)
        } else {
            0.0
        };
        if let Some(&label) = self.parts.nodes.first() {
            let mut style = styles.style(label).clone();
            style.padding.top = length(top);
            styles.set_style(label, style);
        }
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let width = tree.layout(node).map_or(0.0, |layout| layout.size.width);
        let (label, _) = self.group_needs.unwrap_or(self.own_needs);
        let required = label + self.pair_gap + self.group_needs.unwrap_or(self.own_needs).1;
        let beside = self.beside_at(AvailableSpace::Definite(width), required);
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
        self.parts.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}
impl Field {
    /// Whether a field `width` wide sets its label beside its control, which together need
    /// `required`.
    fn beside_at(&self, width: AvailableSpace, required: f32) -> bool {
        self.kind != FieldKind::Row
            && self
                .effective
                .is_some_and(|effective| match effective.label_placement.value {
                    LabelPlacement::Automatic => fits(width, required),
                    LabelPlacement::Above => false,
                    LabelPlacement::Beside => true,
                })
    }

    fn place_parts(&self, styles: &mut taffy::Restyle<'_>, beside: bool, label: f32, control: f32) {
        fn restyle(styles: &mut taffy::Restyle<'_>, part: NodeId, edit: impl FnOnce(&mut Style)) {
            let mut style = styles.style(part).clone();
            edit(&mut style);
            styles.set_style(part, style);
        }
        let node = styles.node();
        let help = self.has_help;

        if let Some(&part) = self.parts.nodes.first() {
            let display = if self.kind == FieldKind::Row {
                taffy::Display::None
            } else {
                taffy::Display::Grid
            };
            restyle(styles, part, |style| {
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
            restyle(styles, part, |style| {
                (style.grid_row, style.grid_column) = if beside {
                    place(1, if help { 2 } else { 1 }, 2)
                } else {
                    place(stacked_row, 1, 1)
                };
                if trailing && beside {
                    style.size.width = length(control);
                    style.justify_self = Some(edge);
                    style.justify_items = Some(edge);
                } else {
                    style.size.width = auto();
                    style.justify_self = None;
                    style.justify_items = None;
                }
                style.align_self =
                    (trailing && beside && help).then_some(taffy::AlignItems::Center);
            });
        }
        if let Some(&part) = self.parts.nodes.get(2) {
            restyle(styles, part, |style| {
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
        restyle(styles, node, |style| {
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
    }
}
impl Widget for FieldBody {
    fn accessibility_child_name(&self, child_index: usize) -> Option<String> {
        (child_index == 0)
            .then(|| self.names_content.clone())
            .flatten()
    }
    fn focus_reveal_context(&self) -> Option<Rect> {
        self.error.then(|| self.get_rect())
    }
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
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}
