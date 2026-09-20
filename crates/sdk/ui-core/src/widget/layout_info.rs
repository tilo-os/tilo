//! Read-only resolved layout decisions for retained widgets.

/// Coordinated spacing within a form region. Does not shrink control targets.
#[derive(serde::Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
}

/// Automatic labels use measured content needs, never a caller breakpoint.
#[derive(serde::Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelPlacement {
    #[default]
    Automatic,
    Above,
    Beside,
}

/// The nearest source that supplied an effective property, not a source location.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutOrigin {
    Theme,
    Pattern,
    Form,
    Section,
    Field,
    Page,
}

/// A resolved property and its authoring origin, available after layout build.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct Effective<T> {
    pub value: T,
    pub origin: LayoutOrigin,
}
impl<T> Effective<T> {
    pub(super) fn new(value: T, origin: LayoutOrigin) -> Self {
        Self { value, origin }
    }
}

/// Resolved form defaults; local section/field overrides remain independent.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct FormLayout {
    pub density: Effective<Density>,
    pub section_spacing: Effective<f32>,
    pub field_spacing: Effective<f32>,
    pub label_placement: Effective<LabelPlacement>,
}
/// Effective values within one section.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct SectionLayout {
    pub density: Effective<Density>,
    pub field_spacing: Effective<f32>,
    pub label_placement: Effective<LabelPlacement>,
}
/// The label policy and measured reason for its current arrangement.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct FieldLayout {
    pub label_placement: Effective<LabelPlacement>,
    pub resolved_placement: LabelPlacement,
    pub available_width: f32,
    pub beside_required_width: f32,
    pub label_width: f32,
}
/// Page width excludes its single pair of outer gutters.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct PageLayout {
    pub max_content_width: Effective<f32>,
    pub gutter: Effective<f32>,
}

/// Concrete layout policies. Manual layout information can extend this enum.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub enum LayoutInfo {
    Page(PageLayout),
    Form(FormLayout),
    Section(SectionLayout),
    Field(FieldLayout),
    Manual(ManualLayout),
}

/// Actual region allocation, requested child extent, and overflowing content.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct ManualLayout {
    pub allocation: [f32; 2],
    pub requested_bounds: [f32; 4],
    pub content_bounds: [f32; 4],
    pub overflow: bool,
    pub first_conflict: Option<ManualConflict>,
}
/// An unsatisfied manual constraint, e.g. smaller than a child's border/padding.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct ManualConflict {
    pub child: usize,
    pub requested: [f32; 4],
    pub effective: [f32; 4],
}
