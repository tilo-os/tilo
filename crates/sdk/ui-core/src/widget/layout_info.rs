//! Read-only resolved layout decisions for retained widgets.

/// Coordinated spacing within a form region.
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

/// The grid a list row lays its content out on, as the row paints it: where its title starts and
/// ends, where the facts column stands and how wide the room for it is, and whether the title was
/// cut short.
///
/// An audit reads this to compare rows with each other: the titles of one list must stand on one
/// x for each depth, a nesting step must indent by the same gutter the toolkit nests by, and a
/// title cut short while most of the row stands empty is a grid that gave its columns the wrong
/// room.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct RowLayout {
    pub depth: u32,
    pub content_left: f32,
    pub text_x: f32,
    pub text_right: f32,
    pub facts_left: f32,
    pub content_right: f32,
    pub marks_left: Option<f32>,
}

/// One column's header row as the shell laid it out: what its title says against what the row
/// gave it, and what its actions kept of their labels and their place in the row.
///
/// An audit reads this to hold a header to the rule the row lays itself out by: a title is cut
/// short only once every action in the row has given what it can — its label first, then its
/// place in the row for the menu's door — and the app's primary action never leaves the row.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct HeaderRowLayout {
    pub column: &'static str,
    pub title_needs: f32,
    pub title_has: f32,
    pub free_beside_title: f32,
    pub labelled_action: bool,
    pub primary_shown: bool,
}

/// The header rows one shell laid out, one per column it shows.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct HeaderRows {
    pub rows: [Option<HeaderRowLayout>; 4],
}

/// Where a window's shell stood its sidebar against the content (`layout.md`, "Responding
/// to space").
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SidebarStanding {
    None,
    Docked,
    Over,
    Alone,
}

/// One window's shell as its last layout left it: where its sidebar stood, the class of the
/// content beside it, and how the shell held each column's header row.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct ShellLayout {
    pub sidebar: SidebarStanding,
    pub beside: crate::widget::arrange::WidthClass,
    pub needs: crate::widget::arrange::WidthClass,
    pub beside_width: f32,
    pub rows: HeaderRows,
}

/// Where a field's text stands in it, as the field paints it: the band its first line
/// occupies (in window coordinates, scrolled in), how tall that line is, how many rows the
/// text takes, and how many rows the field asked to stand of its own.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct TextLayout {
    pub top: f32,
    pub line: f32,
    /// The rows the text takes, never less than one.
    pub rows: u32,
    pub lines: u32,
    pub band: f32,
    pub document: bool,
}

/// A list as its last layout left it: whether its first row names a group, and the room the
/// list keeps over the first band it draws.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct ListLayout {
    pub heads_rows: bool,
    pub lead: f32,
}

/// Concrete layout policies.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub enum LayoutInfo {
    Page(PageLayout),
    Form(FormLayout),
    Section(SectionLayout),
    Field(FieldLayout),
    Manual(ManualLayout),
    Scroll(ScrollLayout),
    Row(RowLayout),
    List(ListLayout),
    Shell(ShellLayout),
    Text(TextLayout),
}

/// Which way a scroll view lets its content go on past its own box.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Vertical,
    Horizontal,
    Both,
}

/// A scroll view's axis, so a layout audit tells content that scrolls by design from
/// content that overflows.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollLayout {
    pub direction: ScrollDirection,
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
