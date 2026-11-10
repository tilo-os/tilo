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
    /// How many rows over it the row stands: each step indents its content one disclosure gutter.
    pub depth: u32,
    /// Where the row's own content starts: its leading control stands here, or its title when it
    /// has none. A nesting step indents it one disclosure gutter.
    pub content_left: f32,
    /// Where the title starts, after the row's leading control.
    pub text_x: f32,
    /// Where the title's room ends: the facts column narrows it.
    pub text_right: f32,
    /// The left edge of what stands in the facts column, or the row's content edge when nothing
    /// does.
    pub facts_left: f32,
    /// The right edge of the row's content, where the facts end.
    pub content_right: f32,
    /// The left edge of the marks a row shows on its edge, when it shows any.
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
    /// The column the row heads: `sidebar`, `overview`, `content` or `inspector`.
    pub column: &'static str,
    /// How wide the title says it is.
    pub title_needs: f32,
    /// How wide the row gave it.
    pub title_has: f32,
    /// How much room stood empty beyond it, before the next thing in the row.
    pub free_beside_title: f32,
    /// Whether an action standing in the row kept a label it could have shown as its icon.
    pub labelled_action: bool,
    /// Whether the column's primary action stands in the row: a column without one answers
    /// `true`.
    pub primary_shown: bool,
}

/// The header rows one shell laid out, one per column it shows.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct HeaderRows {
    pub rows: [Option<HeaderRowLayout>; 4],
}

/// Where a window's shell stood its sidebar against the content (`layout.md`, "Responding to
/// space").
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SidebarStanding {
    /// The window shows no sidebar.
    None,
    /// Beside the content, as a column of the window's own.
    Docked,
    /// Over the content, as a drawer the bar's toggle brings forward.
    Over,
    /// As a screen of the window's own, taking turns with the content.
    Alone,
}

/// One window's shell as its last layout left it: where its sidebar stood, the class of the
/// content beside it, and how the shell held each column's header row.
///
/// An audit reads this to hold the shell to the rule that decides it: a sidebar stands beside a
/// pane of the class the content asks for there ([`ShellLayout::needs`]) and folds behind the bar's
/// toggle where the pane beside it would be a phone's screen instead.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub struct ShellLayout {
    /// How the sidebar stood.
    pub sidebar: SidebarStanding,
    /// The class of the pane standing beside the sidebar, by its own width.
    pub beside: crate::widget::arrange::WidthClass,
    /// The class that pane is in at the least: a column of text asks for a column, a grid of
    /// tiles or a surface drawn on for less.
    pub needs: crate::widget::arrange::WidthClass,
    /// That pane's width in logical pixels; zero where the window shows none.
    pub beside_width: f32,
    /// Each column's header row, `[sidebar, overview, content, inspector]`.
    pub rows: HeaderRows,
}

/// Concrete layout policies. Manual layout information can extend this enum.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
pub enum LayoutInfo {
    Page(PageLayout),
    Form(FormLayout),
    Section(SectionLayout),
    Field(FieldLayout),
    Manual(ManualLayout),
    Scroll(ScrollLayout),
    Row(RowLayout),
    Shell(ShellLayout),
}

/// Which way a scroll view lets its content go on past its own box.
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    /// Down: content taller than the view (a column's body).
    Vertical,
    /// Sideways: content wider than the view (a board's lanes).
    Horizontal,
    /// Both ways (a canvas).
    Both,
}

/// A scroll view's axis, so a layout audit tells content that scrolls by
/// design from content that overflows.
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
